//! HTTP client for Epic's account, launcher, catalog, library, entitlement
//! and cloud-save services.

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::{Digest as _, Sha1};

use crate::manifest::Secrets;

/// Launcher identity Epic's services expect. Defaults match the Epic Games
/// Launcher; the overrides service can supply newer values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub client_id: String,
    pub client_secret: String,
    pub user_agent: String,
    /// Launcher build label (`Live-EternalKnight`).
    pub label: String,
}

impl Default for Identity {
    fn default() -> Self {
        Self {
            client_id: "34a02cf8f4414e29b15921876da36f9a".into(),
            client_secret: "daafbccc737745039dffe53d94fc76cf".into(),
            user_agent:
                "UELauncher/11.0.1-14907503+++Portal+Release-Live Windows/10.0.19041.1.256.64bit"
                    .into(),
            label: "Live-EternalKnight".into(),
        }
    }
}

impl Identity {
    /// Apply the `egl_config` object of legendary's version service.
    pub fn apply_overrides(&mut self, egl_config: &Value) {
        let s = |k: &str| {
            egl_config
                .get(k)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
        };
        if let Some(v) = s("version") {
            self.user_agent = format!("UELauncher/{v} Windows/10.0.19041.1.256.64bit");
        }
        if let Some(v) = s("label") {
            self.label = v.to_string();
        }
        if let (Some(id), Some(secret)) = (s("client_id"), s("client_secret")) {
            self.client_id = id.to_string();
            self.client_secret = secret.to_string();
        }
    }
}

/// Service base addresses (overridable for tests).
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub oauth: String,
    pub launcher: String,
    /// Akamai-fronted launcher host used for download tickets.
    pub launcher2: String,
    pub entitlements: String,
    pub catalog: String,
    pub ecommerce: String,
    pub datastorage: String,
    pub library: String,
    pub artifact_delivery: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        let h = |host: &str| format!("https://{host}");
        Self {
            oauth: h("account-public-service-prod03.ol.epicgames.com"),
            launcher: h("launcher-public-service-prod06.ol.epicgames.com"),
            launcher2: h("launcher-public-service-prod.ak.epicgames.com"),
            entitlements: h("entitlement-public-service-prod08.ol.epicgames.com"),
            catalog: h("catalog-public-service-prod06.ol.epicgames.com"),
            ecommerce: h("ecommerceintegration-public-service-ecomprod02.ol.epicgames.com"),
            datastorage: h("datastorage-public-service-liveegs.live.use1a.on.epicgames.com"),
            library: h("library-service.live.use1a.on.epicgames.com"),
            artifact_delivery: h("artifact-delivery-service-public-prod.ol.epicgames.com"),
        }
    }
}

/// A signed-in account. Irori's own format.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix time the access token expires.
    pub expires_at: u64,
    /// Unix time the refresh token expires.
    #[serde(default)]
    pub refresh_expires_at: u64,
    pub account_id: String,
    pub display_name: String,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("account_id", &self.account_id)
            .field("display_name", &self.display_name)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Whether the access token expires within ten minutes of `now`.
    pub fn needs_refresh(&self, now: u64) -> bool {
        now + 600 >= self.expires_at
    }
}

/// An app build available to the account (`/launcher/api/public/assets`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Asset {
    pub app_name: String,
    pub asset_id: String,
    pub build_version: String,
    pub catalog_item_id: String,
    /// `<label>-<platform>`, e.g. `Live-Windows`.
    pub label_name: String,
    pub namespace: String,
    pub metadata: Value,
    pub sidecar_rvn: u64,
}

/// Extra per-app configuration distributed with the manifest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    pub config: Value,
    pub rev: u64,
}

/// Where a build's manifest lives.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ManifestInfo {
    pub app_name: String,
    pub label_name: String,
    pub build_version: String,
    /// SHA-1 (hex) of the manifest file.
    pub hash: String,
    /// Chunks must be fetched through signed addresses.
    pub use_signed_url: bool,
    pub manifest_urls: Vec<String>,
    /// Folders the chunks are served from.
    pub base_urls: Vec<String>,
    pub secrets: Secrets,
    pub sidecar: Option<Sidecar>,
}

