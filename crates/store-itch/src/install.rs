//! Install, update and repair: choose the upload, download it through
//! [`ember_depot`] into `<base>/.itch-download/<app>/` (resumable), unpack
//! it, find the program and record the install.
//!
//! A first install unpacks straight into the game folder. An update or
//! repair unpacks into a sibling folder first and swaps it in once that
//! worked, so a failure leaves the installed game as it was.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use ember_core::{InstallPlatform, InstallProgress, OpContext, Outcome, util};
use ember_itch_api::{Client, OwnedKey, Upload};

use crate::records::InstalledRecord;
use crate::{ItchStore, detect, extract, os_of, session_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Install,
    /// Update or repair an installed game in place.
    Update,
}

/// Fetches a download link right before the file starts (and again when one
/// expires): itch.io's links are short-lived.
struct LinkSigner {
    api: Client,
    upload_id: u64,
    key_id: u64,
}

impl ember_depot::UrlSigner for LinkSigner {
    fn sign<'a>(
        &'a self,
        keys: &'a [String],
    ) -> futures::future::BoxFuture<'a, Result<HashMap<String, String>>> {
        Box::pin(async move {
            let url = self
                .api
                .download_location(self.upload_id, self.key_id)
                .await?;
            Ok(keys.iter().map(|k| (k.clone(), url.clone())).collect())
        })
    }
}

fn progress_sink(ctx: &OpContext) -> ember_depot::ProgressFn {
    let sink = ctx.progress.clone();
    Arc::new(move |p: ember_depot::Progress| {
        sink(InstallProgress {
            percent: Some(p.percent.clamp(0.0, 100.0)),
            bytes_done: Some(p.bytes_done),
            bytes_total: Some(p.bytes_total),
            eta_secs: p.eta_secs,
            download_speed: Some(p.download_speed),
            disk_speed: Some(p.disk_speed),
            bottleneck: p.bottleneck,
            preparing: p.preparing,
        })
    })
}

/// Bytes of every file under `dir` (blocking).
pub(crate) fn folder_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => folder_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn log_line(ctx: &OpContext, line: &str) {
    if let Some(log) = &ctx.log_file {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
        {
            let _ = writeln!(f, "[itch.io] {line}");
        }
    }
}

/// Free bytes on the filesystem holding `path` (or its nearest existing parent).
fn free_space(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    fs4::available_space(existing).ok()
}

/// Size of the file behind `url`, from a one-byte range request (for
/// uploads whose size the API leaves out).
async fn probe_size(url: &str) -> Result<u64> {
    let resp = ember_core::http::client()
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .context("Could not reach itch.io's download server")?;
    let total = match resp.status() {
        reqwest::StatusCode::PARTIAL_CONTENT => resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|v| v.trim().parse().ok()),
        s if s.is_success() => resp.content_length(),
        s => bail!("the download server answered HTTP {}", s.as_u16()),
    };
    total
        .filter(|n| *n > 0)
        .context("the download server did not report the file size")
}

impl ItchStore {
    fn staging_dir(&self, app: &str, base: &Path) -> PathBuf {
        base.join(".itch-download")
            .join(ember_core::paths::sanitize(app))
    }

    /// The download key and the upload to install for `platform`.
    pub(crate) async fn choose_upload(
        &self,
        app: &str,
        platform: InstallPlatform,
    ) -> Result<(OwnedKey, Upload)> {
        let key = self.key_of(app)?;
        let game_id = key.game.as_ref().map_or(key.game_id, |g| g.id);
        let uploads = self.api()?.uploads(game_id, key.id).await?;
        let upload = ember_itch_api::best_upload(&uploads, os_of(platform)?)
            .cloned()
            .with_context(|| {
                let title = key.game.as_ref().map_or("This game", |g| g.title.as_str());
                format!("{title} has no {} download on itch.io", platform.label())
            })?;
        Ok((key, upload))
    }

