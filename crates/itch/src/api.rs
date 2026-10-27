//! The itch.io API client (`https://api.itch.io`, version 2), authenticated
//! with the user's API key in the `Authorization` header.

use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::model::{OwnedKey, Upload, User};

/// itch.io's API server.
pub const API_BASE: &str = "https://api.itch.io";

/// Owned keys asked for per page (the API allows up to 500).
const PER_PAGE: usize = 100;

/// itch.io refused a request; `unauthorized` when the key itself was
/// refused (revoked or mistyped), which only a new key fixes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("itch.io answered {status}: {message}")]
pub struct ApiError {
    pub status: u16,
    pub message: String,
    pub unauthorized: bool,
}

/// An authenticated API client. Cheap to clone.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    /// For download links, which answer with a redirect to the file.
    no_redirect: reqwest::Client,
    base: String,
    key: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("base", &self.base).finish()
    }
}

impl Client {
    pub fn new(key: &str) -> Result<Self> {
        Self::with_base(API_BASE, key)
    }

    /// A client for another server (a test server).
    pub fn with_base(base: &str, key: &str) -> Result<Self> {
        let build = |redirects: reqwest::redirect::Policy| {
            reqwest::Client::builder()
                .user_agent(concat!("Irori/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(60))
                .redirect(redirects)
                .build()
        };
        Ok(Self {
            http: build(reqwest::redirect::Policy::default())?,
            no_redirect: build(reqwest::redirect::Policy::none())?,
            base: base.trim_end_matches('/').to_string(),
            key: key.trim().to_string(),
        })
    }

    fn get(&self, client: &reqwest::Client, path: &str) -> reqwest::RequestBuilder {
        client
            .get(format!("{}{path}", self.base))
            .header(reqwest::header::AUTHORIZATION, &self.key)
            .header(reqwest::header::ACCEPT, "application/vnd.itch.v2")
    }

    async fn json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .get(&self.http, path)
            .send()
            .await
            .context("Could not reach itch.io")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if let Some(e) = api_error(status.as_u16(), &value) {
            return Err(e.into());
        }
        serde_json::from_value(value)
            .with_context(|| format!("unexpected answer from itch.io for {path}"))
    }

    /// The account the key belongs to.
    pub async fn profile(&self) -> Result<User> {
        #[derive(serde::Deserialize)]
        struct Answer {
            user: User,
        }
        Ok(self.json::<Answer>("/profile").await?.user)
    }

    /// Every download key the account owns, page by page.
    pub async fn owned_keys(&self) -> Result<Vec<OwnedKey>> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct Page {
            owned_keys: Vec<OwnedKey>,
        }
        let mut all = Vec::new();
        for page in 1.. {
            let got: Page = self
                .json(&format!(
                    "/profile/owned-keys?page={page}&per_page={PER_PAGE}"
                ))
                .await?;
            let n = got.owned_keys.len();
            all.extend(got.owned_keys);
            if n < PER_PAGE {
                break;
            }
        }
        Ok(all)
    }

    /// The uploads of a game the key (`download_key_id`) gives access to.
    pub async fn uploads(&self, game_id: u64, key_id: u64) -> Result<Vec<Upload>> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct Answer {
            uploads: Vec<Upload>,
        }
        Ok(self
            .json::<Answer>(&format!(
                "/games/{game_id}/uploads?download_key_id={key_id}"
            ))
            .await?
            .uploads)
    }

    /// Where to download an upload from right now: itch.io answers with a
    /// redirect to a short-lived address on its file servers.
    pub async fn download_location(&self, upload_id: u64, key_id: u64) -> Result<String> {
        let path = format!("/uploads/{upload_id}/download?download_key_id={key_id}");
        let resp = self
            .get(&self.no_redirect, &path)
            .send()
            .await
            .context("Could not reach itch.io")?;
        let status = resp.status();
        if status.is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .context("itch.io sent a download redirect without an address")?;
            return Ok(match reqwest::Url::parse(location) {
                Ok(url) => url.to_string(),
                Err(_) => format!("{}/{}", self.base, location.trim_start_matches('/')),
            });
        }
        let value: Value = resp.json().await.unwrap_or(Value::Null);
        if let Some(e) = api_error(status.as_u16(), &value) {
            return Err(e.into());
        }
        value
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("itch.io did not say where to download the game from")
    }
}

/// The error in an answer: a failed status, or `{"errors": [...]}` (which
/// itch.io also sends with status 200 for some refusals).
fn api_error(status: u16, value: &Value) -> Option<ApiError> {
    let messages: Vec<&str> = value
        .get("errors")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if (200..300).contains(&status) && messages.is_empty() {
        return None;
    }
    let message = if messages.is_empty() {
        format!("HTTP {status}")
    } else {
        messages.join(", ")
    };
    let key_refused = messages
        .iter()
        .any(|m| m.contains("invalid key") || m.contains("invalid api key"));
    Some(ApiError {
        status,
        unauthorized: matches!(status, 401 | 403) || key_refused,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_depot::test_server::{Resp, TestServer};
    use serde_json::json;

    #[tokio::test]
    async fn sends_the_key_and_pages_through_owned_keys() {
        let server = TestServer::start(|req| {
            if req.header("authorization") != Some("KEY") {
                return Resp::json(json!({"errors": ["invalid key"]}));
            }
            let path = req.path.as_str();
            if path == "/profile" {
                Resp::json(json!({"user": {"id": 9, "username": "u", "display_name": "U"}}))
            } else if path.starts_with("/profile/owned-keys?page=1&") {
                let keys: Vec<Value> = (0..PER_PAGE)
                    .map(|i| json!({"id": i, "game_id": 1000 + i}))
                    .collect();
                Resp::json(json!({"owned_keys": keys, "page": 1}))
            } else if path.starts_with("/profile/owned-keys?page=2&") {
                Resp::json(json!({"owned_keys": [{"id": 500, "game_id": 7}], "page": 2}))
            } else if path == "/games/7/uploads?download_key_id=500" {
                Resp::json(json!({"uploads": [{"id": 3, "filename": "a.zip", "type": "default"}]}))
            } else if path == "/uploads/3/download?download_key_id=500" {
                Resp {
                    status: 302,
                    headers: vec![(
                        "Location".into(),
                        "https://files.example/a.zip?sig=1".into(),
                    )],
                    ..Default::default()
                }
            } else {
                Resp::status(404)
            }
        });
        let api = Client::with_base(&server.base(), "KEY").unwrap();
        assert_eq!(api.profile().await.unwrap().name(), "U");
        let keys = api.owned_keys().await.unwrap();
        assert_eq!(keys.len(), PER_PAGE + 1);
        assert_eq!(api.uploads(7, 500).await.unwrap()[0].id, 3);
        assert_eq!(
            api.download_location(3, 500).await.unwrap(),
            "https://files.example/a.zip?sig=1"
        );

        let wrong = Client::with_base(&server.base(), "nope").unwrap();
        let e = wrong.profile().await.unwrap_err();
        let api_error = e.downcast_ref::<ApiError>().unwrap();
        assert!(api_error.unauthorized, "{api_error}");
        let missing = api.uploads(8, 1).await.unwrap_err();
        assert!(!missing.downcast_ref::<ApiError>().unwrap().unauthorized);
    }
}