/// A ticket for signing chunk addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub signed_ticket: String,
    /// Unix time.
    pub expires_at: u64,
}

/// An entry of the account's library (`/library/api/public/items`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LibraryItem {
    pub app_name: Option<String>,
    pub namespace: String,
    pub catalog_item_id: String,
    pub sandbox_type: String,
}

/// Total time Epic recorded for one app.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaytimeItem {
    pub artifact_id: String,
    /// Seconds.
    pub total_time: u64,
}

/// How Epic answered a playtime upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytimeUpload {
    Accepted,
    /// Refused for good (malformed or unknown app): drop it.
    Rejected,
    /// Network trouble, an expired sign-in or a busy server: keep it.
    Retry,
}

/// A file of the cloud-save store.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SaveFile {
    pub read_link: Option<String>,
    pub write_link: Option<String>,
    pub last_modified: Option<String>,
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn parse_time(s: &str) -> Option<u64> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|t| t.unix_timestamp().max(0) as u64)
}

/// Epic's account service refused the request: the saved sign-in is no
/// longer valid, or the account needs attention on Epic's website. Signing in
/// again fixes it; retrying does not.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct SignInRejected(pub String);

/// The message of an Epic error body.
fn epic_error(v: &Value) -> Option<String> {
    let msg = v.get("errorMessage").and_then(Value::as_str);
    let code = v.get("errorCode").and_then(Value::as_str);
    match (msg, code) {
        (Some(m), _) if !m.is_empty() => Some(m.to_string()),
        (_, Some(c)) => Some(c.to_string()),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoints: Endpoints,
    identity: Identity,
    /// `en-US` style locale for catalog data.
    pub language: String,
    pub country: String,
}

impl Client {
    pub fn new(identity: Identity) -> Result<Self> {
        Self::with_endpoints(identity, Endpoints::default())
    }

    pub fn with_endpoints(identity: Identity, endpoints: Endpoints) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(identity.user_agent.clone())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(16)
            .build()?;
        Ok(Self {
            http,
            endpoints,
            identity,
            language: "en".into(),
            country: "US".into(),
        })
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Client for chunk and manifest transfers (no total timeout).
    pub fn download_client(&self) -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .user_agent(self.identity.user_agent.clone())
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(32)
            .build()?)
    }

    async fn json<T: DeserializeOwned>(&self, req: reqwest::RequestBuilder) -> Result<T> {
        let resp = req.send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            let reason = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .as_ref()
                .and_then(epic_error)
                .unwrap_or_else(|| status.to_string());
            bail!("Epic answered: {reason}");
        }
        serde_json::from_slice(&bytes).context("Epic sent an unexpected answer")
    }

    fn auth(&self, req: reqwest::RequestBuilder, session: &Session) -> reqwest::RequestBuilder {
        req.bearer_auth(&session.access_token)
    }

    // ---- accounts ------------------------------------------------------

    async fn token(&self, form: &[(&str, &str)]) -> Result<Value> {
        let resp = self
            .http
            .post(format!("{}/account/api/oauth/token", self.endpoints.oauth))
            .basic_auth(&self.identity.client_id, Some(&self.identity.client_secret))
            .form(form)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        // A client error with Epic's own error body is a verdict on the
        // credentials; a busy or failing server (429, 5xx) is not.
        let refused = status.is_client_error() && status.as_u16() != 429;
        if let Some(reason) = epic_error(&v) {
            let message = if v.get("errorCode").and_then(Value::as_str)
                == Some("errors.com.epicgames.oauth.corrective_action_required")
                && let Some(url) = v.get("continuationUrl").and_then(Value::as_str)
            {
                format!("{reason} Open {url} to take care of it, then sign in again.")
            } else {
                format!("Epic refused the sign-in: {reason}")
            };
            if refused {
                return Err(SignInRejected(message).into());
            }
            bail!(message);
        }
        if !status.is_success() {
            bail!("Epic refused the sign-in ({status})");
        }
        Ok(v)
    }

    fn session_from(v: &Value) -> Result<Session> {
        let s = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .with_context(|| format!("Epic sent no {k}"))
        };
        let n = now();
        let expires_at = v
            .get("expires_at")
            .and_then(Value::as_str)
            .and_then(parse_time)
            .or_else(|| v.get("expires_in").and_then(Value::as_u64).map(|e| n + e))
            .unwrap_or(n + 3600);
        let refresh_expires_at = v
            .get("refresh_expires_at")
            .and_then(Value::as_str)
            .and_then(parse_time)
            .or_else(|| {
                v.get("refresh_expires")
                    .and_then(Value::as_u64)
                    .map(|e| n + e)
            })
            .unwrap_or(0);
        Ok(Session {
            access_token: s("access_token")?,
            refresh_token: s("refresh_token")?,
            expires_at,
            refresh_expires_at,
            account_id: s("account_id")?,
            display_name: v
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// Sign in with the authorization code from the sign-in page.
    pub async fn login(&self, code: &str) -> Result<Session> {
        let v = self
            .token(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("token_type", "eg1"),
            ])
            .await?;
        Self::session_from(&v)
    }

    /// Sign in with an exchange code (Epic Games Launcher hand-off).
    pub async fn login_exchange(&self, code: &str) -> Result<Session> {
        let v = self
            .token(&[
                ("grant_type", "exchange_code"),
                ("exchange_code", code),
                ("token_type", "eg1"),
            ])
            .await?;
        Self::session_from(&v)
    }

    /// A fresh access token for `session`.
    pub async fn refresh(&self, session: &Session) -> Result<Session> {
        let v = self
            .token(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &session.refresh_token),
                ("token_type", "eg1"),
            ])
            .await?;
        Self::session_from(&v)
    }

    /// An anonymous access token (EOS overlay downloads).
    pub async fn client_credentials(&self) -> Result<Session> {
        let v = self
            .token(&[("grant_type", "client_credentials"), ("token_type", "eg1")])
            .await?;
        let n = now();
        Ok(Session {
            access_token: v
                .get("access_token")
                .and_then(Value::as_str)
                .context("Epic sent no access token")?
                .to_string(),
            refresh_token: String::new(),
            expires_at: n + v.get("expires_in").and_then(Value::as_u64).unwrap_or(3600),
            refresh_expires_at: 0,
            account_id: String::new(),
            display_name: String::new(),
        })
    }

    /// End the session server-side (sign out).
    pub async fn kill_session(&self, session: &Session) -> Result<()> {
        let resp = self
            .auth(
                self.http.delete(format!(
                    "{}/account/api/oauth/sessions/kill/{}",
                    self.endpoints.oauth, session.access_token
                )),
                session,
            )
            .send()
            .await?;
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
            bail!("Epic did not end the session ({})", resp.status());
        }
        Ok(())
    }

    /// A one-time exchange code passed to games as `-AUTH_PASSWORD`.
    pub async fn exchange_code(&self, session: &Session) -> Result<String> {
        let v: Value = self
            .json(self.auth(
                self.http.get(format!(
                    "{}/account/api/oauth/exchange",
                    self.endpoints.oauth
                )),
                session,
            ))
            .await
            .context("Could not get a game login token")?;
        v.get("code")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("Epic sent no game login token")
    }

    /// Ownership verification token for DRM-protected games.
    pub async fn ownership_token(
        &self,
        session: &Session,
        namespace: &str,
        catalog_item_id: &str,
    ) -> Result<Vec<u8>> {
        let resp = self
            .auth(
                self.http.post(format!(
                    "{}/ecommerceintegration/api/public/platforms/EPIC/identities/{}/ownershipToken",
                    self.endpoints.ecommerce, session.account_id
                )),
                session,
            )
            .form(&[("nsCatalogItemId", format!("{namespace}:{catalog_item_id}"))])
            .send()
            .await?
            .error_for_status()
            .context("Could not get the ownership token")?;
        Ok(resp.bytes().await?.to_vec())
    }

    // ---- library -------------------------------------------------------

    /// Builds available to the account on `platform`.
    pub async fn assets(&self, session: &Session, platform: &str) -> Result<Vec<Asset>> {
        self.json(
            self.auth(
                self.http.get(format!(
                    "{}/launcher/api/public/assets/{platform}",
                    self.endpoints.launcher
                )),
                session,
            )
            .query(&[("label", "Live")]),
        )
        .await
        .with_context(|| format!("Could not list the Epic games for {platform}"))
    }

    /// Store metadata of a catalog item.
    pub async fn catalog_item(
        &self,
        session: &Session,
        namespace: &str,
        catalog_item_id: &str,
    ) -> Result<Option<Value>> {
        let mut v: Value = self
            .json(
                self.auth(
                    self.http.get(format!(
                        "{}/catalog/api/shared/namespace/{namespace}/bulk/items",
                        self.endpoints.catalog
                    )),
                    session,
                )
                .query(&[
                    ("id", catalog_item_id),
                    ("includeDLCDetails", "true"),
                    ("includeMainGameDetails", "true"),
                    ("country", &self.country),
                    ("locale", &self.language),
                ])
                .timeout(Duration::from_secs(30)),
            )
            .await?;
        Ok(v.get_mut(catalog_item_id).map(Value::take))
    }

    /// Every library entry (includes games without downloadable builds).
    pub async fn library_items(&self, session: &Session) -> Result<Vec<LibraryItem>> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            records: Vec<LibraryItem>,
            #[serde(default, rename = "responseMetadata")]
            meta: Value,
        }
        let url = format!("{}/library/api/public/items", self.endpoints.library);
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..1000 {
            let mut req = self
                .auth(self.http.get(&url), session)
                .query(&[("includeMetadata", "true")]);
            if let Some(c) = &cursor {
                req = req.query(&[("cursor", c)]);
            }
            let page: Page = self
                .json(req)
                .await
                .context("Could not read the Epic library")?;
            out.extend(page.records);
            cursor = page
                .meta
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(str::to_string);
            if cursor.is_none() {
                return Ok(out);
            }
        }
        bail!("The Epic library did not finish paging")
    }

    /// Entitlement names the account owns.
    pub async fn entitlements(&self, session: &Session) -> Result<Vec<String>> {
        let url = format!(
            "{}/entitlement/api/account/{}/entitlements",
            self.endpoints.entitlements, session.account_id
        );
        let mut out = Vec::new();
        loop {
            let start = out.len().to_string();
            let page: Vec<Value> = self
                .json(
                    self.auth(self.http.get(&url), session)
                        .query(&[("start", start.as_str()), ("count", "1000")]),
                )
                .await
                .context("Could not read the Epic entitlements")?;
            let n = page.len();
            out.extend(
                page.iter()
                    .filter_map(|e| e.get("entitlementName").and_then(Value::as_str))
                    .map(str::to_string),
            );
            if n < 1000 {
                return Ok(out);
            }
        }
    }

    // ---- playtime ------------------------------------------------------

    /// Seconds played per app as Epic recorded them (its launcher's sessions
    /// and those Irori uploaded).
    pub async fn playtime(&self, session: &Session) -> Result<Vec<PlaytimeItem>> {
        let url = format!(
            "{}/library/api/public/playtime/account/{}/all",
            self.endpoints.library, session.account_id
        );
        self.json(self.auth(self.http.get(&url), session))
            .await
            .context("Could not read the Epic playtime")
    }

    /// Record one play session. `start` and `end` are RFC 3339.
    pub async fn add_playtime(
        &self,
        session: &Session,
        machine_id: &str,
        artifact_id: &str,
        start: &str,
        end: &str,
    ) -> Result<PlaytimeUpload> {
        let url = format!(
            "{}/library/api/public/playtime/account/{}",
            self.endpoints.library, session.account_id
        );
        let body = serde_json::json!({
            "machineId": machine_id,
            "artifactId": artifact_id,
            "startTime": start,
            "endTime": end,
        });
        let status = match self
            .auth(self.http.put(&url), session)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => resp.status(),
            // No answer at all: try again later.
            Err(_) => return Ok(PlaytimeUpload::Retry),
        };
        Ok(if status.is_success() {
            PlaytimeUpload::Accepted
        } else if status.is_client_error()
            && status != reqwest::StatusCode::UNAUTHORIZED
            && status != reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            PlaytimeUpload::Rejected
        } else {
            PlaytimeUpload::Retry
        })
    }

    // ---- builds --------------------------------------------------------

    /// Manifest location of an app's current build.
    pub async fn manifest_info(
        &self,
        session: &Session,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: &str,
    ) -> Result<ManifestInfo> {
        let v: Value = self
            .json(self.auth(
                self.http.get(format!(
                    "{}/launcher/api/public/assets/v2/platform/{platform}/namespace/{namespace}/catalogItem/{catalog_item_id}/app/{app_name}/label/Live",
                    self.endpoints.launcher
                )),
                session,
            ))
            .await
            .with_context(|| format!("Could not get the download information of {app_name}"))?;
        parse_manifest_info(&v)
    }

    /// Download a build's manifest from the first CDN that serves it and
    /// check its hash.
    pub async fn download_manifest(&self, info: &ManifestInfo) -> Result<Vec<u8>> {
        let mut last = anyhow!("Epic listed no manifest addresses");
        for url in &info.manifest_urls {
            let resp = match self.http.get(url).send().await {
                Ok(r) => r,
                Err(e) => {
                    last = e.into();
                    continue;
                }
            };
            if !resp.status().is_success() {
                last = anyhow!("the CDN answered {}", resp.status());
                continue;
            }
            let bytes = resp.bytes().await?.to_vec();
            if !info.hash.is_empty()
                && hex::encode(Sha1::digest(&bytes)) != info.hash.to_lowercase()
            {
                last = anyhow!("the manifest hash does not match");
                continue;
            }
            return Ok(bytes);
        }
        Err(last.context("Could not download the game's manifest"))
    }

    /// The optimised delta manifest between two builds, if Epic has one.
    pub async fn delta_manifest(
        &self,
        base_url: &str,
        old_build_id: &str,
        new_build_id: &str,
    ) -> Option<Vec<u8>> {
        if old_build_id == new_build_id {
            return None;
        }
        let url = format!("{base_url}/Deltas/{new_build_id}/{old_build_id}.delta");
        let resp = self.http.get(url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.bytes().await.ok().map(|b| b.to_vec())
    }

    /// A ticket for signing chunk addresses of a build.
    pub async fn download_ticket(&self, session: &Session, asset: &Asset) -> Result<Ticket> {
        let (label, platform) = asset
            .label_name
            .split_once('-')
            .unwrap_or(("Live", asset.label_name.as_str()));
        let v: Value = self
            .json(
                self.auth(
                    self.http.post(format!(
                        "{}/launcher/api/public/assets/v2/ticket",
                        self.endpoints.launcher2
                    )),
                    session,
                )
                .json(&json!({
                    "catalogItemId": asset.catalog_item_id,
                    "buildVersion": asset.build_version,
                    "appName": asset.app_name,
                    "namespace": asset.namespace,
                    "label": label,
                    "platform": platform,
                })),
            )
            .await
            .context("Could not get a download ticket")?;
        Ok(Ticket {
            signed_ticket: v
                .get("signedTicket")
                .and_then(Value::as_str)
                .context("Epic sent no download ticket")?
                .to_string(),
            expires_at: v
                .get("expiresAt")
                .and_then(Value::as_str)
                .and_then(parse_time)
                .unwrap_or_else(|| now() + 600),
        })
    }

    /// Signed addresses for chunk paths.
    pub async fn signed_chunk_urls(
        &self,
        session: &Session,
        ticket: &Ticket,
        paths: &[String],
    ) -> Result<HashMap<String, String>> {
        self.json(
            self.auth(
                self.http.post(format!(
                    "{}/artifact-delivery/api/public/v1/delivery/account/{}/downloadurls",
                    self.endpoints.artifact_delivery, session.account_id
                )),
                session,
            )
            .json(&json!({
                "signedTicket": ticket.signed_ticket,
                "chunkIds": paths,
                "deltaId": "",
                "clientMetrics": {},
            })),
        )
        .await
    }

    // ---- cloud saves ---------------------------------------------------

    fn saves_url(&self, session: &Session, app_name: &str, manifests: bool) -> String {
        let mut url = format!(
            "{}/api/v1/access/egstore/savesync/{}/",
            self.endpoints.datastorage, session.account_id
        );
        if !app_name.is_empty() {
            url.push_str(app_name);
            url.push_str(if manifests { "/manifests/" } else { "/" });
        }
        url
    }

    /// Cloud-save files of an app (manifests only when `manifests`).
    pub async fn save_files(
        &self,
        session: &Session,
        app_name: &str,
        manifests: bool,
    ) -> Result<HashMap<String, SaveFile>> {
        #[derive(Deserialize)]
        struct Files {
            #[serde(default)]
            files: HashMap<String, SaveFile>,
        }
        let f: Files = self
            .json(self.auth(
                self.http.get(self.saves_url(session, app_name, manifests)),
                session,
            ))
            .await
            .context("Could not list the cloud saves")?;
        Ok(f.files)
    }

    /// Read or write links for named save files.
    pub async fn save_links(
        &self,
        session: &Session,
        app_name: &str,
        filenames: &[String],
    ) -> Result<HashMap<String, SaveFile>> {
        #[derive(Deserialize)]
        struct Files {
            #[serde(default)]
            files: HashMap<String, SaveFile>,
        }
        let f: Files = self
            .json(
                self.auth(
                    self.http.post(self.saves_url(session, app_name, false)),
                    session,
                )
                .json(&json!({ "files": filenames })),
            )
            .await
            .context("Could not prepare the cloud save transfer")?;
        Ok(f.files)
    }

    /// Delete a cloud-save file by its full path.
    pub async fn delete_save_file(&self, session: &Session, path: &str) -> Result<()> {
        self.auth(
            self.http.delete(format!(
                "{}/api/v1/data/egstore/{path}",
                self.endpoints.datastorage
            )),
            session,
        )
        .send()
        .await?
        .error_for_status()?;
        Ok(())
    }

    /// GET a pre-signed link.
    pub async fn fetch_link(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.http.get(url).send().await?.error_for_status()?;
        Ok(resp.bytes().await?.to_vec())
    }

    /// PUT to a pre-signed link.
    pub async fn put_link(&self, url: &str, body: Vec<u8>) -> Result<()> {
        self.http
            .put(url)
            .body(body)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

fn parse_manifest_info(v: &Value) -> Result<ManifestInfo> {
    let elements = v
        .get("elements")
        .and_then(Value::as_array)
        .context("Epic sent no build information")?;
    if elements.len() != 1 {
        bail!("Epic sent {} builds instead of one", elements.len());
    }
    let e = &elements[0];
    let s = |k: &str| {
        e.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut info = ManifestInfo {
        app_name: s("appName"),
        label_name: s("labelName"),
        build_version: s("buildVersion"),
        hash: s("hash"),
        use_signed_url: e.get("useSignedUrl").and_then(Value::as_bool) == Some(true),
        ..Default::default()
    };
    for m in e
        .get("manifests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(uri) = m.get("uri").and_then(Value::as_str) else {
            continue;
        };
        let base = uri.rsplit_once('/').map_or(uri, |(b, _)| b).to_string();
        if !info.base_urls.contains(&base) {
            info.base_urls.push(base);
        }
        let params: Vec<String> = m
            .get("queryParams")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some(format!(
                    "{}={}",
                    p.get("name")?.as_str()?,
                    p.get("value")?.as_str()?
                ))
            })
            .collect();
        info.manifest_urls.push(if params.is_empty() {
            uri.to_string()
        } else {
            format!("{uri}?{}", params.join("&"))
        });
    }
    if let Some(secrets) = e.get("secrets").and_then(Value::as_object) {
        info.secrets = secrets
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
            .collect();
    }
    if let Some(sc) = e.get("sidecar") {
        let config = sc
            .get("config")
            .and_then(Value::as_str)
            .and_then(|c| serde_json::from_str(c).ok())
            .unwrap_or(Value::Null);
        info.sidecar = Some(Sidecar {
            config,
            rev: sc.get("rvn").and_then(Value::as_u64).unwrap_or(0),
        });
    }
    Ok(info)
}

