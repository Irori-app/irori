//! HTTP client for Amazon's device, entitlement and distribution services.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::auth::{APP_NAME, APP_VERSION, Credentials, DEVICE_TYPE, LoginRequest};
use crate::library::{self, Entitlement};
use crate::manifest::{Manifest, build_url};

/// Distribution channel of the Amazon Games app, which carries the SDK files
/// games load at runtime.
const LAUNCHER_CHANNEL: &str = "87d38116-4cbf-4af0-a371-a5b498975346";
const SDS_USER_AGENT: &str = "com.amazon.agslauncher.win/3.0.9202.1";

/// Service base addresses (overridable for tests).
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub auth: String,
    pub distribution: String,
    pub entitlements: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            auth: "https://api.amazon.com".into(),
            distribution: "https://gaming.amazon.com/api/distribution/v2/public".into(),
            entitlements: "https://gaming.amazon.com/api/distribution/entitlements".into(),
        }
    }
}

/// Where a build is served from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameDownload {
    pub download_url: String,
    pub version_id: String,
}

/// A downloaded, verified manifest and its raw bytes (kept to diff the next
/// update against).
#[derive(Debug, Clone)]
pub struct FetchedManifest {
    pub manifest: Manifest,
    pub bytes: Vec<u8>,
}

/// An error answer from an Amazon service.
#[derive(Debug, Clone)]
struct Answered {
    status: u16,
    reason: String,
}

impl std::fmt::Display for Answered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Amazon answered: {}", self.reason)
    }
}

impl std::error::Error for Answered {}

/// Amazon refused the saved login: the device registration was revoked or
/// ran out. Signing in again fixes it; retrying does not.
#[derive(Debug, Clone)]
pub struct SignInRejected(pub String);

impl std::fmt::Display for SignInRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SignInRejected {}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoints: Endpoints,
    /// Key manifests must be signed with.
    manifest_key: rsa::RsaPublicKey,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `expires_in` comes as a number or a numeric string.
fn seconds(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.trim().parse().ok())
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(v, |v, k| v.get(k))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

impl Client {
    pub fn new() -> Result<Self> {
        Self::with_endpoints(Endpoints::default())
    }

