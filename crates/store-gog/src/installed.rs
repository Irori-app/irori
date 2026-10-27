//! Installed-game records, stored in Heroic's `installed.json` shape so a
//! Heroic install list imports unchanged.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use irori_core::{InstallPlatform, InstalledInfo, util};

/// Cyberpunk 2077 REDmod selection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CyberpunkConfig {
    pub mods_enabled: bool,
    pub mods_to_load: Vec<String>,
}

/// One installed GOG game (`installed.json` → `installed[]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct InstalledRecord {
    #[serde(rename = "appName")]
    pub app_name: String,
    pub platform: InstallPlatform,
    pub executable: String,
    pub install_path: String,
    pub install_size: String,
    pub is_dlc: bool,
    pub version: String,
    #[serde(rename = "installedDLCs")]
    pub installed_dlcs: Vec<String>,
    #[serde(rename = "installedWithDLCs", skip_serializing_if = "Option::is_none")]
    pub installed_with_dlcs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(rename = "versionEtag", skip_serializing_if = "Option::is_none")]
    pub version_etag: Option<String>,
    #[serde(rename = "buildId", skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(rename = "pinnedVersion")]
    pub pinned_version: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cyberpunk: Option<CyberpunkConfig>,
    /// Fields written by other tools, kept on rewrite.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl InstalledRecord {
    pub fn path(&self) -> PathBuf {
        PathBuf::from(&self.install_path)
    }

    /// GOG's spelling of the platform (`windows`, `linux`, `osx`).
    pub fn platform_str(&self) -> &'static str {
        self.platform.as_str()
    }

    /// Whether DLCs were chosen.
    pub fn with_dlcs(&self) -> bool {
        self.installed_with_dlcs.unwrap_or(false) || !self.installed_dlcs.is_empty()
    }

    pub fn build_id(&self) -> Option<&str> {
        self.build_id.as_deref().filter(|s| !s.is_empty())
    }

    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref().filter(|s| !s.is_empty())
    }

    /// Install language, defaulting to English.
    pub fn language_or_default(&self) -> &str {
        self.language
            .as_deref()
            .filter(|s| !s.is_empty() && *s != "undefined")
            .unwrap_or("en-US")
    }

    /// Irori's view of this install.
    pub fn to_info(&self) -> InstalledInfo {
        InstalledInfo {
            install_path: self.install_path.clone(),
            executable: self.executable.clone(),
            install_size: self.install_size.clone(),
            install_size_bytes: util::parse_size(&self.install_size).unwrap_or(0),
            version: self.version.clone(),
            platform: self.platform,
            is_dlc: self.is_dlc,
            installed_dlcs: self.installed_dlcs.clone(),
            installed_with_dlcs: self.with_dlcs(),
            language: self.language.clone(),
            version_etag: self.version_etag.clone(),
            build_id: self.build_id.clone(),
            branch: self.branch.clone(),
            pinned_version: self.pinned_version,
            is_dosbox: false,
            sdl: Vec::new(),
        }
    }
}

/// File wrapper: `{"installed": [...]}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct InstalledFile {
    pub installed: Vec<InstalledRecord>,
}

impl InstalledFile {
    /// Records with an app name (Heroic skips the rest too).
    pub fn into_records(self) -> Vec<InstalledRecord> {
        self.installed
            .into_iter()
            .filter(|r| !r.app_name.is_empty())
            .collect()
    }
}

/// A download that started and has not finished. Kept so a paused or
/// interrupted install resumes into the same folder, and a canceled one can
/// be cleaned up.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingInstall {
    pub install_path: String,
    pub platform: InstallPlatform,
}

/// Total size of the regular files under `dir` (symlinks count zero).
pub fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(meta) = e.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(e.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEROIC: &str = r#"{ "installed": [ {
      "platform": "windows", "executable": "", "install_path": "/g/Cyberpunk 2077",
      "install_size": "75.85 GiB", "is_dlc": false, "version": "2.5", "appName": "1423049311",
      "installedDLCs": ["1256837418"], "language": "en-US",
      "versionEtag": "\"4e4d\"", "buildId": "60148683633405433",
      "pinnedVersion": false, "cyberpunk": {"modsEnabled": true, "modsToLoad": ["a"]},
      "someFutureKey": 1 }, {"platform": "linux"} ] }"#;

    #[test]
    fn heroic_installed_roundtrip() {
        let f: InstalledFile = serde_json::from_str(HEROIC).unwrap();
        let recs = f.into_records();
        assert_eq!(recs.len(), 1);
        let r = &recs[0];
        assert_eq!(r.platform, InstallPlatform::Windows);
        assert!(r.with_dlcs());
        assert_eq!(r.cyberpunk.as_ref().unwrap().mods_to_load, vec!["a"]);
        let info = r.to_info();
        assert_eq!(info.build_id.as_deref(), Some("60148683633405433"));
        assert!(info.install_size_bytes > 75 * 1024 * 1024 * 1024);
        let back = serde_json::to_value(r).unwrap();
        assert_eq!(back["someFutureKey"], 1);
        assert_eq!(back["installedDLCs"][0], "1256837418");
        assert!(back.get("branch").is_none());
    }

    #[test]
    fn language_default_never_undefined() {
        let r = InstalledRecord {
            language: Some("undefined".into()),
            ..Default::default()
        };
        assert_eq!(r.language_or_default(), "en-US");
    }

    #[test]
    fn sums_dir_sizes() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("a/b")).unwrap();
        std::fs::write(d.path().join("a/b/x"), [0u8; 10]).unwrap();
        std::fs::write(d.path().join("y"), [0u8; 5]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(d.path().join("y"), d.path().join("z")).unwrap();
        assert_eq!(dir_size(d.path()), 15);
    }
}
