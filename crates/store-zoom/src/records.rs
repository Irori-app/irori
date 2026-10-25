//! Files the store keeps on disk: credentials, installed games, interrupted
//! downloads and API caches.
//!
//! Layout (config dir = `Paths::store_config_dir(Zoom)`, cache dir =
//! `Paths::store_cache_dir(Zoom)`):
//!
//! | File | Content |
//! |---|---|
//! | `<config>/auth.json` (0600) | [`Auth`] |
//! | `<config>/installed.json` | `{app_name: InstalledRecord}` |
//! | `<config>/pending.json` | `{app_name: PendingInstall}` (interrupted installs) |
//! | `<cache>/library.json` | [`LibraryCache`] (raw API entries) |
//! | `<cache>/files/<id>.json` | [`FilesCache`] |

use std::collections::BTreeMap;
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use ember_core::{InstallPlatform, InstalledInfo, Paths, Runner, json_store, util};
use serde::{Deserialize, Serialize};

use crate::api::{FilesResponse, ZoomGame};

/// Where the store keeps its files.
#[derive(Debug, Clone)]
pub struct Layout {
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Default for Layout {
    fn default() -> Self {
        let p = Paths::get();
        Self {
            config_dir: p.store_config_dir(Runner::Zoom),
            cache_dir: p.store_cache_dir(Runner::Zoom),
        }
    }
}

impl Layout {
    pub fn auth_file(&self) -> PathBuf {
        self.config_dir.join("auth.json")
    }

    pub fn installed_file(&self) -> PathBuf {
        self.config_dir.join("installed.json")
    }

    pub fn pending_file(&self) -> PathBuf {
        self.config_dir.join("pending.json")
    }

    pub fn library_cache(&self) -> PathBuf {
        self.cache_dir.join("library.json")
    }

    pub fn files_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("files")
    }

    pub fn files_cache(&self, app_name: &str) -> PathBuf {
        self.files_cache_dir()
            .join(format!("{}.json", ember_core::paths::sanitize(app_name)))
    }
}

/// Saved login.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Auth {
    pub token: String,
    pub username: String,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// Identity of an installer file, used to notice new builds.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(default)]
pub struct InstalledFile {
    pub id: String,
    pub name: String,
    pub size: String,
}

/// An installed game. Paths inside the install (`executable`, `dosbox_conf`,
/// `dosbox_dir`) are relative to `install_path`, so moving an install only
/// changes `install_path`. Absolute paths are accepted for entries imported
/// from elsewhere.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstalledRecord {
    pub app_name: String,
    pub install_path: String,
    pub executable: String,
    /// Platform the game runs as on this host (`Linux` for native builds and
    /// DOSBox games played with the system DOSBox).
    pub platform: InstallPlatform,
    /// Platform of the downloaded build (`Windows` for DOSBox games).
    pub build_platform: InstallPlatform,
    pub is_dosbox: bool,
    pub dosbox_conf: Vec<String>,
    /// Working directory of DOSBox (directory of the game's `.conf` files).
    pub dosbox_dir: String,
    pub install_size_bytes: u64,
    pub language: Option<String>,
    pub version: String,
    /// Installer files of the installed build (empty for imported games
    /// until the first update check records a baseline).
    pub files: Vec<InstalledFile>,
    pub pinned_version: bool,
    /// Unix seconds.
    pub installed_at: u64,
}

impl InstalledRecord {
    /// Resolve a path stored relative to the install directory.
    pub fn resolve(&self, rel: &str) -> PathBuf {
        let p = Path::new(rel);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            Path::new(&self.install_path).join(p)
        }
    }

    pub fn executable_path(&self) -> Option<PathBuf> {
        (!self.executable.is_empty()).then(|| self.resolve(&self.executable))
    }

    pub fn to_installed_info(&self) -> InstalledInfo {
        InstalledInfo {
            install_path: self.install_path.clone(),
            executable: self
                .executable_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            install_size: util::format_bytes(self.install_size_bytes),
            install_size_bytes: self.install_size_bytes,
            version: self.version.clone(),
            platform: self.platform,
            is_dlc: false,
            installed_dlcs: Vec::new(),
            installed_with_dlcs: false,
            language: self.language.clone(),
            version_etag: None,
            build_id: None,
            branch: None,
            pinned_version: self.pinned_version,
            is_dosbox: self.is_dosbox,
            sdl: Vec::new(),
        }
    }
}

