//! The Wine manager: downloadable Wine/Proton builds from GitHub releases
//! (GE-Proton, Proton-CachyOS, Wine-GE and Kron4ek's Wine builds), a cached
//! release list with install state, and install/remove with progress,
//! cancellation and SHA-512 verification.
//!
//! The cache file uses Heroic's `wine-downloader-info.json` shape
//! (`{"wine-releases": [...]}`) so entries are interchangeable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use irori_core::{AppSettings, OpContext, Paths, WineInstallation};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::discovery;
use crate::download;
use crate::locations;

/// A release source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReleaseKind {
    GeProton,
    ProtonCachyOs,
    WineGe,
    WineKron4ek,
    WineCrossover,
    WineStagingMac,
    GamePortingToolkit,
}

impl ReleaseKind {
    pub const ALL: [ReleaseKind; 7] = [
        ReleaseKind::GeProton,
        ReleaseKind::ProtonCachyOs,
        ReleaseKind::WineGe,
        ReleaseKind::WineKron4ek,
        ReleaseKind::GamePortingToolkit,
        ReleaseKind::WineCrossover,
        ReleaseKind::WineStagingMac,
    ];

    /// Sources refreshed by default (Heroic's Linux defaults).
    pub const DEFAULT: [ReleaseKind; 2] = [ReleaseKind::GeProton, ReleaseKind::ProtonCachyOs];

    /// Sources offered on a host, in display order.
    pub fn for_host(host: irori_core::HostPlatform) -> &'static [Self] {
        match host {
            irori_core::HostPlatform::Linux => &[
                Self::GeProton,
                Self::ProtonCachyOs,
                Self::WineGe,
                Self::WineKron4ek,
            ],
            irori_core::HostPlatform::Mac => &[
                Self::GamePortingToolkit,
                Self::WineCrossover,
                Self::WineStagingMac,
            ],
            irori_core::HostPlatform::Windows => &[],
        }
    }
    pub fn available() -> &'static [Self] {
        Self::for_host(irori_core::HostPlatform::current())
    }

    /// The `type` string stored in release entries.
    pub fn type_str(self) -> &'static str {
        match self {
            ReleaseKind::GeProton => "GE-Proton",
            ReleaseKind::ProtonCachyOs => "Proton-CachyOS",
            ReleaseKind::WineGe => "Wine-GE",
            ReleaseKind::WineKron4ek => "Wine-Kron4ek",
            Self::WineCrossover => "Wine-Crossover",
            Self::WineStagingMac => "Wine-Staging-macOS",
            Self::GamePortingToolkit => "Game-Porting-Toolkit",
        }
    }

    pub fn from_type(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.type_str() == s)
    }

    /// GitHub releases API endpoint.
    pub fn url(self) -> &'static str {
        match self {
            ReleaseKind::GeProton => {
                "https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases"
            }
            ReleaseKind::ProtonCachyOs => {
                "https://api.github.com/repos/CachyOS/proton-cachyos/releases"
            }
            ReleaseKind::WineGe => {
                "https://api.github.com/repos/GloriousEggroll/wine-ge-custom/releases"
            }
            ReleaseKind::WineKron4ek => "https://api.github.com/repos/Kron4ek/Wine-Builds/releases",
            Self::WineCrossover => {
                "https://api.github.com/repos/Heroic-Games-Launcher/wine-crossover/releases"
            }
            Self::WineStagingMac => "https://api.github.com/repos/Gcenx/macOS_Wine_builds/releases",
            Self::GamePortingToolkit => {
                "https://api.github.com/repos/Gcenx/game-porting-toolkit/releases"
            }
        }
    }

    /// Whether builds of this source are Proton (installed into the Proton
    /// directory) rather than Wine.
    pub fn is_proton(self) -> bool {
        matches!(self, ReleaseKind::GeProton | ReleaseKind::ProtonCachyOs)
    }
}

/// One downloadable build (Heroic's `WineVersionInfo`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Release {
    /// Install folder name and display name (`GE-Proton10-20`,
    /// `GE-Proton-latest`, …).
    pub version: String,
    /// Source type string (see [`ReleaseKind::type_str`]).
    #[serde(rename = "type")]
    pub kind: String,
    /// Publication date, `YYYY-MM-DD`.
    pub date: String,
    pub download: String,
    /// Download size in bytes.
    pub downsize: u64,
    /// Size on disk after install, in bytes (0 when unknown).
    pub disksize: u64,
    /// URL of the `.sha512sum` file, empty when the source publishes none.
    pub checksum: String,
    pub release_notes_link: String,
    #[serde(rename = "installDir", skip_serializing_if = "Option::is_none")]
    pub install_dir: Option<String>,
    #[serde(rename = "isInstalled", skip_serializing_if = "Option::is_none")]
    pub is_installed: Option<bool>,
    #[serde(rename = "hasUpdate", skip_serializing_if = "Option::is_none")]
    pub has_update: Option<bool>,
    /// The build this one replaced in place, kept so it can come back.
    #[serde(rename = "previousDir", skip_serializing_if = "Option::is_none")]
    pub previous_dir: Option<String>,
}

impl Release {
    pub fn release_kind(&self) -> Option<ReleaseKind> {
        ReleaseKind::from_type(&self.kind)
    }

    /// Installed and present on disk.
    pub fn installed(&self) -> bool {
        self.is_installed == Some(true)
            && self
                .install_dir
                .as_deref()
                .is_some_and(|d| !d.is_empty() && Path::new(d).is_dir())
    }

    /// The Wine/Proton installation of an installed release.
    pub fn installation(&self) -> Option<WineInstallation> {
        let dir = PathBuf::from(self.install_dir.as_deref()?);
        if !dir.is_dir() {
            return None;
        }
        if self.release_kind() == Some(ReleaseKind::GamePortingToolkit) {
            let mut wine = discovery::wine_build(&dir, &self.version);
            wine.kind = irori_core::WineKind::Toolkit;
            return Some(wine);
        }
        let is_wine = self.kind.contains("Wine");
        Some(if is_wine {
            discovery::wine_build(&dir, &self.version)
        } else {
            discovery::proton_build(&dir, &self.version)
        })
    }

