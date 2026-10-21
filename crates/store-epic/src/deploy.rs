//! Install, update, repair and import through the depot engine.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use ember_core::{InstallPlatform, OpContext, Outcome};
use ember_depot::{Finished, Job, Verified};
use ember_epic_api::install::{ChunkSigner, EntryBuilder, Mode as PlanMode, PlanInput, plan};
use ember_epic_api::{Manifest, Secrets};

use crate::state::InstalledGame;
use crate::{EpicStore, FetchedBuild, install_info};

/// A planned download and what to record once it finishes.
struct Download<'a> {
    app: &'a str,
    platform: &'a str,
    asset: &'a ember_epic_api::Asset,
    use_signed_url: bool,
    dir: &'a Path,
    files: (Vec<ember_depot::FileEntry>, Vec<String>),
    target: &'a Manifest,
    target_bytes: Vec<u8>,
    secrets: Secrets,
    tags: &'a [String],
    ctx: &'a OpContext,
}

/// Epic's platform name for an install platform.
pub(crate) fn platform_arg(platform: InstallPlatform) -> Result<&'static str> {
    match platform {
        InstallPlatform::Windows => Ok("Windows"),
        InstallPlatform::Mac => Ok("Mac"),
        other => bail!("Epic Games does not offer {} builds", other.label()),
    }
}

/// What a deployment does.
#[derive(Debug, Clone)]
pub(crate) enum Mode {
    /// Install into `base/<folder>` (or the existing install folder).
    Install { base: PathBuf, tags: Vec<String> },
    /// Fetch what changed since the installed build.
    Update,
    /// Hash the installed build and fetch what is missing or damaged.
    Repair,
}

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
        let _ = writeln!(f, "[epic] {line}");
    }
}

fn progress_sink(ctx: &OpContext) -> ember_depot::ProgressFn {
    let sink = ctx.progress.clone();
    Arc::new(move |p: ember_depot::Progress| {
        sink(ember_core::InstallProgress {
            percent: Some(p.percent.clamp(0.0, 100.0)),
            bytes_done: Some(p.bytes_done),
            bytes_total: Some(p.bytes_total),
            eta_secs: p.eta_secs,
            download_speed: Some(p.download_speed),
            disk_speed: Some(p.disk_speed),
        })
    })
}

/// Delete the files of `manifest` selected by `tags` below `dir`, then the
/// folders left empty (and `dir` itself unless `keep_root`).
pub(crate) fn delete_listed(
    dir: &Path,
    manifest: &Manifest,
    tags: &[String],
    keep_root: bool,
) -> Result<()> {
    for f in manifest.files.iter().filter(|f| f.selected_by(tags)) {
        let Ok(rel) = ember_depot::path::normalize(&f.filename) else {
            continue;
        };
        let p = ember_depot::path::join(dir, &rel);
        for p in [ember_depot::path::part_path(&p), p] {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => bail!("Could not delete {}: {e}", p.display()),
            }
        }
    }
    remove_empty_dirs(dir, keep_root);
    Ok(())
}

fn remove_empty_dirs(dir: &Path, keep_root: bool) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                remove_empty_dirs(&e.path(), false);
            }
        }
    }
    if !keep_root {
        let _ = std::fs::remove_dir(dir);
    }
}

impl EpicStore {
    fn depot_options(&self, app: &str) -> ember_depot::Options {
        let settings = (self.settings)();
        ember_depot::Options {
            workers: settings.max_workers as usize,
            max_bytes_per_sec: u64::from(settings.max_download_speed) * 1024,
            journal: Some(self.layout.resume_file(app)),
            ..Default::default()
        }
    }

    /// Chunk folders of a build, honouring the plain-HTTP setting (for
    /// local caching proxies).
    fn base_urls(&self, build: &FetchedBuild) -> Vec<String> {
        let plain = (self.settings)().download_no_https;
        build
            .info
            .base_urls
            .iter()
            .map(|u| {
                if plain {
                    u.replacen("https://", "http://", 1)
                } else {
                    u.clone()
                }
            })
            .collect()
    }

    /// Bring `app` to the current build. Errors become [`Outcome::Error`].
    pub(crate) async fn deploy(
        &self,
        app: &str,
        platform: &str,
        mode: Mode,
        ctx: &OpContext,
    ) -> Result<Outcome> {
        let result = self.deploy_inner(app, platform, mode, ctx).await;
        match result {
            Ok(outcome) => Ok(outcome),
            Err(_) if ctx.cancel.is_cancelled() => Ok(Outcome::Abort),
            Err(e) => {
                log_line(ctx, &format!("failed: {e:#}"));
                Ok(Outcome::Error(format!("{e:#}")))
            }
        }
    }

