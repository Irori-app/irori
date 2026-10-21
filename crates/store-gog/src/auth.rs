//! GOG account: sign-in and tokens come from [`ember_gog_api::auth`]; this
//! module adds the account details shown in the UI.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use ember_core::http;

pub use ember_gog_api::auth::{
    CLIENT_ID, Credentials, LOGIN_CAPTURE, LOGIN_URL, TokenStore as Auth, extract_login_code,
};

/// Account details shown in the UI. Never contains tokens or e-mail.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserData {
    pub id: String,
    pub username: String,
    pub avatar: Option<String>,
}

/// Fetch the account details of the logged-in user.
pub async fn fetch_user(creds: &Credentials) -> Result<UserData> {
    #[derive(Deserialize)]
    struct Avatar {
        #[serde(default)]
        medium: Option<String>,
        #[serde(default)]
        small: Option<String>,
    }
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        id: Value,
        #[serde(default)]
        username: String,
        #[serde(default)]
        avatar: Option<Avatar>,
    }
    let raw: Raw = http::client()
        .get(format!("https://users.gog.com/users/{}", creds.user_id))
        .bearer_auth(&creds.access_token)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Could not reach GOG ({})", e.without_url()))?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("GOG refused the account request ({})", e.without_url()))?
        .json()
        .await
        .context("unexpected account data from GOG")?;
    let id = match raw.id {
        Value::String(s) => s,
        Value::Number(n) => n.to_string(),
        _ => creds.user_id.clone(),
    };
    Ok(UserData {
        id,
        username: raw.username,
        avatar: raw.avatar.and_then(|a| a.medium.or(a.small)),
    })
}
