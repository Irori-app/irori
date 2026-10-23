//! Install info, install, update, repair, uninstall, import and update
//! checks. Builds come from GOG's content system (or, for Linux, the
//! offline installers) through [`ember_gog_api`] and are downloaded by
//! [`ember_depot`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use ember_core::process::{self, CommandSpec, RunOptions};
use ember_core::{
    BuildInfo, DlcInfo, InstallInfo, InstallInfoQuery, InstallPlatform, InstallProgress,
    InstallRequest, LanguageSize, OpContext, Outcome, PrepareStage, Preparing, RemoveArgs,
    UpdateRequest, util,
};
use ember_depot::{Finished, Job, Verified};
use ember_gog_api::content::{BuildItem, BuildMeta, Builds, Client, Repository, id_string};
use ember_gog_api::linux::Installer;
use ember_gog_api::plan::{self as gplan, BuildFiles, Plan, Selection};

use crate::build::{self, BuildRecord};
use crate::installed::{InstalledRecord, PendingInstall, dir_size};
use crate::library::REDIST_APP;
use crate::manifest;
use crate::{GogStore, load_json, save_json};
use ember_core::Store as _;

const INFO_TTL_SECS: u64 = 6 * 3600;

/// Download and disk size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SizePair {
    pub download_size: u64,
    pub disk_size: u64,
}

/// An owned DLC offered with a build.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DlcSizes {
    pub title: String,
    pub id: String,
    /// Language code (`*` for every language) → size.
    pub size: HashMap<String, SizePair>,
}

/// What the install dialog needs to know about a build.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildSummary {
    /// Language code (`*` for every language) → size of the base game.
    pub size: HashMap<String, SizePair>,
    /// Totals that do not depend on the language (Linux installers).
    pub download_size: Option<u64>,
    pub disk_size: Option<u64>,
    pub languages: Vec<String>,
    pub dlcs: Vec<DlcSizes>,
    pub build_id: String,
    pub version_name: String,
    pub version_etag: String,
    pub folder_name: String,
    pub branches: Vec<String>,
    pub builds: Builds,
    pub dependencies: Vec<String>,
    /// The game predates the content system (nothing to install).
    pub legacy: bool,
}

impl BuildSummary {
    fn lang_size(sizes: &HashMap<String, SizePair>, lang: &str) -> SizePair {
        let all = sizes.get("*").copied().unwrap_or_default();
        let l = sizes.get(lang).copied().unwrap_or_default();
        SizePair {
            download_size: all.download_size + l.download_size,
            disk_size: all.disk_size + l.disk_size,
        }
    }

    /// Base game plus every DLC for `lang` (fixed totals win when given).
    pub fn total_for(&self, lang: &str) -> SizePair {
        let mut t = Self::lang_size(&self.size, lang);
        for d in &self.dlcs {
            let s = Self::lang_size(&d.size, lang);
            t.download_size += s.download_size;
            t.disk_size += s.disk_size;
        }
        SizePair {
            download_size: self.download_size.unwrap_or(t.download_size),
            disk_size: self.disk_size.unwrap_or(t.disk_size),
        }
    }
}

/// Sizes of `product`'s depots by language.
pub fn size_map(meta: &BuildMeta, product: &str) -> HashMap<String, SizePair> {
    let mut out: HashMap<String, SizePair> = HashMap::new();
    out.insert("*".into(), SizePair::default());
    let bits = if cfg!(target_pointer_width = "64") {
        "64"
    } else {
        "32"
    };
    for d in meta
        .depots
        .iter()
        .filter(|d| d.product_id == product)
        .filter(|d| d.bitness.is_empty() || d.bitness.iter().any(|b| b == bits))
    {
        for l in &d.languages {
            let key = if l == "*" || l.eq_ignore_ascii_case("neutral") {
                "*"
            } else {
                ember_gog_api::languages::code(l).unwrap_or(l)
            };
            let e = out.entry(key.to_string()).or_default();
            e.download_size += d.compressed_size;
            e.disk_size += d.size;
        }
    }
    out
}

/// Summarise a content-system build. `owned` lists owned product ids;
/// `repo` adds the redistributables installed into the game folder.
pub fn summarize(
    builds: &Builds,
    build: &BuildItem,
    meta: &BuildMeta,
    etag: Option<&str>,
    owned: &[String],
    repo: Option<&Repository>,
) -> BuildSummary {
    let mut size = size_map(meta, &meta.base_product);
    if let Some(repo) = repo {
        let all = size.entry("*".into()).or_default();
        for d in repo
            .depots
            .iter()
            .filter(|d| meta.dependencies.contains(&d.dependency_id) && !d.is_global())
        {
            all.download_size += d.compressed_size;
            all.disk_size += d.size;
        }
    }
    let dlcs = meta
        .products
        .iter()
        .filter(|p| p.id != meta.base_product && owned.contains(&p.id))
        .map(|p| DlcSizes {
            title: p.name.clone(),
            id: p.id.clone(),
            size: size_map(meta, &p.id),
        })
        .collect();
    BuildSummary {
        size,
        download_size: None,
        disk_size: None,
        languages: meta.languages(),
        dlcs,
        build_id: build.build_id.clone(),
        version_name: build.version_name.clone(),
        version_etag: etag.unwrap_or_default().to_string(),
        folder_name: meta.install_directory.clone(),
        branches: builds.branches(),
        builds: builds.clone(),
        dependencies: meta.dependencies.clone(),
        legacy: false,
    }
}

/// Linux installers of a product (product API with `downloads`).
pub fn linux_installers(product: &Value) -> Vec<&Value> {
    product
        .pointer("/downloads/installers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|i| i.get("os").and_then(Value::as_str) == Some("linux"))
        .collect()
}

/// The installer for `language`: the only one, the one in that language,
/// else the English one (they are multilingual).
pub fn pick_installer<'a>(installers: &[&'a Value], language: &str) -> Option<&'a Value> {
    if installers.len() == 1 {
        return installers.first().copied();
    }
    let lang = |i: &Value| {
        i.get("language")
            .and_then(Value::as_str)
            .and_then(ember_gog_api::languages::code)
    };
    let wanted = ember_gog_api::languages::code(language);
    installers
        .iter()
        .find(|i| wanted.is_some() && lang(i) == wanted)
        .or_else(|| installers.iter().find(|i| lang(i) == Some("en-US")))
        .copied()
}

fn installer_downlink(installer: &Value) -> Option<String> {
    installer
        .pointer("/files/0/downlink")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Pick the install language: the requested one when offered, then one
/// matching the UI language, then English, then the first offered.
pub fn choose_language(available: &[String], requested: Option<&str>, ui: &str) -> Option<String> {
    let requested = requested.filter(|r| !r.is_empty() && *r != "undefined");
    if let Some(r) = requested
        && (available.is_empty() || available.iter().any(|a| a.eq_ignore_ascii_case(r)))
    {
        return Some(r.to_string());
    }
    let ui = ui.replace('_', "-").to_ascii_lowercase();
    if !ui.is_empty()
        && let Some(a) = available
            .iter()
            .find(|a| a.to_ascii_lowercase() == ui)
            .or_else(|| {
                available
                    .iter()
                    .find(|a| a.to_ascii_lowercase().starts_with(&ui))
            })
    {
        return Some(a.clone());
    }
    available
        .iter()
        .find(|a| *a == "en-US")
        .or_else(|| available.iter().find(|a| a.starts_with("en")))
        .or_else(|| available.first())
        .cloned()
}

/// Map a [`BuildSummary`] to the install dialog's [`InstallInfo`].
pub fn map_install_info(
    app_name: &str,
    title: &str,
    platform: InstallPlatform,
    raw: &BuildSummary,
    language: Option<&str>,
    installed_dlcs: &[String],
) -> InstallInfo {
    let lang = language.map(str::to_string).unwrap_or_default();
    let total = raw.total_for(&lang);
    let mut builds: Vec<BuildInfo> = raw
        .builds
        .items
        .iter()
        .filter(|b| b.os.is_empty() || b.os == platform.as_str())
        .map(|b| BuildInfo {
            build_id: b.build_id.clone(),
            version_name: b.version_name.clone(),
            date: b.date_published.clone(),
            branch: b.branch.clone().filter(|s| !s.is_empty()),
        })
        .collect();
    builds.sort_by(|a, b| b.date.cmp(&a.date));
    InstallInfo {
        app_name: app_name.to_string(),
        title: title.to_string(),
        platform,
        download_size: total.download_size,
        disk_size: total.disk_size,
        version: (!raw.version_name.is_empty()).then(|| raw.version_name.clone()),
        languages: raw.languages.clone(),
        language_sizes: raw
            .languages
            .iter()
            .map(|l| {
                let s = raw.total_for(l);
                LanguageSize {
                    language: l.clone(),
                    download_size: s.download_size,
                    disk_size: s.disk_size,
                }
            })
            .collect(),
        dlcs: raw
            .dlcs
            .iter()
            .map(|d| DlcInfo {
                app_name: d.id.clone(),
                title: d.title.clone(),
                is_installed: installed_dlcs.contains(&d.id),
                owned: true,
            })
            .collect(),
        sdl: Vec::new(),
        branches: raw.branches.clone(),
        builds,
        dependencies: raw.dependencies.clone(),
        third_party_managed_app: None,
        is_native: platform == InstallPlatform::Linux,
        resumable: false,
    }
}

/// GOG's spelling of a platform.
pub fn platform_arg(p: InstallPlatform) -> Result<&'static str> {
    match p {
        InstallPlatform::Windows => Ok("windows"),
        InstallPlatform::Linux => Ok("linux"),
        InstallPlatform::Mac => Ok("osx"),
        InstallPlatform::Browser => bail!("GOG games cannot be installed as browser games"),
    }
}

