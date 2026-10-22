//! SteamGridDB: game search and artwork listings for picking custom art.
//!
//! The API key comes from `AppSettings::steam_grid_db_api_key`. It is wrapped
//! in [`ApiKey`], whose `Debug` output is redacted, and it is never logged or
//! put into error messages.

use std::fmt;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use ember_core::AppSettings;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::util;

const BASE: &str = "https://www.steamgriddb.com/api/v2";
const TIMEOUT: Duration = Duration::from_secs(20);
/// Prefix of keys Heroic encrypted with Electron's safeStorage.
const HEROIC_ENCRYPTED_PREFIX: &str = "sgdb:v1:";

/// A SteamGridDB API key.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

impl ApiKey {
    /// A trimmed, non-empty key. Heroic's encrypted form is not usable.
    pub fn new(key: &str) -> Option<Self> {
        let key = key.trim();
        (!key.is_empty() && !key.starts_with(HEROIC_ENCRYPTED_PREFIX))
            .then(|| Self(key.to_string()))
    }

    /// The key from settings, if one is set.
    pub fn from_settings(app: &AppSettings) -> Option<Self> {
        Self::new(&app.steam_grid_db_api_key)
    }
}

/// A search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SgdbGame {
    pub id: u64,
    pub name: String,
}

/// An artwork asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SgdbImage {
    pub id: u64,
    pub url: String,
    pub thumb: String,
}

/// Optional filters for artwork listings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageFilter {
    /// e.g. `"600x900"`, `"920x430"`.
    pub dimensions: Vec<String>,
    /// e.g. `"alternate"`, `"material"`.
    pub styles: Vec<String>,
}

impl ImageFilter {
    fn query(&self) -> String {
        let mut parts = Vec::new();
        if !self.dimensions.is_empty() {
            parts.push(format!(
                "dimensions={}",
                util::encode_component(&self.dimensions.join(","))
            ));
        }
        if !self.styles.is_empty() {
            parts.push(format!(
                "styles={}",
                util::encode_component(&self.styles.join(","))
            ));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.join("&"))
        }
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    success: bool,
    #[serde(default = "Option::default")]
    data: Option<T>,
    #[serde(default)]
    errors: Vec<String>,
}

/// Decode the response envelope.
fn unwrap_envelope<T: DeserializeOwned>(body: &[u8], what: &str) -> Result<T> {
    let env: Envelope<T> = serde_json::from_slice(body)
        .with_context(|| format!("SteamGridDB: unexpected {what} response"))?;
    if !env.success {
        let msg = if env.errors.is_empty() {
            format!("SteamGridDB: {what} failed")
        } else {
            format!("SteamGridDB: {}", env.errors.join(", "))
        };
        bail!(msg);
    }
    env.data
        .with_context(|| format!("SteamGridDB: {what} returned no data"))
}

async fn get<T: DeserializeOwned>(key: &ApiKey, path: &str, what: &str) -> Result<T> {
    let resp = ember_core::http::client()
        .get(format!("{BASE}{path}"))
        .bearer_auth(&key.0)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("SteamGridDB {what} failed: {}", e.without_url()))?;
    let status = resp.status();
    let body = resp
        .bytes()
        .await
        .context("SteamGridDB: reading response")?;
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        bail!("SteamGridDB rejected the API key. Check it in Settings.");
    }
    if !status.is_success() {
        if let Ok(env) = serde_json::from_slice::<Envelope<serde_json::Value>>(&body)
            && !env.errors.is_empty()
        {
            bail!("SteamGridDB: {}", env.errors.join(", "));
        }
        bail!("SteamGridDB {what} failed (HTTP {status})");
    }
    unwrap_envelope(&body, what)
}

/// Search games by name (autocomplete).
pub async fn search_games(key: &ApiKey, query: &str) -> Result<Vec<SgdbGame>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    get(
        key,
        &format!("/search/autocomplete/{}", util::encode_component(query)),
        "search",
    )
    .await
}

/// Grid (capsule) images of a game.
pub async fn grids(key: &ApiKey, game_id: u64, filter: &ImageFilter) -> Result<Vec<SgdbImage>> {
    get(
        key,
        &format!("/grids/game/{game_id}{}", filter.query()),
        "grids",
    )
    .await
}

/// Hero (banner) images of a game.
pub async fn heroes(key: &ApiKey, game_id: u64, filter: &ImageFilter) -> Result<Vec<SgdbImage>> {
    get(
        key,
        &format!("/heroes/game/{game_id}{}", filter.query()),
        "heroes",
    )
    .await
}

/// Logo images of a game.
pub async fn logos(key: &ApiKey, game_id: u64, filter: &ImageFilter) -> Result<Vec<SgdbImage>> {
    get(
        key,
        &format!("/logos/game/{game_id}{}", filter.query()),
        "logos",
    )
    .await
}

/// Icon images of a game.
pub async fn icons(key: &ApiKey, game_id: u64, filter: &ImageFilter) -> Result<Vec<SgdbImage>> {
    get(
        key,
        &format!("/icons/game/{game_id}{}", filter.query()),
        "icons",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_handling() {
        assert!(ApiKey::new("  ").is_none());
        assert!(ApiKey::new("sgdb:v1:abcd").is_none());
        let k = ApiKey::new(" secret ").unwrap();
        assert_eq!(format!("{k:?}"), "ApiKey(***)");
        assert_eq!(k.0, "secret");
    }

    #[test]
    fn filter_query() {
        assert_eq!(ImageFilter::default().query(), "");
        let f = ImageFilter {
            dimensions: vec!["600x900".into(), "342x482".into()],
            styles: vec!["alternate".into()],
        };
        assert_eq!(f.query(), "?dimensions=600x900%2C342x482&styles=alternate");
    }

    #[test]
    fn envelopes() {
        let games: Vec<SgdbGame> = unwrap_envelope(
            br#"{"success":true,"data":[{"id":1,"name":"Doom","types":["steam"],"verified":true}]}"#,
            "search",
        )
        .unwrap();
        assert_eq!(
            games,
            [SgdbGame {
                id: 1,
                name: "Doom".into()
            }]
        );
        let imgs: Vec<SgdbImage> = unwrap_envelope(
            br#"{"success":true,"data":[{"id":5,"url":"https://x/a.png","thumb":"https://x/t.png","style":"alternate"}]}"#,
            "grids",
        )
        .unwrap();
        assert_eq!(imgs[0].thumb, "https://x/t.png");
        let err = unwrap_envelope::<Vec<SgdbGame>>(
            br#"{"success":false,"errors":["Game not found"]}"#,
            "grids",
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "SteamGridDB: Game not found");
        let err = unwrap_envelope::<Vec<SgdbGame>>(br#"{"success":false}"#, "heroes").unwrap_err();
        assert_eq!(err.to_string(), "SteamGridDB: heroes failed");
    }
}
