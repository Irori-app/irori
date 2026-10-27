//! What the itch.io store keeps on disk: the API key and account
//! (`auth.json`, owner-only), the owned games last fetched
//! (`library.json` in the cache) and the installed games (`installed.json`).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use irori_core::util::format_bytes;
use irori_core::{InstallPlatform, InstalledInfo, json_store};
use irori_itch_api::{OwnedKey, User};
use serde::{Deserialize, Serialize};

/// Where the store's files live.
#[derive(Debug, Clone)]
pub struct Layout {
    pub config: PathBuf,
    pub cache: PathBuf,
}

impl Layout {
    pub fn auth_file(&self) -> PathBuf {
        self.config.join("auth.json")
    }

    pub fn installed_file(&self) -> PathBuf {
        self.config.join("installed.json")
    }

    pub fn library_file(&self) -> PathBuf {
        self.cache.join("library.json")
    }
}

/// The saved login. The key is a secret: it never goes to logs.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Auth {
    pub api_key: String,
    pub user: User,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("user", &self.user.username)
            .finish_non_exhaustive()
    }
}

/// Owned games as last fetched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryCache {
    pub keys: Vec<OwnedKey>,
}

/// An installed game.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstalledRecord {
    pub title: String,
    pub install_path: String,
    pub platform: InstallPlatform,
    /// The upload the files came from; 0 for imported folders.
    pub upload_id: u64,
    /// [`irori_itch_api::Upload::revision`] of the installed files.
    pub revision: String,
    pub version: String,
    /// The program to start, relative to the install folder.
    pub executable: String,
    pub size: u64,
    /// The user keeps this version: no update checks.
    pub pinned_version: bool,
}

impl InstalledRecord {
    pub fn executable_path(&self) -> Option<PathBuf> {
        (!self.executable.is_empty()).then(|| self.resolve(&self.executable))
    }

    /// A path relative to the install folder (absolute paths stay as they are).
    pub fn resolve(&self, path: &str) -> PathBuf {
        Path::new(&self.install_path).join(path)
    }

    pub fn to_installed_info(&self) -> InstalledInfo {
        InstalledInfo {
            install_path: self.install_path.clone(),
            executable: self.executable.clone(),
            install_size: format_bytes(self.size),
            install_size_bytes: self.size,
            version: self.version.clone(),
            platform: self.platform,
            pinned_version: self.pinned_version,
            ..Default::default()
        }
    }
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
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

pub fn load_auth(layout: &Layout) -> Option<Auth> {
    json_store::read_or_default::<Option<Auth>>(&layout.auth_file())
        .filter(|a| !a.api_key.is_empty())
}

pub fn save_auth(layout: &Layout, auth: &Auth) -> Result<()> {
    write_private(&layout.auth_file(), &serde_json::to_vec_pretty(auth)?)
        .context("Could not save the itch.io login")
}

pub fn load_installed(layout: &Layout) -> BTreeMap<String, InstalledRecord> {
    json_store::read_or_default(&layout.installed_file())
}

pub fn save_installed(
    layout: &Layout,
    installed: &BTreeMap<String, InstalledRecord>,
) -> Result<()> {
    json_store::write(&layout.installed_file(), installed)
        .context("Could not save the list of installed itch.io games")
}

pub fn load_library(layout: &Layout) -> LibraryCache {
    json_store::read_or_default(&layout.library_file())
}

pub fn save_library(layout: &Layout, library: &LibraryCache) -> Result<()> {
    json_store::write(&layout.library_file(), library)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_key_private_and_out_of_debug_output() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout {
            config: dir.path().join("config"),
            cache: dir.path().join("cache"),
        };
        assert!(load_auth(&layout).is_none());
        let auth = Auth {
            api_key: "secret-key".into(),
            user: User {
                username: "player".into(),
                ..Default::default()
            },
        };
        assert!(!format!("{auth:?}").contains("secret-key"));
        save_auth(&layout, &auth).unwrap();
        assert_eq!(load_auth(&layout).unwrap().api_key, "secret-key");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(layout.auth_file())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