/// `[InstallSettings] productID` of an Inno Setup uninstaller `.ini`.
pub fn ini_product_id(text: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line.eq_ignore_ascii_case("[InstallSettings]");
            continue;
        }
        if in_section
            && let Some((k, v)) = line.split_once('=')
            && k.trim().eq_ignore_ascii_case("productID")
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct CachedInfo {
    at: u64,
    info: BuildSummary,
}

fn info_cache_key(
    app: &str,
    platform: &str,
    branch: Option<&str>,
    build: Option<&str>,
    password: &str,
) -> String {
    use sha2::Digest as _;
    let pw = if password.is_empty() {
        String::new()
    } else {
        hex::encode(&sha2::Sha256::digest(password.as_bytes())[..8])
    };
    format!(
        "{app}_{platform}_{}_{}_{pw}",
        branch.unwrap_or("null"),
        build.unwrap_or("")
    )
}

/// A GOG game found in a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportScan {
    pub app_name: String,
    pub build_id: String,
    pub title: String,
    pub language: Option<String>,
    pub dlcs: Vec<String>,
    pub platform: InstallPlatform,
    /// Linux installers record their version in `gameinfo`.
    pub version_name: Option<String>,
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Find the GOG game in `path`: `goggame-*.info` files in the folder
/// (Windows), `Contents/Resources` (macOS) or `game` (Linux), or the
/// `gameinfo` file of a Linux installer.
pub fn scan_import(path: &Path) -> Option<ImportScan> {
    let candidates = [
        (path.to_path_buf(), InstallPlatform::Windows),
        (
            path.join("Contents").join("Resources"),
            InstallPlatform::Mac,
        ),
        (path.join("game"), InstallPlatform::Linux),
    ];
    let mut found: Option<(PathBuf, InstallPlatform, Vec<PathBuf>)> = None;
    for (dir, platform) in candidates {
        let mut infos: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("goggame-") && n.ends_with(".info"))
            })
            .collect();
        if !infos.is_empty() {
            infos.sort();
            found = Some((dir, platform, infos));
            break;
        }
    }
    let mut scan = ImportScan::default();
    if let Some((dir, platform, infos)) = &found {
        scan.platform = *platform;
        let mut root: Option<String> = None;
        let mut root_info = None;
        for p in infos {
            let Some(v) = read_json(p) else { continue };
            let id = v.get("gameId").map(id_string).unwrap_or_default();
            let r = root
                .get_or_insert_with(|| v.get("rootGameId").map(id_string).unwrap_or_default())
                .clone();
            if id == r {
                root_info = Some(v);
            } else if !id.is_empty() {
                scan.dlcs.push(id);
            }
        }
        scan.app_name = root.unwrap_or_default();
        if let Some(info) = root_info.filter(|_| *platform != InstallPlatform::Linux) {
            scan.title = info
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            scan.build_id = info.get("buildId").map(id_string).unwrap_or_default();
            scan.language = info
                .pointer("/languages/0")
                .or_else(|| info.get("language"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(id) = read_json(&dir.join(format!("goggame-{}.id", scan.app_name)))
                .and_then(|v| v.get("buildId").map(id_string))
                .filter(|b| !b.is_empty())
            {
                scan.build_id = id;
            }
        }
    }
    if found
        .as_ref()
        .is_none_or(|(_, p, _)| *p == InstallPlatform::Linux)
        && let Ok(text) = std::fs::read_to_string(path.join("gameinfo"))
    {
        let lines: Vec<&str> = text.lines().collect();
        scan.platform = InstallPlatform::Linux;
        scan.title = lines.first().unwrap_or(&"").to_string();
        scan.version_name = lines.get(1).map(|s| s.to_string());
        if scan.language.is_none() {
            scan.language = lines
                .get(3)
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty());
        }
        if lines.len() > 4 {
            scan.app_name = lines[4].trim().to_string();
            scan.build_id = lines.get(6).unwrap_or(&"").trim().to_string();
        }
    }
    scan.dlcs.sort();
    (!scan.app_name.is_empty()).then_some(scan)
}

/// What a deployment does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Install,
    Update,
    Repair,
}

/// A planned deployment and what to record once it finishes.
struct Deployment {
    dir: PathBuf,
    plan: Plan,
    record: BuildRecord,
    version: String,
    /// Content-system builds sign their chunk addresses.
    client: Option<Client>,
}

/// Append a line to the operation's log file.
fn log_line(ctx: &OpContext, line: &str) {
    use std::io::Write as _;
    let Some(path) = &ctx.log_file else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "[gog] {line}");
    }
}

pub(crate) fn progress_sink(ctx: &OpContext) -> ember_depot::ProgressFn {
    let sink = ctx.progress.clone();
    Arc::new(move |p: ember_depot::Progress| {
        sink(ember_core::InstallProgress {
            percent: Some(p.percent.clamp(0.0, 100.0)),
            bytes_done: Some(p.bytes_done),
            bytes_total: Some(p.bytes_total),
            eta_secs: p.eta_secs,
            download_speed: Some(p.download_speed),
            disk_speed: Some(p.disk_speed),
            bottleneck: p.bottleneck,
            preparing: p.preparing,
        })
    })
}

/// Whether `rel` (a normalised path) exists below `root`.
fn exists_below(root: &Path, rel: &str) -> bool {
    ember_depot::path::join(root, rel).is_file()
}

/// Create the plan's folders and symbolic links below `dir`.
fn finish_tree(dir: &Path, plan: &Plan) -> Result<()> {
    for d in &plan.dirs {
        let p = ember_depot::path::join(dir, d);
        std::fs::create_dir_all(&p).with_context(|| format!("creating {}", p.display()))?;
    }
    for (path, target) in &plan.links {
        let p = ember_depot::path::join(dir, path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ = std::fs::remove_file(&p);
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &p)
            .with_context(|| format!("linking {}", p.display()))?;
        #[cfg(not(unix))]
        tracing::debug!("not creating link {} → {target}", p.display());
    }
    Ok(())
}

impl GogStore {
    fn ui_language(&self) -> String {
        (self.settings)().ui_language.clone()
    }

    /// Content-system client with a valid Galaxy token.
    pub(crate) async fn content(&self) -> Result<Client> {
        let creds = self.auth.credentials().await?;
        Ok(self.content.with_token(&creds.access_token))
    }

    pub(crate) fn is_resumable(&self, app_name: &str) -> bool {
        self.paths.resume_file(app_name).is_file()
    }