    pub(crate) async fn has_partial_download(&self, app: &str) -> bool {
        let base = ember_core::paths::expand_home(&(self.settings)().default_install_path);
        tokio::fs::try_exists(self.staging_dir(app, &base))
            .await
            .unwrap_or(false)
    }

    /// Run the pipeline; errors become [`Outcome::Error`] for the queue.
    pub(crate) async fn run_pipeline(
        &self,
        app: &str,
        install_dir: PathBuf,
        platform: InstallPlatform,
        mode: Mode,
        ctx: &OpContext,
    ) -> Result<Outcome> {
        match self.pipeline(app, &install_dir, platform, mode, ctx).await {
            Ok(o) => Ok(o),
            Err(e) => {
                let e = session_error(e);
                log_line(ctx, &format!("failed: {e:#}"));
                if ctx.cancel.is_cancelled() {
                    Ok(Outcome::Abort)
                } else {
                    Ok(Outcome::Error(format!("{e:#}")))
                }
            }
        }
    }

    async fn pipeline(
        &self,
        app: &str,
        install_dir: &Path,
        platform: InstallPlatform,
        mode: Mode,
        ctx: &OpContext,
    ) -> Result<Outcome> {
        (ctx.step)("Fetching file list");
        let (key, upload) = self.choose_upload(app, platform).await?;
        let title = key
            .game
            .as_ref()
            .map(|g| g.title.clone())
            .unwrap_or_else(|| app.to_string());
        let base = install_dir
            .parent()
            .context("invalid install folder")?
            .to_path_buf();
        if mode == Mode::Install
            && std::fs::read_dir(install_dir).is_ok_and(|mut d| d.next().is_some())
        {
            bail!(
                "{} already exists and is not empty. Choose another folder, or import the game from there.",
                install_dir.display()
            );
        }
        let staging = self.staging_dir(app, &base);
        tokio::fs::create_dir_all(&staging)
            .await
            .with_context(|| format!("creating {}", staging.display()))?;
        let api = self.api()?;
        let size = match upload.size {
            0 => probe_size(&api.download_location(upload.id, key.id).await?).await?,
            n => n,
        };
        let needed = size.saturating_mul(2);
        if let Some(free) = free_space(&base)
            && free < needed
        {
            bail!(
                "Not enough free disk space in {}: {} needed, {} available",
                base.display(),
                util::format_bytes(needed),
                util::format_bytes(free)
            );
        }

        (ctx.step)("Downloading");
        let name = ember_core::paths::sanitize(
            Path::new(&upload.filename)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("upload-{}", upload.id))
                .as_str(),
        );
        log_line(ctx, &format!("downloading {} ({name})", upload.label()));
        // Butler builds are served as archives made on demand; only a
        // hosted file has the checksum the API reports.
        let hash = (upload.storage == "hosted" && upload.md5_hash.trim().len() == 32)
            .then(|| {
                ember_depot::Digest::from_hex(ember_depot::Algorithm::Md5, upload.md5_hash.trim())
                    .ok()
            })
            .flatten();
        let entry = ember_depot::FileEntry::whole(
            ember_depot::path::normalize(&name)?,
            size,
            hash,
            Vec::new(),
        );
        let settings = (self.settings)();
        let finished = ember_depot::download(
            ember_core::http::download_client(),
            ember_depot::Job {
                root: &staging,
                files: std::slice::from_ref(&entry),
                remove: &[],
            },
            &ember_depot::Options {
                workers: settings.max_workers as usize,
                max_bytes_per_sec: u64::from(settings.max_download_speed) * 1024,
                signer: Some(ember_depot::Signer(Arc::new(LinkSigner {
                    api,
                    upload_id: upload.id,
                    key_id: key.id,
                }))),
                ..Default::default()
            },
            progress_sink(ctx),
            &ctx.cancel,
        )
        .await?;
        if finished == ember_depot::Finished::Cancelled {
            log_line(ctx, "download paused; the partial file is kept");
            return Ok(Outcome::Abort);
        }