    /// Where a new install of `app` goes.
    fn install_dir(&self, app: &str, base: &Path) -> Result<PathBuf> {
        if let Some(existing) = self
            .installed_entry(app)
            .filter(|g| !g.install_path.is_empty())
        {
            return Ok(PathBuf::from(existing.install_path));
        }
        let record = self.require_record(app)?;
        if record.is_dlc() {
            let base_app = record
                .main_game_app()
                .context("The DLC's base game is unknown")?;
            let base_game = self
                .installed_entry(base_app)
                .with_context(|| format!("Install the base game of {} first", record.app_title))?;
            return Ok(PathBuf::from(base_game.install_path));
        }
        let folder = record.folder_name().trim().to_string();
        Ok(base.join(ember_core::paths::sanitize(&folder)))
    }

    async fn deploy_inner(
        &self,
        app: &str,
        platform: &str,
        mode: Mode,
        ctx: &OpContext,
    ) -> Result<Outcome> {
        let record = self.require_record(app)?;
        let asset = record
            .asset_infos
            .get(platform)
            .cloned()
            .with_context(|| format!("{} has no {platform} build", record.app_title))?;
        let installed = self.installed_entry(app);
        let (dir, tags) = match &mode {
            Mode::Install { base, tags } => (self.install_dir(app, base)?, tags.clone()),
            Mode::Update | Mode::Repair => {
                let g = installed.as_ref().context("The game is not installed")?;
                (PathBuf::from(&g.install_path), g.install_tags.clone())
            }
        };
        (ctx.step)("Fetching file list");
        if matches!(mode, Mode::Update) {
            // Always plan updates against the newest build.
            self.builds.lock().remove(&format!("{app}:{platform}"));
        }
        let build = self.fetch_build(app, platform).await?;
        let old = match (&mode, &installed) {
            (Mode::Update | Mode::Repair, Some(g)) => self.installed_manifest(g).await,
            _ => None,
        };
        log_line(
            ctx,
            &format!(
                "{mode:?} {app} build {} into {}",
                build.manifest.meta.build_version,
                dir.display()
            ),
        );
        let base_urls = self.base_urls(&build);
        let build_secrets = build.info.secrets.clone();

        // The build to end up with (repairs keep the installed one) and the
        // files to fetch.
        let (target, target_bytes, secrets, files) = match (&mode, old) {
            (Mode::Repair, old) => {
                let (target, bytes, secrets) = match (old, &installed) {
                    (Some(old), Some(g)) => (
                        old,
                        std::fs::read(self.layout.build_manifest(app)).unwrap_or_default(),
                        g.secrets(),
                    ),
                    _ => (build.manifest.clone(), build.bytes.clone(), build_secrets),
                };
                let Some(broken) = self
                    .broken_files(&target, &secrets, &base_urls, &tags, &dir, ctx)
                    .await?
                else {
                    return Ok(Outcome::Abort);
                };
                (target, bytes, secrets, (broken, Vec::new()))
            }
            (Mode::Update, Some(old)) => {
                let mut new = build.manifest.clone();
                let (old_id, new_id) = (old.meta.build_id(), new.meta.build_id());
                if let Some(base) = base_urls.first()
                    && let Some(delta) = self
                        .client()
                        .await?
                        .delta_manifest(base, &old_id, &new_id)
                        .await
                {
                    match Manifest::parse(&delta, &build_secrets) {
                        Ok(d) => {
                            log_line(ctx, "using the optimised delta manifest");
                            new.apply_delta(d);
                        }
                        Err(e) => tracing::debug!("ignoring the delta manifest: {e:#}"),
                    }
                }
                let files =
                    self.plan_files(&new, Some(&old), &base_urls, &build_secrets, &tags, &dir)?;
                (new, build.bytes.clone(), build_secrets, files)
            }
            _ => {
                let files = self.plan_files(
                    &build.manifest,
                    None,
                    &base_urls,
                    &build_secrets,
                    &tags,
                    &dir,
                )?;
                (
                    build.manifest.clone(),
                    build.bytes.clone(),
                    build_secrets,
                    files,
                )
            }
        };
        self.run_download(Download {
            app,
            platform,
            asset: &asset,
            use_signed_url: build.info.use_signed_url,
            dir: &dir,
            files,
            target: &target,
            target_bytes,
            secrets,
            tags: &tags,
            ctx,
        })
        .await
    }

