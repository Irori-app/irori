//! The Epic Games Launcher's install records (`*.item` files in
//! `ProgramData/Epic/EpicGamesLauncher/Data/Manifests`), for syncing
//! installs with an EGL in a Wine prefix.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde_json::{Map, Value, json};

/// The manifests folder of an EGL installed in `prefix`.
pub fn manifests_dir(prefix: &Path) -> PathBuf {
    prefix.join("drive_c/ProgramData/Epic/EpicGamesLauncher/Data/Manifests")
}

/// One EGL install record. Unknown fields are kept when rewriting.
#[derive(Debug, Clone, PartialEq)]
pub struct EglItem(pub Map<String, Value>);

impl EglItem {
    fn s(&self, k: &str) -> &str {
        self.0.get(k).and_then(Value::as_str).unwrap_or_default()
    }

    pub fn app_name(&self) -> &str {
        self.s("AppName")
    }

    pub fn main_game_app_name(&self) -> &str {
        self.s("MainGameAppName")
    }

    pub fn version(&self) -> &str {
        self.s("AppVersionString")
    }

    pub fn display_name(&self) -> &str {
        self.s("DisplayName")
    }

    /// `C:\Games\X` style path of the install.
    pub fn install_location(&self) -> &str {
        self.s("InstallLocation")
    }

    pub fn executable(&self) -> &str {
        self.s("LaunchExecutable")
    }

    pub fn launch_command(&self) -> &str {
        self.s("LaunchCommand")
    }

    pub fn installation_guid(&self) -> &str {
        self.s("InstallationGuid")
    }

    pub fn install_size(&self) -> u64 {
        self.0
            .get("InstallSize")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    pub fn can_run_offline(&self) -> bool {
        self.0
            .get("bCanRunOffline")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    pub fn requires_ownership_token(&self) -> bool {
        match self.0.get("OwnershipToken") {
            Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
            Some(Value::Bool(b)) => *b,
            _ => false,
        }
    }

    pub fn install_tags(&self) -> Vec<String> {
        self.0
            .get("InstallTags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Fields of a record written for an Irori install.
pub struct ExportInfo<'a> {
    pub app_name: &'a str,
    pub title: &'a str,
    pub version: &'a str,
    pub namespace: &'a str,
    pub catalog_item_id: &'a str,
    pub folder_name: &'a str,
    /// Windows path of the install (`Z:\home\…`).
    pub install_location: &'a str,
    pub install_size: u64,
    pub install_tags: &'a [String],
    pub installation_guid: &'a str,
    pub launch_command: &'a str,
    pub executable: &'a str,
    pub can_run_offline: bool,
    pub requires_ownership_token: bool,
    pub base_urls: &'a [String],
}

impl EglItem {
    pub fn from_export(e: &ExportInfo<'_>) -> Self {
        let v = json!({
            "AppCategories": ["public", "games", "applications"],
            "AppName": e.app_name,
            "AppVersionString": e.version,
            "BaseURLs": e.base_urls,
            "BuildLabel": "Live",
            "CatalogItemId": e.catalog_item_id,
            "CatalogNamespace": e.namespace,
            "ChunkDbs": [],
            "CompatibleApps": [],
            "DisplayName": e.title,
            "FormatVersion": 0,
            "FullAppName": "",
            "HostInstallationGuid": "",
            "InstallComponents": [],
            "InstallLocation": e.install_location,
            "InstallSessionId": "",
            "InstallSize": e.install_size,
            "InstallTags": e.install_tags,
            "InstallationGuid": e.installation_guid,
            "LaunchCommand": e.launch_command,
            "LaunchExecutable": e.executable,
            "MainGameAppName": e.app_name,
            "MainWindowProcessName": "",
            "MandatoryAppFolderName": e.folder_name,
            "ManifestLocation": format!("{}/.egstore", e.install_location),
            "OwnershipToken": if e.requires_ownership_token { "true" } else { "false" },
            "PrereqIds": [],
            "ProcessNames": [],
            "StagingLocation": format!("{}/.egstore/bps", e.install_location),
            "TechnicalType": "",
            "VaultThumbnailUrl": "",
            "VaultTitleText": "",
            "bCanRunOffline": e.can_run_offline,
            "bIsApplication": true,
            "bIsExecutable": true,
            "bIsIncompleteInstall": false,
            "bIsManaged": false,
            "bNeedsValidation": false,
            "bRequiresAuth": true
        });
        match v {
            Value::Object(m) => Self(m),
            _ => unreachable!("a JSON object literal"),
        }
    }
}

/// Every record in `dir`.
pub fn read_items(dir: &Path) -> Vec<EglItem> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "item"))
        .filter_map(|e| {
            let bytes = std::fs::read(e.path()).ok()?;
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Object(m)) => Some(EglItem(m)),
                _ => {
                    tracing::warn!("ignoring unreadable EGL record {}", e.path().display());
                    None
                }
            }
        })
        .filter(|i| !i.app_name().is_empty())
        .collect()
}