    pub fn with_endpoints(endpoints: Endpoints) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("AGSLauncher/1.0.0")
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            endpoints,
            manifest_key: crate::manifest::amazon_key(),
        })
    }

    /// Accept manifests signed with another key (a test server's).
    pub fn trust_manifest_key(mut self, pem: &str) -> Result<Self> {
        use rsa::pkcs8::DecodePublicKey as _;
        self.manifest_key =
            rsa::RsaPublicKey::from_public_key_pem(pem).context("invalid manifest key")?;
        Ok(self)
    }

    /// The client used for file transfers (no total timeout).
    pub fn download_client() -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .user_agent("AGSLauncher/1.0.0")
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .build()?)
    }

    async fn post_json(&self, url: &str, body: &Value, token: Option<&str>) -> Result<Value> {
        let mut req = self.http.post(url).json(body);
        if let Some(t) = token {
            req = req.header("Authorization", format!("bearer {t}"));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !status.is_success() {
            let reason = str_at(&value, &["response", "error", "message"])
                .or_else(|| str_at(&value, &["error_description"]))
                .or_else(|| str_at(&value, &["message"]))
                .map(str::to_string)
                .unwrap_or_else(|| status.to_string());
            return Err(Answered {
                status: status.as_u16(),
                reason,
            }
            .into());
        }
        Ok(value)
    }

    /// Register Irori as a device with the authorization code the sign-in
    /// page returned.
    pub async fn register(&self, login: &LoginRequest, code: &str) -> Result<Credentials> {
        let body = json!({
            "auth_data": {
                "authorization_code": code,
                "client_domain": "DeviceLegacy",
                "client_id": login.client_id,
                "code_algorithm": "SHA-256",
                "code_verifier": login.code_verifier,
                "use_global_authentication": false,
            },
            "registration_data": {
                "app_name": APP_NAME,
                "app_version": APP_VERSION,
                "device_model": "Windows",
                "device_name": null,
                "device_serial": login.serial,
                "device_type": DEVICE_TYPE,
                "domain": "Device",
                "os_version": "10.0.19044.0",
            },
            "requested_extensions": ["customer_info", "device_info"],
            "requested_token_type": ["bearer", "mac_dms"],
            "user_context_map": {},
        });
        let url = format!("{}/auth/register", self.endpoints.auth);
        let res = self
            .post_json(&url, &body, None)
            .await
            .context("Could not register this device with Amazon")?;
        parse_registration(&res, now())
    }

    /// A new access token for `creds`.
    pub async fn refresh(&self, creds: &Credentials) -> Result<Credentials> {
        let body = json!({
            "source_token": creds.refresh_token,
            "source_token_type": "refresh_token",
            "requested_token_type": "access_token",
            "app_name": APP_NAME,
            "app_version": APP_VERSION,
        });
        let url = format!("{}/auth/token", self.endpoints.auth);
        let res = match self.post_json(&url, &body, None).await {
            Ok(res) => res,
            Err(e) => {
                if let Some(answer) = e.downcast_ref::<Answered>()
                    && matches!(answer.status, 400 | 401 | 403)
                {
                    return Err(SignInRejected(format!(
                        "Amazon refused the saved login: {}",
                        answer.reason
                    ))
                    .into());
                }
                return Err(e.context("Could not renew the Amazon session"));
            }
        };
        let access_token =
            str_at(&res, &["access_token"]).context("Amazon sent no access token")?;
        let expires_in = res.get("expires_in").and_then(seconds).unwrap_or(3600);
        Ok(Credentials {
            access_token: access_token.to_string(),
            expires_at: now() + expires_in,
            ..creds.clone()
        })
    }

    /// Unregister the device (sign out).
    pub async fn deregister(&self, creds: &Credentials) -> Result<()> {
        let url = format!("{}/auth/deregister", self.endpoints.auth);
        let body = json!({
            "request_metadata": {"app_name": APP_NAME, "app_version": APP_VERSION}
        });
        self.post_json(&url, &body, Some(&creds.access_token))
            .await
            .context("Could not sign this device out of Amazon")?;
        Ok(())
    }

    /// Call a distribution or entitlement service operation.
    async fn service<T: DeserializeOwned>(
        &self,
        url: &str,
        target: &str,
        creds: &Credentials,
        body: Value,
    ) -> Result<T> {
        let resp = self
            .http
            .post(url)
            .header("X-Amz-Target", target)
            .header("x-amzn-token", &creds.access_token)
            .header("UserAgent", SDS_USER_AGENT)
            .header("Content-Encoding", "amz-1.0")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let reason = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| str_at(&v, &["message"]).map(str::to_string))
                .unwrap_or_else(|| status.to_string());
            return Err(anyhow!("Amazon answered: {reason}"));
        }
        Ok(resp.json().await?)
    }

    /// Every owned game.
    pub async fn entitlements(&self, creds: &Credentials) -> Result<Vec<Entitlement>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            #[serde(default)]
            entitlements: Vec<Entitlement>,
            next_token: Option<String>,
        }
        let mut all = Vec::new();
        let mut next_token: Option<String> = None;
        for _ in 0..1000 {
            let body = json!({
                "Operation": "GetEntitlements",
                "clientId": "Sonic",
                "syncPoint": null,
                "nextToken": next_token,
                "maxResults": 50,
                "productIdFilter": null,
                "keyId": "d5dc8b8b-86c8-4fc4-ae93-18c0def5314d",
                "hardwareHash": creds.hardware_hash(),
            });
            let page: Page = self
                .service(
                    &self.endpoints.entitlements,
                    "com.amazon.animusdistributionservice.entitlement.AnimusEntitlementsService.GetEntitlements",
                    creds,
                    body,
                )
                .await
                .context("Could not read the Amazon library")?;
            all.extend(page.entitlements);
            match page.next_token.filter(|t| !t.is_empty()) {
                Some(t) => next_token = Some(t),
                None => return Ok(library::dedupe(all)),
            }
        }
        bail!("The Amazon library did not finish paging")
    }

    /// Where the current build of an owned game is served from.
    pub async fn game_download(
        &self,
        creds: &Credentials,
        entitlement_id: &str,
    ) -> Result<GameDownload> {
        let res: Value = self
            .service(
                &self.endpoints.distribution,
                "com.amazon.animusdistributionservice.external.AnimusDistributionService.GetGameDownload",
                creds,
                json!({"entitlementId": entitlement_id, "Operation": "GetGameDownload"}),
            )
            .await
            .context("Could not get the game's download information")?;
        parse_download(&res)
    }

    /// Current build id of each product.
    pub async fn live_versions(
        &self,
        creds: &Credentials,
        product_ids: &[String],
    ) -> Result<HashMap<String, String>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Versions {
            #[serde(default)]
            adg_product_id_to_version_id_map: HashMap<String, String>,
        }
        if product_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let res: Versions = self
            .service(
                &self.endpoints.distribution,
                "com.amazon.animusdistributionservice.external.AnimusDistributionService.GetLiveVersionIds",
                creds,
                json!({"adgProductIds": product_ids, "Operation": "GetLiveVersions"}),
            )
            .await
            .context("Could not check Amazon games for updates")?;
        Ok(res.adg_product_id_to_version_id_map)
    }

    /// Download and verify a build's manifest.
    pub async fn manifest(&self, download: &GameDownload) -> Result<FetchedManifest> {
        let url = build_url(&download.download_url, "manifest.proto")?;
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .context("Could not download the game's file list")?;
        if !resp.status().is_success() {
            bail!(
                "Could not download the game's file list: the server answered {}",
                resp.status()
            );
        }
        let bytes = resp.bytes().await?.to_vec();
        let manifest = Manifest::parse_with_key(&bytes, &self.manifest_key)?;
        Ok(FetchedManifest { manifest, bytes })
    }

    /// Parse a stored manifest, checking it was signed with the trusted key.
    pub fn parse_manifest(&self, bytes: &[u8]) -> Result<Manifest> {
        Manifest::parse_with_key(bytes, &self.manifest_key)
    }

    /// The Amazon Games app's current build, which carries the SDK.
    pub async fn launcher_download(&self) -> Result<GameDownload> {
        let url = format!(
            "{}/download/channel/{LAUNCHER_CHANNEL}",
            self.endpoints.distribution
        );
        let resp = self.http.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!("Could not locate the Amazon Games SDK: {}", resp.status());
        }
        parse_download(&resp.json().await?)
    }
}