    fn same_as(&self, other: &Release) -> bool {
        self.kind == other.kind
            && (self.version == other.version
                || self.version == format!("Proton-{}", other.version)
                || (other.version == "GE-Proton-latest" && self.version == "Proton-GE-latest"))
    }
}

/// The cache file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReleaseStore {
    #[serde(rename = "wine-releases")]
    pub releases: Vec<Release>,
    /// Unix time of the last successful fetch, per type.
    #[serde(rename = "fetchedAt")]
    pub fetched_at: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

static ARM_ASSET: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"-(aarch64|arm64)\.tar\.(gz|xz)$").expect("valid regex"));
static NUMBERED: Lazy<Regex> = Lazy::new(|| Regex::new(r"\d+-\d+$").expect("valid regex"));

/// CPU architecture name as used by release assets.
pub fn host_arch() -> &'static str {
    std::env::consts::ARCH
}

/// Heroic's `isCompatibleDownload`: whether `url` is a build for `arch`.
pub fn is_compatible_download(kind: &str, url: &str, arch: &str) -> bool {
    let arm = arch == "aarch64" || arch == "arm64";
    match ReleaseKind::from_type(kind) {
        Some(ReleaseKind::ProtonCachyOs) => {
            let suffix = if arm {
                "-arm64.tar.xz"
            } else {
                "-x86_64.tar.xz"
            };
            url.ends_with(suffix)
        }
        Some(ReleaseKind::GeProton) => {
            (url.ends_with(".tar.gz") || url.ends_with(".tar.xz")) && ARM_ASSET.is_match(url) == arm
        }
        Some(ReleaseKind::WineKron4ek) => !arm,
        Some(ReleaseKind::GamePortingToolkit) => arm,
        _ => true,
    }
}

fn is_tarball(name: &str) -> bool {
    name.ends_with(".tar.gz")
        || name.ends_with(".tar.xz")
        || name.ends_with(".tar.zst")
        || name.ends_with(".tar.bz2")
}

fn checksum_name(asset: &str) -> String {
    let stem = asset
        .strip_suffix(".tar.gz")
        .or_else(|| asset.strip_suffix(".tar.xz"))
        .or_else(|| asset.strip_suffix(".tar.zst"))
        .unwrap_or(asset);
    format!("{stem}.sha512sum")
}

fn select_asset<'a>(kind: ReleaseKind, assets: &'a [GhAsset], arch: &str) -> Option<&'a GhAsset> {
    let ty = kind.type_str();
    match kind {
        ReleaseKind::GeProton | ReleaseKind::ProtonCachyOs => assets
            .iter()
            .find(|a| is_compatible_download(ty, &a.browser_download_url, arch)),
        ReleaseKind::WineCrossover => assets
            .iter()
            .find(|a| a.name.starts_with("Wine-Crossover") && is_tarball(&a.name)),
        ReleaseKind::WineStagingMac => assets.iter().find(|a| {
            a.name.starts_with("wine-staging-") && a.name.contains("osx64") && is_tarball(&a.name)
        }),
        ReleaseKind::GamePortingToolkit => assets.iter().find(|a| {
            is_compatible_download(ty, &a.browser_download_url, arch) && is_tarball(&a.name)
        }),
        ReleaseKind::WineGe => assets
            .iter()
            .filter(|a| is_tarball(&a.name))
            .find(|a| !a.name.contains("LoL"))
            .or_else(|| assets.iter().find(|a| is_tarball(&a.name))),
        ReleaseKind::WineKron4ek => {
            if !is_compatible_download(ty, "", arch) {
                return None;
            }
            assets
                .iter()
                .find(|a| a.name.ends_with("-staging-amd64.tar.xz"))
                .or_else(|| {
                    assets
                        .iter()
                        .find(|a| a.name.ends_with("-amd64.tar.xz") && !a.name.contains("tkg"))
                })
        }
    }
}

fn version_of(kind: ReleaseKind, tag: &str) -> String {
    match kind {
        ReleaseKind::WineKron4ek => format!("Wine-Kron4ek-{tag}-staging"),
        ReleaseKind::WineCrossover => format!("Wine-Crossover-{tag}"),
        ReleaseKind::WineStagingMac => format!("Wine-Staging-macOS-{tag}"),
        k if k.type_str().contains("Wine") => format!("Wine-{tag}"),
        _ => tag.to_string(),
    }
}

/// Turn a GitHub releases response into release entries, newest first, with
/// a synthetic `<type>-latest` entry at index 0 (copied from the newest
/// regularly numbered release).
fn releases_from_github(kind: ReleaseKind, gh: Vec<GhRelease>, arch: &str) -> Vec<Release> {
    let mut out: Vec<Release> = gh
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let asset = select_asset(kind, &r.assets, arch)?;
            let wanted = checksum_name(&asset.name);
            let checksum = r
                .assets
                .iter()
                .find(|a| a.name == wanted)
                .map(|a| a.browser_download_url.clone())
                .unwrap_or_default();
            Some(Release {
                version: version_of(kind, &r.tag_name),
                kind: kind.type_str().into(),
                date: r
                    .published_at
                    .as_deref()
                    .and_then(|d| d.split('T').next())
                    .unwrap_or_default()
                    .to_string(),
                download: asset.browser_download_url.clone(),
                downsize: asset.size,
                disksize: 0,
                checksum,
                release_notes_link: r.html_url,
                ..Default::default()
            })
        })
        .collect();
    if let Some(base) = out
        .iter()
        .find(|r| NUMBERED.is_match(&r.version))
        .or(out.first())
        .cloned()
    {
        out.insert(
            0,
            Release {
                version: format!("{}-latest", kind.type_str()),
                ..base
            },
        );
    }
    out
}

/// Fetch the newest `count` releases of a source from GitHub.
pub async fn fetch_releases(kind: ReleaseKind, count: u32) -> Result<Vec<Release>> {
    anyhow::ensure!(
        ReleaseKind::available().contains(&kind),
        "This runtime source is not available on this operating system"
    );
    let url = format!("{}?per_page={count}", kind.url());
    fetch_releases_at(kind, &url, host_arch()).await
}