    /// Entries of the installed files that are missing or damaged; `None`
    /// when cancelled.
    async fn broken_files(
        &self,
        manifest: &Manifest,
        secrets: &Secrets,
        base_urls: &[String],
        tags: &[String],
        dir: &Path,
        ctx: &OpContext,
    ) -> Result<Option<Vec<ember_depot::FileEntry>>> {
        let builder = EntryBuilder::new(manifest, base_urls, Arc::new(secrets.clone()));
        let entries = builder.all(tags)?;
        (ctx.step)("Verifying files");
        let verified =
            ember_depot::verify(dir, &entries, 0, progress_sink(ctx), &ctx.cancel).await?;
        let broken = match verified {
            Verified::Cancelled => return Ok(None),
            Verified::Checked(bad) => bad,
        };
        log_line(ctx, &format!("{} files need repair", broken.len()));
        Ok(Some(
            broken.iter().map(|(i, _)| entries[*i].clone()).collect(),
        ))
    }

    /// The files to fetch and paths to delete.
    fn plan_files(
        &self,
        manifest: &Manifest,
        old: Option<&Manifest>,
        base_urls: &[String],
        secrets: &Secrets,
        tags: &[String],
        dir: &Path,
    ) -> Result<(Vec<ember_depot::FileEntry>, Vec<String>)> {
        let exists = |rel: &str| {
            ember_depot::path::normalize(rel)
                .map(|r| ember_depot::path::join(dir, &r).is_file())
                .unwrap_or(false)
        };
        let p = plan(
            &PlanInput {
                manifest,
                old,
                base_urls,
                secrets: Arc::new(secrets.clone()),
                install_tags: tags,
                exists: &exists,
            },
            if old.is_some() {
                PlanMode::Update
            } else {
                PlanMode::Install
            },
        )?;
        Ok((p.files, p.remove))
    }

    async fn run_download(&self, d: Download<'_>) -> Result<Outcome> {
        let Download {
            app,
            platform,
            asset,
            use_signed_url,
            dir,
            files: (files, remove),
            target,
            target_bytes,
            secrets,
            tags,
            ctx,
        } = d;
        let client = self.client().await?;
        let mut opts = self.depot_options(app);
        if use_signed_url {
            opts.signer = Some(ember_depot::Signer(Arc::new(ChunkSigner::new(
                client.clone(),
                self.session_source(),
                asset.clone(),
            ))));
        }
        (ctx.step)("Downloading");
        let finished = ember_depot::download(
            &client.download_client()?,
            Job {
                root: dir,
                files: &files,
                remove: &remove,
            },
            &opts,
            progress_sink(ctx),
            &ctx.cancel,
        )
        .await?;
        if finished == Finished::Cancelled {
            log_line(ctx, "canceled");
            return Ok(Outcome::Abort);
        }
        if !target_bytes.is_empty() {
            let path = self.layout.build_manifest(app);
            Self::blocking(move || ember_core::json_store::write_bytes(&path, &target_bytes))
                .await??;
        }
        let record = self.require_record(app)?;
        let mut executable = target.meta.launch_exe.clone();
        if platform != "Mac"
            && let Some(exe) = self.executable_override(app).await
            && target
                .files
                .iter()
                .any(|f| f.filename.eq_ignore_ascii_case(&exe))
        {
            log_line(ctx, &format!("launching {exe} instead of {executable}"));
            executable = exe;
        }
        let previous = self.installed_entry(app).unwrap_or_default();
        let entry = InstalledGame {
            app_name: app.to_string(),
            title: record.app_title.clone(),
            version: target.meta.build_version.clone(),
            platform: platform.to_string(),
            install_path: dir.to_string_lossy().into_owned(),
            executable,
            launch_parameters: target.meta.launch_command.clone(),
            install_size: target.install_size(tags),
            install_tags: tags.to_vec(),
            is_dlc: record.is_dlc(),
            can_run_offline: record.can_run_offline(),
            requires_ot: record.requires_ownership_token(),
            prereq: install_info::prerequisites_of(target),
            save_path: previous.save_path,
            save_timestamp: previous.save_timestamp,
            egl_guid: previous.egl_guid,
            manifest_secrets: secrets.into_iter().collect(),
            third_party: false,
        };
        let export = !entry.is_dlc && entry.platform.starts_with("Win");
        self.update_installed(|map| {
            map.insert(app.to_string(), entry);
        })
        .await?;
        self.install_info.clear().await;
        if export
            && let Some(prefix) = self.egl_prefix()
            && let Err(e) = self.egl_export(&prefix, app).await
        {
            tracing::warn!("exporting {app} to the Epic Games Launcher: {e:#}");
        }
        log_line(ctx, "done");
        Ok(Outcome::Done)
    }

