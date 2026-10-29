//! [`Client`]: the HTTP transport shared by the sign-in and library calls.
//!
//! `IAuthenticationService` methods take their request as
//! `input_protobuf_encoded=<base64 protobuf>` and answer with a protobuf
//! body; the result code is the `x-eresult` header. The library calls are
//! plain JSON Web API and store requests.
//!
//! Access tokens travel in query strings, so every transport error is
//! stripped of its URL before it can reach a message or a log.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::DeserializeOwned;

use crate::error::SteamError;

/// Steam's Web API server.
pub const API_BASE: &str = "https://api.steampowered.com";

/// Steam's store server (app details).
pub const STORE_BASE: &str = "https://store.steampowered.com";

/// Steam's CDN for store art (`IStoreBrowseService` asset paths are
/// relative to it).
pub const ASSETS_BASE: &str = "https://shared.cloudflare.steamstatic.com/store_item_assets";

/// `EResult::OK`.
const ERESULT_OK: i32 = 1;

/// Server base addresses (overridable for tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// The Web API, `https://api.steampowered.com`.
    pub api: String,
    /// The store, `https://store.steampowered.com`.
    pub store: String,
    /// The store art CDN, [`ASSETS_BASE`].
    pub assets: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            api: API_BASE.into(),
            store: STORE_BASE.into(),
            assets: ASSETS_BASE.into(),
        }
    }
}

/// How an `IAuthenticationService` method is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Get,
    Post,
}

/// A Steam client. Holds no credentials: every call takes the token it
/// needs. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoints: Endpoints,
}

impl Client {
    /// A client for Steam's own servers.
    pub fn new() -> Result<Self> {
        Self::with_endpoints(Endpoints::default())
    }

    /// A client for other servers (a test server).
    pub fn with_endpoints(endpoints: Endpoints) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("Irori/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .context("Could not set up the connection to Steam")?;
        Ok(Self {
            http,
            endpoints: Endpoints {
                api: endpoints.api.trim_end_matches('/').to_string(),
                store: endpoints.store.trim_end_matches('/').to_string(),
                assets: endpoints.assets.trim_end_matches('/').to_string(),
            },
        })
    }

    pub(crate) fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    /// Call `IAuthenticationService/<method>/v1` with a protobuf request.
    pub(crate) async fn auth_call<Req, Resp>(
        &self,
        verb: Verb,
        method: &str,
        request: &Req,
    ) -> Result<Resp>
    where
        Req: prost::Message,
        Resp: prost::Message + Default,
    {
        let url = format!("{}/IAuthenticationService/{method}/v1", self.endpoints.api);
        let params = [("input_protobuf_encoded", form_value(request))];
        let builder = match verb {
            Verb::Get => self.http.get(url).query(&params),
            Verb::Post => self.http.post(url).form(&params),
        };
        let resp = builder
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .context("Could not reach Steam")?;
        let status = resp.status();
        let eresult = resp
            .headers()
            .get("x-eresult")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<i32>().ok());
        tracing::debug!(
            method,
            status = status.as_u16(),
            ?eresult,
            "steam auth call"
        );
        if let Some(code) = eresult
            && code != ERESULT_OK
        {
            return Err(SteamError::from_eresult(code).into());
        }
        if !status.is_success() {
            if let Some(e) = SteamError::from_http_status(status.as_u16()) {
                return Err(e.into());
            }
            bail!("Steam answered with HTTP status {}.", status.as_u16());
        }
        let body = resp
            .bytes()
            .await
            .map_err(reqwest::Error::without_url)
            .context("Steam's answer was cut off")?;
        Resp::decode(body).context("Steam sent a sign-in answer Irori could not read")
    }

    /// GET a JSON document; `what` names it in error messages.
    pub(crate) async fn get_json<T: DeserializeOwned>(
        &self,
        url: String,
        query: &[(&str, String)],
        what: &str,
    ) -> Result<T> {
        let resp = self
            .http
            .get(url)
            .query(query)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .context("Could not reach Steam")?;
        let status = resp.status();
        if !status.is_success() {
            if let Some(e) = SteamError::from_http_status(status.as_u16()) {
                return Err(e.into());
            }
            bail!(
                "Steam answered with HTTP status {} for {what}.",
                status.as_u16()
            );
        }
        let body = resp
            .bytes()
            .await
            .map_err(reqwest::Error::without_url)
            .context("Steam's answer was cut off")?;
        serde_json::from_slice(&body)
            .with_context(|| format!("Steam sent an unexpected answer for {what}"))
    }
}

/// The `input_protobuf_encoded` value for a request.
pub(crate) fn form_value(request: &impl prost::Message) -> String {
    STANDARD.encode(request.encode_to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::GetPasswordRsaPublicKeyRequest;
    use prost::Message as _;

    #[test]
    fn encodes_requests_as_base64_protobuf() {
        let req = GetPasswordRsaPublicKeyRequest {
            account_name: "player".into(),
        };
        let value = form_value(&req);
        let bytes = STANDARD.decode(&value).unwrap();
        assert_eq!(
            GetPasswordRsaPublicKeyRequest::decode(&bytes[..]).unwrap(),
            req
        );
    }

    #[test]
    fn trims_endpoint_slashes() {
        let c = Client::with_endpoints(Endpoints {
            api: "http://a/".into(),
            store: "http://s//".into(),
            assets: "http://c/".into(),
        })
        .unwrap();
        assert_eq!(c.endpoints().api, "http://a");
        assert_eq!(c.endpoints().store, "http://s");
        assert_eq!(c.endpoints().assets, "http://c");
        assert_eq!(Endpoints::default().api, API_BASE);
    }
}
