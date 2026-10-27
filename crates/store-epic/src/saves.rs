//! Cloud saves.

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use irori_core::{OpContext, SaveConflict, Store as _, SyncDirection};
use irori_epic_api::saves::{self, Filters, SaveLocation, SaveStatus};

use crate::EpicStore;
use crate::state::real_prefix;

/// What a sync does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Download,
    Upload,
    /// Both sides changed; only the user can say which to keep.
    Conflict,
    Nothing,
}

/// Choose the action for a save `status` and the requested `direction`. A
/// conflict stays a conflict unless the direction is forced: syncing one
/// way, even the way the caller asked, would overwrite the other side.
pub(crate) fn decide(status: SaveStatus, direction: SyncDirection) -> Action {
    use SaveStatus as S;
    use SyncDirection as D;
    match (direction, status) {
        (D::ForceDownload, _) => Action::Download,
        (D::ForceUpload, _) => Action::Upload,
        (_, S::Conflict) => Action::Conflict,
        (_, S::NoSave | S::SameAge) => Action::Nothing,
        (D::Download, S::RemoteNewer) | (D::Auto, S::RemoteNewer) => Action::Download,
        (D::Upload, S::LocalNewer) | (D::Auto, S::LocalNewer) => Action::Upload,
        _ => Action::Nothing,
    }
}

impl EpicStore {
    /// Compute (and remember) the save folder of an installed game.
    pub(crate) async fn resolve_save_path(
        &self,
        app: &str,
        prefix: Option<&Path>,
    ) -> Result<String> {
        let (game, installed) = self.require_installed(app)?;
        let record = self.require_record(app)?;
        let template = record
            .cloud_save_folder(installed.platform_arg())
            .with_context(|| format!("{} does not support cloud saves", game.title))?
            .to_string();
        let wine_prefix = if installed.install_platform().needs_wine() {
            let prefix = prefix.context("A Wine prefix is needed to find the save folder")?;
            Some(real_prefix(prefix).with_context(|| {
                format!(
                    "The Wine prefix {} is not set up yet. Launch the game once, then try again.",
                    prefix.display()
                )
            })?)
        } else {
            None
        };
        let account = self.user().map(|u| u.account_id).unwrap_or_default();
        let install_dir = std::path::PathBuf::from(&installed.install_path);
        let platform = installed.platform_arg().to_string();
        let path = Self::blocking(move || {
            saves::resolve_save_path(&SaveLocation {
                template: &template,
                install_dir: &install_dir,
                account_id: &account,
                wine_prefix: wine_prefix.as_deref(),
                platform: &platform,
            })
        })
        .await??
        .to_string_lossy()
        .into_owned();
        let saved = path.clone();
        self.update_installed(|map| {
            if let Some(g) = map.get_mut(app) {
                g.save_path = Some(saved);
            }
        })
        .await?;
        Ok(path)
    }

    pub(crate) async fn sync_cloud_saves(
        &self,
        app: &str,
        direction: SyncDirection,
        save_path: &str,
        ctx: &OpContext,
    ) -> Result<String> {
        if save_path.is_empty() {
            bail!("No path provided.");
        }
        let (game, installed) = self.require_installed(app)?;
        let record = self.require_record(app)?;
        let session = self.session().await?;
        let client = self.client().await?;
        let dir = std::path::PathBuf::from(save_path);
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("Could not create {save_path}"))?;
        let (include, exclude) = record.cloud_filters();
        let filters = Filters { include, exclude };

        (ctx.step)("Checking cloud saves");
        let remote = saves::remote_saves(&client, &session, app).await?;
        let latest = remote.last();
        let local = {
            let dir = dir.clone();
            Self::blocking(move || saves::newest_mtime(&dir)).await?
        };
        let status = saves::status(local, latest.map(|s| s.time), installed.save_timestamp);
        let action = decide(status, direction);
        let mut log = format!("{}: {status:?}\n", game.title);
        match action {
            Action::Conflict => {
                return Err(SaveConflict {
                    local: (local > 0).then_some(local),
                    cloud: latest.map(|s| s.time),
                }
                .into());
            }
            Action::Nothing => log.push_str("Saves are up to date.\n"),
            Action::Download => {
                let save = latest.context("There is no cloud save to download")?;
                (ctx.step)("Downloading cloud save");
                saves::download(&client, &session, save, &dir, &filters).await?;
                log.push_str(&format!("Downloaded cloud save {}.\n", save.name));
            }
            Action::Upload => {
                (ctx.step)("Uploading save");
                let account = session.account_id.clone();
                let folder = record
                    .cloud_save_folder("Windows")
                    .unwrap_or_default()
                    .to_string();
                let mac = record.cloud_save_folder("Mac").map(str::to_string);
                let app_name = app.to_string();
                let (src, f) = (dir.clone(), filters.clone());
                let time = if local > 0 { local } else { saves::now() };
                let pkg = Self::blocking(move || {
                    saves::package(
                        &src,
                        &app_name,
                        &account,
                        (&folder, mac.as_deref()),
                        &f,
                        time,
                    )
                })
                .await??;
                match pkg {
                    Some(pkg) => {
                        let name = pkg.manifest_name.clone();
                        saves::upload(&client, &session, app, pkg).await?;
                        log.push_str(&format!("Uploaded save {name}.\n"));
                    }
                    None => log.push_str("No save files to upload.\n"),
                }
            }
        }
        if action != Action::Nothing {
            let now = saves::now();
            let path = save_path.to_string();
            self.update_installed(|map| {
                if let Some(g) = map.get_mut(app) {
                    g.save_timestamp = Some(now);
                    g.save_path = Some(path);
                }
            })
            .await?;
        }
        Ok(log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decides_sync_actions() {
        use SaveStatus as S;
        use SyncDirection as D;
        assert_eq!(decide(S::RemoteNewer, D::Auto), Action::Download);
        assert_eq!(decide(S::LocalNewer, D::Auto), Action::Upload);
        assert_eq!(decide(S::SameAge, D::Auto), Action::Nothing);
        assert_eq!(decide(S::Conflict, D::Auto), Action::Conflict);
        assert_eq!(decide(S::LocalNewer, D::Download), Action::Nothing);
        assert_eq!(decide(S::RemoteNewer, D::Upload), Action::Nothing);
        // Before a launch (download) or after it (upload), a conflict must
        // not be skipped quietly.
        assert_eq!(decide(S::Conflict, D::Download), Action::Conflict);
        assert_eq!(decide(S::Conflict, D::Upload), Action::Conflict);
        assert_eq!(decide(S::Conflict, D::ForceUpload), Action::Upload);
        assert_eq!(decide(S::NoSave, D::ForceDownload), Action::Download);
    }
}