        (ctx.step)("Unpacking");
        let downloaded = staging.join(&name);
        let target = match mode {
            Mode::Install => install_dir.to_path_buf(),
            Mode::Update => {
                // Left over from an update that failed while unpacking.
                let fresh = sibling(install_dir, "itch-new");
                self.discard(&fresh).await;
                fresh
            }
        };
        let unpacked = {
            let (from, to, filename, cancel) = (
                downloaded.clone(),
                target.clone(),
                upload.filename.clone(),
                ctx.cancel.clone(),
            );
            Self::blocking(move || extract::unpack(&from, &filename, &to, &cancel)).await?
        };
        if let Err(e) = unpacked {
            self.discard(&target).await;
            if ctx.cancel.is_cancelled() {
                return Ok(Outcome::Abort);
            }
            return Err(e.context(format!("Could not unpack {title}")));
        }
        if mode == Mode::Update {
            self.swap_in(&target, install_dir).await?;
        }

        let previous = self.record(app);
        let (dir, plat) = (install_dir.to_path_buf(), platform);
        let (exe, size) =
            Self::blocking(move || (detect::find_executable(&dir, plat), folder_size(&dir)))
                .await?;
        let executable = exe
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| previous.as_ref().map(|p| p.executable.clone()))
            .unwrap_or_default();
        if executable.is_empty() {
            log_line(
                ctx,
                "no program to start was found; set one in the game settings",
            );
        }
        let record = InstalledRecord {
            title: title.clone(),
            install_path: install_dir.to_string_lossy().into_owned(),
            platform,
            upload_id: upload.id,
            revision: upload.revision(),
            version: upload.version_label(),
            executable,
            size,
            pinned_version: previous.is_some_and(|p| p.pinned_version),
        };
        self.update_installed(|m| {
            m.insert(app.to_string(), record);
        })
        .await?;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        log_line(
            ctx,
            &format!("installed {title} in {}", install_dir.display()),
        );
        Ok(Outcome::Done)
    }

    /// Replace `install_dir` with the freshly unpacked `new` folder.
    async fn swap_in(&self, new: &Path, install_dir: &Path) -> Result<()> {
        let old = sibling(install_dir, "itch-old");
        let (new, dir, app) = (
            new.to_path_buf(),
            install_dir.to_path_buf(),
            (self.settings)(),
        );
        Self::blocking(move || -> Result<()> {
            if old.exists() {
                ember_core::fs_guard::ensure_deletable(&old, &app)?;
                std::fs::remove_dir_all(&old)?;
            }
            if dir.exists() {
                std::fs::rename(&dir, &old)
                    .with_context(|| format!("moving {} aside", dir.display()))?;
            }
            if let Err(e) = std::fs::rename(&new, &dir) {
                let _ = std::fs::rename(&old, &dir);
                return Err(e).with_context(|| format!("replacing {}", dir.display()));
            }
            if old.exists() {
                ember_core::fs_guard::ensure_deletable(&old, &app)?;
                if let Err(e) = std::fs::remove_dir_all(&old) {
                    tracing::warn!("removing the previous files at {}: {e}", old.display());
                }
            }
            Ok(())
        })
        .await?
    }

    /// Remove a folder this pipeline created and failed to fill.
    async fn discard(&self, dir: &Path) {
        let (dir, app) = (dir.to_path_buf(), (self.settings)());
        let _ = Self::blocking(move || {
            if dir.exists() && ember_core::fs_guard::ensure_deletable(&dir, &app).is_ok() {
                let _ = std::fs::remove_dir_all(&dir);
            }
        })
        .await;
    }
}

/// `<parent>/.<name>.<suffix>` next to `dir`.
fn sibling(dir: &Path, suffix: &str) -> PathBuf {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    dir.with_file_name(format!(".{name}.{suffix}"))
}