    /// The builds of `app` and the one to install.
    async fn resolve_build(
        &self,
        client: &Client,
        app_name: &str,
        os: &str,
        branch: Option<&str>,
        build: Option<&str>,
    ) -> Result<Option<(Builds, BuildItem)>> {
        let password = self.private_branch_password(app_name).await;
        let builds = client.builds(app_name, os, Some(&password)).await?;
        let Some(item) = builds.select(branch, build).cloned() else {
            if let Some(b) = build.filter(|b| !b.is_empty()) {
                bail!(
                    "Build {b} of {} is no longer offered by GOG",
                    self.title_of(app_name)
                );
            }
            return Ok(None);
        };
        Ok(Some((builds, item)))
    }

    /// Folder name of a Linux install: the Windows build's install
    /// directory, as GOG Galaxy would use.
    async fn linux_folder(&self, client: &Client, app_name: &str, product: &Value) -> String {
        let from_windows = async {
            let (_, item) = self
                .resolve_build(client, app_name, "windows", None, None)
                .await
                .ok()??;
            let (meta, _) = client.build_meta(&item).await.ok()?;
            Some(meta.install_directory).filter(|d| !d.is_empty())
        };
        match from_windows.await {
            Some(d) => d,
            None => self
                .game(app_name)
                .and_then(|g| g.folder_name.clone())
                .filter(|f| !f.is_empty())
                .or_else(|| {
                    product
                        .get("title")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| app_name.to_string()),
        }
    }

    async fn summarize_linux(&self, client: &Client, app_name: &str) -> Result<BuildSummary> {
        let product = client
            .product(app_name, &["downloads", "expanded_dlcs"])
            .await
            .context("Could not read the game's downloads")?;
        let installers = linux_installers(&product);
        let mut languages: Vec<String> = installers
            .iter()
            .filter_map(|i| i.get("language").and_then(Value::as_str))
            .filter_map(ember_gog_api::languages::code)
            .map(str::to_string)
            .collect();
        languages.sort();
        languages.dedup();
        let base = pick_installer(&installers, &self.ui_language())
            .or_else(|| installers.first().copied())
            .with_context(|| format!("{} has no Linux installer", self.title_of(app_name)))?;
        let downlink = installer_downlink(base).context("the Linux installer has no download")?;
        let url = client.resolve_downlink(&downlink).await?;
        let index = ember_gog_api::linux::read_installer(&Client::download_client()?, &url).await?;
        let (download_size, disk_size) = index
            .entries
            .iter()
            .filter(|e| e.install_path().is_some())
            .fold((0, 0), |(d, s), e| (d + e.compressed_size, s + e.size));
        let owned = client.owned().await.unwrap_or_default();
        let dlcs = product
            .get("expanded_dlcs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|d| {
                let id = d.get("id").map(id_string)?;
                let total: u64 = linux_installers(d)
                    .iter()
                    .filter_map(|i| i.get("total_size").and_then(Value::as_u64))
                    .max()?;
                owned.contains(&id).then(|| DlcSizes {
                    title: d
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    id,
                    size: HashMap::from([(
                        "*".to_string(),
                        SizePair {
                            download_size: total,
                            disk_size: total,
                        },
                    )]),
                })
            })
            .collect::<Vec<_>>();
        let dlc_total: u64 = dlcs
            .iter()
            .map(|d| d.size.get("*").map_or(0, |s| s.download_size))
            .sum();
        Ok(BuildSummary {
            download_size: Some(download_size + dlc_total),
            disk_size: Some(disk_size + dlc_total),
            languages,
            dlcs,
            version_name: base
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            folder_name: self.linux_folder(client, app_name, &product).await,
            ..Default::default()
        })
    }

    async fn summarize_build(
        &self,
        app_name: &str,
        platform: &str,
        branch: Option<&str>,
        build: Option<&str>,
    ) -> Result<BuildSummary> {
        let client = self.content().await?;
        if platform == "linux" {
            return self.summarize_linux(&client, app_name).await;
        }
        let Some((builds, item)) = self
            .resolve_build(&client, app_name, platform, branch, build)
            .await?
        else {
            return Ok(BuildSummary {
                legacy: true,
                ..Default::default()
            });
        };
        let (meta, etag) = client.build_meta(&item).await?;
        let owned = client.owned().await.unwrap_or_default();
        let repo = if meta.dependencies.is_empty() {
            None
        } else {
            client.repository().await.ok()
        };
        Ok(summarize(
            &builds,
            &item,
            &meta,
            etag.as_deref(),
            &owned,
            repo.as_ref(),
        ))
    }

    /// Build summary, cached for six hours per app/platform/branch/build/password.
    pub(crate) async fn build_summary(
        &self,
        app_name: &str,
        platform: &str,
        branch: Option<&str>,
        build: Option<&str>,
        force: bool,
    ) -> Result<BuildSummary> {
        let branch = branch.filter(|b| !b.is_empty());
        let build = build.filter(|b| !b.is_empty());
        let password = self.private_branch_password(app_name).await;
        let key = info_cache_key(app_name, platform, branch, build, &password);
        let path = self.paths.install_info_cache();
        let mut cache: HashMap<String, CachedInfo> = load_json(path.clone()).await;
        if !force
            && let Some(c) = cache.get(&key)
            && util::unix_now().saturating_sub(c.at) < INFO_TTL_SECS
        {
            return Ok(c.info.clone());
        }
        let info = self
            .summarize_build(app_name, platform, branch, build)
            .await
            .with_context(|| {
                format!(
                    "Could not get install information for {}",
                    self.title_of(app_name)
                )
            })?;
        cache.retain(|_, c| util::unix_now().saturating_sub(c.at) < INFO_TTL_SECS);
        cache.insert(
            key,
            CachedInfo {
                at: util::unix_now(),
                info: info.clone(),
            },
        );
        save_json(path, cache).await?;
        let known_folder = self.game(app_name).and_then(|g| g.folder_name.clone());
        if !info.folder_name.is_empty()
            && known_folder.as_deref() != Some(info.folder_name.as_str())
        {
            let folder = info.folder_name.clone();
            self.update_catalogue_entry(app_name, move |g| g.folder_name = Some(folder))
                .await;
        }
        Ok(info)
    }

    pub(crate) async fn clear_install_info_cache(&self, app_name: &str) {
        let path = self.paths.install_info_cache();
        let mut cache: HashMap<String, CachedInfo> = load_json(path.clone()).await;
        let prefix = format!("{app_name}_");
        cache.retain(|k, _| !k.starts_with(&prefix));
        let _ = save_json(path, cache).await;
    }

    pub(crate) async fn get_install_info(
        &self,
        app_name: &str,
        platform: InstallPlatform,
        query: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        let title = self.title_of(app_name);
        if app_name == REDIST_APP {
            return Ok(InstallInfo {
                app_name: app_name.into(),
                title,
                ..Default::default()
            });
        }
        let p = platform_arg(platform)?;
        let raw = self
            .build_summary(
                app_name,
                p,
                query.branch.as_deref(),
                query.build.as_deref(),
                false,
            )
            .await?;
        let rec = self.record(app_name);
        let lang = choose_language(
            &raw.languages,
            query
                .language
                .as_deref()
                .or(rec.as_ref().and_then(|r| r.language.as_deref())),
            &self.ui_language(),
        );
        let installed_dlcs = rec
            .as_ref()
            .map(|r| r.installed_dlcs.clone())
            .unwrap_or_default();
        let mut info = map_install_info(
            app_name,
            &title,
            platform,
            &raw,
            lang.as_deref(),
            &installed_dlcs,
        );
        info.resumable =
            self.state.read().pending.contains_key(app_name) && self.is_resumable(app_name);
        Ok(info)
    }

    async fn measure(&self, dir: PathBuf) -> String {
        let bytes = tokio::task::spawn_blocking(move || dir_size(&dir))
            .await
            .unwrap_or(0);
        util::format_bytes(bytes)
    }

