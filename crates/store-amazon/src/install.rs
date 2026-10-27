//! Install, update, repair and uninstall through the depot engine.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use irori_amazon_api::{Entitlement, FetchedManifest, GameDownload, Manifest};
use irori_core::{OpContext, Outcome};
use irori_depot::{FileEntry, Finished, Job, Verified};

use crate::AmazonStore;
use crate::mapping;
use crate::state::InstalledGame;

/// Append a line to the operation's log file.
pub(crate) fn log_line(ctx: &OpContext, line: &str) {
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
        let _ = writeln!(f, "[amazon] {line}");
    }
}

fn progress_sink(ctx: &OpContext) -> irori_depot::ProgressFn {
    let sink = ctx.progress.clone();
    Arc::new(move |p| sink(mapping::progress(p)))
}

/// What a deployment does with the files already on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Fresh install (intact files from an interrupted run are kept).
    Install,
    /// Fetch only what changed since the installed build.
    Update,
    /// Hash everything and fetch what is missing or damaged.
    Repair,
}

impl AmazonStore {
    fn depot_options(&self, app_name: &str) -> irori_depot::Options {
        let settings = (self.settings)();
        irori_depot::Options {
            workers: settings.max_workers as usize,
            max_bytes_per_sec: u64::from(settings.max_download_speed) * 1024,
            journal: Some(self.journal_file(app_name)),
            ..Default::default()
        }
    }

    /// Current build of a game: where it is served and its manifest.
    pub(crate) async fn current_build(
        &self,
        ent: &Entitlement,
    ) -> Result<(GameDownload, FetchedManifest)> {
        let creds = self.credentials().await?;
        let client = self.client()?;
        let download = client.game_download(&creds, &ent.id).await?;
        let manifest = client.manifest(&download).await?;
        Ok((download, manifest))
    }

    fn installed_manifest(&self, app_name: &str) -> Option<Manifest> {
        let bytes = self.layout.read_manifest(app_name)?;
        self.client()
            .and_then(|c| c.parse_manifest(&bytes))
            .map_err(|e| tracing::warn!("stored manifest of {app_name}: {e:#}"))
            .ok()
    }

    /// Bring `dir` to the current build of `app_name`.
    pub(crate) async fn deploy(
        &self,
        app_name: &str,
        dir: &Path,
        mode: Mode,
        ctx: &OpContext,
    ) -> Result<Outcome> {
        match self.deploy_inner(app_name, dir, mode, ctx).await {
            Ok(outcome) => Ok(outcome),
            Err(_) if ctx.cancel.is_cancelled() => Ok(Outcome::Abort),
            Err(e) => {
                log_line(ctx, &format!("failed: {e:#}"));
                Ok(Outcome::Error(format!("{e:#}")))
            }
        }
    }