fn parse_download(v: &Value) -> Result<GameDownload> {
    Ok(GameDownload {
        download_url: str_at(v, &["downloadUrl"])
            .context("Amazon sent no download address")?
            .to_string(),
        version_id: str_at(v, &["versionId"]).unwrap_or_default().to_string(),
    })
}

fn parse_registration(res: &Value, now: u64) -> Result<Credentials> {
    let success = res
        .pointer("/response/success")
        .context("Amazon did not confirm the device registration")?;
    let bearer = success
        .pointer("/tokens/bearer")
        .context("Amazon sent no tokens")?;
    let text = |v: &Value, path: &[&str], what: &str| -> Result<String> {
        str_at(v, path)
            .map(str::to_string)
            .with_context(|| format!("Amazon sent no {what}"))
    };
    Ok(Credentials {
        access_token: text(bearer, &["access_token"], "access token")?,
        refresh_token: text(bearer, &["refresh_token"], "refresh token")?,
        expires_at: now + bearer.get("expires_in").and_then(seconds).unwrap_or(3600),
        device_serial: text(
            success,
            &["extensions", "device_info", "device_serial_number"],
            "device serial",
        )?,
        user_id: text(
            success,
            &["extensions", "customer_info", "user_id"],
            "account id",
        )?,
        display_name: str_at(success, &["extensions", "customer_info", "given_name"])
            .or_else(|| str_at(success, &["extensions", "customer_info", "name"]))
            .unwrap_or_default()
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use irori_depot::test_server::{Resp, TestServer};

    use super::*;

    fn creds() -> Credentials {
        Credentials {
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            expires_at: 0,
            device_serial: "SER".into(),
            user_id: "amzn1.account.U".into(),
            display_name: "Radu".into(),
        }
    }

    fn client(server: &TestServer) -> Client {
        Client::with_endpoints(Endpoints {
            auth: server.base(),
            distribution: server.url("/dist"),
            entitlements: server.url("/ent"),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn tells_a_refused_refresh_from_a_failing_server() {
        let refuse = std::sync::atomic::AtomicBool::new(true);
        let server = TestServer::start(move |_| {
            if refuse.swap(false, std::sync::atomic::Ordering::SeqCst) {
                Resp {
                    status: 400,
                    ..Resp::json(
                        json!({"error_description": "The request has an invalid parameter"}),
                    )
                }
            } else {
                Resp::status(500)
            }
        });
        let c = client(&server);
        let e = c.refresh(&creds()).await.unwrap_err();
        assert!(e.downcast_ref::<SignInRejected>().is_some(), "{e:#}");
        let e = c.refresh(&creds()).await.unwrap_err();
        assert!(e.downcast_ref::<SignInRejected>().is_none(), "{e:#}");
    }

    #[test]
    fn parses_registration() {
        let res = json!({"response": {"success": {
            "tokens": {"bearer": {"access_token": "Atna|a", "refresh_token": "Atnr|r", "expires_in": "3600"},
                       "mac_dms": {"device_private_key": "k"}},
            "extensions": {
                "device_info": {"device_serial_number": "SERIAL"},
                "customer_info": {"given_name": "Radu", "user_id": "amzn1.account.X"}}
        }}});
        let c = parse_registration(&res, 100).unwrap();
        assert_eq!(c.access_token, "Atna|a");
        assert_eq!(c.refresh_token, "Atnr|r");
        assert_eq!(c.expires_at, 3700);
        assert_eq!(c.device_serial, "SERIAL");
        assert_eq!(
            (c.user_id.as_str(), c.display_name.as_str()),
            ("amzn1.account.X", "Radu")
        );
        assert!(parse_registration(&json!({"response": {"error": {}}}), 0).is_err());
    }

    #[tokio::test]
    async fn talks_to_services() {
        let server = TestServer::start(|req| match req.path.as_str() {
            "/auth/token" => Resp::json(json!({"access_token": "NEW", "expires_in": 60})),
            "/auth/register" => Resp {
                status: 400,
                ..Resp::json(json!({"response": {"error": {"message": "InvalidValue"}}}))
            },
            "/ent" => {
                let target = req.header("x-amz-target").unwrap_or_default();
                assert!(target.ends_with(".GetEntitlements"));
                assert_eq!(req.header("x-amzn-token"), Some("AT"));
                // One page (no nextToken) with a repeated product.
                Resp::json(json!({"entitlements": [
                    {"id": "e1", "product": {"id": "p1", "title": "One"}},
                    {"id": "e1b", "product": {"id": "p1", "title": "Dup"}}
                ]}))
            }
            "/dist" => match req.header("x-amz-target").unwrap_or_default() {
                t if t.ends_with(".GetGameDownload") => {
                    Resp::json(json!({"downloadUrl": "https://cdn/v1", "versionId": "v1"}))
                }
                t if t.ends_with(".GetLiveVersionIds") => {
                    Resp::json(json!({"adgProductIdToVersionIdMap": {"p1": "v2"}}))
                }
                _ => Resp::status(400),
            },
            p if p.starts_with("/dist/download/channel/") => {
                Resp::json(json!({"downloadUrl": "https://cdn/launcher", "versionId": "L"}))
            }
            _ => Resp::status(404),
        });
        let c = client(&server);
        let fresh = c.refresh(&creds()).await.unwrap();
        assert_eq!(fresh.access_token, "NEW");
        assert_eq!(fresh.refresh_token, "RT");
        assert!(fresh.expires_at >= now() + 59);

        let err = c.register(&LoginRequest::new(), "code").await.unwrap_err();
        assert!(format!("{err:#}").contains("InvalidValue"), "{err:#}");

        let lib = c.entitlements(&creds()).await.unwrap();
        assert_eq!(lib.len(), 1);
        assert_eq!(lib[0].product.title.as_deref(), Some("One"));

        let dl = c.game_download(&creds(), "e1").await.unwrap();
        assert_eq!(
            dl,
            GameDownload {
                download_url: "https://cdn/v1".into(),
                version_id: "v1".into()
            }
        );
        let versions = c.live_versions(&creds(), &["p1".into()]).await.unwrap();
        assert_eq!(versions["p1"], "v2");
        assert!(c.live_versions(&creds(), &[]).await.unwrap().is_empty());
        assert_eq!(
            c.launcher_download().await.unwrap().download_url,
            "https://cdn/launcher"
        );
    }
}