    async fn run_postinst(&self, rec: &InstalledRecord, ctx: &OpContext) {
        let script = rec.path().join("support").join("postinst.sh");
        if !script.is_file() {
            return;
        }
        (ctx.step)("Running post-install script");
        let mut opts = RunOptions::default();
        if let Some(l) = &ctx.log_file {
            opts = opts.log_file(l.clone());
        }
        match process::run(&CommandSpec::new("sh").arg(&script).cwd(rec.path()), opts).await {
            Ok(r) if !r.success() => {
                tracing::warn!("postinst.sh of {}: {}", rec.app_name, r.error_summary())
            }
            Err(e) => tracing::warn!("postinst.sh of {}: {e:#}", rec.app_name),
            _ => {}
        }
    }

    pub(crate) async fn build_record(&self, app_name: &str) -> Option<BuildRecord> {
        let path = self.paths.build_file(app_name);
        tokio::task::spawn_blocking(move || build::read(&path))
            .await
            .ok()
            .flatten()
    }

    async fn save_build_record(&self, app_name: &str, rec: BuildRecord) -> Result<()> {
        save_json(self.paths.build_file(app_name), rec).await
    }

    /// The files of a content-system build, including the redistributables
    /// that go into the game folder.
    async fn build_files(
        &self,
        client: &Client,
        meta: &BuildMeta,
        sel: &Selection,
        fetched: &(dyn Fn(usize) + Send + Sync),
    ) -> Result<BuildFiles> {
        let mut files = gplan::resolve(client, meta, sel, fetched).await?;
        if meta.generation == 2 && !meta.dependencies.is_empty() {
            let repo = client.repository().await?;
            files.items.extend(
                gplan::resolve_dependencies(client, &repo, &meta.dependencies, false).await?,
            );
        }
        Ok(files)
    }

    /// Plan a content-system deployment into `dir`.
    #[allow(clippy::too_many_arguments)]
    async fn plan_content(
        &self,
        app_name: &str,
        platform: &str,
        mode: Mode,
        dir: &Path,
        language: &str,
        dlcs: &[String],
        branch: Option<&str>,
        build: Option<&str>,
        ctx: &OpContext,
    ) -> Result<Deployment> {
        let client = self.content().await?;
        let old = self
            .build_record(app_name)
            .await
            .filter(|r| r.platform == platform && mode != Mode::Install);
        // Repairs restore the installed build.
        let (meta, etag, build_id, version) = match (&mode, &old) {
            (Mode::Repair, Some(o)) if o.parsed_meta().is_some() => (
                o.parsed_meta().unwrap_or_default(),
                o.etag.clone(),
                o.build_id.clone(),
                self.record(app_name).map(|r| r.version).unwrap_or_default(),
            ),
            _ => {
                let build = match mode {
                    Mode::Repair => self.record(app_name).and_then(|r| r.build_id),
                    _ => build.map(str::to_string),
                };
                let (_, item) = self
                    .resolve_build(&client, app_name, platform, branch, build.as_deref())
                    .await?
                    .with_context(|| {
                        format!(
                            "{} cannot be downloaded through GOG Galaxy; use its offline installer",
                            self.title_of(app_name)
                        )
                    })?;
                let (meta, etag) = client.build_meta(&item).await?;
                (meta, etag, item.build_id, item.version_name)
            }
        };
        let dlcs: Vec<String> = dlcs
            .iter()
            .filter(|d| meta.products.iter().any(|p| &p.id == *d))
            .cloned()
            .collect();
        let sel = Selection {
            language: language.to_string(),
            dlcs: dlcs.clone(),
            os: platform.to_string(),
        };
        // Updates read the file lists of both builds.
        let old_meta = match (&mode, &old) {
            (Mode::Update, Some(o)) => o.parsed_meta().map(|m| (m, o.selection())),
            _ => None,
        };
        let new_count = gplan::manifest_count(&meta, &sel);
        let total = new_count
            + old_meta
                .as_ref()
                .map_or(0, |(m, s)| gplan::manifest_count(m, s));
        let report = |done: usize| {
            (ctx.progress)(InstallProgress {
                preparing: Some(Preparing {
                    stage: PrepareStage::FileLists,
                    done: done as u64,
                    total: total as u64,
                }),
                ..Default::default()
            })
        };
        report(0);
        let new = self.build_files(&client, &meta, &sel, &report).await?;
        let previous = match old_meta {
            Some((m, old_sel)) => {
                let fetched = |n: usize| report(new_count + n);
                match self.build_files(&client, &m, &old_sel, &fetched).await {
                    Ok(f) => Some(f),
                    Err(e) => {
                        tracing::warn!("file list of the installed build of {app_name}: {e:#}");
                        None
                    }
                }
            }
            None => None,
        };
        let support = self.paths.support_dir(app_name);
        let game_dir = dir.to_path_buf();
        let plan = {
            let exists = |is_support: bool, rel: &str| {
                exists_below(if is_support { &support } else { &game_dir }, rel)
            };
            gplan::plan(&new, previous.as_ref(), &exists)?
        };
        Ok(Deployment {
            dir: dir.to_path_buf(),
            plan,
            record: BuildRecord {
                platform: platform.to_string(),
                build_id,
                etag,
                language: language.to_string(),
                dlcs,
                meta: meta.raw.clone(),
                linux: None,
            },
            version,
            client: Some(client),
        })
    }

    /// Plan a deployment from the Linux installers into `dir`.
    async fn plan_linux(
        &self,
        app_name: &str,
        mode: Mode,
        dir: &Path,
        language: &str,
        dlcs: &[String],
    ) -> Result<Deployment> {
        let client = self.content().await?;
        let product = client
            .product(app_name, &["downloads", "expanded_dlcs"])
            .await
            .context("Could not read the game's downloads")?;
        let http = Client::download_client()?;
        let mut wanted: Vec<(String, Value)> = Vec::new();
        let base = pick_installer(&linux_installers(&product), language)
            .with_context(|| format!("{} has no Linux installer", self.title_of(app_name)))?;
        let version = base
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        wanted.push((app_name.to_string(), base.clone()));
        let mut chosen = Vec::new();
        if !dlcs.is_empty() {
            let owned = client.owned().await.unwrap_or_default();
            for d in product
                .get("expanded_dlcs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let id = d.get("id").map(id_string).unwrap_or_default();
                if !dlcs.contains(&id) || !owned.contains(&id) {
                    continue;
                }
                match pick_installer(&linux_installers(d), language) {
                    Some(i) => {
                        wanted.push((id.clone(), i.clone()));
                        chosen.push(id);
                    }
                    None => tracing::warn!("DLC {id} has no Linux installer"),
                }
            }
        }
        let mut installers: Vec<(String, Installer)> = Vec::new();
        for (product_id, inst) in &wanted {
            let downlink =
                installer_downlink(inst).context("the Linux installer has no download")?;
            let url = client.resolve_downlink(&downlink).await?;
            installers.push((
                product_id.clone(),
                ember_gog_api::linux::read_installer(&http, &url).await?,
            ));
        }
        let old = self
            .build_record(app_name)
            .await
            .filter(|_| mode == Mode::Update)
            .and_then(|r| r.linux);
        let game_dir = dir.to_path_buf();
        let (mut plan, files) = {
            let exists = |rel: &str| exists_below(&game_dir, rel);
            gplan::linux_plan(&installers, old.as_ref(), &exists)?
        };
        gplan::resolve_linux_links(&http, &installers, &mut plan).await?;
        Ok(Deployment {
            dir: dir.to_path_buf(),
            plan,
            record: BuildRecord {
                platform: "linux".into(),
                build_id: String::new(),
                etag: None,
                language: language.to_string(),
                dlcs: chosen,
                meta: Value::Null,
                linux: Some(files),
            },
            version,
            client: None,
        })
    }

    fn depot_options(
        &self,
        journal: Option<PathBuf>,
        signer: Option<&Client>,
    ) -> ember_depot::Options {
        let settings = (self.settings)();
        ember_depot::Options {
            workers: settings.max_workers as usize,
            max_bytes_per_sec: u64::from(settings.max_download_speed) * 1024,
            journal,
            signer: signer.map(|c| ember_depot::Signer(Arc::new(gplan::Signer::new(c.clone())))),
            ..Default::default()
        }
    }

