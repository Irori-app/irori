//! GOG web APIs used by the store. Thin request functions; mapping to Irori
//! types lives with the callers.

use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use reqwest::{RequestBuilder, StatusCode};
use serde::Deserialize;
use serde_json::Value;

use irori_core::{CloudSaveLocation, http};

use crate::auth::Credentials;

fn net_err(e: reqwest::Error) -> anyhow::Error {
    anyhow!("Could not reach GOG ({})", e.without_url())
}

async fn send(req: RequestBuilder) -> Result<reqwest::Response> {
    req.send().await.map_err(net_err)
}

/// gamesdb/galaxy ids are numbers in some payloads and strings in others.
pub fn id_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

// ---- Galaxy library ------------------------------------------------------

/// One owned release from the Galaxy library.
#[derive(Debug, Clone, Deserialize)]
pub struct GalaxyItem {
    #[serde(default)]
    pub platform_id: String,
    #[serde(default)]
    pub external_id: Value,
    #[serde(default)]
    pub certificate: Option<String>,
    /// When the release joined the account (unix seconds; a number or a
    /// string depending on the payload).
    #[serde(default)]
    pub owned_since: Value,
    #[serde(default)]
    pub date_created: Value,
}

impl GalaxyItem {
    /// When the account got this release, as RFC 3339.
    pub fn date_added(&self) -> Option<String> {
        let secs = |v: &Value| match v {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        };
        secs(&self.owned_since)
            .or_else(|| secs(&self.date_created))
            .filter(|s| *s > 0)
            .and_then(irori_core::util::rfc3339_from_unix)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilteredProducts {
    #[serde(default)]
    products: Vec<FilteredProduct>,
    #[serde(default)]
    total_pages: u32,
}

#[derive(Debug, Deserialize)]
struct FilteredProduct {
    #[serde(default)]
    id: Value,
}

/// Ids of the products the user hid in their library on gog.com.
pub async fn hidden_products(creds: &Credentials) -> Result<std::collections::HashSet<String>> {
    let mut out = std::collections::HashSet::new();
    for page in 1..=100u32 {
        let req = http::client()
            .get("https://embed.gog.com/account/getFilteredProducts")
            .query(&[
                ("hiddenFlag", "1"),
                ("mediaType", "1"),
                ("page", &page.to_string()),
            ])
            .bearer_auth(&creds.access_token);
        let data: FilteredProducts = send(req)
            .await?
            .error_for_status()
            .map_err(|e| anyhow!("GOG hidden games request failed ({})", e.without_url()))?
            .json()
            .await
            .context("unexpected GOG hidden games data")?;
        out.extend(
            data.products
                .iter()
                .map(|p| id_string(&p.id))
                .filter(|id| !id.is_empty()),
        );
        if page >= data.total_pages {
            break;
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct GalaxyPage {
    #[serde(default)]
    items: Vec<GalaxyItem>,
    #[serde(default)]
    next_page_token: Option<String>,
}

/// Every owned GOG release, following pagination. Any failing page fails the
/// whole call (so a partial list never replaces the cached library).
pub async fn galaxy_library(creds: &Credentials) -> Result<Vec<GalaxyItem>> {
    let mut out = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..1000 {
        let mut req = http::client()
            .get(format!(
                "https://galaxy-library.gog.com/users/{}/releases",
                creds.user_id
            ))
            .bearer_auth(&creds.access_token);
        if let Some(t) = &token {
            req = req.query(&[("page_token", t)]);
        }
        let page: GalaxyPage = send(req)
            .await?
            .error_for_status()
            .map_err(|e| anyhow!("GOG library request failed ({})", e.without_url()))?
            .json()
            .await
            .context("unexpected GOG library data")?;
        out.extend(page.items);
        match page.next_page_token.filter(|t| !t.is_empty()) {
            Some(t) if Some(&t) != token.as_ref() => token = Some(t),
            _ => break,
        }
    }
    Ok(out)
}

// ---- GamesDB -------------------------------------------------------------

/// Result of a conditional GamesDB request.
#[derive(Debug)]
pub enum GamesDb {
    /// The cached copy (sent as `If-None-Match`) is current.
    NotModified,
    /// GamesDB has no entry for this id.
    NotFound,
    Data {
        json: Value,
        etag: Option<String>,
    },
}

pub async fn gamesdb(
    id: &str,
    certificate: Option<&str>,
    token: Option<&str>,
    etag: Option<&str>,
) -> Result<GamesDb> {
    let mut req = http::client()
        .get(format!(
            "https://gamesdb.gog.com/platforms/gog/external_releases/{id}"
        ))
        .timeout(Duration::from_secs(30));
    if let Some(e) = etag {
        req = req.header("If-None-Match", e);
    }
    if let Some(c) = certificate {
        req = req.header("X-GOG-Library-Cert", c);
    }
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = send(req).await?;
    match resp.status() {
        StatusCode::NOT_MODIFIED => Ok(GamesDb::NotModified),
        StatusCode::NOT_FOUND => Ok(GamesDb::NotFound),
        s if s.is_success() => {
            let etag = resp
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let json = resp.json().await.context("unexpected GamesDB data")?;
            Ok(GamesDb::Data { json, etag })
        }
        s => bail!("GamesDB answered {s}"),
    }
}

// ---- Product API ---------------------------------------------------------

/// `api.gog.com/products/<id>`; `None` when the product is unknown.
pub async fn product(
    id: &str,
    expand: &[&str],
    locale: &str,
    token: Option<&str>,
) -> Result<Option<Value>> {
    let mut req = http::client()
        .get(format!("https://api.gog.com/products/{id}"))
        .query(&[("locale", locale)]);
    if !expand.is_empty() {
        req = req.query(&[("expand", expand.join(","))]);
    }
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = send(req).await?;
    if resp.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let resp = resp
        .error_for_status()
        .map_err(|e| anyhow!("GOG product request failed ({})", e.without_url()))?;
    Ok(Some(resp.json().await.context("unexpected product data")?))
}

/// Version of the first Linux installer listed by the product API.
pub fn linux_installer_version(product: &Value) -> Option<String> {
    product
        .pointer("/downloads/installers")?
        .as_array()?
        .iter()
        .find(|i| i.get("os").and_then(Value::as_str) == Some("linux"))
        .and_then(|i| i.get("version"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Languages of the Linux installers (product API `downloads`).
pub fn linux_installer_languages(product: &Value) -> Vec<String> {
    let mut out: Vec<String> = product
        .pointer("/downloads/installers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|i| i.get("os").and_then(Value::as_str) == Some("linux"))
        .filter_map(|i| i.get("language").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    out.dedup();
    out
}

/// `api.gog.com/v2/games/<id>` (store page data and requirements).
pub async fn games_v2(id: &str) -> Result<Option<Value>> {
    let resp = send(
        http::client()
            .get(format!("https://api.gog.com/v2/games/{id}"))
            .query(&[("locale", "en-US")]),
    )
    .await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    Ok(resp.json().await.ok())
}

// ---- Cloud saves ---------------------------------------------------------

/// Cloud-save locations of a client. `Ok(None)` when cloud saves are disabled.
pub async fn cloud_save_locations(
    client_id: &str,
    platform: &str,
) -> Result<Option<Vec<CloudSaveLocation>>> {
    let v: Value = send(
        http::client()
            .get(format!(
                "https://remote-config.gog.com/components/galaxy_client/clients/{client_id}"
            ))
            .query(&[("component_version", "2.0.45")]),
    )
    .await?
    .error_for_status()
    .map_err(|e| anyhow!("GOG remote config failed ({})", e.without_url()))?
    .json()
    .await
    .context("unexpected remote config data")?;
    Ok(parse_cloud_storage(&v, platform))
}

/// `content.<Windows|MacOS>.cloudStorage` → locations when enabled.
pub fn parse_cloud_storage(v: &Value, platform: &str) -> Option<Vec<CloudSaveLocation>> {
    let key = if platform == "osx" {
        "MacOS"
    } else {
        "Windows"
    };
    let cs = v.get("content")?.get(key)?.get("cloudStorage")?;
    if !cs.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    Some(
        cs.get("locations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|l| {
                Some(CloudSaveLocation {
                    name: l.get("name")?.as_str()?.to_string(),
                    location: l.get("location")?.as_str()?.to_string(),
                })
            })
            .collect(),
    )
}

// ---- Gameplay ------------------------------------------------------------

/// Outcome of a playtime upload.
#[derive(Debug, PartialEq, Eq)]
pub enum PostResult {
    Accepted,
    /// Temporary failure (network, auth, server); retry later.
    Retry,
    /// GOG refused this session permanently.
    Rejected,
}

pub async fn post_playtime(
    creds: &Credentials,
    app_name: &str,
    session_date: u64,
    minutes: u64,
) -> PostResult {
    let req = http::client()
        .post(format!(
            "https://gameplay.gog.com/games/{app_name}/users/{}/sessions",
            creds.user_id
        ))
        .bearer_auth(&creds.access_token)
        .json(&serde_json::json!({"session_date": session_date, "time": minutes}));
    match req.send().await {
        Ok(r) if r.status().is_success() => PostResult::Accepted,
        Ok(r)
            if r.status().is_client_error()
                && !matches!(
                    r.status(),
                    StatusCode::UNAUTHORIZED
                        | StatusCode::FORBIDDEN
                        | StatusCode::REQUEST_TIMEOUT
                        | StatusCode::TOO_MANY_REQUESTS
                ) =>
        {
            PostResult::Rejected
        }
        _ => PostResult::Retry,
    }
}

/// Total minutes GOG recorded for a game.
pub async fn playtime_minutes(creds: &Credentials, app_name: &str) -> Result<u64> {
    let v: Value = send(
        http::client()
            .get(format!(
                "https://gameplay.gog.com/games/{app_name}/users/{}/sessions",
                creds.user_id
            ))
            .bearer_auth(&creds.access_token),
    )
    .await?
    .error_for_status()
    .map_err(|e| anyhow!("GOG playtime request failed ({})", e.without_url()))?
    .json()
    .await
    .context("unexpected playtime data")?;
    Ok(v.get("time_sum").and_then(Value::as_u64).unwrap_or(0))
}

/// Raw achievement items for a client (or product) id.
pub async fn achievements(creds: &Credentials, id: &str, language: &str) -> Result<Vec<Value>> {
    let v: Value = send(
        http::client()
            .get(format!(
                "https://gameplay.gog.com/clients/{id}/users/{}/achievements",
                creds.user_id
            ))
            .bearer_auth(&creds.access_token)
            .header("X-Gog-Lc", language),
    )
    .await?
    .error_for_status()
    .map_err(|e| anyhow!("GOG achievements request failed ({})", e.without_url()))?
    .json()
    .await
    .context("unexpected achievements data")?;
    Ok(v.get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

// ---- Presence ------------------------------------------------------------

pub async fn set_presence(creds: &Credentials, game_id: Option<&str>) -> Result<()> {
    let mut body = serde_json::json!({
        "application_type": "Irori",
        "force_update": false,
        "presence": "online",
        "version": env!("CARGO_PKG_VERSION"),
    });
    if let Some(g) = game_id {
        body["game_id"] = Value::String(g.to_string());
    }
    send(
        http::client()
            .post(format!(
                "https://presence.gog.com/users/{}/status",
                creds.user_id
            ))
            .bearer_auth(&creds.access_token)
            .json(&body),
    )
    .await?
    .error_for_status()
    .map_err(|e| anyhow!("GOG presence failed ({})", e.without_url()))?;
    Ok(())
}

pub async fn delete_presence(creds: &Credentials) -> Result<()> {
    send(
        http::client()
            .delete(format!(
                "https://presence.gog.com/users/{}/status",
                creds.user_id
            ))
            .bearer_auth(&creds.access_token),
    )
    .await?
    .error_for_status()
    .map_err(|e| anyhow!("GOG presence failed ({})", e.without_url()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cloud_storage() {
        let v = serde_json::json!({"content": {
            "Windows": {"cloudStorage": {"enabled": true, "locations": [
                {"name": "saves", "location": "<?SAVED_GAMES?>/CD Projekt Red/Cyberpunk 2077"}]}},
            "MacOS": {"cloudStorage": {"enabled": false}}}});
        let w = parse_cloud_storage(&v, "windows").unwrap();
        assert_eq!(w[0].name, "saves");
        assert!(parse_cloud_storage(&v, "osx").is_none());
        let empty =
            serde_json::json!({"content": {"Windows": {"cloudStorage": {"enabled": true}}}});
        assert_eq!(parse_cloud_storage(&empty, "windows"), Some(vec![]));
    }

    #[test]
    fn linux_installers() {
        let p = serde_json::json!({"downloads": {"installers": [
            {"os": "windows", "version": "1.0", "language": "en"},
            {"os": "linux", "version": "2.1 (gog-3)", "language": "en"},
            {"os": "linux", "version": "2.1 (gog-3)", "language": "de"}]}});
        assert_eq!(linux_installer_version(&p).as_deref(), Some("2.1 (gog-3)"));
        assert_eq!(linux_installer_languages(&p), vec!["en", "de"]);
        assert_eq!(linux_installer_version(&serde_json::json!({})), None);
    }
}
