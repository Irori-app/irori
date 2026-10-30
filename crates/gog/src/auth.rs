//! GOG Galaxy OAuth: the sign-in page, code exchange, token refresh and
//! game-scoped tokens (cloud saves use each game's own client).
//!
//! Tokens live in one `irori_secrets` file mapping client ids to credentials. Concurrent
//! callers share one refresh (single flight).

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Galaxy client id.
pub const CLIENT_ID: &str = "46899977096215655";
/// Public Galaxy client secret (shipped in every Galaxy client).
pub const CLIENT_SECRET: &str = "9d85c43b1482497dbbce61f6e4aa173a433796eeae2ca8c5f6129f2dc4de46d9";
/// Redirect target of the sign-in page; carries `code` in its query.
pub const REDIRECT_URI: &str = "https://embed.gog.com/on_login_success?origin=client";
/// Page the user signs in on.
pub const LOGIN_URL: &str = "https://auth.gog.com/auth?client_id=46899977096215655&redirect_uri=https%3A%2F%2Fembed.gog.com%2Fon_login_success%3Forigin%3Dclient&response_type=code&layout=galaxy";
/// Regex (one capture group) that finds the code in the redirect address.
pub const LOGIN_CAPTURE: &str = r"on_login_success\?(?:[^\s#]*&)?code=([A-Za-z0-9._~-]+)";

/// Refresh this many seconds before the token actually expires.
const EXPIRY_MARGIN_SECS: f64 = 60.0;

/// Tokens of one client. Unknown fields are preserved on rewrite.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub expires_in: f64,
    /// Unix time the tokens were issued.
    #[serde(default, rename = "loginTime")]
    pub login_time: f64,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("user_id", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .field("login_time", &self.login_time)
            .finish()
    }
}

impl Credentials {
    /// Whether the access token is (about to be) expired at `now` (unix seconds).
    pub fn is_expired(&self, now: f64) -> bool {
        self.access_token.is_empty()
            || now + EXPIRY_MARGIN_SECS >= self.login_time + self.expires_in
    }
}