/// A started but unfinished install: where its downloads are staged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingInstall {
    pub staging_dir: PathBuf,
    pub install_path: PathBuf,
    pub platform: InstallPlatform,
    /// The install directory did not exist before this install started, so
    /// cancelling may delete it.
    pub created_install_dir: bool,
}

/// Cached library listing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryCache {
    /// Unix seconds.
    pub fetched_at: u64,
    pub games: Vec<ZoomGame>,
}

/// Cached file listing of one game.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesCache {
    pub fetched_at: u64,
    pub files: FilesResponse,
}

/// Write `bytes` atomically with owner-only permissions (credentials).
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("invalid credentials path")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

pub fn save_auth(layout: &Layout, auth: &Auth) -> Result<()> {
    write_private(&layout.auth_file(), &serde_json::to_vec_pretty(auth)?)
}

pub fn load_auth(layout: &Layout) -> Option<Auth> {
    json_store::read_or_default::<Option<Auth>>(&layout.auth_file()).filter(|a| !a.token.is_empty())
}

pub fn load_installed(layout: &Layout) -> BTreeMap<String, InstalledRecord> {
    json_store::read_or_default(&layout.installed_file())
}

pub fn save_installed(
    layout: &Layout,
    installed: &BTreeMap<String, InstalledRecord>,
) -> Result<()> {
    json_store::write(&layout.installed_file(), installed).context("saving installed ZOOM games")
}

pub fn load_pending(layout: &Layout) -> BTreeMap<String, PendingInstall> {
    json_store::read_or_default(&layout.pending_file())
}

pub fn save_pending(layout: &Layout, pending: &BTreeMap<String, PendingInstall>) -> Result<()> {
    json_store::write(&layout.pending_file(), pending).context("saving ZOOM download state")
}

pub fn load_files_cache(
    layout: &Layout,
    app_name: &str,
    max_age_secs: u64,
) -> Option<FilesResponse> {
    let cache: FilesCache = json_store::read(&layout.files_cache(app_name))
        .ok()
        .flatten()?;
    (util::unix_now().saturating_sub(cache.fetched_at) <= max_age_secs).then_some(cache.files)
}

pub fn save_files_cache(layout: &Layout, app_name: &str, files: &FilesResponse) {
    let cache = FilesCache {
        fetched_at: util::unix_now(),
        files: files.clone(),
    };
    if let Err(e) = json_store::write(&layout.files_cache(app_name), &cache) {
        tracing::warn!("caching ZOOM file list: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_private() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout {
            config_dir: dir.path().join("c"),
            cache_dir: dir.path().join("k"),
        };
        save_auth(
            &layout,
            &Auth {
                token: "t".into(),
                username: "u".into(),
            },
        )
        .unwrap();
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(layout.auth_file())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(load_auth(&layout).unwrap().username, "u");
    }

    #[test]
    fn relative_paths_follow_install_path() {
        let mut r = InstalledRecord {
            install_path: "/games/a".into(),
            executable: "bin/game.exe".into(),
            ..Default::default()
        };
        assert_eq!(
            r.executable_path().unwrap(),
            PathBuf::from("/games/a/bin/game.exe")
        );
        r.install_path = "/mnt/b".into();
        assert_eq!(r.to_installed_info().executable, "/mnt/b/bin/game.exe");
        r.executable = "/abs/start.sh".into();
        assert_eq!(r.executable_path().unwrap(), PathBuf::from("/abs/start.sh"));
    }
}