    async fn deploy_inner(
        &self,
        app_name: &str,
        dir: &Path,
        mode: Mode,
        ctx: &OpContext,
    ) -> Result<Outcome> {
        let ent = self.require_entitlement(app_name)?;
        (ctx.step)("Fetching file list");
        let (download, fetched) = self.current_build(&ent).await?;
        log_line(
            ctx,
            &format!(
                "{mode:?} {app_name} build {} into {}",
                download.version_id,
                dir.display()
            ),
        );
        let entries = Manifest::entries(&fetched.manifest.files, &download.download_url)?;
        let old = match mode {
            Mode::Install => None,
            Mode::Update | Mode::Repair => self.installed_manifest(app_name),
        };
        let removed = old
            .as_ref()
            .map(|old| {
                irori_depot::diff(
                    old.files.iter().map(|f| (f.path.as_str(), Some(&f.hash))),
                    &entries,
                )
            })
            .map(|d| (d.changed, d.removed));

        let (files, remove): (Vec<FileEntry>, Vec<String>) = match (mode, removed) {
            (Mode::Update, Some((changed, removed))) => (changed, removed),
            (Mode::Repair, removed) => {
                (ctx.step)("Verifying files");
                let verified =
                    irori_depot::verify(dir, &entries, 0, progress_sink(ctx), &ctx.cancel).await?;
                let broken = match verified {
                    Verified::Cancelled => return Ok(Outcome::Abort),
                    Verified::Checked(bad) => bad,
                };
                log_line(ctx, &format!("{} files need repair", broken.len()));
                let files = broken.iter().map(|(i, _)| entries[*i].clone()).collect();
                (files, removed.map(|(_, r)| r).unwrap_or_default())
            }
            (_, _) => (entries, Vec::new()),
        };

        (ctx.step)("Downloading");
        let client = AmazonStore::download_client()?;
        let finished = irori_depot::download(
            &client,
            Job {
                root: dir,
                files: &files,
                remove: &remove,
            },
            &self.depot_options(app_name),
            progress_sink(ctx),
            &ctx.cancel,
        )
        .await?;
        if finished == Finished::Cancelled {
            log_line(ctx, "canceled");
            return Ok(Outcome::Abort);
        }

        self.layout.write_manifest(app_name, &fetched.bytes)?;
        let record = InstalledGame {
            version: download.version_id,
            path: dir.to_path_buf(),
            size: fetched.manifest.size(),
        };
        self.update_installed(|installed| {
            installed.insert(app_name.to_string(), record);
        })
        .await?;
        self.forget_install_info(app_name).await;
        log_line(ctx, "done");
        Ok(Outcome::Done)
    }

    /// Delete the files of an install: those its manifest lists (user files
    /// such as saves stay), then folders left empty. Without a manifest the
    /// whole folder goes.
    pub(crate) async fn delete_install_files(&self, app_name: &str, dir: &Path) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        let Some(manifest) = self.installed_manifest(app_name) else {
            irori_core::fs_guard::ensure_deletable(dir, &(self.settings)())?;
            return tokio::fs::remove_dir_all(dir)
                .await
                .with_context(|| format!("Could not delete {}", dir.display()));
        };
        let dir = dir.to_path_buf();
        tokio::task::spawn_blocking(move || remove_listed(&dir, &manifest)).await?
    }

    pub(crate) fn journal_file(&self, app_name: &str) -> PathBuf {
        self.cache_dir
            .join("resume")
            .join(format!("{}.jsonl", irori_core::paths::sanitize(app_name)))
    }
}

fn remove_listed(dir: &Path, manifest: &Manifest) -> Result<()> {
    for f in &manifest.files {
        let p = irori_depot::path::join(dir, &f.path);
        for p in [irori_depot::path::part_path(&p), p] {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => bail!("Could not delete {}: {e}", p.display()),
            }
        }
    }
    remove_empty_dirs(dir);
    Ok(())
}

/// Remove empty folders below and including `dir`.
fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                remove_empty_dirs(&e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_listed_files_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Game");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("saves")).unwrap();
        std::fs::write(dir.join("bin/game.exe"), b"x").unwrap();
        std::fs::write(dir.join("bin/data.pak.irori-part"), b"x").unwrap();
        std::fs::write(dir.join("saves/slot1"), b"x").unwrap();
        let manifest = Manifest {
            files: vec![
                irori_amazon_api::ManifestFile {
                    path: "bin/game.exe".into(),
                    size: 1,
                    hash: irori_depot::Digest::new(irori_depot::Algorithm::Sha256, vec![0; 32]),
                },
                irori_amazon_api::ManifestFile {
                    path: "bin/data.pak".into(),
                    size: 1,
                    hash: irori_depot::Digest::new(irori_depot::Algorithm::Sha256, vec![0; 32]),
                },
            ],
        };
        remove_listed(&dir, &manifest).unwrap();
        assert!(!dir.join("bin").exists());
        assert!(dir.join("saves/slot1").exists());
    }
}