async fn fetch_releases_at(kind: ReleaseKind, url: &str, arch: &str) -> Result<Vec<Release>> {
    let gh: Vec<GhRelease> = download::get_json(url)
        .await
        .with_context(|| format!("could not fetch {} releases", kind.type_str()))?;
    Ok(releases_from_github(kind, gh, arch))
}

fn keep_cached(r: &Release) -> bool {
    is_compatible_download(&r.kind, &r.download, host_arch())
        || r.install_dir
            .as_deref()
            .is_some_and(|d| !d.is_empty() && Path::new(d).is_dir())
}

/// Merge freshly fetched releases of `fetched` sources into the cache,
/// carrying install state over and flagging updates of installed `-latest`
/// builds.
fn merge(cached: Vec<Release>, fresh: Vec<Release>, fetched: &[ReleaseKind]) -> Vec<Release> {
    let fetched_types: Vec<&str> = fetched.iter().map(|k| k.type_str()).collect();
    let mut result: Vec<Release> = Vec::new();
    let mut fresh = fresh;
    for old in cached.into_iter().filter(keep_cached) {
        if !fetched_types.contains(&old.kind.as_str()) {
            result.push(old);
            continue;
        }
        let on_disk = old
            .install_dir
            .as_deref()
            .is_some_and(|d| !d.is_empty() && Path::new(d).is_dir());
        if !on_disk {
            continue;
        }
        match fresh.iter_mut().find(|n| old.same_as(n)) {
            Some(new) => {
                new.install_dir = old.install_dir.clone();
                new.previous_dir = old.previous_dir.clone();
                new.is_installed = old.is_installed;
                new.disksize = old.disksize;
                let changed = new.download != old.download || new.checksum != old.checksum;
                new.has_update = Some(changed || old.has_update == Some(true));
            }
            None => result.push(old),
        }
    }
    result.extend(fresh);
    result.sort_by(|a, b| b.date.cmp(&a.date));
    result
}

fn read_store() -> ReleaseStore {
    irori_core::json_store::read_or_default(&locations::releases_file())
}

fn write_store(store: &ReleaseStore) -> Result<()> {
    irori_core::json_store::write(&locations::releases_file(), store)
}

