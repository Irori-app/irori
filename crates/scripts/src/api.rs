//! lutris.net client: game search and a game's install scripts, fetched only
//! when the user asks. lutris.net publishes no API terms, so Irori behaves
//! like the Lutris client itself: an honest user agent, at most two
//! requests a second, answers cached on disk for a day, and every script
//! attributed to its author with a link to its page.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::error::ScriptError;
use crate::model::Script;
use crate::parse::scalar;

/// lutris.net.
pub const SITE: &str = "https://lutris.net";
/// How long answers are reused.
pub const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// The shortest time between two requests.
pub const MIN_INTERVAL: Duration = Duration::from_millis(500);

/// A game's page on lutris.net.
pub fn page_url(game_slug: &str) -> String {
    format!("{SITE}/games/{game_slug}/")
}

/// A search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameResult {
    pub id: u64,
    pub name: String,
    pub slug: String,
    pub year: Option<u32>,
    pub platforms: Vec<String>,
    /// Install scripts lutris.net has for the game.
    pub installers: usize,
    pub page_url: String,
}

/// An install script as lutris.net lists it, with its attribution.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallerListing {
    pub id: Option<u64>,
    pub name: String,
    pub game_slug: String,
    pub slug: Option<String>,
    /// The installer's name ("GOG", "Setup"…).
    pub version: String,
    pub runner: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub notes: Option<String>,
    pub credits: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub page_url: String,
    /// The installer object as received, for [`InstallerListing::script`].
    pub raw: Value,
}

impl InstallerListing {
    /// When it was last changed (or published).
    pub fn date(&self) -> Option<&str> {
        self.updated_at.as_deref().or(self.created_at.as_deref())
    }

    /// Parse and validate the script.
    pub fn script(&self) -> Result<Script, ScriptError> {
        Script::from_value(&self.raw)
    }
}

/// The lutris.net client.
pub struct LutrisClient {
    base: String,
    cache_dir: PathBuf,
    http: reqwest::Client,
    min_interval: Duration,
    ttl: Duration,
    last: Mutex<Option<Instant>>,
}

