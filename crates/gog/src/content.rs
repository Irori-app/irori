//! GOG's content system: builds, build metadata ("meta"), depot manifests,
//! secure (signed) download links and the redistributables repository.

use std::collections::HashMap;
use std::fmt;
use std::io::Read as _;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A JSON id that may be a number or a string, as a string.
pub fn id_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

pub(crate) fn de_id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(id_string(&Value::deserialize(d)?))
}

fn de_ids<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(a) => a.iter().map(id_string).filter(|s| !s.is_empty()).collect(),
        _ => Vec::new(),
    })
}

fn de_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_u64().unwrap_or(0),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    })
}

/// `ab/cd/abcdef…`: where content-addressed files live on GOG's CDN.
pub fn galaxy_path(hash: &str) -> String {
    if hash.contains('/') || hash.len() < 4 {
        hash.to_string()
    } else {
        format!("{}/{}/{hash}", &hash[0..2], &hash[2..4])
    }
}

/// Service base addresses (overridable for tests).
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub content_system: String,
    pub cdn: String,
    pub api: String,
    pub embed: String,
    pub cloud_storage: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            content_system: "https://content-system.gog.com".into(),
            cdn: "https://gog-cdn-fastly.gog.com".into(),
            api: "https://api.gog.com".into(),
            embed: "https://embed.gog.com".into(),
            cloud_storage: "https://cloudstorage.gog.com".into(),
        }
    }
}

/// One build of a product.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildItem {
    #[serde(deserialize_with = "de_id")]
    pub build_id: String,
    #[serde(deserialize_with = "de_id")]
    pub legacy_build_id: String,
    pub os: String,
    pub branch: Option<String>,
    pub version_name: String,
    pub date_published: String,
    pub generation: u32,
    pub link: Option<String>,
    pub urls: Vec<BuildUrl>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildUrl {
    pub url: String,
}

impl BuildItem {
    /// Candidate meta URLs of this build.
    pub fn meta_urls(&self) -> Vec<String> {
        let mut out: Vec<String> = self.urls.iter().map(|u| u.url.clone()).collect();
        if let Some(l) = &self.link
            && !out.contains(l)
        {
            out.push(l.clone());
        }
        out.retain(|u| !u.is_empty());
        out
    }
}

/// Builds of a product on one OS, newest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Builds {
    pub items: Vec<BuildItem>,
    pub has_private_branches: bool,
}

impl Builds {
    /// Newest build of `branch` (`None` = default branch).
    pub fn latest(&self, branch: Option<&str>) -> Option<&BuildItem> {
        let branch = branch.filter(|b| !b.is_empty());
        self.items
            .iter()
            .find(|b| b.branch.as_deref().filter(|s| !s.is_empty()) == branch)
    }

    /// The build to install: `build` when given, else the newest of `branch`,
    /// else the newest default-branch build, else the newest.
    pub fn select(&self, branch: Option<&str>, build: Option<&str>) -> Option<&BuildItem> {
        if let Some(id) = build.filter(|b| !b.is_empty()) {
            return self.items.iter().find(|b| b.build_id == id);
        }
        self.latest(branch)
            .or_else(|| self.latest(None))
            .or_else(|| self.items.first())
    }

    /// Branch names offered (without the default branch).
    pub fn branches(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .items
            .iter()
            .filter_map(|b| b.branch.clone())
            .filter(|b| !b.is_empty())
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

/// A depot of a build (part of a product in some languages).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DepotRef {
    pub product_id: String,
    pub languages: Vec<String>,
    /// v2: hash of the depot manifest; v1: manifest file name.
    pub manifest: String,
    pub size: u64,
    pub compressed_size: u64,
    /// v2: `["32"]`/`["64"]` when bitness-specific.
    pub bitness: Vec<String>,
}

/// A product (game or DLC) of a build.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProductRef {
    pub id: String,
    pub name: String,
}