static STORE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Refresh the release list (Heroic's `updateWineVersionInfos`). With
/// `fetch`, the given sources are re-fetched from GitHub (failures of single
/// sources are logged); without, the cache is only pruned of entries that
/// can no longer be used. Returns the whole list.
pub async fn update_releases(fetch: bool, kinds: &[ReleaseKind]) -> Result<Vec<Release>> {
    let _guard = STORE_LOCK.lock().await;
    let mut store = read_store();
    if !fetch {
        let before = store.releases.len();
        store.releases.retain(keep_cached);
        if store.releases.len() != before {
            write_store(&store)?;
        }
        return Ok(store.releases);
    }
    let mut fresh = Vec::new();
    let mut fetched = Vec::new();
    let mut last_err = None;
    for &kind in kinds {
        match fetch_releases(kind, 50).await {
            Ok(list) => {
                fresh.extend(list);
                fetched.push(kind);
                store
                    .fetched_at
                    .insert(kind.type_str().into(), irori_core::util::unix_now());
            }
            Err(e) => {
                tracing::warn!("{e:#}");
                last_err = Some(e);
            }
        }
    }
    if fetched.is_empty() {
        if let Some(e) = last_err
            && store.releases.is_empty()
        {
            return Err(e);
        }
        return Ok(store.releases);
    }
    store.releases = merge(std::mem::take(&mut store.releases), fresh, &fetched);
    write_store(&store)?;
    Ok(store.releases)
}

/// Releases of one source, fetched from GitHub when the cache has none or is
/// older than a day. Install state reflects what is on disk now.
pub async fn available_releases(kind: ReleaseKind) -> Result<Vec<Release>> {
    let store = read_store();
    let fresh_enough = store
        .fetched_at
        .get(kind.type_str())
        .is_some_and(|t| irori_core::util::unix_now().saturating_sub(*t) < 24 * 3600);
    let has_any = store.releases.iter().any(|r| r.kind == kind.type_str());
    let list = if has_any && fresh_enough {
        store.releases
    } else {
        match update_releases(true, &[kind]).await {
            Ok(list) => list,
            Err(e) if has_any => {
                tracing::warn!("{e:#}; using cached releases");
                store.releases
            }
            Err(e) => return Err(e),
        }
    };
    Ok(list
        .into_iter()
        .filter(|r| r.kind == kind.type_str())
        .map(|mut r| {
            if r.is_installed == Some(true) && !r.installed() {
                r.is_installed = Some(false);
                r.install_dir = None;
            }
            r
        })
        .collect())
}

/// A build from Irori's Wine manager with something newer to install.
#[derive(Debug, Clone, PartialEq)]
pub struct WineUpdate {
    pub installed: Release,
    pub latest: Release,
}

/// What can be updated, one entry per source, from `releases` (newest first).
/// A release republished under its name updates in place; a newer one
/// installs beside the old build, which games keep using.
pub fn pending_updates(releases: &[Release]) -> Vec<WineUpdate> {
    let mut kinds: Vec<&str> = Vec::new();
    for r in releases {
        if !kinds.contains(&r.kind.as_str()) {
            kinds.push(&r.kind);
        }
    }
    kinds
        .into_iter()
        .filter_map(|kind| {
            let mut of_kind = releases.iter().filter(|r| r.kind == kind);
            let newest = of_kind.next()?;
            let installed = std::iter::once(newest)
                .chain(of_kind)
                .find(|r| r.is_installed == Some(true))?;
            let latest = if installed.has_update == Some(true) {
                installed
            } else if newest.date > installed.date {
                newest
            } else {
                return None;
            };
            Some(WineUpdate {
                installed: installed.clone(),
                latest: latest.clone(),
            })
        })
        .collect()
}

/// [`pending_updates`] of the sources that have a build installed. Lists come
/// from a cache of up to a day.
pub async fn check_updates() -> Result<Vec<WineUpdate>> {
    let mut kinds: Vec<ReleaseKind> = Vec::new();
    for kind in cached_releases()
        .iter()
        .filter(|r| r.is_installed == Some(true))
        .filter_map(Release::release_kind)
    {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    let mut releases = Vec::new();
    for kind in kinds {
        releases.extend(available_releases(kind).await?);
    }
    Ok(pending_updates(&releases))
}

/// Every cached release (no network).
pub fn cached_releases() -> Vec<Release> {
    read_store().releases
}

/// Directory a release installs into.
pub fn install_base(release: &Release, app: &AppSettings) -> PathBuf {
    if release.release_kind() == Some(ReleaseKind::GamePortingToolkit) {
        return Paths::get().tools_dir().join("game-porting-toolkit");
    }
    if release.kind.contains("Wine") {
        return Paths::get().wine_dir();
    }
    if app.download_proton_to_steam {
        let steam = locations::steam_path(app);
        if steam.join("steam.sh").is_file() {
            return steam.join("compatibilitytools.d");
        }
        tracing::warn!(
            "Steam was not found at {}; installing Proton into Irori's tools",
            steam.display()
        );
    }
    Paths::get().proton_dir()
}

fn file_name_of(url: &str) -> String {
    url.split('?')
        .next()
        .and_then(|u| u.rsplit('/').next())
        .filter(|n| !n.is_empty())
        .unwrap_or("download.tar")
        .to_string()
}

/// Download, verify and unpack a release (Heroic's `installWineVersion`).
///
/// The archive's SHA-512 must equal the published digest exactly. The build
/// is extracted into a staging folder and moved into place only when
/// extraction succeeded, so a failed or cancelled install never leaves a
/// broken build (or destroys the previous one on update). Returns the
/// release with its install state. Cancellation surfaces as
/// [`download::Cancelled`].
pub async fn install(release: &Release, app: &AppSettings, ctx: &OpContext) -> Result<Release> {
    let kind = release
        .release_kind()
        .context("Unknown runtime release source")?;
    anyhow::ensure!(
        ReleaseKind::available().contains(&kind),
        "This runtime cannot be installed on this operating system"
    );
    anyhow::ensure!(
        Path::new(&release.version).components().count() == 1
            && release.version != "."
            && release.version != ".."
            && !release.version.contains(['/', '\\']),
        "Invalid release directory"
    );
    anyhow::ensure!(
        kind != ReleaseKind::GamePortingToolkit || discovery::toolkit_supported(),
        "Game Porting Toolkit requires Apple silicon and macOS 14 or later"
    );
    if release.download.is_empty() {
        bail!("{} has no download", release.version);
    }
    if !is_compatible_download(&release.kind, &release.download, host_arch()) {
        bail!(
            "Download for {} is not compatible with {}. Refresh the compatibility-tool list.",
            release.version,
            host_arch()
        );
    }
    let base = install_base(release, app);
    if base.exists() && !base.is_dir() {
        bail!("{} is not a directory", base.display());
    }
    tokio::fs::create_dir_all(&base)
        .await
        .with_context(|| format!("creating {}", base.display()))?;
    let overwrite = release.has_update == Some(true);
    let target = match release.install_dir.as_deref().filter(|d| !d.is_empty()) {
        Some(d) if overwrite => PathBuf::from(d),
        _ => base.join(&release.version),
    };

    if target.is_dir() && !overwrite {
        tracing::warn!("{} is already installed", release.version);
        let size = dir_size(&target).await;
        return record_installed(release, &target, size).await;
    }

    let file_name = file_name_of(&release.download);
    let expected = if release.checksum.is_empty() {
        tracing::warn!("{} has no published checksum", release.version);
        None
    } else {
        let text = download::get_text(&release.checksum)
            .await
            .context("could not fetch the checksum")?;
        Some(
            download::parse_sha512sum(&text, &file_name)
                .context("the published checksum file is not valid")?,
        )
    };

    (ctx.step)(&format!("Downloading {}", release.version));
    let archive = base.join(&file_name);
    let _ = tokio::fs::remove_file(&archive).await;
    let downloaded =
        download::download(&release.download, &archive, &ctx.progress, &ctx.cancel).await?;
    if let Some(expected) = &expected
        && !download::checksum_matches(expected, &downloaded.sha512)
    {
        let _ = tokio::fs::remove_file(&archive).await;
        bail!("Checksum verification failed for {}", release.version);
    }

    (ctx.step)(&format!("Extracting {}", release.version));
    let staging = base.join(format!(".{}.extracting", release.version));
    let _ = tokio::fs::remove_dir_all(&staging).await;
    let (a, s, cancel, progress) = (
        archive.clone(),
        staging.clone(),
        ctx.cancel.clone(),
        ctx.progress.clone(),
    );
    let extracted =
        tokio::task::spawn_blocking(move || download::extract_tar(&a, &s, 1, progress, &cancel))
            .await
            .context("extraction task failed");
    let _ = tokio::fs::remove_file(&archive).await;
    if let Err(e) = extracted.and_then(|r| r) {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(e).with_context(|| format!("extracting {}", release.version));
    }

    let mut candidate = release.clone();
    candidate.install_dir = Some(staging.to_string_lossy().into_owned());
    if !candidate
        .installation()
        .is_some_and(|w| discovery::is_usable(&w))
    {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        bail!(
            "The downloaded archive does not contain a usable {} runtime",
            release.kind
        );
    }
    let backup = PathBuf::from(format!("{}_backup", target.display()));
    if target.exists() {
        let _ = tokio::fs::remove_dir_all(&backup).await;
        tokio::fs::rename(&target, &backup)
            .await
            .with_context(|| format!("replacing {}", target.display()))?;
    }
    if let Err(e) = tokio::fs::rename(&staging, &target).await {
        if backup.exists() {
            let _ = tokio::fs::rename(&backup, &target).await;
        }
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(e).with_context(|| format!("installing {}", target.display()));
    }
    let previous = if overwrite && backup.exists() {
        keep_previous(&base, &release.version, &backup).await
    } else {
        let _ = tokio::fs::remove_dir_all(&backup).await;
        None
    };
    let size = dir_size(&target).await;
    let mut release = release.clone();
    release.previous_dir = previous;
    record_installed(&release, &target, size).await
}

/// Move the build an update replaced to `<base>/.rollback/<version>`. It is
/// nested so build discovery doesn't list it. Going without is fine: the
/// update itself succeeded.
async fn keep_previous(base: &Path, version: &str, backup: &Path) -> Option<String> {
    let dir = base.join(".rollback");
    let kept = dir.join(version);
    let moved = async {
        tokio::fs::create_dir_all(&dir).await?;
        if kept.exists() {
            tokio::fs::remove_dir_all(&kept).await?;
        }
        tokio::fs::rename(backup, &kept).await
    }
    .await;
    match moved {
        Ok(()) => Some(kept.to_string_lossy().into_owned()),
        Err(e) => {
            tracing::warn!("keeping the previous {version}: {e:#}");
            let _ = tokio::fs::remove_dir_all(backup).await;
            None
        }
    }
}

/// Builds with an earlier version kept, from the cache (no network).
pub fn rollbacks() -> Vec<Release> {
    cached_releases()
        .into_iter()
        .filter(|r| {
            r.installed()
                && r.previous_dir
                    .as_deref()
                    .is_some_and(|d| Path::new(d).is_dir())
        })
        .collect()
}

/// Put `previous` in `target`'s place, then drop the replaced build. A failure
/// half way puts `target` back.
async fn swap_in_previous(target: &Path, previous: &Path) -> Result<()> {
    let replaced = previous.with_extension("replaced");
    let _ = tokio::fs::remove_dir_all(&replaced).await;
    tokio::fs::rename(target, &replaced)
        .await
        .with_context(|| format!("moving {} aside", target.display()))?;
    if let Err(e) = tokio::fs::rename(previous, target).await {
        let _ = tokio::fs::rename(&replaced, target).await;
        return Err(e).with_context(|| format!("restoring {}", target.display()));
    }
    tokio::fs::remove_dir_all(&replaced)
        .await
        .with_context(|| format!("deleting {}", replaced.display()))
}

/// Bring back the build `release` replaced. The update is on offer again,
/// and can be skipped.
pub async fn rollback(release: &Release) -> Result<Release> {
    let target = release
        .install_dir
        .as_deref()
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .context("This build is not installed")?;
    let previous = release
        .previous_dir
        .as_deref()
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .with_context(|| format!("No earlier {} is kept", release.version))?;
    irori_core::fs_guard::check_deletable(&target, Some(&locations::home()), &[])?;
    swap_in_previous(&target, &previous).await?;
    let mut updated = release.clone();
    updated.previous_dir = None;
    updated.has_update = Some(true);
    updated.disksize = dir_size(&target).await;
    let _guard = STORE_LOCK.lock().await;
    let mut store = read_store();
    if let Some(entry) = store
        .releases
        .iter_mut()
        .find(|r| r.kind == release.kind && r.version == release.version)
    {
        *entry = updated.clone();
    }
    write_store(&store)?;
    Ok(updated)
}

async fn dir_size(path: &Path) -> u64 {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || download::dir_size(&p))
        .await
        .unwrap_or(0)
}

async fn record_installed(release: &Release, dir: &Path, size: u64) -> Result<Release> {
    let mut updated = release.clone();
    updated.install_dir = Some(dir.to_string_lossy().into_owned());
    updated.is_installed = Some(true);
    updated.has_update = Some(false);
    updated.disksize = size;
    let _guard = STORE_LOCK.lock().await;
    let mut store = read_store();
    match store
        .releases
        .iter_mut()
        .find(|r| r.kind == release.kind && r.version == release.version)
    {
        Some(entry) => *entry = updated.clone(),
        None => store.releases.push(updated.clone()),
    }
    write_store(&store)?;
    Ok(updated)
}

/// Delete an installed release and clear its install state.
pub async fn remove(release: &Release) -> Result<()> {
    if let Some(dir) = release.install_dir.as_deref().filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        let home = locations::home();
        if dir.components().count() < 3 || dir == home {
            bail!("refusing to delete {}", dir.display());
        }
        irori_core::fs_guard::check_deletable(&dir, Some(&home), &[])?;
        if dir.exists() {
            tokio::fs::remove_dir_all(&dir)
                .await
                .with_context(|| format!("deleting {}", dir.display()))?;
        }
        if let Some(previous) = release.previous_dir.as_deref().map(Path::new)
            && previous.exists()
        {
            irori_core::fs_guard::check_deletable(previous, Some(&home), &[])?;
            tokio::fs::remove_dir_all(previous)
                .await
                .with_context(|| format!("deleting {}", previous.display()))?;
        }
    }
    let _guard = STORE_LOCK.lock().await;
    let mut store = read_store();
    if let Some(entry) = store
        .releases
        .iter_mut()
        .find(|r| r.kind == release.kind && r.version == release.version)
    {
        entry.is_installed = Some(false);
        entry.install_dir = Some(String::new());
        entry.previous_dir = None;
        entry.disksize = 0;
        entry.has_update = Some(false);
    }
    write_store(&store)?;
    Ok(())
}