    /// Register an existing installation of `app` (and its DLCs).
    pub(crate) async fn import_one(
        &self,
        app: &str,
        path: &Path,
        platform: &str,
        with_dlcs: bool,
    ) -> Result<()> {
        let record = self.require_record(app)?;
        if self.installed_entry(app).is_some() {
            bail!("{} is already installed", record.app_title);
        }
        if record.is_dlc() {
            let base = record.main_game_app().unwrap_or_default();
            if self.installed_entry(base).is_none() {
                bail!(
                    "Install or import the base game of {} first",
                    record.app_title
                );
            }
        }
        let build = self.fetch_build(app, platform).await?;
        let manifest = &build.manifest;
        let dir = path.to_path_buf();
        let names: Vec<String> = manifest.files.iter().map(|f| f.filename.clone()).collect();
        let exe = manifest.meta.launch_exe.trim_start_matches('/').to_string();
        let (found, exe_found) = Self::blocking(move || {
            let exists = |rel: &str| {
                ember_depot::path::normalize(rel)
                    .map(|r| {
                        ember_epic_api::saves::case_insensitive(&ember_depot::path::join(&dir, &r))
                            .is_file()
                    })
                    .unwrap_or(false)
            };
            (
                names.iter().filter(|n| exists(n)).count(),
                exe.is_empty() || exists(&exe),
            )
        })
        .await?;
        if found == 0 {
            bail!(
                "No files of {} were found in {}. Choose the folder that contains the game.",
                record.app_title,
                path.display()
            );
        }
        if !record.is_dlc() && !exe_found {
            bail!(
                "The executable of {} was not found in {}",
                record.app_title,
                path.display()
            );
        }
        if found * 100 < manifest.files.len() * 95 {
            tracing::warn!(
                "{}: {found} of {} files present; repair the game before playing",
                record.app_title,
                manifest.files.len()
            );
        }
        let path_bytes = build.bytes.clone();
        let target = self.layout.build_manifest(app);
        Self::blocking(move || ember_core::json_store::write_bytes(&target, &path_bytes)).await??;
        let entry = InstalledGame {
            app_name: app.to_string(),
            title: record.app_title.clone(),
            version: manifest.meta.build_version.clone(),
            platform: platform.to_string(),
            install_path: path.to_string_lossy().into_owned(),
            executable: manifest.meta.launch_exe.clone(),
            launch_parameters: manifest.meta.launch_command.clone(),
            install_size: manifest.install_size(&[]),
            is_dlc: record.is_dlc(),
            can_run_offline: record.can_run_offline(),
            requires_ot: record.requires_ownership_token(),
            prereq: install_info::prerequisites_of(manifest),
            manifest_secrets: build.info.secrets.clone().into_iter().collect(),
            ..Default::default()
        };
        self.update_installed(|map| {
            map.insert(app.to_string(), entry);
        })
        .await?;
        if with_dlcs {
            for dlc in self.dlc_records(&record) {
                if !dlc.asset_infos.contains_key(platform)
                    || self.installed_entry(&dlc.app_name).is_some()
                {
                    continue;
                }
                if let Err(e) =
                    Box::pin(self.import_one(&dlc.app_name, path, platform, false)).await
                {
                    tracing::info!("not importing DLC {}: {e:#}", dlc.app_name);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ember_epic_api::{ChunkPart, FileManifest};

    use super::*;

    #[test]
    fn deletes_listed_files_by_tag() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Game");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("lang")).unwrap();
        std::fs::write(dir.join("bin/game.exe"), b"x").unwrap();
        std::fs::write(dir.join("lang/de.pak"), b"x").unwrap();
        std::fs::write(dir.join("lang/user.cfg"), b"x").unwrap();
        let mut m = Manifest::new("G".into(), "1".into());
        let file = |name: &str, tags: &[&str]| FileManifest {
            filename: name.into(),
            install_tags: tags.iter().map(|t| t.to_string()).collect(),
            parts: vec![ChunkPart {
                guid: Default::default(),
                offset: 0,
                size: 1,
                file_offset: 0,
            }],
            ..Default::default()
        };
        m.files = vec![file("bin/game.exe", &[]), file("lang/de.pak", &["de"])];
        delete_listed(&dir, &m, &["fr".into()], false).unwrap();
        assert!(!dir.join("bin").exists());
        assert!(
            dir.join("lang/de.pak").exists(),
            "unselected tags are not installed"
        );
        delete_listed(&dir, &m, &[], false).unwrap();
        assert!(!dir.join("lang/de.pak").exists());
        assert!(dir.join("lang/user.cfg").exists());
        assert!(platform_arg(InstallPlatform::Linux).is_err());
    }
}
