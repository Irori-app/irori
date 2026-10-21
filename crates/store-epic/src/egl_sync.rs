//! Keeping installs in sync with an Epic Games Launcher (EGL) running in a
//! Wine prefix: games installed by EGL show up in Ember and Ember's installs
//! show up in EGL.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ember_epic_api::egl::{self, EglItem, ExportInfo};

use crate::EpicStore;
use crate::state::{InstalledGame, real_prefix};

/// Host path of a Windows path from EGL (`Z:\home\…`, `C:\Games\…`).
fn host_path(prefix: &Path, win: &str) -> Option<PathBuf> {
    if win.starts_with('/') {
        return Some(PathBuf::from(win));
    }
    let unified = win.replace('\\', "/");
    let (drive, rest) = unified.split_once(':')?;
    if drive.eq_ignore_ascii_case("z") {
        return Some(PathBuf::from(rest));
    }
    let mapped = prefix
        .join("dosdevices")
        .join(format!("{}:", drive.to_lowercase()));
    let base = std::fs::canonicalize(&mapped).ok()?;
    let mut out = base;
    out.extend(rest.split('/').filter(|p| !p.is_empty()));
    Some(out)
}

/// Windows path of a host path, through the `Z:` drive.
fn windows_path(host: &str) -> String {
    format!("Z:{}", host.replace('/', "\\"))
}

/// Remove an install's EGL record (and `.egstore` when its files go).
pub(crate) fn forget_export(prefix: &Path, installed: &InstalledGame, delete_files: bool) {
    let Some(pfx) = real_prefix(prefix) else {
        return;
    };
    if let Err(e) = egl::delete_item(&egl::manifests_dir(&pfx), &installed.egl_guid) {
        tracing::warn!("removing the EGL record of {}: {e:#}", installed.app_name);
    }
    if delete_files {
        let _ = std::fs::remove_dir_all(egl::egstore_dir(Path::new(&installed.install_path)));
    }
}

impl EpicStore {
    /// The linked EGL prefix, if sync is enabled.
    pub(crate) fn egl_prefix(&self) -> Option<PathBuf> {
        let linked = (self.settings)().egs_linked_path.clone();
        (!linked.trim().is_empty()).then(|| ember_core::paths::expand_home(linked.trim()))
    }

    /// Link with an Epic Games Launcher in a Wine prefix (`Some(prefix)`) and
    /// sync once, or unlink (`None`). Returns a summary.
    pub async fn egl_sync(&self, egl_prefix: Option<&Path>) -> Result<String> {
        match egl_prefix {
            Some(prefix) => self.egl_sync_with(prefix).await,
            None => {
                self.update_installed(|map| {
                    for g in map.values_mut() {
                        g.egl_guid.clear();
                    }
                })
                .await?;
                Ok("Unlinked from the Epic Games Launcher. Games stay installed in both.".into())
            }
        }
    }

    /// Import EGL's installs, export Ember's, and drop installs EGL removed.
    pub(crate) async fn egl_sync_with(&self, prefix: &Path) -> Result<String> {
        let pfx = real_prefix(prefix).unwrap_or_else(|| prefix.to_path_buf());
        let dir = egl::manifests_dir(&pfx);
        if !dir.is_dir() {
            let data = dir.parent().map(Path::to_path_buf).unwrap_or_default();
            if !data.is_dir() {
                bail!(
                    "No Epic Games Launcher data in {} (wrong folder, or EGL was never started there)",
                    prefix.display()
                );
            }
            std::fs::create_dir_all(&dir)?;
        }
        let items = {
            let dir = dir.clone();
            Self::blocking(move || egl::read_items(&dir)).await?
        };
        let mut imported = Vec::new();
        for item in items
            .iter()
            .filter(|i| i.app_name() == i.main_game_app_name())
        {
            let owned = self
                .record(item.app_name())
                .is_some_and(|r| r.asset_infos.contains_key("Windows"));
            if !owned {
                continue;
            }
            let current = self.installed_entry(item.app_name());
            let changed = current.as_ref().is_none_or(|g| {
                g.version != item.version() || g.install_tags != item.install_tags()
            });
            if changed {
                match self.egl_import(&pfx, item).await {
                    Ok(()) => imported.push(item.display_name().to_string()),
                    Err(e) => tracing::warn!("importing {} from EGL: {e:#}", item.app_name()),
                }
            }
        }

        let known: std::collections::HashSet<String> =
            items.iter().map(|i| i.app_name().to_string()).collect();
        let installs: Vec<InstalledGame> = self.state.read().installed.values().cloned().collect();
        let mut exported = Vec::new();
        for g in installs {
            if g.third_party || g.is_dlc || !g.platform.starts_with("Win") {
                continue;
            }
            if known.contains(&g.app_name) {
                continue;
            }
            let exe = Path::new(&g.install_path).join(g.executable.trim_start_matches('/'));
            if !g.egl_guid.is_empty() && !exe.exists() {
                // Uninstalled through EGL.
                self.update_installed(|map| {
                    map.remove(&g.app_name);
                })
                .await?;
                continue;
            }
            match self.egl_export(prefix, &g.app_name).await {
                Ok(()) => exported.push(g.title.clone()),
                Err(e) => tracing::warn!("exporting {} to EGL: {e:#}", g.app_name),
            }
        }
        Ok(format!(
            "Imported from the Epic Games Launcher: {}. Exported: {}.",
            if imported.is_empty() {
                "nothing".into()
            } else {
                imported.join(", ")
            },
            if exported.is_empty() {
                "nothing".into()
            } else {
                exported.join(", ")
            },
        ))
    }