#[cfg(test)]
pub(crate) mod tests {
    use irori_depot::test_server::{Resp, TestServer};

    use super::*;

    pub(crate) fn endpoints(server: &TestServer) -> Endpoints {
        let b = server.base();
        Endpoints {
            oauth: b.clone(),
            launcher: b.clone(),
            launcher2: b.clone(),
            entitlements: b.clone(),
            catalog: b.clone(),
            ecommerce: b.clone(),
            datastorage: b.clone(),
            library: b.clone(),
            artifact_delivery: b,
        }
    }

    pub(crate) fn session() -> Session {
        Session {
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            expires_at: now() + 3600,
            refresh_expires_at: 0,
            account_id: "acc".into(),
            display_name: "Player".into(),
        }
    }

    #[test]
    fn parses_manifest_info() {
        let v = json!({"elements": [{
            "appName": "Sugar", "labelName": "Live-Windows", "buildVersion": "1.0",
            "hash": "abc", "useSignedUrl": true,
            "manifests": [
                {"uri": "https://cdn1/Builds/Org/x/default/m.manifest",
                 "queryParams": [{"name": "f_token", "value": "t"}]},
                {"uri": "https://cdn2/Builds/Org/x/default/m.manifest"}],
            "secrets": {"AAAA": "00ff"},
            "sidecar": {"config": "{\"deploymentId\": \"dep\"}", "rvn": 3}
        }]});
        let info = parse_manifest_info(&v).unwrap();
        assert_eq!(
            info.manifest_urls,
            [
                "https://cdn1/Builds/Org/x/default/m.manifest?f_token=t",
                "https://cdn2/Builds/Org/x/default/m.manifest"
            ]
        );
        assert_eq!(
            info.base_urls,
            [
                "https://cdn1/Builds/Org/x/default",
                "https://cdn2/Builds/Org/x/default"
            ]
        );
        assert!(info.use_signed_url);
        assert_eq!(info.secrets["AAAA"], "00ff");
        let sc = info.sidecar.unwrap();
        assert_eq!(
            (sc.config["deploymentId"].as_str(), sc.rev),
            (Some("dep"), 3)
        );
        assert!(parse_manifest_info(&json!({"elements": []})).is_err());
    }