/// Download the default build when no Wine or Proton is available (Heroic's
/// `downloadDefaultWine`): the latest Proton-CachyOS, else the latest
/// GE-Proton. Returns the installation that was just installed (Heroic
/// returns whichever Proton sorts first instead).
pub async fn download_default_wine(app: &AppSettings, ctx: &OpContext) -> Result<WineInstallation> {
    let kinds = if cfg!(target_os = "linux") {
        &ReleaseKind::DEFAULT[..]
    } else {
        ReleaseKind::available()
    };
    let releases = update_releases(true, kinds).await?;
    let pick = |kind: ReleaseKind| {
        releases
            .iter()
            .find(|r| {
                r.kind == kind.type_str()
                    && is_compatible_download(&r.kind, &r.download, host_arch())
            })
            .cloned()
    };
    let release = if cfg!(target_os = "macos") {
        discovery::toolkit_supported()
            .then(|| pick(ReleaseKind::GamePortingToolkit))
            .flatten()
            .or_else(|| pick(ReleaseKind::WineStagingMac))
            .or_else(|| pick(ReleaseKind::WineCrossover))
    } else {
        pick(ReleaseKind::ProtonCachyOs).or_else(|| pick(ReleaseKind::GeProton))
    }
    .context("No compatible Wine runtime could be downloaded")?;
    let installed = if release.installed() {
        release
    } else {
        install(&release, app, ctx).await?
    };
    installed
        .installation()
        .context("the downloaded build could not be found after installing")
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn the_previous_build_takes_the_place_of_the_current_one() {
        let dir = tempfile::tempdir().unwrap();
        let (target, previous) = (dir.path().join("GE"), dir.path().join(".rollback/GE"));
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::write(target.join("new"), b"").unwrap();
        std::fs::write(previous.join("old"), b"").unwrap();
        swap_in_previous(&target, &previous).await.unwrap();
        assert!(target.join("old").is_file());
        assert!(!target.join("new").exists());
        assert!(!previous.exists());
        assert!(!previous.with_extension("replaced").exists());
    }

    #[test]
    fn a_refreshed_list_keeps_the_previous_build() {
        let dir = tempfile::tempdir().unwrap();
        let old = Release {
            kind: "GE".into(),
            version: "GE-latest".into(),
            is_installed: Some(true),
            install_dir: Some(dir.path().to_string_lossy().into_owned()),
            previous_dir: Some("/kept".into()),
            ..Default::default()
        };
        let fresh = Release {
            kind: "GE".into(),
            version: "GE-latest".into(),
            ..Default::default()
        };
        let kind = ReleaseKind::from_type("GE");
        let merged = merge(
            vec![old],
            vec![fresh],
            &kind.into_iter().collect::<Vec<_>>(),
        );
        assert_eq!(merged[0].previous_dir.as_deref(), Some("/kept"));
    }

    #[test]
    fn newer_builds_are_found_per_source() {
        let release =
            |kind: &str, version: &str, date: &str, installed: bool, update: bool| Release {
                kind: kind.into(),
                version: version.into(),
                date: date.into(),
                is_installed: installed.then_some(true),
                has_update: update.then_some(true),
                ..Default::default()
            };
        let list = [
            release("GE", "GE-11", "2026-10-02", false, false),
            release("GE", "GE-10", "2026-09-01", true, false),
            release("Kron", "K-2", "2026-10-01", false, false),
            release("Cachy", "C-latest", "2026-10-01", true, true),
            release("Cachy", "C-old", "2026-01-01", false, false),
            release("Wine", "W-9", "2026-05-01", true, false),
        ];
        let updates = pending_updates(&list);
        let versions: Vec<_> = updates
            .iter()
            .map(|u| (u.installed.version.as_str(), u.latest.version.as_str()))
            .collect();
        assert_eq!(versions, [("GE-10", "GE-11"), ("C-latest", "C-latest")]);
    }

    use super::*;

    fn asset(name: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "browser_download_url": format!("https://github.com/x/releases/download/t/{name}"),
            "size": 100
        })
    }

    fn gh(tag: &str, date: &str, assets: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "tag_name": tag,
            "published_at": format!("{date}T10:00:00Z"),
            "html_url": format!("https://github.com/x/releases/tag/{tag}"),
            "assets": assets.iter().map(|a| asset(a)).collect::<Vec<_>>()
        })
    }

    fn parse(v: serde_json::Value) -> Vec<GhRelease> {
        serde_json::from_value(v).unwrap()
    }

    #[tokio::test]
    #[ignore = "requires the local Postman runtime-releases simulation"]
    async fn simulated_macos_releases() {
        let base = std::env::var("IRORI_RELEASE_TEST_URL").expect("Set the simulator's local URL");
        let url = url::Url::parse(&base).unwrap();
        assert!(
            matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1")),
            "This test only talks to a local simulator"
        );
        for kind in ReleaseKind::for_host(irori_core::HostPlatform::Mac) {
            let path = kind.url().strip_prefix("https://api.github.com").unwrap();
            let releases =
                fetch_releases_at(*kind, &format!("{base}/github{path}?per_page=1"), "aarch64")
                    .await
                    .unwrap();
            assert_eq!(releases.len(), 2);
            assert_eq!(releases[0].kind, kind.type_str());
            assert!(releases[1].downsize > 100_000_000);
            let error = fetch_releases_at(
                *kind,
                &format!("{base}/limited{path}?per_page=1"),
                "aarch64",
            )
            .await
            .unwrap_err();
            assert!(format!("{error:#}").contains("429"));
        }
    }

    #[test]
    fn mac_sources_and_asset_architectures() {
        use irori_core::HostPlatform;
        assert!(ReleaseKind::for_host(HostPlatform::Windows).is_empty());
        assert_eq!(
            ReleaseKind::for_host(HostPlatform::Mac),
            &[
                ReleaseKind::GamePortingToolkit,
                ReleaseKind::WineCrossover,
                ReleaseKind::WineStagingMac
            ]
        );
        for (kind, tag, assets, selected) in [
            (
                ReleaseKind::WineCrossover,
                "23.7.1-1",
                vec!["Wine-Crossover-23.7.1-1.tar.xz"],
                "Wine-Crossover-23.7.1-1.tar.xz",
            ),
            (
                ReleaseKind::WineStagingMac,
                "11.18",
                vec![
                    "wine-devel-11.18-osx64.tar.xz",
                    "wine-staging-11.18-osx64.tar.xz",
                ],
                "wine-staging-11.18-osx64.tar.xz",
            ),
            (
                ReleaseKind::GamePortingToolkit,
                "Game-Porting-Toolkit-3.0-3",
                vec!["game-porting-toolkit-3.0-3.tar.xz"],
                "game-porting-toolkit-3.0-3.tar.xz",
            ),
        ] {
            let fixture = serde_json::json!([gh(tag, "2026-10-25", &assets)]);
            let arm = releases_from_github(kind, parse(fixture.clone()), "aarch64");
            assert_eq!(arm.len(), 2);
            assert!(arm[1].download.ends_with(selected));
            assert_eq!(arm[0].kind, kind.type_str());
            let intel = releases_from_github(kind, parse(fixture), "x86_64");
            assert_eq!(intel.is_empty(), kind == ReleaseKind::GamePortingToolkit);
        }
    }

    #[test]
    fn ge_proton_assets() {
        let list = parse(serde_json::json!([
            gh(
                "GE-Proton10-20",
                "2026-10-19",
                &[
                    "GE-Proton10-20-aarch64.tar.gz",
                    "GE-Proton10-20.sha512sum",
                    "GE-Proton10-20.tar.gz",
                ]
            ),
            gh(
                "GE-Proton10-19-LoL",
                "2026-10-01",
                &["GE-Proton10-19-LoL.tar.gz"]
            ),
            gh("nothing", "2026-09-01", &["source.zip"]),
        ]));
        let rel = releases_from_github(ReleaseKind::GeProton, list, "x86_64");
        assert_eq!(rel.len(), 3);
        assert_eq!(rel[0].version, "GE-Proton-latest");
        assert_eq!(rel[0].download, rel[1].download);
        assert_eq!(rel[1].version, "GE-Proton10-20");
        assert!(rel[1].download.ends_with("/GE-Proton10-20.tar.gz"));
        assert!(rel[1].checksum.ends_with("/GE-Proton10-20.sha512sum"));
        assert_eq!(rel[1].date, "2026-10-19");
        assert_eq!(rel[1].downsize, 100);
        assert_eq!(rel[2].checksum, "");

        let list = parse(serde_json::json!([gh(
            "GE-Proton10-20",
            "2026-10-19",
            &["GE-Proton10-20.tar.gz", "GE-Proton10-20-aarch64.tar.gz",]
        )]));
        let rel = releases_from_github(ReleaseKind::GeProton, list, "aarch64");
        assert!(rel[1].download.ends_with("-aarch64.tar.gz"));
    }

    #[test]
    fn cachyos_assets_and_latest_fallback() {
        let list = parse(serde_json::json!([gh(
            "cachyos-11.0-20261015-slr",
            "2026-10-20",
            &[
                "proton-cachyos-11.0-20261015-slr-x86_64_v3.tar.xz",
                "proton-cachyos-11.0-20261015-slr-x86_64.tar.xz",
                "proton-cachyos-11.0-20261015-slr-x86_64.sha512sum",
                "proton-cachyos-11.0-20261015-slr-arm64.tar.xz",
            ]
        )]));
        let rel = releases_from_github(ReleaseKind::ProtonCachyOs, list, "x86_64");
        assert_eq!(rel[0].version, "Proton-CachyOS-latest");
        assert_eq!(rel[1].version, "cachyos-11.0-20261015-slr");
        assert!(rel[1].download.ends_with("-slr-x86_64.tar.xz"));
        assert!(rel[1].checksum.ends_with("-slr-x86_64.sha512sum"));
    }

    #[test]
    fn wine_sources() {
        let list = parse(serde_json::json!([gh(
            "GE-Proton8-26",
            "2024-01-01",
            &[
                "wine-lutris-GE-Proton8-26-LoL-x86_64.tar.xz",
                "wine-lutris-GE-Proton8-26-x86_64.tar.xz",
                "wine-lutris-GE-Proton8-26-x86_64.sha512sum",
            ]
        )]));
        let rel = releases_from_github(ReleaseKind::WineGe, list, "x86_64");
        assert_eq!(rel[1].version, "Wine-GE-Proton8-26");
        assert!(rel[1].download.ends_with("GE-Proton8-26-x86_64.tar.xz"));
        assert!(rel[1].checksum.ends_with("GE-Proton8-26-x86_64.sha512sum"));

        let list = parse(serde_json::json!([gh(
            "10.17",
            "2026-10-10",
            &[
                "wine-10.17-amd64-wow64.tar.xz",
                "wine-10.17-amd64.tar.xz",
                "wine-10.17-staging-amd64.tar.xz",
                "wine-10.17-staging-tkg-amd64.tar.xz",
            ]
        )]));
        let rel = releases_from_github(ReleaseKind::WineKron4ek, list.clone(), "x86_64");
        assert_eq!(rel[1].version, "Wine-Kron4ek-10.17-staging");
        assert!(rel[1].download.ends_with("wine-10.17-staging-amd64.tar.xz"));
        assert!(releases_from_github(ReleaseKind::WineKron4ek, list, "aarch64").is_empty());
    }

    #[test]
    fn compatibility() {
        assert!(is_compatible_download(
            "GE-Proton",
            "x/GE-Proton9-1.tar.gz",
            "x86_64"
        ));
        assert!(!is_compatible_download(
            "GE-Proton",
            "x/GE-Proton9-1-arm64.tar.xz",
            "x86_64"
        ));
        assert!(!is_compatible_download(
            "GE-Proton",
            "x/GE-Proton9-1.zip",
            "x86_64"
        ));
        assert!(is_compatible_download(
            "Proton-CachyOS",
            "x/p-arm64.tar.xz",
            "aarch64"
        ));
        assert!(!is_compatible_download(
            "Proton-CachyOS",
            "x/p-x86_64_v3.tar.xz",
            "x86_64"
        ));
        assert!(is_compatible_download("Wine-Lutris", "anything", "x86_64"));
    }

    #[test]
    fn merging_keeps_install_state() {
        let dir = tempfile::tempdir().unwrap();
        let installed_dir = dir.path().join("GE-Proton-latest");
        std::fs::create_dir_all(&installed_dir).unwrap();
        let old_latest = Release {
            version: "GE-Proton-latest".into(),
            kind: "GE-Proton".into(),
            date: "2026-01-01".into(),
            download: "https://x/GE-Proton9-1.tar.gz".into(),
            install_dir: Some(installed_dir.to_string_lossy().into_owned()),
            is_installed: Some(true),
            disksize: 7,
            ..Default::default()
        };
        let gone = Release {
            version: "GE-Proton8-1".into(),
            kind: "GE-Proton".into(),
            date: "2025-01-01".into(),
            download: "https://x/GE-Proton8-1.tar.gz".into(),
            ..Default::default()
        };
        let other = Release {
            version: "Wine-GE-Proton8-26".into(),
            kind: "Wine-GE".into(),
            date: "2024-01-01".into(),
            download: "https://x/w.tar.xz".into(),
            ..Default::default()
        };
        let fresh = vec![
            Release {
                version: "GE-Proton-latest".into(),
                kind: "GE-Proton".into(),
                date: "2026-10-19".into(),
                download: "https://x/GE-Proton10-20.tar.gz".into(),
                ..Default::default()
            },
            Release {
                version: "GE-Proton10-20".into(),
                kind: "GE-Proton".into(),
                date: "2026-10-19".into(),
                download: "https://x/GE-Proton10-20.tar.gz".into(),
                ..Default::default()
            },
        ];
        let merged = merge(
            vec![old_latest, gone, other],
            fresh,
            &[ReleaseKind::GeProton],
        );
        let versions: Vec<&str> = merged.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(
            versions,
            vec!["GE-Proton-latest", "GE-Proton10-20", "Wine-GE-Proton8-26"]
        );
        let latest = &merged[0];
        assert_eq!(latest.is_installed, Some(true));
        assert_eq!(latest.has_update, Some(true));
        assert_eq!(latest.disksize, 7);
        assert!(merged[1].install_dir.is_none());
    }

    /// Opt-in real archive verification; never downloads during ordinary CI.
    /// Run alone with a dedicated empty IRORI_HOME and IRORI_RUNTIME_SMOKE set
    /// to one of the three macOS source names.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "downloads and runs a real macOS Wine runtime; requires Rosetta on Apple silicon"]
    async fn macos_runtime_archive_smoke() {
        let root = std::env::var_os("IRORI_HOME").expect("set a dedicated IRORI_HOME");
        assert!(Path::new(&root).is_absolute());
        let source = std::env::var("IRORI_RUNTIME_SMOKE").expect("set IRORI_RUNTIME_SMOKE");
        let kind = ReleaseKind::from_type(&source).expect("unknown source");
        assert!(ReleaseKind::available().contains(&kind));
        let releases = update_releases(true, &[kind]).await.unwrap();
        let release = releases
            .iter()
            .find(|r| {
                r.kind == source
                    && !r.version.ends_with("latest")
                    && is_compatible_download(&r.kind, &r.download, host_arch())
            })
            .unwrap();
        let installed = install(release, &AppSettings::default(), &OpContext::detached())
            .await
            .unwrap();
        let wine = installed.installation().unwrap();
        assert!(discovery::is_usable(&wine));
        let spec =
            irori_core::process::CommandSpec::new(discovery::unquote(&wine.bin)).arg("--version");
        let result = discovery::run_with_timeout(&spec, std::time::Duration::from_secs(30))
            .await
            .expect("Wine timed out or could not start");
        assert!(result.success(), "{}", result.error_summary());
        assert!(
            result.stdout.to_lowercase().contains("wine"),
            "{}",
            result.stdout
        );
        eprintln!("Verified {}: {}", installed.version, result.stdout.trim());
        let prefix_owner = tempfile::Builder::new()
            .prefix("runtime-prefix-")
            .tempdir_in(&Paths::get().data)
            .unwrap();
        let prefix = prefix_owner.path().to_path_buf();
        let settings = irori_core::GameSettings {
            wine_version: wine.clone(),
            wine_prefix: prefix.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let game = irori_core::GameInfo {
            app_name: "runtime-smoke".into(),
            title: "Runtime smoke".into(),
            ..Default::default()
        };
        let id = irori_core::GameId::new(irori_core::Runner::Sideload, "runtime-smoke");
        let context = OpContext::detached();
        let environment = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            crate::prepare_launch(
                &AppSettings::default(),
                &settings,
                crate::Target {
                    id: &id,
                    game: &game,
                    platform: irori_core::InstallPlatform::Windows,
                },
                &context,
            ),
        )
        .await
        .expect("prefix initialization timed out")
        .unwrap();
        assert!(prefix.join("user.reg").is_file());
        assert!(!environment.env.contains_key("STEAM_COMPAT_DATA_PATH"));
        let spec = irori_core::process::CommandSpec::new(discovery::unquote(&wine.bin))
            .args(["cmd", "/c", "echo", "IRORI_RUNTIME_OK"])
            .envs(environment.env);
        let result = discovery::run_with_timeout(&spec, std::time::Duration::from_secs(30))
            .await
            .expect("Windows command did not finish");
        let _ = crate::prefix::kill(&wine, &prefix).await;
        assert!(result.success(), "{}", result.error_summary());
        assert!(
            result.stdout.contains("IRORI_RUNTIME_OK"),
            "{}",
            result.stdout
        );
        eprintln!(
            "Verified prefix initialization and Windows command execution for {}",
            installed.version
        );
    }

    #[test]
    fn reads_heroic_cache_shape() {
        let json = r#"{"wine-releases":[{"version":"GE-Proton-latest","type":"GE-Proton","date":"2026-10-19","download":"https://x/a.tar.gz","downsize":5,"disksize":0,"checksum":"https://x/a.sha512sum","release_notes_link":"https://x","installDir":"/nonexistent","isInstalled":true,"hasUpdate":false}]}"#;
        let store: ReleaseStore = serde_json::from_str(json).unwrap();
        let r = &store.releases[0];
        assert_eq!(r.release_kind(), Some(ReleaseKind::GeProton));
        assert!(!r.installed(), "missing directory is not installed");
        assert_eq!(file_name_of(&r.download), "a.tar.gz");
        assert_eq!(checksum_name("a-x86_64.tar.xz"), "a-x86_64.sha512sum");
        let back = serde_json::to_value(&store).unwrap();
        assert_eq!(back["wine-releases"][0]["installDir"], "/nonexistent");
    }
}