/// A parsed build meta, generation 1 or 2. The raw document is kept for
/// callers that need more (setup scripts).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BuildMeta {
    pub generation: u32,
    pub base_product: String,
    pub install_directory: String,
    pub products: Vec<ProductRef>,
    pub depots: Vec<DepotRef>,
    pub dependencies: Vec<String>,
    /// The game's own client (cloud saves).
    pub client_id: String,
    pub client_secret: String,
    /// v1: depot manifests are served below this timestamp folder.
    pub timestamp: String,
    pub raw: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct V2Meta {
    #[serde(default, deserialize_with = "de_id")]
    base_product_id: String,
    #[serde(default)]
    install_directory: String,
    #[serde(default)]
    products: Vec<V2Product>,
    #[serde(default)]
    depots: Vec<V2Depot>,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default, deserialize_with = "de_id")]
    client_id: String,
    #[serde(default)]
    client_secret: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct V2Product {
    #[serde(default, deserialize_with = "de_id")]
    product_id: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct V2Depot {
    #[serde(default, deserialize_with = "de_id")]
    product_id: String,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    manifest: String,
    #[serde(default, deserialize_with = "de_u64")]
    size: u64,
    #[serde(default, deserialize_with = "de_u64")]
    compressed_size: u64,
    #[serde(default)]
    os_bitness: Vec<String>,
}

impl BuildMeta {
    pub fn parse(raw: Value) -> Result<Self> {
        let generation = raw.get("version").and_then(Value::as_u64).unwrap_or(2) as u32;
        if generation == 1 {
            return Self::parse_v1(raw);
        }
        let m: V2Meta = serde_json::from_value(raw.clone()).context("invalid build meta")?;
        Ok(Self {
            generation: 2,
            base_product: m.base_product_id,
            install_directory: m.install_directory,
            products: m
                .products
                .into_iter()
                .map(|p| ProductRef {
                    id: p.product_id,
                    name: p.name,
                })
                .collect(),
            depots: m
                .depots
                .into_iter()
                .map(|d| DepotRef {
                    product_id: d.product_id,
                    languages: d.languages,
                    manifest: d.manifest,
                    size: d.size,
                    compressed_size: d.compressed_size,
                    bitness: d.os_bitness,
                })
                .collect(),
            dependencies: m.dependencies,
            client_id: m.client_id,
            client_secret: m.client_secret,
            timestamp: String::new(),
            raw,
        })
    }

    fn parse_v1(raw: Value) -> Result<Self> {
        let p = raw.get("product").context("invalid v1 build meta")?;
        let s = |k: &str| p.get(k).map(id_string).unwrap_or_default();
        let mut depots = Vec::new();
        let mut dependencies = Vec::new();
        for d in p
            .get("depots")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(redist) = d.get("redist").and_then(Value::as_str) {
                dependencies.push(redist.to_string());
                continue;
            }
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct V1Depot {
                #[serde(default, rename = "gameIDs", deserialize_with = "de_ids")]
                game_ids: Vec<String>,
                #[serde(default)]
                languages: Vec<String>,
                #[serde(default)]
                manifest: String,
                #[serde(default, deserialize_with = "de_u64")]
                size: u64,
            }
            let v: V1Depot = serde_json::from_value(d.clone()).context("invalid v1 depot")?;
            // A depot shared by several products is listed once per product.
            for id in v.game_ids {
                depots.push(DepotRef {
                    product_id: id,
                    languages: v.languages.clone(),
                    manifest: v.manifest.clone(),
                    size: v.size,
                    compressed_size: v.size,
                    bitness: Vec::new(),
                });
            }
        }
        let products = p
            .get("gameIDs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|g| ProductRef {
                id: g.get("gameID").map(id_string).unwrap_or_default(),
                name: g
                    .pointer("/name/en")
                    .or_else(|| g.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect();
        Ok(Self {
            generation: 1,
            base_product: s("rootGameID"),
            install_directory: s("installDirectory"),
            products,
            depots,
            dependencies,
            client_id: String::new(),
            client_secret: String::new(),
            timestamp: s("timestamp"),
            raw,
        })
    }

    /// Languages offered (canonical codes).
    pub fn languages(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .depots
            .iter()
            .flat_map(|d| &d.languages)
            .filter_map(|l| crate::languages::code(l))
            .map(str::to_string)
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Depots of `products` for `language` (and this machine's bitness).
    pub fn depots_for(&self, products: &[String], language: &str) -> Vec<&DepotRef> {
        let bits = if cfg!(target_pointer_width = "64") {
            "64"
        } else {
            "32"
        };
        self.depots
            .iter()
            .filter(|d| products.contains(&d.product_id))
            .filter(|d| {
                d.languages
                    .iter()
                    .any(|l| crate::languages::matches(l, language))
            })
            .filter(|d| d.bitness.is_empty() || d.bitness.iter().any(|b| b == bits))
            .collect()
    }

    /// `(download, disk)` sizes of `product` for `language` (the language's
    /// own depots plus the language-neutral ones).
    pub fn size_of(&self, product: &str, language: &str) -> (u64, u64) {
        self.depots_for(&[product.to_string()], language)
            .iter()
            .fold((0, 0), |(dl, disk), d| {
                (dl + d.compressed_size, disk + d.size)
            })
    }
}

/// A file of a v2 depot.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct V2Chunk {
    pub md5: String,
    #[serde(deserialize_with = "de_u64")]
    pub size: u64,
    #[serde(rename = "compressedMd5")]
    pub compressed_md5: String,
    #[serde(rename = "compressedSize", deserialize_with = "de_u64")]
    pub compressed_size: u64,
}

/// An entry of a depot manifest.
#[derive(Debug, Clone, PartialEq)]
pub enum DepotItem {
    File {
        product: String,
        path: String,
        chunks: Vec<V2Chunk>,
        md5: Option<String>,
        sha256: Option<String>,
        executable: bool,
        /// Installer support file (kept outside the game folder).
        support: bool,
    },
    Link {
        path: String,
        target: String,
    },
    Dir {
        path: String,
    },
}

impl DepotItem {
    pub fn path(&self) -> &str {
        match self {
            DepotItem::File { path, .. }
            | DepotItem::Link { path, .. }
            | DepotItem::Dir { path } => path,
        }
    }
}

/// Parse a v2 depot manifest of `product`.
pub fn parse_v2_depot(v: &Value, product: &str) -> Result<Vec<DepotItem>> {
    let items = v
        .pointer("/depot/items")
        .and_then(Value::as_array)
        .context("invalid depot manifest")?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let path = item
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .replace('\\', "/");
        match item.get("type").and_then(Value::as_str) {
            Some("DepotFile") => {
                let flags: Vec<&str> = item
                    .get("flags")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let chunks: Vec<V2Chunk> = serde_json::from_value(
                    item.get("chunks")
                        .cloned()
                        .unwrap_or(Value::Array(Vec::new())),
                )
                .context("invalid depot chunks")?;
                let s = |k: &str| {
                    item.get(k)
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                };
                out.push(DepotItem::File {
                    product: product.to_string(),
                    path,
                    chunks,
                    md5: s("md5"),
                    sha256: s("sha256"),
                    executable: flags.contains(&"executable"),
                    support: flags.contains(&"support"),
                });
            }
            Some("DepotLink") => out.push(DepotItem::Link {
                path,
                target: item
                    .get("target")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .replace('\\', "/"),
            }),
            Some("DepotDirectory") => out.push(DepotItem::Dir { path }),
            _ => {}
        }
    }
    Ok(out)
}

/// A file of a v1 depot: a slice of the product's `main.bin`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct V1File {
    pub product: String,
    pub path: String,
    pub offset: u64,
    pub size: u64,
    pub md5: String,
    pub executable: bool,
    pub support: bool,
}

/// Parse a v1 depot manifest of `product`: files and directories.
pub fn parse_v1_depot(v: &Value, product: &str) -> Result<(Vec<V1File>, Vec<String>)> {
    let files = v
        .pointer("/depot/files")
        .and_then(Value::as_array)
        .context("invalid v1 depot manifest")?;
    let mut out = Vec::new();
    let mut dirs = Vec::new();
    for f in files {
        let path = f
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim_start_matches('/')
            .replace('\\', "/");
        if f.get("directory").and_then(Value::as_bool) == Some(true) {
            dirs.push(path);
            continue;
        }
        let n = |k: &str| f.get(k).map(|v| v.as_u64().unwrap_or(0)).unwrap_or(0);
        out.push(V1File {
            product: product.to_string(),
            path,
            offset: n("offset"),
            size: n("size"),
            md5: f
                .get("hash")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            executable: f.get("executable").and_then(Value::as_bool) == Some(true),
            support: f.get("support").and_then(Value::as_bool) == Some(true),
        });
    }
    Ok((out, dirs))
}

/// A download endpoint from a secure link.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Endpoint {
    #[serde(default)]
    pub url_format: Option<String>,
    #[serde(default)]
    pub parameters: HashMap<String, Value>,
    #[serde(default)]
    pub url: Option<String>,
}