impl LutrisClient {
    /// A client for lutris.net caching under `cache_dir`. Call from within
    /// the Tokio runtime.
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self::with_base(SITE, cache_dir)
    }

    /// A client for another server (tests).
    pub fn with_base(base: impl Into<String>, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            cache_dir: cache_dir.into(),
            http: irori_core::http::client().clone(),
            min_interval: MIN_INTERVAL,
            ttl: CACHE_TTL,
            last: Mutex::new(None),
        }
    }

    pub fn min_interval(mut self, interval: Duration) -> Self {
        self.min_interval = interval;
        self
    }

    pub fn cache_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Games matching `query` that have install scripts.
    pub async fn search(&self, query: &str) -> Result<Vec<GameResult>> {
        let query: String = query.trim().to_lowercase().chars().take(255).collect();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut url = url::Url::parse(&format!("{}/api/games", self.base))
            .context("lutris.net address is not valid")?;
        url.query_pairs_mut()
            .append_pair("search", &query)
            .append_pair("with-installers", "True");
        let key = format!(
            "search-{}",
            &hex::encode(Sha256::digest(query.as_bytes()))[..16]
        );
        let body = self.get_cached(&key, url.as_str()).await?;
        Ok(results(&body).iter().filter_map(game_result).collect())
    }

    /// Every install script lutris.net has for the game `slug`.
    pub async fn installers(&self, slug: &str) -> Result<Vec<InstallerListing>> {
        let slug = slug.trim();
        if slug.is_empty()
            || !slug
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            || slug.starts_with('.')
        {
            bail!("“{slug}” is not a lutris.net game id");
        }
        let url = format!("{}/api/installers/{slug}", self.base);
        let body = self.get_cached(&format!("installers-{slug}"), &url).await?;
        Ok(results(&body)
            .iter()
            .filter_map(installer_listing)
            .collect())
    }

    async fn get_cached(&self, key: &str, url: &str) -> Result<Value> {
        let path = self.cache_dir.join(format!("{key}.json"));
        let cached: Option<Cached> = tokio::fs::read(&path)
            .await
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some(c) = &cached
            && now.saturating_sub(c.fetched) < self.ttl.as_secs()
        {
            return Ok(c.body.clone());
        }
        match self.fetch(url).await {
            Ok(body) => {
                let entry = Cached {
                    fetched: now,
                    body: body.clone(),
                };
                let result = tokio::task::spawn_blocking(move || {
                    irori_core::json_store::write(&path, &entry)
                })
                .await;
                if let Ok(Err(e)) | Err(e) = result.map_err(anyhow::Error::from) {
                    tracing::warn!("could not cache the lutris.net answer: {e:#}");
                }
                Ok(body)
            }
            Err(e) => match cached {
                // Offline: an old answer beats none.
                Some(c) => {
                    tracing::warn!("lutris.net unreachable, using a cached answer: {e:#}");
                    Ok(c.body)
                }
                None => Err(e),
            },
        }
    }

    /// Wait until a request is allowed.
    async fn throttle(&self) {
        let mut last = self.last.lock().await;
        if let Some(t) = *last {
            let next = t + self.min_interval;
            let now = Instant::now();
            if next > now {
                tokio::time::sleep(next - now).await;
            }
        }
        *last = Some(Instant::now());
    }

    async fn fetch(&self, url: &str) -> Result<Value> {
        let mut attempts = 0;
        loop {
            self.throttle().await;
            let resp = self
                .http
                .get(url)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .context("Could not reach lutris.net")?;
            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempts < 2 {
                attempts += 1;
                let wait = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map_or(self.min_interval * 4, Duration::from_secs)
                    .min(Duration::from_secs(10));
                tokio::time::sleep(wait).await;
                continue;
            }
            if status == reqwest::StatusCode::NOT_FOUND {
                return Ok(Value::Object(Default::default()));
            }
            if !status.is_success() {
                bail!("lutris.net answered {status}");
            }
            return resp
                .json()
                .await
                .context("lutris.net sent an answer Irori could not read");
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Cached {
    fetched: u64,
    body: Value,
}

fn results(body: &Value) -> Vec<Value> {
    match body {
        Value::Array(items) => items.clone(),
        Value::Object(m) => match m.get("results") {
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn text(m: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    m.get(key).and_then(scalar).filter(|s| !s.is_empty())
}

fn game_result(v: &Value) -> Option<GameResult> {
    let m = v.as_object()?;
    let slug = text(m, "slug")?;
    let installers = match m.get("installers") {
        Some(Value::Array(items)) => items.len(),
        Some(v) => v.as_u64().unwrap_or(0) as usize,
        None => ["installer_count", "installers_count"]
            .iter()
            .find_map(|k| m.get(*k).and_then(Value::as_u64))
            .unwrap_or(0) as usize,
    };
    Some(GameResult {
        id: m.get("id").and_then(Value::as_u64).unwrap_or(0),
        name: text(m, "name").unwrap_or_else(|| slug.clone()),
        year: m
            .get("year")
            .and_then(|y| y.as_u64().or_else(|| scalar(y)?.parse().ok()))
            .and_then(|y| u32::try_from(y).ok()),
        platforms: m
            .get("platforms")
            .and_then(Value::as_array)
            .map(|ps| {
                ps.iter()
                    .filter_map(|p| match p {
                        Value::Object(o) => text(o, "name"),
                        v => scalar(v),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        installers,
        page_url: page_url(&slug),
        slug,
    })
}

fn installer_listing(v: &Value) -> Option<InstallerListing> {
    let m = v.as_object()?;
    let game_slug = text(m, "game_slug")?;
    let author = match m.get("user").or_else(|| m.get("author")) {
        Some(Value::Object(u)) => text(u, "username"),
        Some(v) => scalar(v),
        None => None,
    };
    Some(InstallerListing {
        id: m.get("id").and_then(Value::as_u64),
        name: text(m, "name").unwrap_or_else(|| game_slug.clone()),
        slug: text(m, "slug"),
        version: text(m, "version").unwrap_or_default(),
        runner: text(m, "runner").unwrap_or_default(),
        author,
        description: text(m, "description"),
        notes: text(m, "notes"),
        credits: text(m, "credits"),
        created_at: text(m, "created_at"),
        updated_at: text(m, "updated_at"),
        page_url: page_url(&game_slug),
        game_slug,
        raw: v.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_depot::test_server::{Resp, TestServer};
    use serde_json::json;

    fn installers_body() -> Value {
        json!({
            "count": 1,
            "results": [{
                "id": 4242,
                "game_slug": "quake",
                "name": "Quake",
                "slug": "quake-dosbox",
                "version": "DOSBox",
                "runner": "dosbox",
                "user": "someone",
                "description": "Shareware",
                "notes": "",
                "created_at": "2020-01-02T03:04:05.000Z",
                "updated_at": "2023-05-06T07:08:09.000Z",
                "script": {
                    "files": [{"zip": "https://example.org/quake.zip"}],
                    "game": {"main_file": "$GAMEDIR/quake.conf"},
                    "installer": [{"extract": {"file": "zip", "dst": "$GAMEDIR"}}]
                }
            }]
        })
    }

    #[tokio::test]
    async fn search_and_installers_are_parsed_and_cached() {
        let server = TestServer::start(|req| {
            if req.path.starts_with("/api/games?") {
                Resp::json(json!({
                    "count": 1,
                    "results": [{
                        "id": 7, "name": "Quake", "slug": "quake", "year": 1996,
                        "platforms": [{"name": "Linux"}, {"name": "Windows"}],
                        "installers": [{"slug": "a"}, {"slug": "b"}]
                    }]
                }))
            } else if req.path == "/api/installers/quake" {
                Resp::json(installers_body())
            } else {
                Resp::status(404)
            }
        });
        let tmp = tempfile::tempdir().unwrap();
        let client = LutrisClient::with_base(server.base(), tmp.path())
            .min_interval(Duration::from_millis(1));

        let games = client.search("  Quake ").await.unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].slug, "quake");
        assert_eq!(games[0].year, Some(1996));
        assert_eq!(games[0].platforms, vec!["Linux", "Windows"]);
        assert_eq!(games[0].installers, 2);
        assert_eq!(games[0].page_url, "https://lutris.net/games/quake/");

        let list = client.installers("quake").await.unwrap();
        assert_eq!(list.len(), 1);
        let i = &list[0];
        assert_eq!(i.author.as_deref(), Some("someone"));
        assert_eq!(i.version, "DOSBox");
        assert_eq!(i.date(), Some("2023-05-06T07:08:09.000Z"));
        assert_eq!(i.page_url, "https://lutris.net/games/quake/");
        let script = i.script().unwrap();
        assert_eq!(script.info.id, Some(4242));
        assert_eq!(script.info.author.as_deref(), Some("someone"));

        // Served from the cache: no new requests.
        client.search("quake").await.unwrap();
        client.installers("quake").await.unwrap();
        let reqs = server.requests();
        assert_eq!(reqs.len(), 2);
        let search = &reqs[0];
        assert!(search.path.contains("search=quake"));
        assert!(search.path.contains("with-installers=True"));
        assert!(
            search
                .header("user-agent")
                .is_some_and(|ua| ua.starts_with("Irori/"))
        );

        // A stale cache is refreshed.
        let client = LutrisClient::with_base(server.base(), tmp.path())
            .min_interval(Duration::from_millis(1))
            .cache_ttl(Duration::ZERO);
        client.installers("quake").await.unwrap();
        assert_eq!(server.requests().len(), 3);
    }

    #[tokio::test]
    async fn requests_are_rate_limited() {
        let server = TestServer::start(|_| Resp::json(json!({"results": []})));
        let tmp = tempfile::tempdir().unwrap();
        let client = LutrisClient::with_base(server.base(), tmp.path())
            .min_interval(Duration::from_millis(150));
        let start = Instant::now();
        for q in ["a", "b", "c"] {
            client.search(q).await.unwrap();
        }
        assert!(start.elapsed() >= Duration::from_millis(300));
        assert_eq!(server.requests().len(), 3);
    }

    #[tokio::test]
    async fn bad_slugs_are_refused_without_a_request() {
        let server = TestServer::start(|_| Resp::json(json!({"results": []})));
        let tmp = tempfile::tempdir().unwrap();
        let client = LutrisClient::with_base(server.base(), tmp.path());
        assert!(client.installers("../etc").await.is_err());
        assert!(client.installers("a/b").await.is_err());
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn offline_falls_back_to_a_stale_cache() {
        let server = TestServer::start(|_| Resp::json(installers_body()));
        let tmp = tempfile::tempdir().unwrap();
        let client = LutrisClient::with_base(server.base(), tmp.path())
            .min_interval(Duration::from_millis(1));
        client.installers("quake").await.unwrap();
        let offline = LutrisClient::with_base("http://127.0.0.1:9", tmp.path())
            .min_interval(Duration::from_millis(1))
            .cache_ttl(Duration::ZERO);
        assert_eq!(offline.installers("quake").await.unwrap().len(), 1);
    }
}
