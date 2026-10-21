//! ZOOM Platform HTTP API: response types and an authenticated client.
//!
//! Every endpoint lives under the site root and authenticates with the
//! `li_token` obtained from the browser login as a Bearer token. Response
//! decoding is lenient (ids as numbers or strings, `null` for empty fields)
//! because the API is undocumented.

use std::fmt;

use anyhow::{Context as _, Result, bail};
use reqwest::StatusCode;
use reqwest::header::ACCEPT;
use serde::{Deserialize, Deserializer, Serialize};

/// Site and API root.
pub const DEFAULT_API_URL: &str = "https://www.zoom-platform.com";

/// Page that signs the user in and redirects to a URL carrying `li_token`.
pub const LOGIN_URL: &str = "https://www.zoom-platform.com/login?li=heroic&return_li_token=true";

/// The saved token was rejected (HTTP 401). The store treats this as logged out.
#[derive(Debug, thiserror::Error)]
#[error("ZOOM Platform rejected the saved login; please log in again")]
pub struct Unauthorized;

/// One owned game as listed by `/li/games`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ZoomGame {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub slug: String,
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub poster_url: String,
    #[serde(deserialize_with = "nullable")]
    pub description: String,
    #[serde(deserialize_with = "nullable")]
    pub store_url: String,
    /// `windows`, `linux` and/or `osx`.
    #[serde(deserialize_with = "nullable")]
    pub operating_systems: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub developers: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub publishers: Vec<String>,
}

/// One page of the library listing.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LibraryPage {
    #[serde(deserialize_with = "de_u32")]
    pub current_page: u32,
    #[serde(deserialize_with = "de_u32")]
    pub total_pages: u32,
    #[serde(deserialize_with = "nullable")]
    pub games: Vec<ZoomGame>,
}

/// Downloadable files of a game, grouped by kind (`/li/game/<id>/files`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesResponse {
    #[serde(deserialize_with = "nullable")]
    pub manual: Vec<ZoomFile>,
    #[serde(deserialize_with = "nullable")]
    pub misc: Vec<ZoomFile>,
    #[serde(deserialize_with = "nullable")]
    pub soundtrack: Vec<ZoomFile>,
    #[serde(deserialize_with = "nullable")]
    pub windows: Vec<ZoomFile>,
    #[serde(deserialize_with = "nullable")]
    pub linux: Vec<ZoomFile>,
}

/// A downloadable file. `size` is human readable ("1.2 GB").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ZoomFile {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub size: String,
    #[serde(deserialize_with = "nullable")]
    pub filename: String,
    /// Exact size in bytes when the API provides it.
    #[serde(
        deserialize_with = "de_opt_u64",
        skip_serializing_if = "Option::is_none"
    )]
    pub total_size: Option<u64>,
    /// Checksums, used for verification when present.
    #[serde(
        deserialize_with = "de_opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub md5: Option<String>,
    #[serde(
        deserialize_with = "de_opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct LoggedIn {
    #[serde(deserialize_with = "nullable")]
    name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct DownloadLink {
    #[serde(deserialize_with = "nullable")]
    url: String,
}

/// Authenticated API client. Cheap to construct; uses the shared HTTP pool.
#[derive(Clone)]
pub struct ZoomApi {
    base: String,
    token: String,
}

impl fmt::Debug for ZoomApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZoomApi")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl ZoomApi {
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{path}", self.base);
        let resp = ember_core::http::client()
            .get(&url)
            .bearer_auth(&self.token)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .context("Could not reach ZOOM Platform")?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(Unauthorized.into());
        }
        if !status.is_success() {
            bail!("ZOOM Platform returned HTTP {} for {path}", status.as_u16());
        }
        resp.json()
            .await
            .with_context(|| format!("Unexpected response from ZOOM Platform for {path}"))
    }

    /// Validate the token and return the account name.
    pub async fn logged_in(&self) -> Result<String> {
        let me: LoggedIn = self.get("/li/loggedin").await?;
        Ok(me.name)
    }

    /// One page of the library; `None` requests the first page.
    pub async fn library_page(&self, page: Option<u32>) -> Result<LibraryPage> {
        match page {
            None => self.get("/li/games").await,
            Some(p) => self.get(&format!("/li/games?page={p}")).await,
        }
    }

    pub async fn files(&self, game_id: &str) -> Result<FilesResponse> {
        self.get(&format!("/li/game/{}/files", urlencode(game_id)))
            .await
    }

    /// Resolve a file id to a short-lived direct download URL.
    pub async fn download_url(&self, file_id: &str) -> Result<String> {
        let link: DownloadLink = self
            .get(&format!("/li/download/{}", urlencode(file_id)))
            .await?;
        if link.url.is_empty() {
            bail!("ZOOM Platform returned no download link for file {file_id}");
        }
        Ok(link.url)
    }
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// `null` → `Default`.
fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Ids arrive as numbers or strings.
fn de_id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    })
}

fn de_opt_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f as u64)),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

fn de_u32<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    Ok(de_opt_u64(d)?
        .map(|v| v.min(u32::MAX as u64) as u32)
        .unwrap_or(0))
}

fn de_opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.filter(|s| !s.trim().is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_library_page() {
        let json = r#"{"current_page":1,"total_pages":"2","games":[
            {"id":42,"slug":"some-game","name":"Some Game","poster_url":"https://x/p.jpg",
             "description":null,"store_url":"https://www.zoom-platform.com/product/some-game",
             "operating_systems":["windows","linux"],"developers":["Dev A","Dev B"],"publishers":null}
        ]}"#;
        let page: LibraryPage = serde_json::from_str(json).unwrap();
        assert_eq!(page.total_pages, 2);
        let g = &page.games[0];
        assert_eq!(g.id, "42");
        assert_eq!(g.description, "");
        assert!(g.publishers.is_empty());
        assert_eq!(g.operating_systems, vec!["windows", "linux"]);
    }

    #[test]
    fn parses_files_with_nulls() {
        let json = r#"{"manual":[],"misc":null,"soundtrack":[{"id":"9","name":"OST.zip","size":"100 MB","filename":"ost.zip"}],
            "windows":[{"id":7,"name":"setup_game_1.0.exe","size":"1.5 GB","filename":"setup_game_1.0.exe","total_size":"1610612736","md5":""}]}"#;
        let f: FilesResponse = serde_json::from_str(json).unwrap();
        assert!(f.linux.is_empty());
        assert!(f.misc.is_empty());
        assert_eq!(f.windows[0].id, "7");
        assert_eq!(f.windows[0].total_size, Some(1_610_612_736));
        assert_eq!(f.windows[0].md5, None);
        assert_eq!(f.soundtrack[0].total_size, None);
    }
}
