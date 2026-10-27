//! Irori's Amazon state files, all inside
//! `Paths::store_config_dir(Runner::Nile)`:
//!
//! * `credentials.json`: device tokens (owner-only permissions).
//! * `entitlements.json`: owned games as last synced.
//! * `installed.json`: installed games by product id.
//! * `manifests/<product id>.proto`: manifest of each installed build, the
//!   base of update diffs and uninstalls.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use irori_amazon_api::{Credentials, Entitlement};
use irori_core::json_store;
use serde::{Deserialize, Serialize};

/// An installed game.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InstalledGame {
    /// Build id (`versionId`).
    pub version: String,
    pub path: PathBuf,
    /// Size of the build in bytes.
    #[serde(default)]
    pub size: u64,
}

pub type Installed = BTreeMap<String, InstalledGame>;

/// File locations below one config directory.
#[derive(Debug, Clone)]
pub struct Layout {
    pub dir: PathBuf,
}

impl Layout {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn credentials(&self) -> PathBuf {
        self.dir.join("credentials.json")
    }

    pub fn entitlements(&self) -> PathBuf {
        self.dir.join("entitlements.json")
    }

    pub fn installed(&self) -> PathBuf {
        self.dir.join("installed.json")
    }

    pub fn manifest(&self, product_id: &str) -> PathBuf {
        self.dir
            .join("manifests")
            .join(format!("{}.proto", irori_core::paths::sanitize(product_id)))
    }

    pub fn read_credentials(&self) -> Option<Credentials> {
        json_store::read::<Credentials>(&self.credentials())
            .map_err(|e| tracing::warn!("{e:#}"))
            .ok()
            .flatten()
            .filter(|c| !c.refresh_token.is_empty())
    }

    pub fn write_credentials(&self, creds: &Credentials) -> Result<()> {
        write_private(&self.credentials(), &serde_json::to_vec_pretty(creds)?)
    }

    pub fn read_entitlements(&self) -> Vec<Entitlement> {
        json_store::read(&self.entitlements())
            .map_err(|e| tracing::warn!("{e:#}"))
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    pub fn write_entitlements(&self, list: &[Entitlement]) -> Result<()> {
        json_store::write_bytes(&self.entitlements(), &serde_json::to_vec(list)?)
    }

    pub fn read_installed(&self) -> Installed {
        json_store::read(&self.installed())
            .map_err(|e| tracing::warn!("{e:#}"))
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    pub fn write_installed(&self, installed: &Installed) -> Result<()> {
        json_store::write(&self.installed(), installed)
            .context("could not update the list of installed Amazon games")
    }

    pub fn read_manifest(&self, product_id: &str) -> Option<Vec<u8>> {
        std::fs::read(self.manifest(product_id)).ok()
    }

    pub fn write_manifest(&self, product_id: &str, bytes: &[u8]) -> Result<()> {
        json_store::write_bytes(&self.manifest(product_id), bytes)
    }

    pub fn remove_manifest(&self, product_id: &str) {
        remove_quietly(&self.manifest(product_id));
    }
}

/// Delete a file, ignoring a missing one.
pub fn remove_quietly(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("removing {}: {e}", path.display());
    }
}

/// Write `bytes` atomically, readable only by the user where the platform
/// supports it (credentials).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_state() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().join("nile"));
        assert!(layout.read_credentials().is_none());
        assert!(layout.read_installed().is_empty());
        let creds = Credentials {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 5,
            device_serial: "s".into(),
            user_id: "u".into(),
            display_name: "n".into(),
        };
        layout.write_credentials(&creds).unwrap();
        assert_eq!(layout.read_credentials(), Some(creds));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(layout.credentials())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let mut installed = Installed::new();
        installed.insert(
            "p".into(),
            InstalledGame {
                version: "v".into(),
                path: "/g".into(),
                size: 3,
            },
        );
        layout.write_installed(&installed).unwrap();
        assert_eq!(layout.read_installed(), installed);
        layout.write_manifest("amzn1.adg.product.x", b"m").unwrap();
        assert_eq!(layout.read_manifest("amzn1.adg.product.x").unwrap(), b"m");
        layout.remove_manifest("amzn1.adg.product.x");
        assert!(layout.read_manifest("amzn1.adg.product.x").is_none());
    }
}