impl Endpoint {
    /// The URL of `suffix` (appended to the endpoint's path). The
    /// redistributables' endpoints have a plain address as their format,
    /// with no `{path}` to fill in.
    pub fn url_for(&self, suffix: &str) -> Option<String> {
        if let Some(format) = &self.url_format {
            let mut params = self.parameters.clone();
            let path = params
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            params.insert("path".into(), Value::String(format!("{path}/{suffix}")));
            let mut url = format.clone();
            for (k, v) in &params {
                let value = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                url = url.replace(&format!("{{{k}}}"), &value);
            }
            if !format.contains("{path}") {
                url = format!("{}/{suffix}", url.trim_end_matches('/'));
            }
            return Some(url);
        }
        self.url.as_ref().map(|u| format!("{u}/{suffix}"))
    }
}

/// The redistributables repository.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Repository {
    #[serde(deserialize_with = "de_id")]
    pub build_id: String,
    pub depots: Vec<DependencyDepot>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DependencyDepot {
    pub dependency_id: String,
    pub executable: DependencyExecutable,
    pub manifest: String,
    #[serde(deserialize_with = "de_u64")]
    pub size: u64,
    #[serde(deserialize_with = "de_u64")]
    pub compressed_size: u64,
    pub readable_name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DependencyExecutable {
    pub path: String,
    pub arguments: String,
}

impl DependencyDepot {
    /// Installed once in the shared redistributables folder (`__redist`),
    /// rather than into each game.
    pub fn is_global(&self) -> bool {
        self.executable.path.starts_with("__redist")
    }
}

fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    match flate2::read::ZlibDecoder::new(data).read_to_end(&mut out) {
        Ok(_) => Ok(out),
        // Some documents are served uncompressed.
        Err(_) if data.first() == Some(&b'{') => Ok(data.to_vec()),
        Err(e) => Err(e).context("could not decompress the GOG document"),
    }
}