/// Write `item` as `<InstallationGuid>.item`.
pub fn write_item(dir: &Path, item: &EglItem) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.item", item.installation_guid()));
    let text = serde_json::to_string_pretty(&item.0)?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}

/// Delete the record of an installation.
pub fn delete_item(dir: &Path, installation_guid: &str) -> Result<()> {
    let path = dir.join(format!("{installation_guid}.item"));
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("deleting {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// The `.egstore` folder EGL keeps in an install (`<guid>.manifest` and
/// `<guid>.mancpn`).
pub fn egstore_dir(install_dir: &Path) -> PathBuf {
    install_dir.join(".egstore")
}

/// Write the manifest and `.mancpn` EGL expects inside an install.
pub fn write_egstore(
    install_dir: &Path,
    guid: &str,
    manifest: &[u8],
    app_name: &str,
    namespace: &str,
    catalog_item_id: &str,
) -> Result<()> {
    let dir = egstore_dir(install_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{guid}.manifest")), manifest)?;
    let mancpn = json!({
        "AppName": app_name,
        "CatalogItemId": catalog_item_id,
        "CatalogNamespace": namespace,
        "FormatVersion": 0
    });
    std::fs::write(
        dir.join(format!("{guid}.mancpn")),
        serde_json::to_string_pretty(&mancpn)?,
    )?;
    Ok(())
}

/// A new installation GUID (32 upper-case hex digits).
pub fn new_installation_guid() -> String {
    uuid::Uuid::new_v4().simple().to_string().to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_records() {
        let dir = tempfile::tempdir().unwrap();
        let tags = vec!["de".to_string()];
        let item = EglItem::from_export(&ExportInfo {
            app_name: "Sugar",
            title: "Sugar Game",
            version: "1.0",
            namespace: "ns",
            catalog_item_id: "cat",
            folder_name: "Sugar",
            install_location: "Z:/games/Sugar",
            install_size: 10,
            install_tags: &tags,
            installation_guid: "ABCD",
            launch_command: "-x",
            executable: "Game.exe",
            can_run_offline: true,
            requires_ownership_token: true,
            base_urls: &[],
        });
        write_item(dir.path(), &item).unwrap();
        std::fs::write(dir.path().join("bad.item"), b"{").unwrap();
        let items = read_items(dir.path());
        assert_eq!(items.len(), 1);
        let i = &items[0];
        assert_eq!(
            (i.app_name(), i.version(), i.installation_guid()),
            ("Sugar", "1.0", "ABCD")
        );
        assert_eq!(i.install_tags(), tags);
        assert!(i.requires_ownership_token() && i.can_run_offline());
        assert_eq!(i.install_size(), 10);
        delete_item(dir.path(), "ABCD").unwrap();
        delete_item(dir.path(), "ABCD").unwrap();
        assert!(read_items(dir.path()).is_empty());
        assert_eq!(new_installation_guid().len(), 32);
        write_egstore(dir.path(), "G", b"m", "Sugar", "ns", "cat").unwrap();
        assert!(dir.path().join(".egstore/G.mancpn").is_file());
    }
}