    async fn egl_import(&self, pfx: &Path, item: &EglItem) -> Result<()> {
        let path = host_path(pfx, item.install_location())
            .with_context(|| format!("cannot resolve {}", item.install_location()))?;
        let manifest =
            egl::egstore_dir(&path).join(format!("{}.manifest", item.installation_guid()));
        let bytes = std::fs::read(&manifest)
            .with_context(|| format!("{} is missing", manifest.display()))?;
        ember_epic_api::Manifest::parse(&bytes, &Default::default())
            .context("the EGL manifest is unreadable")?;
        ember_core::json_store::write_bytes(&self.layout.build_manifest(item.app_name()), &bytes)?;
        let previous = self.installed_entry(item.app_name()).unwrap_or_default();
        let entry = InstalledGame {
            app_name: item.app_name().to_string(),
            title: item.display_name().to_string(),
            version: item.version().to_string(),
            platform: "Windows".into(),
            install_path: path.to_string_lossy().into_owned(),
            executable: item.executable().to_string(),
            launch_parameters: item.launch_command().to_string(),
            install_size: item.install_size(),
            install_tags: item.install_tags(),
            can_run_offline: item.can_run_offline(),
            requires_ot: item.requires_ownership_token(),
            egl_guid: item.installation_guid().to_string(),
            save_path: previous.save_path,
            save_timestamp: previous.save_timestamp,
            prereq: previous.prereq,
            ..Default::default()
        };
        let app = item.app_name().to_string();
        self.update_installed(|map| {
            map.insert(app, entry);
        })
        .await
    }

    /// Write EGL's record (and `.egstore`) for an Ember install.
    pub(crate) async fn egl_export(&self, prefix: &Path, app: &str) -> Result<()> {
        let pfx = real_prefix(prefix).unwrap_or_else(|| prefix.to_path_buf());
        let g = self.installed_entry(app).context("not installed")?;
        let record = self.require_record(app)?;
        let bytes = std::fs::read(self.layout.build_manifest(app))
            .context("the build's manifest is missing")?;
        let guid = if g.egl_guid.is_empty() {
            egl::new_installation_guid()
        } else {
            g.egl_guid.clone()
        };
        let install_dir = PathBuf::from(&g.install_path);
        egl::write_egstore(
            &install_dir,
            &guid,
            &bytes,
            app,
            record.namespace(),
            record.catalog_item_id(),
        )?;
        let location = windows_path(&g.install_path);
        let item = EglItem::from_export(&ExportInfo {
            app_name: app,
            title: &g.title,
            version: &g.version,
            namespace: record.namespace(),
            catalog_item_id: record.catalog_item_id(),
            folder_name: record.folder_name(),
            install_location: &location,
            install_size: g.install_size,
            install_tags: &g.install_tags,
            installation_guid: &guid,
            launch_command: &g.launch_parameters,
            executable: &g.executable,
            can_run_offline: g.can_run_offline,
            requires_ownership_token: g.requires_ot,
            base_urls: &[],
        });
        egl::write_item(&egl::manifests_dir(&pfx), &item)?;
        if g.egl_guid != guid {
            self.update_installed(|map| {
                if let Some(e) = map.get_mut(app) {
                    e.egl_guid = guid;
                }
            })
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_paths() {
        let dir = tempfile::tempdir().unwrap();
        let pfx = dir.path();
        std::fs::create_dir_all(pfx.join("drive_c/Games")).unwrap();
        std::fs::create_dir_all(pfx.join("dosdevices")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../drive_c", pfx.join("dosdevices/c:")).unwrap();
        assert_eq!(
            host_path(pfx, r"Z:\home\u\Games\X"),
            Some(PathBuf::from("/home/u/Games/X"))
        );
        #[cfg(unix)]
        assert_eq!(
            host_path(pfx, r"C:\Games\X"),
            Some(
                std::fs::canonicalize(pfx.join("drive_c"))
                    .unwrap()
                    .join("Games/X")
            )
        );
        assert_eq!(host_path(pfx, r"D:\X"), None);
        assert_eq!(windows_path("/home/u/G"), r"Z:\home\u\G");
    }
}