/// Pull the authorization code out of what the sign-in produced: the
/// redirect URL, a query fragment or the bare code.
pub fn extract_login_code(input: &str) -> Option<String> {
    static QUERY_CODE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?:^|[?&#\s])code=([^&#\s]+)").expect("valid regex"));
    static BARE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^[A-Za-z0-9._~-]{8,}$").expect("valid regex"));
    let input = input.trim().trim_matches(|c| c == '"' || c == '\'');
    if input.is_empty() {
        return None;
    }
    if let Ok(url) = url::Url::parse(input)
        && let Some((_, code)) = url.query_pairs().find(|(k, _)| k == "code")
    {
        return (!code.is_empty()).then(|| code.into_owned());
    }
    if let Some(c) = QUERY_CODE.captures(input) {
        let raw = &c[1];
        let decoded = url::form_urlencoded::parse(format!("c={raw}").as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap_or_else(|| raw.to_string());
        return Some(decoded);
    }
    BARE.is_match(input).then(|| input.to_string())
}

pub(crate) fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Owner of the token file. One per store.
pub struct TokenStore {
    file: PathBuf,
    token_url: String,
    http: reqwest::Client,
    refresh_lock: tokio::sync::Mutex<()>,
}

impl TokenStore {
    pub fn new(file: PathBuf) -> Self {
        Self::with_endpoint(file, "https://auth.gog.com/token".into())
    }

    /// A store talking to another token endpoint (tests).
    pub fn with_endpoint(file: PathBuf, token_url: String) -> Self {
        Self {
            file,
            token_url,
            http: reqwest::Client::builder()
                .user_agent(crate::USER_AGENT)
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(30))
                .build()
                .expect("http client"),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    async fn read_map(&self) -> Result<Map<String, Value>> {
        let path = self.file.clone();
        let value = tokio::task::spawn_blocking(move || irori_secrets::read_json::<Value>(&path))
            .await
            .context("reading GOG credentials")?
            .context("Could not read the GOG credentials")?;
        Ok(match value {
            Some(Value::Object(map)) => map,
            _ => Map::new(),
        })
    }

    async fn write_map(&self, map: Map<String, Value>) -> Result<()> {
        let path = self.file.clone();
        tokio::task::spawn_blocking(move || irori_secrets::write_json(&path, &Value::Object(map)))
            .await
            .context("saving GOG credentials")?
    }

    fn entry(map: &Map<String, Value>, client_id: &str) -> Option<Credentials> {
        map.get(client_id)
            .cloned()
            .and_then(|v| serde_json::from_value::<Credentials>(v).ok())
            .filter(|c| !c.refresh_token.is_empty() || !c.access_token.is_empty())
    }

    /// Stored Galaxy credentials without refreshing.
    pub async fn stored(&self) -> Result<Option<Credentials>> {
        let map = self.read_map().await?;
        Ok(Self::entry(&map, CLIENT_ID).filter(|c| !c.refresh_token.is_empty()))
    }

    /// Valid Galaxy credentials, refreshing them first when expired.
    pub async fn credentials(&self) -> Result<Credentials> {
        self.client_credentials(CLIENT_ID, CLIENT_SECRET).await
    }

    /// Valid credentials for a game's own client (cloud saves), derived
    /// from the Galaxy session.
    pub async fn client_credentials(&self, client_id: &str, secret: &str) -> Result<Credentials> {
        let map = self.read_map().await?;
        if let Some(c) = Self::entry(&map, client_id)
            && !c.is_expired(now())
        {
            return Ok(c);
        }
        let _guard = self.refresh_lock.lock().await;
        // Another caller may have refreshed while we waited.
        let map = self.read_map().await?;
        if let Some(c) = Self::entry(&map, client_id)
            && !c.is_expired(now())
        {
            return Ok(c);
        }
        let galaxy = Self::entry(&map, CLIENT_ID)
            .filter(|c| !c.refresh_token.is_empty())
            .context("You are not signed in to GOG")?;
        let mut params = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", galaxy.refresh_token.as_str()),
        ];
        let scoped = client_id != CLIENT_ID;
        if scoped {
            params.push(("without_new_session", "1"));
        }
        let fresh = self
            .request_token(client_id, secret, &params)
            .await
            .context("Could not refresh the GOG session")?;
        let mut map = map;
        map.insert(client_id.to_string(), serde_json::to_value(&fresh)?);
        self.write_map(map).await?;
        Ok(fresh)
    }

    /// Exchange a sign-in code for tokens and store them (replacing any
    /// session and its game tokens).
    pub async fn login_with_code(&self, code: &str) -> Result<Credentials> {
        let _guard = self.refresh_lock.lock().await;
        let creds = self
            .request_token(
                CLIENT_ID,
                CLIENT_SECRET,
                &[
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", REDIRECT_URI),
                ],
            )
            .await
            .context("GOG did not accept the sign-in code. Sign in again and copy the new code")?;
        let mut map = Map::new();
        map.insert(CLIENT_ID.to_string(), serde_json::to_value(&creds)?);
        self.write_map(map).await?;
        Ok(creds)
    }

    /// Store Galaxy credentials obtained elsewhere (imports).
    pub async fn put(&self, creds: &Credentials) -> Result<()> {
        let mut map = self.read_map().await.unwrap_or_default();
        map.insert(CLIENT_ID.to_string(), serde_json::to_value(creds)?);
        self.write_map(map).await
    }

    /// Forget the session.
    pub async fn clear(&self) -> Result<()> {
        let _guard = self.refresh_lock.lock().await;
        match tokio::fs::remove_file(&self.file).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("removing GOG credentials"),
        }
    }

    async fn request_token(
        &self,
        client_id: &str,
        secret: &str,
        params: &[(&str, &str)],
    ) -> Result<Credentials> {
        let mut query: Vec<(&str, &str)> =
            vec![("client_id", client_id), ("client_secret", secret)];
        query.extend_from_slice(params);
        let resp = self
            .http
            .get(&self.token_url)
            .query(&query)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Could not reach GOG ({})", e.without_url()))?;
        let status = resp.status();
        if matches!(status.as_u16(), 400 | 401) {
            return Err(SignInRejected(format!("GOG refused the login ({status})")).into());
        }
        if !status.is_success() {
            bail!("GOG answered {status}");
        }
        let mut creds: Credentials = resp.json().await.context("unexpected answer from GOG")?;
        if creds.access_token.is_empty() {
            bail!("GOG returned no tokens");
        }
        creds.login_time = now();
        Ok(creds)
    }
}

/// GOG's token service refused the saved login (HTTP 400 or 401): the
/// refresh token was revoked or ran out. Signing in again fixes it; retrying
/// does not.
#[derive(Debug, Clone)]
pub struct SignInRejected(pub String);

impl std::fmt::Display for SignInRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SignInRejected {}

#[cfg(test)]
mod tests {
    use irori_depot::test_server::{Resp, TestServer};
    use serde_json::json;

    use super::*;

    #[test]
    fn extracts_codes() {
        let url = "https://embed.gog.com/on_login_success?origin=client&code=abcDEF123_-";
        assert_eq!(extract_login_code(url).as_deref(), Some("abcDEF123_-"));
        assert_eq!(
            extract_login_code("x code=abc12345 y").as_deref(),
            Some("abc12345")
        );
        assert_eq!(
            extract_login_code("abcdefgh123").as_deref(),
            Some("abcdefgh123")
        );
        assert_eq!(extract_login_code("nope"), None);
        let re = Regex::new(LOGIN_CAPTURE).unwrap();
        assert_eq!(&re.captures(url).unwrap()[1], "abcDEF123_-");
    }

    #[tokio::test]
    async fn logs_in_refreshes_and_scopes_tokens() {
        let server = TestServer::start(|req| {
            let p = &req.path;
            let id = if p.contains("client_id=46899977096215655") {
                "galaxy"
            } else {
                "game"
            };
            let scoped = p.contains("without_new_session=1");
            Resp::json(json!({
                "access_token": format!("{id}-{}", if p.contains("authorization_code") { "login" } else { "refresh" }),
                "refresh_token": "RT", "user_id": "u1", "expires_in": if scoped { 3600 } else { 0 }}))
        });
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::with_endpoint(dir.path().join("auth.json"), server.url("/token"));
        assert!(store.stored().await.unwrap().is_none());
        let c = store.login_with_code("code").await.unwrap();
        assert_eq!(c.access_token, "galaxy-login");
        // Expired (expires_in 0): refreshed on use.
        assert_eq!(
            store.credentials().await.unwrap().access_token,
            "galaxy-refresh"
        );
        let game = store.client_credentials("123", "sec").await.unwrap();
        assert_eq!(game.access_token, "game-refresh");
        let before = server.requests().len();
        store.client_credentials("123", "sec").await.unwrap();
        assert_eq!(server.requests().len(), before, "valid game token reused");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join("auth.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        store.clear().await.unwrap();
        assert!(store.credentials().await.is_err());
    }

    #[tokio::test]
    async fn tells_a_refused_refresh_from_a_failing_server() {
        let refuse = std::sync::atomic::AtomicBool::new(true);
        let server = TestServer::start(move |_| {
            if refuse.swap(false, std::sync::atomic::Ordering::SeqCst) {
                Resp::status(400)
            } else {
                Resp::status(502)
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::with_endpoint(dir.path().join("auth.json"), server.url("/token"));
        store
            .put(&Credentials {
                access_token: "AT".into(),
                refresh_token: "RT".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let e = store.credentials().await.unwrap_err();
        assert!(e.downcast_ref::<SignInRejected>().is_some(), "{e:#}");
        let e = store.credentials().await.unwrap_err();
        assert!(e.downcast_ref::<SignInRejected>().is_none(), "{e:#}");
    }
}