    /// Keep only the files that are missing or damaged; `false` when cancelled.
    async fn keep_broken(
        &self,
        d: &mut Deployment,
        app_name: &str,
        ctx: &OpContext,
    ) -> Result<bool> {
        (ctx.step)("Verifying files");
        let support = self.paths.support_dir(app_name);
        for (root, files, sink) in [
            (
                support.as_path(),
                &mut d.plan.support,
                ember_depot::ignore_progress(),
            ),
            (d.dir.as_path(), &mut d.plan.game, progress_sink(ctx)),
        ] {
            match ember_depot::verify(root, files, 0, sink, &ctx.cancel).await? {
                Verified::Cancelled => return Ok(false),
                Verified::Checked(bad) => {
                    let broken: Vec<_> = bad.iter().map(|(i, _)| files[*i].clone()).collect();
                    log_line(ctx, &format!("{} files need repair", broken.len()));
                    *files = broken;
                }
            }
        }
        d.plan.remove_game.clear();
        d.plan.remove_support.clear();
        Ok(true)
    }

    /// Download a planned deployment and record the new build.
    async fn run_deployment(
        &self,
        app_name: &str,
        mut d: Deployment,
        mode: Mode,
        ctx: &OpContext,
    ) -> Result<Outcome> {
        if mode == Mode::Repair && !self.keep_broken(&mut d, app_name, ctx).await? {
            return Ok(Outcome::Abort);
        }
        let http = Client::download_client()?;
        let support = self.paths.support_dir(app_name);
        (ctx.step)(if mode == Mode::Update {
            "Updating"
        } else {
            "Downloading"
        });
        log_line(
            ctx,
            &format!(
                "{mode:?} {app_name} into {}: {} files, {} to download",
                d.dir.display(),
                d.plan.game.len() + d.plan.support.len(),
                util::format_bytes(d.plan.download_size)
            ),
        );
        if !d.plan.support.is_empty() || !d.plan.remove_support.is_empty() {
            let finished = ember_depot::download(
                &http,
                Job {
                    root: &support,
                    files: &d.plan.support,
                    remove: &d.plan.remove_support,
                },
                &self.depot_options(None, d.client.as_ref()),
                ember_depot::ignore_progress(),
                &ctx.cancel,
            )
            .await?;
            if finished == Finished::Cancelled {
                return Ok(Outcome::Abort);
            }
        }
        let finished = ember_depot::download(
            &http,
            Job {
                root: &d.dir,
                files: &d.plan.game,
                remove: &d.plan.remove_game,
            },
            &self.depot_options(Some(self.paths.resume_file(app_name)), d.client.as_ref()),
            progress_sink(ctx),
            &ctx.cancel,
        )
        .await?;
        if finished == Finished::Cancelled {
            log_line(ctx, "canceled");
            return Ok(Outcome::Abort);
        }
        let (dir, plan) = (d.dir.clone(), std::mem::take(&mut d.plan));
        tokio::task::spawn_blocking(move || finish_tree(&dir, &plan)).await??;
        if mode != Mode::Repair || self.build_record(app_name).await.is_none() {
            self.save_build_record(app_name, d.record).await?;
        }
        log_line(ctx, "done");
        Ok(Outcome::Done)
    }

    /// Plan and run a deployment; errors become [`Outcome::Error`].
    #[allow(clippy::too_many_arguments)]
    async fn deploy(
        &self,
        app_name: &str,
        platform: InstallPlatform,
        mode: Mode,
        dir: &Path,
        language: &str,
        dlcs: &[String],
        branch: Option<&str>,
        build: Option<&str>,
        ctx: &OpContext,
    ) -> Result<(Outcome, String)> {
        // Records from other tools may name the language ("English").
        let language = ember_gog_api::languages::code(language).unwrap_or(language);
        let result = async {
            (ctx.step)("Fetching file list");
            let d = if platform == InstallPlatform::Linux {
                self.plan_linux(app_name, mode, dir, language, dlcs).await?
            } else {
                self.plan_content(
                    app_name,
                    platform_arg(platform)?,
                    mode,
                    dir,
                    language,
                    dlcs,
                    branch,
                    build,
                    ctx,
                )
                .await?
            };
            let version = d.version.clone();
            Ok::<_, anyhow::Error>((self.run_deployment(app_name, d, mode, ctx).await?, version))
        }
        .await;
        match result {
            Ok(r) => Ok(r),
            Err(_) if ctx.cancel.is_cancelled() => Ok((Outcome::Abort, String::new())),
            Err(e) => {
                log_line(ctx, &format!("failed: {e:#}"));
                Ok((Outcome::Error(format!("{e:#}")), String::new()))
            }
        }
    }

    pub(crate) async fn install_game(
        &self,
        app_name: &str,
        request: &InstallRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        if app_name == REDIST_APP {
            return self.update_redist(&request.dependencies, &ctx).await;
        }
        if self.record(app_name).is_some() {
            anyhow::bail!(
                "{} is already installed; verify or modify it instead",
                self.title_of(app_name)
            );
        }
        if let Some(installer) = &request.installer {
            return self
                .install_from_installer(app_name, request, installer, ctx)
                .await;
        }
        let platform = platform_arg(request.platform)?;
        (ctx.step)("Preparing");
        let raw = self
            .build_summary(
                app_name,
                platform,
                request.branch.as_deref(),
                request.build.as_deref(),
                false,
            )
            .await?;
        if raw.legacy {
            bail!(
                "{} cannot be downloaded through GOG Galaxy; use its offline installer",
                self.title_of(app_name)
            );
        }
        let folder = Some(raw.folder_name.clone())
            .filter(|f| !f.is_empty())
            .or_else(|| self.game(app_name).and_then(|g| g.folder_name.clone()))
            .filter(|f| !f.is_empty())
            .with_context(|| {
                format!(
                    "GOG did not report an install folder for {}",
                    self.title_of(app_name)
                )
            })?;
        let install_dir = request.path.join(ember_core::paths::sanitize(&folder));
        let pending = PendingInstall {
            install_path: install_dir.to_string_lossy().into_owned(),
            platform: request.platform,
        };
        let resuming = self.state.read().pending.get(app_name) == Some(&pending);
        if !resuming {
            let _ = tokio::fs::remove_file(self.paths.resume_file(app_name)).await;
        }
        self.set_pending(app_name, Some(pending)).await;

        let language = choose_language(
            &raw.languages,
            request.language.as_deref(),
            &self.ui_language(),
        )
        .unwrap_or_else(|| "en-US".into());
        let (outcome, version) = self
            .deploy(
                app_name,
                request.platform,
                Mode::Install,
                &install_dir,
                &language,
                &request.dlcs,
                request.branch.as_deref(),
                request.build.as_deref(),
                &ctx,
            )
            .await?;
        if outcome != Outcome::Done {
            return Ok(outcome);
        }

        (ctx.step)("Finishing");
        let build = self.build_record(app_name).await.unwrap_or_default();
        let linux = request.platform == InstallPlatform::Linux;
        let rec = InstalledRecord {
            app_name: app_name.to_string(),
            platform: request.platform,
            executable: String::new(),
            install_path: install_dir.to_string_lossy().into_owned(),
            install_size: self.measure(install_dir.clone()).await,
            is_dlc: false,
            version,
            installed_with_dlcs: Some(!build.dlcs.is_empty()),
            installed_dlcs: build.dlcs.clone(),
            language: Some(language),
            version_etag: build.etag.clone().filter(|e| !e.is_empty() && !linux),
            build_id: Some(build.build_id.clone()).filter(|e| !e.is_empty() && !linux),
            branch: request.branch.clone().filter(|b| !b.is_empty()),
            pinned_version: request.build.as_deref().is_some_and(|b| !b.is_empty()),
            cyberpunk: None,
            rest: Default::default(),
        };
        self.put_record(rec.clone()).await?;
        self.set_pending(app_name, None).await;
        self.clear_install_info_cache(app_name).await;

        match request.platform {
            InstallPlatform::Windows => {
                let mut deps = build.dependencies();
                deps.extend(request.dependencies.iter().cloned());
                if let Err(e) = self.ensure_redist(&deps, &ctx).await {
                    tracing::warn!("GOG redistributables: {e:#}");
                }
            }
            InstallPlatform::Linux => self.run_postinst(&rec, &ctx).await,
            _ => {}
        }
        Ok(Outcome::Done)
    }