/// HTTP client for the content system.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    pub endpoints: Endpoints,
    /// Galaxy access token for authenticated calls.
    token: Option<String>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(endpoints: Endpoints) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(crate::USER_AGENT)
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(60))
                .pool_max_idle_per_host(16)
                .build()?,
            endpoints,
            token: None,
        })
    }

    /// The same client sending `token`.
    pub fn with_token(&self, token: &str) -> Self {
        Self {
            token: Some(token.to_string()),
            ..self.clone()
        }
    }

    /// Client for file transfers (no total timeout).
    pub fn download_client() -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .user_agent(crate::USER_AGENT)
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(32)
            .build()?)
    }

    pub(crate) fn get(&self, url: &str) -> reqwest::RequestBuilder {
        let req = self.http.get(url);
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    pub(crate) async fn json<T: DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<T> {
        let resp = req
            .send()
            .await
            .map_err(|e| anyhow!("Could not reach GOG ({})", e.without_url()))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("GOG answered {status}");
        }
        resp.json().await.context("unexpected answer from GOG")
    }

    /// A zlib-compressed JSON document and its ETag.
    pub async fn zlib_json(&self, url: &str) -> Result<(Value, Option<String>)> {
        let resp = self
            .get(url)
            .send()
            .await
            .map_err(|e| anyhow!("Could not reach GOG ({})", e.without_url()))?;
        if !resp.status().is_success() {
            bail!("GOG answered {}", resp.status());
        }
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = resp.bytes().await?;
        let raw = tokio::task::spawn_blocking(move || inflate(&bytes)).await??;
        Ok((
            serde_json::from_slice(&raw).context("invalid GOG document")?,
            etag,
        ))
    }

    /// Builds of `product` for `os` (`windows`, `osx`).
    pub async fn builds(&self, product: &str, os: &str, password: Option<&str>) -> Result<Builds> {
        let mut req = self
            .get(&format!(
                "{}/products/{product}/os/{os}/builds",
                self.endpoints.content_system
            ))
            .query(&[("generation", "2"), ("_version", "2")]);
        if let Some(p) = password.filter(|p| !p.is_empty()) {
            req = req.query(&[("password", p)]);
        }
        self.json(req)
            .await
            .context("Could not list the game's builds")
    }

    /// The meta document of a build and its ETag.
    pub async fn build_meta(&self, build: &BuildItem) -> Result<(BuildMeta, Option<String>)> {
        let mut last = anyhow!("the build has no meta address");
        for url in build.meta_urls() {
            match self.zlib_json(&url).await {
                Ok((v, etag)) => return Ok((BuildMeta::parse(v)?, etag)),
                Err(e) => last = e,
            }
        }
        Err(last.context("Could not download the build's file list"))
    }

    /// Whether a build's meta changed since `etag` (`None` when no URL
    /// answered).
    pub async fn meta_changed(&self, build: &BuildItem, etag: &str) -> Option<bool> {
        for u in build.meta_urls() {
            let Ok(resp) = self.get(&u).header("If-None-Match", etag).send().await else {
                continue;
            };
            match resp.status() {
                reqwest::StatusCode::NOT_MODIFIED => return Some(false),
                s if s.is_success() => {
                    let new = resp
                        .headers()
                        .get(reqwest::header::ETAG)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    return Some(new.as_deref() != Some(etag));
                }
                _ => {}
            }
        }
        None
    }

    /// A v2 depot manifest.
    pub async fn v2_depot(&self, hash: &str) -> Result<Value> {
        Ok(self
            .zlib_json(&format!(
                "{}/content-system/v2/meta/{}",
                self.endpoints.cdn,
                galaxy_path(hash)
            ))
            .await
            .context("Could not download a depot's file list")?
            .0)
    }

    /// A v1 depot manifest.
    pub async fn v1_depot(
        &self,
        product: &str,
        os: &str,
        timestamp: &str,
        manifest: &str,
    ) -> Result<Value> {
        self.json(self.get(&format!(
            "{}/content-system/v1/manifests/{product}/{os}/{timestamp}/{manifest}",
            self.endpoints.cdn
        )))
        .await
        .context("Could not download a depot's file list")
    }

    /// Signed download endpoints of a product's files (generation 2), or of
    /// its v1 depot (generation 1).
    /// `path` is `/` for generation 2 and the build's `/os/timestamp/` folder
    /// for generation 1.
    pub async fn secure_link(
        &self,
        product: &str,
        generation: u32,
        path: &str,
    ) -> Result<Vec<Endpoint>> {
        let mut req = self
            .get(&format!(
                "{}/products/{product}/secure_link",
                self.endpoints.content_system
            ))
            .query(&[("_version", "2"), ("path", path)]);
        req = if generation == 1 {
            req.query(&[("type", "depot")])
        } else {
            req.query(&[("generation", "2")])
        };
        #[derive(Deserialize)]
        struct Links {
            #[serde(default)]
            urls: Vec<Endpoint>,
        }
        let links: Links = self
            .json(req)
            .await
            .context("Could not get the download addresses")?;
        if links.urls.is_empty() {
            bail!("GOG listed no download addresses");
        }
        Ok(links.urls)
    }

    /// Download endpoints of the redistributables store (no expiry).
    pub async fn dependency_link(&self) -> Result<Vec<Endpoint>> {
        #[derive(Deserialize)]
        struct Links {
            #[serde(default)]
            urls: Vec<Endpoint>,
        }
        let links: Links = self
            .json(
                self.get(&format!("{}/open_link", self.endpoints.content_system))
                    .query(&[
                        ("generation", "2"),
                        ("_version", "2"),
                        ("path", "/dependencies/store/"),
                    ]),
            )
            .await
            .context("Could not get the redistributables address")?;
        Ok(links.urls)
    }

    /// The redistributables repository.
    pub async fn repository(&self) -> Result<Repository> {
        #[derive(Deserialize)]
        struct Build {
            #[serde(default, deserialize_with = "de_id")]
            build_id: String,
            #[serde(default)]
            repository_manifest: String,
        }
        let build: Build = self
            .json(
                self.get(&format!(
                    "{}/dependencies/repository",
                    self.endpoints.content_system
                ))
                .query(&[("generation", "2")]),
            )
            .await
            .context("Could not read the redistributables repository")?;
        let (v, _) = self.zlib_json(&build.repository_manifest).await?;
        let mut repo: Repository = serde_json::from_value(v).context("invalid repository")?;
        repo.build_id = build.build_id;
        Ok(repo)
    }

    /// Build id of the redistributables repository (update checks).
    pub async fn repository_build(&self) -> Result<String> {
        let v: Value = self
            .json(
                self.get(&format!(
                    "{}/dependencies/repository",
                    self.endpoints.content_system
                ))
                .query(&[("generation", "2")]),
            )
            .await?;
        Ok(v.get("build_id").map(id_string).unwrap_or_default())
    }

    /// The file list of a redistributable.
    pub async fn dependency_depot(&self, manifest: &str) -> Result<Value> {
        Ok(self
            .zlib_json(&format!(
                "{}/content-system/v2/dependencies/meta/{}",
                self.endpoints.cdn,
                galaxy_path(manifest)
            ))
            .await?
            .0)
    }

    /// Product ids the account owns.
    pub async fn owned(&self) -> Result<Vec<String>> {
        let v: Value = self
            .json(self.get(&format!("{}/user/data/games", self.endpoints.embed)))
            .await
            .context("Could not list owned games")?;
        Ok(v.get("owned")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(id_string).collect())
            .unwrap_or_default())
    }

    /// `api.gog.com/products/<id>` with the given expansions.
    pub async fn product(&self, id: &str, expand: &[&str]) -> Result<Value> {
        let mut req = self.get(&format!("{}/products/{id}", self.endpoints.api));
        if !expand.is_empty() {
            req = req.query(&[("expand", expand.join(","))]);
        }
        self.json(req).await
    }

    /// Resolve an installer `downlink` to the real file address.
    pub async fn resolve_downlink(&self, downlink: &str) -> Result<String> {
        let v: Value = self.json(self.get(downlink)).await?;
        v.get("downlink")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("GOG gave no installer address")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn plain_endpoints_get_the_file_appended() {
        let e: Endpoint = serde_json::from_value(json!({
            "url_format": "https://cdn/content-system/v2/dependencies/store",
            "parameters": {}
        }))
        .unwrap();
        assert_eq!(
            e.url_for("2c/5f/2c5f").as_deref(),
            Some("https://cdn/content-system/v2/dependencies/store/2c/5f/2c5f")
        );
    }

    #[test]
    fn galaxy_paths_and_endpoints() {
        assert_eq!(galaxy_path("abcdef"), "ab/cd/abcdef");
        assert_eq!(galaxy_path("ab/cd/x"), "ab/cd/x");
        let e: Endpoint = serde_json::from_value(json!({
            "url_format": "{base_url}/token=nva={expires}~hmac={hmac}{path}",
            "parameters": {"base_url": "https://cdn", "path": "/content-system/v2/store/1/", "expires": 5, "hmac": "h"}
        }))
        .unwrap();
        assert_eq!(
            e.url_for("ab/cd/abcd").unwrap(),
            "https://cdn/token=nva=5~hmac=h/content-system/v2/store/1//ab/cd/abcd"
        );
        let plain = Endpoint {
            url: Some("https://redist".into()),
            ..Default::default()
        };
        assert_eq!(plain.url_for("x").unwrap(), "https://redist/x");
    }

    #[test]
    fn parses_v2_meta_and_depots() {
        let meta = BuildMeta::parse(json!({
            "version": 2, "baseProductId": "1", "installDirectory": "Game", "clientId": "c", "clientSecret": "s",
            "dependencies": ["MSVC2017"],
            "products": [{"productId": "1", "name": "Game"}, {"productId": "2", "name": "DLC"}],
            "depots": [
                {"productId": "1", "languages": ["*"], "manifest": "aaaa", "size": 100, "compressedSize": 50},
                {"productId": "1", "languages": ["en-US"], "manifest": "bbbb", "size": 10, "compressedSize": 5},
                {"productId": "1", "languages": ["German"], "manifest": "cccc", "size": 20, "compressedSize": 7},
                {"productId": "1", "languages": ["*"], "manifest": "dddd", "size": 1, "compressedSize": 1, "osBitness": ["32"]},
                {"productId": "2", "languages": ["*"], "manifest": "eeee", "size": 3, "compressedSize": 2}
            ]}))
        .unwrap();
        assert_eq!(meta.generation, 2);
        assert_eq!(meta.languages(), ["de-DE", "en-US"]);
        let en = meta.depots_for(&["1".into()], "en-US");
        let names: Vec<&str> = en.iter().map(|d| d.manifest.as_str()).collect();
        assert_eq!(names, ["aaaa", "bbbb"]);
        assert_eq!(meta.size_of("1", "de-DE"), (57, 120));
        assert_eq!(meta.size_of("2", "en-US"), (2, 3));

        let items = parse_v2_depot(
            &json!({"depot": {"items": [
                {"type": "DepotFile", "path": "bin\\game.exe", "flags": ["executable"], "md5": "m",
                 "chunks": [{"md5": "c", "size": 10, "compressedMd5": "z", "compressedSize": 5}]},
                {"type": "DepotFile", "path": "setup.exe", "flags": ["support"], "chunks": []},
                {"type": "DepotLink", "path": "lib/a.so", "target": "a.so.1"},
                {"type": "DepotDirectory", "path": "saves"}
            ]}}),
            "1",
        )
        .unwrap();
        assert!(
            matches!(&items[0], DepotItem::File { path, executable: true, md5: Some(_), .. } if path == "bin/game.exe")
        );
        assert!(matches!(&items[1], DepotItem::File { support: true, .. }));
        assert!(matches!(&items[2], DepotItem::Link { .. }));
        assert_eq!(items[3].path(), "saves");
    }

    #[test]
    fn parses_v1_meta_and_depots() {
        let meta = BuildMeta::parse(json!({"version": 1, "product": {
            "rootGameID": "10", "installDirectory": "Old", "timestamp": "123",
            "gameIDs": [{"gameID": "10", "name": {"en": "Old"}}, {"gameID": 11, "name": {"en": "Old DLC"}}],
            "depots": [
                {"gameIDs": ["10"], "languages": ["Neutral"], "manifest": "a.json", "size": "100"},
                {"gameIDs": ["10", 11], "languages": ["English"], "manifest": "b.json", "size": "5"},
                {"redist": "DOSBOX", "size": 1}
            ]}}))
        .unwrap();
        assert_eq!(meta.generation, 1);
        assert_eq!(meta.dependencies, ["DOSBOX"]);
        assert_eq!(meta.products[1].id, "11");
        assert_eq!(meta.depots_for(&["10".into()], "en-US").len(), 2);
        assert_eq!(meta.depots_for(&["11".into()], "en-US").len(), 1);
        let (files, dirs) = parse_v1_depot(
            &json!({"depot": {"files": [
                {"path": "/game.exe", "size": 50, "offset": 100, "hash": "h", "executable": true},
                {"path": "/data", "directory": true}]}}),
            "10",
        )
        .unwrap();
        assert_eq!(
            (files[0].offset, files[0].size, files[0].path.as_str()),
            (100, 50, "game.exe")
        );
        assert_eq!(dirs, ["data"]);
    }

    #[test]
    fn selects_builds() {
        let b: Builds = serde_json::from_value(json!({"items": [
            {"build_id": "3", "branch": "beta", "generation": 2},
            {"build_id": "2", "branch": null, "generation": 2},
            {"build_id": 1, "branch": null, "generation": 1}]}))
        .unwrap();
        assert_eq!(b.select(None, None).unwrap().build_id, "2");
        assert_eq!(b.select(Some("beta"), None).unwrap().build_id, "3");
        assert_eq!(b.select(Some("gone"), None).unwrap().build_id, "2");
        assert_eq!(b.select(None, Some("1")).unwrap().generation, 1);
        assert!(b.select(None, Some("9")).is_none());
        assert_eq!(b.branches(), ["beta"]);
    }
}