    #[tokio::test]
    async fn tells_a_refused_refresh_from_a_failing_server() {
        // The first refresh is refused, the second meets a failing server.
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let server = TestServer::start(move |_| {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Resp {
                    status: 400,
                    ..Resp::json(json!({
                        "errorCode": "errors.com.epicgames.account.auth_token.invalid_refresh_token",
                        "errorMessage": "Sorry the refresh token is invalid"}))
                }
            } else {
                Resp {
                    status: 503,
                    ..Resp::json(
                        json!({"errorCode": "errors.com.epicgames.common.server_error",
                                         "errorMessage": "Try again later"}),
                    )
                }
            }
        });
        let client = Client::with_endpoints(Identity::default(), endpoints(&server)).unwrap();
        let e = client.refresh(&session()).await.unwrap_err();
        assert!(e.downcast_ref::<SignInRejected>().is_some(), "{e:#}");
        let e = client.refresh(&session()).await.unwrap_err();
        assert!(e.downcast_ref::<SignInRejected>().is_none(), "{e:#}");
    }

    #[tokio::test]
    async fn talks_to_services() {
        let manifest = b"manifest bytes".to_vec();
        let manifest_hash = hex::encode(Sha1::digest(&manifest));
        let served = manifest.clone();
        let server = TestServer::start(move |req| match req.path.as_str() {
            "/account/api/oauth/token" => {
                assert!(req.header("authorization").unwrap().starts_with("Basic "));
                Resp::json(json!({
                    "access_token": "NEW", "expires_in": 7200, "refresh_token": "RT2",
                    "refresh_expires": 100, "account_id": "acc", "displayName": "Player"}))
            }
            "/account/api/oauth/exchange" => {
                assert_eq!(req.header("authorization"), Some("Bearer AT"));
                Resp::json(json!({"code": "XCHG"}))
            }
            "/launcher/api/public/assets/Windows?label=Live" => Resp::json(json!([
                {"appName": "Sugar", "buildVersion": "2", "catalogItemId": "cat",
                 "labelName": "Live-Windows", "namespace": "ns", "sidecarRvn": 1}])),
            p if p.starts_with("/library/api/public/items") => {
                if p.contains("cursor=c1") {
                    Resp::json(json!({"records": [{"appName": "B", "namespace": "n"}],
                                      "responseMetadata": {}}))
                } else {
                    Resp::json(json!({"records": [{"appName": "A", "namespace": "n",
                                                   "sandboxType": "PUBLIC"}],
                                      "responseMetadata": {"nextCursor": "c1"}}))
                }
            }
            p if p.starts_with("/catalog/api/shared/namespace/ns/bulk/items") => {
                Resp::json(json!({"cat": {"title": "Sugar", "id": "cat"}}))
            }
            "/m.manifest" => Resp {
                status: 200,
                body: served.clone(),
                ..Default::default()
            },
            "/launcher/api/public/assets/v2/ticket" => Resp::json(json!({
                "signedTicket": "TICKET", "expiresAt": "2030-01-01T00:00:00Z"})),
            "/artifact-delivery/api/public/v1/delivery/account/acc/downloadurls" => {
                Resp::json(json!({"ChunksV4/00/x.chunk": "https://signed/x"}))
            }
            "/account/api/oauth/sessions/kill/AT" => Resp::status(204),
            _ => Resp {
                status: 404,
                ..Resp::json(json!({"errorCode": "errors.com.epicgames.not_found",
                                    "errorMessage": "Sorry, not found"}))
            },
        });
        let c = Client::with_endpoints(Identity::default(), endpoints(&server)).unwrap();
        let s = c.login("code").await.unwrap();
        assert_eq!(s.access_token, "NEW");
        assert!(s.expires_at >= now() + 7199);
        assert!(s.refresh_expires_at >= now() + 99);
        assert_eq!(c.exchange_code(&session()).await.unwrap(), "XCHG");
        let assets = c.assets(&session(), "Windows").await.unwrap();
        assert_eq!(assets[0].sidecar_rvn, 1);
        assert_eq!(assets[0].label_name, "Live-Windows");
        let lib = c.library_items(&session()).await.unwrap();
        assert_eq!(lib.len(), 2);
        let item = c
            .catalog_item(&session(), "ns", "cat")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item["title"], "Sugar");
        let info = ManifestInfo {
            hash: manifest_hash,
            manifest_urls: vec![server.url("/missing"), server.url("/m.manifest")],
            ..Default::default()
        };
        assert_eq!(c.download_manifest(&info).await.unwrap(), manifest);
        let bad = ManifestInfo {
            hash: "00".into(),
            ..info
        };
        assert!(c.download_manifest(&bad).await.is_err());
        let ticket = c.download_ticket(&session(), &assets[0]).await.unwrap();
        assert_eq!(ticket.expires_at, 1_893_456_000);
        let urls = c
            .signed_chunk_urls(&session(), &ticket, &["ChunksV4/00/x.chunk".into()])
            .await
            .unwrap();
        assert_eq!(urls["ChunksV4/00/x.chunk"], "https://signed/x");
        let err = c.entitlements(&session()).await.unwrap_err();
        assert!(format!("{err:#}").contains("Sorry, not found"), "{err:#}");
        c.kill_session(&session()).await.unwrap();
    }

    #[test]
    fn applies_identity_overrides() {
        let mut id = Identity::default();
        id.apply_overrides(&json!({"version": "16.0.0", "label": "Live-X",
                                   "client_id": "id", "client_secret": "sec"}));
        assert!(id.user_agent.starts_with("UELauncher/16.0.0 "));
        assert_eq!((id.label.as_str(), id.client_id.as_str()), ("Live-X", "id"));
        let dbg = format!("{:?}", session());
        assert!(!dbg.contains("AT") && dbg.contains("Player"));
    }
}