    /// Run the Inno Setup uninstallers of removed DLCs inside the prefix.
    async fn run_dlc_uninstallers(&self, rec: &InstalledRecord, removed: &[String]) {
        let Some(wine) = self
            .wine_context(&rec.app_name)
            .await
            .filter(|w| w.prefix_exists())
        else {
            return;
        };
        let dir = rec.path();
        let found: Vec<(PathBuf, String)> = {
            let dir = dir.clone();
            let removed = removed.to_vec();
            tokio::task::spawn_blocking(move || {
                let re = regex::Regex::new(r"^unins\d{3}\.ini$").expect("valid regex");
                std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| re.is_match(&e.file_name().to_string_lossy()))
                    .filter_map(|e| {
                        let text = std::fs::read_to_string(e.path()).ok()?;
                        let id = ini_product_id(&text)?;
                        removed
                            .contains(&id)
                            .then(|| (e.path().with_extension("exe"), id))
                    })
                    .collect()
            })
            .await
            .unwrap_or_default()
        };
        for (exe, id) in found {
            if !exe.is_file() {
                continue;
            }
            let args = vec![
                format!("/ProductId={id}"),
                "/VERYSILENT".into(),
                "/galaxyclient".into(),
                "/KEEPSAVES".into(),
            ];
            if let Err(e) = wine
                .run(&exe.to_string_lossy(), &args, Some(&dir), None, None)
                .await
            {
                tracing::warn!("DLC uninstaller {}: {e:#}", exe.display());
            }
        }
    }

    pub(crate) async fn update_game(
        &self,
        app_name: &str,
        request: &UpdateRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        if app_name == REDIST_APP {
            let deps = request.dependencies.clone().unwrap_or_default();
            return self.update_redist(&deps, &ctx).await;
        }
        let rec = self.installed_record(app_name)?;
        if let Some(new) = &request.dlcs {
            let removed: Vec<String> = rec
                .installed_dlcs
                .iter()
                .filter(|d| !new.contains(d))
                .cloned()
                .collect();
            if !removed.is_empty() && rec.platform == InstallPlatform::Windows {
                (ctx.step)("Removing DLCs");
                self.run_dlc_uninstallers(&rec, &removed).await;
            }
        }
        let dlcs = request
            .dlcs
            .clone()
            .unwrap_or_else(|| rec.installed_dlcs.clone());
        let language = request
            .language
            .clone()
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| rec.language_or_default().to_string());
        let branch: Option<String> = match &request.branch {
            Some(b) => Some(b.clone()).filter(|b| !b.is_empty()),
            None => rec.branch().map(str::to_string),
        };
        let install_dir = rec.path();
        let (outcome, version) = self
            .deploy(
                app_name,
                rec.platform,
                Mode::Update,
                &install_dir,
                &language,
                &dlcs,
                branch.as_deref(),
                request.build.as_deref(),
                &ctx,
            )
            .await?;
        if outcome != Outcome::Done {
            return Ok(outcome);
        }

        (ctx.step)("Finishing");
        self.clear_install_info_cache(app_name).await;
        let build = self.build_record(app_name).await.unwrap_or_default();
        let linux = rec.platform == InstallPlatform::Linux;
        let mut updated = rec.clone();
        updated.version = version;
        updated.build_id = Some(build.build_id.clone()).filter(|s| !s.is_empty() && !linux);
        updated.version_etag = build.etag.clone().filter(|s| !s.is_empty() && !linux);
        updated.branch = branch;
        updated.language = Some(language);
        updated.installed_with_dlcs = Some(!build.dlcs.is_empty());
        updated.installed_dlcs = build.dlcs.clone();
        updated.pinned_version = request.build.as_deref().is_some_and(|b| !b.is_empty());
        updated.install_size = self.measure(install_dir).await;
        self.put_record(updated.clone()).await?;

        match rec.platform {
            InstallPlatform::Windows => {
                let mut deps = build.dependencies();
                deps.extend(request.dependencies.clone().unwrap_or_default());
                if let Err(e) = self.ensure_redist(&deps, &ctx).await {
                    tracing::warn!("GOG redistributables: {e:#}");
                }
            }
            InstallPlatform::Linux => self.run_postinst(&updated, &ctx).await,
            _ => {}
        }
        Ok(Outcome::Done)
    }

    pub(crate) async fn repair_game(&self, app_name: &str, ctx: OpContext) -> Result<Outcome> {
        let rec = self.installed_record(app_name)?;
        let build = self.build_record(app_name).await;
        let language = build
            .as_ref()
            .map(|b| b.language.clone())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| rec.language_or_default().to_string());
        let (outcome, _) = self
            .deploy(
                app_name,
                rec.platform,
                Mode::Repair,
                &rec.path(),
                &language,
                &rec.installed_dlcs,
                rec.branch(),
                rec.build_id(),
                &ctx,
            )
            .await?;
        if outcome != Outcome::Done {
            return Ok(outcome);
        }
        let size = self.measure(rec.path()).await;
        self.modify_record(app_name, |r| r.install_size = size)
            .await?;
        Ok(Outcome::Done)
    }

    async fn remove_dir(path: PathBuf) -> Result<()> {
        tokio::task::spawn_blocking(move || match std::fs::remove_dir_all(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("Could not delete {}", path.display())),
        })
        .await?
    }

    async fn remove_build_state(&self, app_name: &str) {
        let _ = tokio::fs::remove_file(self.paths.build_file(app_name)).await;
        let _ = tokio::fs::remove_file(self.paths.resume_file(app_name)).await;
        let _ = Self::remove_dir(self.paths.support_dir(app_name)).await;
    }

    pub(crate) async fn uninstall_game(&self, app_name: &str, args: RemoveArgs) -> Result<()> {
        if app_name == REDIST_APP {
            if args.delete_files {
                Self::remove_dir(self.paths.redist.clone()).await?;
            }
            return Ok(());
        }
        let Some(rec) = self.record(app_name) else {
            let pending = self.state.read().pending.get(app_name).cloned();
            let Some(p) = pending else {
                bail!("{} is not installed", self.title_of(app_name));
            };
            if args.delete_files {
                let dir = PathBuf::from(&p.install_path);
                ember_core::fs_guard::ensure_deletable(&dir, &(self.settings)())?;
                Self::remove_dir(dir).await?;
                self.remove_build_state(app_name).await;
            }
            self.set_pending(app_name, None).await;
            return Ok(());
        };
        if args.delete_files {
            let dir = rec.path();
            ember_core::fs_guard::ensure_deletable(&dir, &(self.settings)())?;
            let uninstaller = dir.join("unins000.exe");
            if rec.platform == InstallPlatform::Windows
                && uninstaller.is_file()
                && !args.remove_prefix
                && let Some(wine) = self
                    .wine_context(app_name)
                    .await
                    .filter(|w| w.prefix_exists())
            {
                let a = vec![
                    "/VERYSILENT".to_string(),
                    format!("/ProductId={app_name}"),
                    "/galaxyclient".into(),
                    "/KEEPSAVES".into(),
                ];
                if let Err(e) = wine
                    .run(&uninstaller.to_string_lossy(), &a, Some(&dir), None, None)
                    .await
                {
                    tracing::warn!("uninstaller of {app_name}: {e:#}");
                }
            }
            Self::remove_dir(dir).await?;
            self.clear_save_timestamps(app_name).await;
        }
        self.remove_build_state(app_name).await;
        self.remove_record(app_name).await?;
        self.set_pending(app_name, None).await;
        Ok(())
    }

    async fn scan(path: &Path) -> Result<ImportScan> {
        let p = path.to_path_buf();
        tokio::task::spawn_blocking(move || scan_import(&p))
            .await?
            .with_context(|| format!("No GOG game was found in {}", path.display()))
    }

    pub(crate) async fn import_game(
        &self,
        app_name: &str,
        path: &Path,
        platform: InstallPlatform,
    ) -> Result<()> {
        if !path.is_dir() {
            bail!("{} is not a folder", path.display());
        }
        let data = Self::scan(path).await?;
        if data.app_name != app_name {
            bail!(
                "{} contains {} ({}), not {}",
                path.display(),
                if data.title.is_empty() {
                    "another game"
                } else {
                    &data.title
                },
                data.app_name,
                self.title_of(app_name)
            );
        }
        let platform = if data.platform == InstallPlatform::Windows {
            platform
        } else {
            data.platform
        };
        let mut rest = serde_json::Map::new();
        if !data.title.is_empty() {
            rest.insert("title".into(), Value::String(data.title.clone()));
        }
        let rec = InstalledRecord {
            app_name: app_name.to_string(),
            platform,
            executable: String::new(),
            install_path: path.to_string_lossy().into_owned(),
            install_size: self.measure(path.to_path_buf()).await,
            is_dlc: false,
            version: data.version_name.clone().unwrap_or_default(),
            installed_with_dlcs: Some(!data.dlcs.is_empty()),
            installed_dlcs: data.dlcs.clone(),
            language: data
                .language
                .as_deref()
                .map(|l| ember_gog_api::languages::code(l).unwrap_or(l).to_string()),
            version_etag: None,
            build_id: Some(data.build_id.clone()).filter(|b| !b.is_empty()),
            branch: None,
            pinned_version: false,
            cyberpunk: None,
            rest,
        };
        self.put_record(rec).await?;
        if platform != InstallPlatform::Linux {
            if let Err(e) = self.restore_build_record(app_name).await {
                tracing::warn!("recording the installed build of {app_name}: {e:#}");
            }
            if platform == InstallPlatform::Windows
                && let Err(e) = self.ensure_redist(&[], &OpContext::detached()).await
            {
                tracing::warn!("GOG redistributables: {e:#}");
            }
        }
        Ok(())
    }

    /// Record the installed build of a game that has no build record
    /// (imports, installs copied from elsewhere, changed offline installers).
    pub(crate) async fn restore_build_record(&self, app_name: &str) -> Result<()> {
        let Some(rec) = self.record(app_name) else {
            return Ok(());
        };
        if self.paths.build_file(app_name).exists()
            || rec.platform == InstallPlatform::Linux
            || !rec.path().is_dir()
        {
            return Ok(());
        }
        let data = Self::scan(&rec.path()).await?;
        let client = self.content().await?;
        let build = Some(data.build_id.clone()).filter(|b| !b.is_empty());
        let (_, item) = self
            .resolve_build(&client, app_name, rec.platform_str(), None, None)
            .await?
            .context("GOG lists no builds of the game")?;
        let item = match &build {
            Some(id) if *id != item.build_id => client
                .builds(
                    app_name,
                    rec.platform_str(),
                    Some(&self.private_branch_password(app_name).await),
                )
                .await?
                .items
                .into_iter()
                .find(|b| b.build_id == *id)
                .context("the installed build is no longer listed by GOG")?,
            _ => item,
        };
        let (meta, etag) = client.build_meta(&item).await?;
        let language = rec.language_or_default().to_string();
        let language = ember_gog_api::languages::code(&language)
            .map(str::to_string)
            .unwrap_or(language);
        self.save_build_record(
            app_name,
            BuildRecord {
                platform: rec.platform_str().to_string(),
                build_id: item.build_id.clone(),
                etag: etag.clone(),
                language,
                dlcs: data.dlcs.clone(),
                meta: meta.raw,
                linux: None,
            },
        )
        .await?;
        let version = item.version_name.clone();
        self.modify_record(app_name, |r| {
            if r.version.is_empty() || r.version == r.build_id.clone().unwrap_or_default() {
                r.version = version;
            }
            r.build_id = Some(item.build_id.clone());
            r.version_etag = etag;
        })
        .await
    }

    /// Bring install records in line with the files on disk (DLCs or builds
    /// changed by offline installers).
    pub(crate) async fn reconcile_installs(&self) {
        let recs: Vec<InstalledRecord> = self
            .state
            .read()
            .installed
            .values()
            .filter(|r| r.platform != InstallPlatform::Linux)
            .cloned()
            .collect();
        for rec in recs {
            let dir = rec.path();
            let app = rec.app_name.clone();
            let platform = rec.platform;
            let busy = self.is_resumable(&app);
            let scanned = tokio::task::spawn_blocking(move || {
                if !dir.is_dir() || busy {
                    return None;
                }
                let dlcs = manifest::installed_dlc_ids(&dir, &app, platform);
                let build = manifest::read_info_file(&dir, &app, platform)
                    .map(|i| i.build_id)
                    .filter(|b| !b.is_empty());
                Some((dlcs, build))
            })
            .await
            .ok()
            .flatten();
            let Some((dlcs, build)) = scanned else {
                continue;
            };
            let mut recorded = rec.installed_dlcs.clone();
            recorded.sort();
            let dlcs_changed = dlcs != recorded;
            let build_changed =
                build.is_some() && rec.build_id().is_some() && build.as_deref() != rec.build_id();
            if !dlcs_changed && !build_changed {
                continue;
            }
            let app = rec.app_name.clone();
            let new_dlcs = dlcs.clone();
            let new_build = build.clone();
            let _ = self
                .modify_record(&app, |r| {
                    if dlcs_changed {
                        r.installed_dlcs = new_dlcs;
                    }
                    if build_changed {
                        r.build_id = new_build;
                        r.version_etag = None;
                    }
                })
                .await;
            if build_changed {
                let _ = tokio::fs::remove_file(self.paths.build_file(&app)).await;
                if let Err(e) = self.restore_build_record(&app).await {
                    tracing::warn!("recording the installed build of {app}: {e:#}");
                }
            } else if let Some(mut b) = self.build_record(&app).await {
                b.dlcs = dlcs;
                let _ = self.save_build_record(&app, b).await;
            }
        }
    }

    async fn has_update(&self, rec: &InstalledRecord, client: &Client) -> bool {
        if rec.platform == InstallPlatform::Linux {
            return match client.product(&rec.app_name, &["downloads"]).await {
                Ok(p) => crate::api::linux_installer_version(&p).is_some_and(|v| v != rec.version),
                _ => false,
            };
        }
        let password = self.private_branch_password(&rec.app_name).await;
        let Ok(builds) = client
            .builds(&rec.app_name, rec.platform_str(), Some(&password))
            .await
        else {
            return false;
        };
        let Some(latest) = builds.latest(rec.branch()) else {
            return false;
        };
        if let Some(etag) = rec.version_etag.as_deref().filter(|e| !e.is_empty()) {
            return client.meta_changed(latest, etag).await.unwrap_or(false);
        }
        rec.build_id().is_some_and(|b| b != latest.build_id)
    }

    pub(crate) async fn list_updatable(&self) -> Result<Vec<String>> {
        if self.user_data().is_none() {
            return Ok(Vec::new());
        }
        let Ok(client) = self.content().await else {
            return Ok(Vec::new());
        };
        let recs: Vec<InstalledRecord> = self
            .state
            .read()
            .installed
            .values()
            .filter(|r| !r.pinned_version)
            .cloned()
            .collect();
        let client = &client;
        let mut out: Vec<String> = futures::stream::iter(recs)
            .map(|rec| async move { self.has_update(&rec, client).await.then_some(rec.app_name) })
            .buffer_unordered(6)
            .filter_map(|x| async move { x })
            .collect()
            .await;
        let any_windows = self
            .state
            .read()
            .installed
            .values()
            .any(|r| r.platform == InstallPlatform::Windows);
        if any_windows && self.redist_needs_update().await {
            out.push(REDIST_APP.to_string());
        }
        out.sort();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const INFO: &str = r#"{"size": {"*": {"download_size": 10, "disk_size": 20},
        "en-US": {"download_size": 100, "disk_size": 200}, "de-DE": {"download_size": 110, "disk_size": 210}},
      "languages": ["de-DE", "en-US"], "build_id": "6014", "version_name": "2.5", "version_etag": "\"e\"",
      "folder_name": "Cyberpunk 2077", "branches": ["beta"], "dependencies": ["MSVC2015"],
      "dlcs": [{"title": "PL", "id": "1256837418", "size": {"*": {"download_size": 1, "disk_size": 2},
                                                          "en-US": {"download_size": 5, "disk_size": 6}}}],
      "builds": {"items": [
         {"build_id": "1", "os": "windows", "branch": null, "version_name": "2.4", "date_published": "2026-01-01T00:00:00+0000", "generation": 2},
         {"build_id": "2", "os": "windows", "branch": "beta", "version_name": "2.5b", "date_published": "2026-02-01T00:00:00+0000", "generation": 2}],
        "has_private_branches": false}}"#;

    #[test]
    fn maps_build_summaries() {
        let raw: BuildSummary = serde_json::from_str(INFO).unwrap();
        let lang = choose_language(&raw.languages, None, "en");
        assert_eq!(lang.as_deref(), Some("en-US"));
        let i = map_install_info(
            "1",
            "CP",
            InstallPlatform::Windows,
            &raw,
            lang.as_deref(),
            &["1256837418".into()],
        );
        assert_eq!(i.download_size, 10 + 100 + 1 + 5);
        assert_eq!(i.disk_size, 20 + 200 + 2 + 6);
        assert_eq!(i.branches, vec!["beta"]);
        assert_eq!(i.builds[0].build_id, "2");
        assert_eq!(i.builds[0].branch.as_deref(), Some("beta"));
        assert_eq!(i.dlcs[0].app_name, "1256837418");
        assert!(i.dlcs[0].is_installed);
        assert_eq!(i.dependencies, vec!["MSVC2015"]);
        let de = i
            .language_sizes
            .iter()
            .find(|l| l.language == "de-DE")
            .unwrap();
        assert_eq!(de.download_size, 10 + 110 + 1);
        assert!(!i.is_native);
    }

    #[test]
    fn summarizes_builds() {
        let meta = BuildMeta::parse(json!({"version": 2, "baseProductId": "1", "installDirectory": "Game",
            "products": [{"productId": "1", "name": "Game"}, {"productId": "2", "name": "DLC"},
                         {"productId": "3", "name": "Unowned"}],
            "depots": [
                {"productId": "1", "languages": ["*"], "manifest": "a", "size": 100, "compressedSize": 50},
                {"productId": "1", "languages": ["English"], "manifest": "b", "size": 10, "compressedSize": 5},
                {"productId": "1", "languages": ["Deutsch", "de"], "manifest": "c", "size": 12, "compressedSize": 6},
                {"productId": "2", "languages": ["en-US"], "manifest": "d", "size": 4, "compressedSize": 2}],
            "dependencies": ["MSVC2017", "ISI"]}))
        .unwrap();
        let builds: Builds = serde_json::from_value(json!({"items": [
            {"build_id": "9", "os": "windows", "branch": "beta", "version_name": "1b", "generation": 2},
            {"build_id": "8", "os": "windows", "version_name": "1", "generation": 2}]}))
        .unwrap();
        let repo: Repository = serde_json::from_value(json!({"depots": [
            {"dependencyId": "MSVC2017", "executable": {"path": "MSVC2017/vc.exe"}, "manifest": "m", "size": 30, "compressedSize": 20},
            {"dependencyId": "ISI", "executable": {"path": "__redist/ISI/si.exe"}, "manifest": "n", "size": 1000, "compressedSize": 900}]}))
        .unwrap();
        let s = summarize(
            &builds,
            &builds.items[1],
            &meta,
            Some("\"t\""),
            &["2".into()],
            Some(&repo),
        );
        assert_eq!(
            s.size["*"].download_size,
            50 + 20,
            "game-folder redistributables count"
        );
        assert_eq!(s.size["en-US"].disk_size, 10);
        assert_eq!(s.size["de-DE"].disk_size, 12);
        assert_eq!(s.dlcs.len(), 1);
        assert_eq!(s.dlcs[0].size["en-US"].download_size, 2);
        assert_eq!(s.total_for("en-US").download_size, 50 + 20 + 5 + 2);
        assert_eq!(s.languages, ["de-DE", "en-US"]);
        assert_eq!((s.build_id.as_str(), s.version_name.as_str()), ("8", "1"));
        assert_eq!(s.folder_name, "Game");
        assert_eq!(s.branches, ["beta"]);
        assert_eq!(s.version_etag, "\"t\"");
    }

    #[test]
    fn picks_linux_installers() {
        let p = json!({"downloads": {"installers": [
            {"os": "windows", "language": "en"},
            {"os": "linux", "language": "en", "version": "1", "files": [{"downlink": "https://d/en"}]},
            {"os": "linux", "language": "de", "version": "1", "files": [{"downlink": "https://d/de"}]}]}});
        let all = linux_installers(&p);
        assert_eq!(all.len(), 2);
        let de = pick_installer(&all, "de-DE").unwrap();
        assert_eq!(installer_downlink(de).as_deref(), Some("https://d/de"));
        let fr = pick_installer(&all, "fr-FR").unwrap();
        assert_eq!(installer_downlink(fr).as_deref(), Some("https://d/en"));
        assert!(pick_installer(&[], "en-US").is_none());
    }

    #[test]
    fn chooses_languages() {
        let langs = vec![
            "fr-FR".to_string(),
            "de-DE".to_string(),
            "en-US".to_string(),
        ];
        assert_eq!(
            choose_language(&langs, Some("fr-FR"), "en").as_deref(),
            Some("fr-FR")
        );
        assert_eq!(
            choose_language(&langs, Some("undefined"), "de").as_deref(),
            Some("de-DE")
        );
        assert_eq!(
            choose_language(&langs, None, "pt").as_deref(),
            Some("en-US")
        );
        assert_eq!(choose_language(&[], None, "pt"), None);
        assert_eq!(
            choose_language(&["pl".into()], None, "").as_deref(),
            Some("pl")
        );
    }

    #[test]
    fn parses_uninstaller_ini() {
        let ini = "[Setup]\nAppName=x\n[InstallSettings]\nProductID = 1256837418\nOther=1\n";
        assert_eq!(ini_product_id(ini).as_deref(), Some("1256837418"));
        assert_eq!(ini_product_id("[Setup]\nproductID=1"), None);
    }

    #[test]
    fn scans_installed_games() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("goggame-1.info"),
            r#"{"gameId": "1", "rootGameId": "1", "name": "Game", "buildId": "5", "languages": ["de-DE"]}"#,
        )
        .unwrap();
        std::fs::write(
            d.path().join("goggame-2.info"),
            r#"{"gameId": 2, "rootGameId": "1", "name": "DLC"}"#,
        )
        .unwrap();
        std::fs::write(d.path().join("goggame-1.id"), r#"{"buildId": 77}"#).unwrap();
        let s = scan_import(d.path()).unwrap();
        assert_eq!(s.app_name, "1");
        assert_eq!(s.build_id, "77");
        assert_eq!(s.dlcs, ["2"]);
        assert_eq!(s.language.as_deref(), Some("de-DE"));
        assert_eq!(s.platform, InstallPlatform::Windows);

        let l = tempfile::tempdir().unwrap();
        std::fs::write(
            l.path().join("gameinfo"),
            "Game\n2.1 (gog-3)\n\nen-US\n1\n\n55\n",
        )
        .unwrap();
        let s = scan_import(l.path()).unwrap();
        assert_eq!(s.platform, InstallPlatform::Linux);
        assert_eq!(s.app_name, "1");
        assert_eq!(s.version_name.as_deref(), Some("2.1 (gog-3)"));
        assert!(scan_import(tempfile::tempdir().unwrap().path()).is_none());
    }

    #[test]
    fn cache_key_hides_password() {
        let k = info_cache_key("1", "windows", None, None, "secret");
        assert!(k.starts_with("1_windows_null__"));
        assert!(!k.contains("secret"));
    }
}
