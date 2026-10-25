//! The install pipeline: file list → downloads into a per-game staging folder
//! (through [`ember_depot`], with fresh download links from the API) → native
//! extraction or a silent Windows installer through Wine → executable/DOSBox
//! detection → installed record.
//!
//! Interrupting at any point keeps the staging folder (tracked in
//! `pending.json`): a later install continues the downloads with HTTP ranges
//! and re-runs the extraction or installer. Updates and repairs run the same
//! pipeline over the existing install.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use ember_core::process::{self, CommandSpec, RunOptions};
use ember_core::settings::GameSettingsOverrides;
use ember_core::{
    AppSettings, GameId, GameInfo, GameSettings, InstallPlatform, InstallProgress, OpContext,
    Outcome, Paths, Runner, Store as _, json_store, util,
};

use crate::ZoomStore;
use crate::api::{ZoomApi, ZoomFile};
use crate::detect::{self, DosboxGame, InstallerKind};
use crate::extract::{self, ArchiveKind};
use crate::mapping;
use crate::records::{self, InstalledRecord, PendingInstall};

/// Why the pipeline runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Install,
    Update,
    Repair,
}

/// Result of a cancellable phase.
enum Phase<T> {
    Done(T),
    Aborted,
}

/// What was found to run after installing.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Detected {
    /// Relative to the install root; empty when nothing was found.
    pub executable: String,
    pub platform: InstallPlatform,
    pub dosbox: Option<DosboxGame>,
}

impl Detected {
    pub fn into_record(
        self,
        app: &str,
        install_path: &Path,
        build_platform: InstallPlatform,
        size: u64,
    ) -> InstalledRecord {
        let (is_dosbox, dosbox_dir, dosbox_conf) = match self.dosbox {
            Some(d) => (true, d.dir, d.confs),
            None => (false, String::new(), Vec::new()),
        };
        InstalledRecord {
            app_name: app.to_string(),
            install_path: install_path.to_string_lossy().into_owned(),
            executable: self.executable,
            platform: self.platform,
            build_platform,
            is_dosbox,
            dosbox_conf,
            dosbox_dir,
            install_size_bytes: size,
            version: "N/A".into(),
            installed_at: util::unix_now(),
            ..Default::default()
        }
    }
}

/// Find what to run in an installed tree (blocking). `confs_before` are the
/// `.conf` files that existed before a Windows installer ran; new ones mark
/// a DOSBox game.
pub(crate) fn detect_installed(
    root: &Path,
    title: &str,
    platform: InstallPlatform,
    confs_before: &[PathBuf],
) -> Detected {
    if platform == InstallPlatform::Linux {
        let executable = detect::linux_executable(root, title).unwrap_or_default();
        if !executable.is_empty() {
            make_executable(&root.join(&executable));
        }
        return Detected {
            executable,
            platform: InstallPlatform::Linux,
            dosbox: None,
        };
    }
    let before: HashSet<&PathBuf> = confs_before.iter().collect();
    let new_confs: Vec<PathBuf> = detect::conf_files(root)
        .into_iter()
        .filter(|c| !before.contains(c))
        .collect();
    if let Some(game) = detect::dosbox_game(root, &new_confs) {
        let platform = if detect::system_dosbox().is_some() {
            ember_core::HostPlatform::current().native()
        } else {
            InstallPlatform::Windows
        };
        return Detected {
            executable: game.exe.clone(),
            platform,
            dosbox: Some(game),
        };
    }
    Detected {
        executable: detect::windows_executable(root, title).unwrap_or_default(),
        platform: InstallPlatform::Windows,
        dosbox: None,
    }
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Ok(meta) = std::fs::metadata(path) {
        let mode = meta.permissions().mode() | 0o755;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
}

/// `Z:\path\in\wine` for a host path.
fn wine_path(p: &Path) -> String {
    if cfg!(windows) {
        return p.to_string_lossy().replace('/', "\\");
    }
    format!("Z:{}", p.to_string_lossy().replace('/', "\\"))
}

/// Silent-install command line for a Windows installer.
pub(crate) fn installer_argv(
    kind: InstallerKind,
    exe: &Path,
    install_path: &Path,
    inf: &Path,
    log: &Path,
) -> Vec<String> {
    let exe = exe.to_string_lossy().into_owned();
    match kind {
        InstallerKind::Nsis => vec![exe, "/S".into(), format!("/D={}", wine_path(install_path))],
        InstallerKind::InnoSetup | InstallerKind::Unknown => vec![
            exe,
            "/VERYSILENT".into(),
            "/SUPPRESSMSGBOXES".into(),
            "/NORESTART".into(),
            "/NOICONS".into(),
            "/SP-".into(),
            format!("/DIR={}", wine_path(install_path)),
            format!("/LOADINF={}", wine_path(inf)),
            format!("/LOG={}", wine_path(log)),
        ],
    }
}

const INNO_INF: &str = "[Setup]\nLang=english\nDisableWelcomePage=yes\nDisableDirPage=yes\nDisableProgramGroupPage=yes\nDisableReadyPage=yes\n";

/// Effective settings of a game: the caller's resolved settings, or the
/// global defaults plus saved overrides when the caller had none.
fn game_settings(id: &GameId, app: &AppSettings, ctx: &OpContext) -> GameSettings {
    if let Some(settings) = &ctx.settings {
        return (**settings).clone();
    }
    let overrides: GameSettingsOverrides =
        json_store::read_or_default(&Paths::get().game_settings_file(id));
    overrides.apply(&app.game)
}

fn log_line(ctx: &OpContext, line: &str) {
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
        let _ = writeln!(f, "[zoom] {line}");
    }
}

fn checksum_of(f: &ZoomFile) -> Option<ember_depot::Digest> {
    use ember_depot::{Algorithm, Digest};
    f.sha256
        .as_deref()
        .and_then(|h| Digest::from_hex(Algorithm::Sha256, h.trim()).ok())
        .or_else(|| {
            f.md5
                .as_deref()
                .and_then(|h| Digest::from_hex(Algorithm::Md5, h.trim()).ok())
        })
}

/// Fetches a download link from the API right before each file starts (and
/// again when one expires): links are short-lived.
struct LinkSigner(ZoomApi);

impl ember_depot::UrlSigner for LinkSigner {
    fn sign<'a>(
        &'a self,
        keys: &'a [String],
    ) -> futures::future::BoxFuture<'a, Result<HashMap<String, String>>> {
        Box::pin(async move {
            // Only the file about to start; later ones get theirs when due.
            let mut out = HashMap::new();
            if let Some(id) = keys.first() {
                out.insert(id.clone(), self.0.download_url(id).await?);
            }
            Ok(out)
        })
    }
}

/// Size of the file behind `url`, from a one-byte range request.
async fn probe_size(url: &str) -> Result<u64> {
    let resp = ember_core::http::client()
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .context("Could not reach ZOOM Platform's download server")?;
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

/// The depot entry of a ZOOM file (one part, keyed by the file id).
fn file_entry(f: &ZoomFile, size: u64) -> Result<ember_depot::FileEntry> {
    let chunk = Arc::new(ember_depot::Chunk {
        key: f.id.clone(),
        download_size: size,
        size,
        ..Default::default()
    });
    Ok(ember_depot::FileEntry {
        path: ember_depot::path::normalize(&mapping::disk_name(f))?,
        size,
        hash: checksum_of(f),
        parts: vec![ember_depot::Part::new(chunk, 0, size)],
        executable: false,
    })
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

/// Free bytes on the filesystem holding `path` (or its nearest existing parent).
fn free_space(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    fs4::available_space(existing).ok()
}

/// Partial file of a download by earlier Ember versions.
fn legacy_part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

async fn local_len(dest: &Path) -> u64 {
    for p in [
        dest.to_path_buf(),
        ember_depot::path::part_path(dest),
        legacy_part_path(dest),
    ] {
        if let Ok(m) = tokio::fs::metadata(&p).await {
            return m.len();
        }
    }
    0
}

impl ZoomStore {
    /// Run the pipeline; errors become [`Outcome::Error`] for the queue.
    pub(crate) async fn run_pipeline(
        &self,
        app: &str,
        install_path: PathBuf,
        platform: InstallPlatform,
        language: Option<String>,
        mode: Mode,
        ctx: &OpContext,
    ) -> Result<Outcome> {
        match self
            .pipeline(app, &install_path, platform, language, mode, ctx)
            .await
        {
            Ok(o) => Ok(o),
            Err(e) => {
                self.expire_session_if_unauthorized(&e);
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
        install_path: &Path,
        platform: InstallPlatform,
        language: Option<String>,
        mode: Mode,
        ctx: &OpContext,
    ) -> Result<Outcome> {
        if !matches!(platform, InstallPlatform::Windows | InstallPlatform::Linux) {
            bail!("ZOOM Platform only offers Windows and Linux builds");
        }
        let game = self
            .game(app)
            .with_context(|| format!("{app} is not in your ZOOM Platform library"))?;
        let api = self.api()?;
        (ctx.step)("Fetching file list");
        let files = api.files(app).await?;
        records::save_files_cache(self.layout(), app, &files);
        let list: Vec<ZoomFile> = mapping::platform_files(&files, platform).to_vec();
        if list.is_empty() {
            bail!(
                "{} has no {} build on ZOOM Platform",
                game.title,
                platform.label()
            );
        }

        let base = install_path
            .parent()
            .context("invalid install folder")?
            .to_path_buf();
        let staging = base
            .join(".zoom-download")
            .join(ember_core::paths::sanitize(app));
        self.begin_pending(app, &staging, install_path, platform)
            .await?;

        let total: u64 = list.iter().map(mapping::file_size).sum();
        let mut have = 0u64;
        for f in &list {
            have += local_len(&staging.join(mapping::disk_name(f))).await;
        }
        let unpacked = if mode == Mode::Install { total } else { 0 };
        let needed = total.saturating_sub(have) + unpacked;
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

        // Download.
        (ctx.step)("Downloading");
        log_line(
            ctx,
            &format!(
                "downloading {} file(s) to {}",
                list.len(),
                staging.display()
            ),
        );
        let mut entries = Vec::with_capacity(list.len());
        let mut local = Vec::with_capacity(list.len());
        for f in &list {
            let dest = staging.join(mapping::disk_name(f));
            // Continue partial files of earlier versions.
            let (old, new) = (legacy_part_path(&dest), ember_depot::path::part_path(&dest));
            if old.is_file() && !new.exists() {
                let _ = tokio::fs::rename(&old, &new).await;
            }
            let size = match mapping::exact_size(f) {
                Some(n) => n,
                None => probe_size(&api.download_url(&f.id).await?)
                    .await
                    .with_context(|| format!("checking the size of {}", mapping::disk_name(f)))?,
            };
            entries.push(file_entry(f, size)?);
            local.push(dest);
        }
        let settings = (self.settings)();
        let finished = ember_depot::download(
            ember_core::http::client(),
            ember_depot::Job {
                root: &staging,
                files: &entries,
                remove: &[],
            },
            &ember_depot::Options {
                workers: settings.max_workers as usize,
                max_bytes_per_sec: u64::from(settings.max_download_speed) * 1024,
                signer: Some(ember_depot::Signer(Arc::new(LinkSigner(api.clone())))),
                ..Default::default()
            },
            progress_sink(ctx),
            &ctx.cancel,
        )
        .await?;
        if finished == ember_depot::Finished::Cancelled {
            log_line(ctx, "download paused; partial files kept");
            return Ok(Outcome::Abort);
        }

        // Install.
        let previous = self.installed_record(app);
        let phase = match platform {
            InstallPlatform::Linux => self.install_linux(&game, &local, install_path, ctx).await?,
            _ => {
                self.install_windows(&game, &local, &staging, install_path, ctx)
                    .await?
            }
        };
        let mut detected = match phase {
            Phase::Done(d) => d,
            Phase::Aborted => return Ok(Outcome::Abort),
        };
        if mode != Mode::Install
            && let Some(prev) = &previous
        {
            // A reinstall creates no new DOSBox configs; keep what we knew.
            if prev.is_dosbox && detected.dosbox.is_none() {
                detected.executable = prev.executable.clone();
                detected.platform = prev.platform;
                detected.dosbox = Some(DosboxGame {
                    exe: prev.executable.clone(),
                    dir: prev.dosbox_dir.clone(),
                    confs: prev.dosbox_conf.clone(),
                });
            } else if detected.executable.is_empty() {
                detected.executable = prev.executable.clone();
            }
        }

        let size = {
            let root = install_path.to_path_buf();
            tokio::task::spawn_blocking(move || detect::dir_size(&root))
                .await
                .unwrap_or(0)
        };
        let mut record = detected.into_record(app, install_path, platform, size);
        record.language = language;
        record.version = mapping::guess_version(&list).unwrap_or_else(|| "N/A".into());
        record.files = mapping::fingerprint(&list);
        record.pinned_version = previous.as_ref().is_some_and(|p| p.pinned_version);
        let missing = record.executable.is_empty() && !record.is_dosbox;
        log_line(
            ctx,
            &format!(
                "installed to {} (executable: {})",
                record.install_path,
                if missing {
                    "not found"
                } else {
                    &record.executable
                }
            ),
        );

        {
            let mut st = self.state.write();
            st.installed.insert(app.to_string(), record);
            st.pending.remove(app);
            self.save_installed(&st)?;
            self.save_pending(&st)?;
            Self::publish(&mut st, app);
        }
        self.remove_dir_if_safe(&staging).await;
        if let Some(parent) = staging.parent() {
            let _ = tokio::fs::remove_dir(parent).await;
        }
        self.bus
            .emit(ember_core::Event::LibraryChanged(Runner::Zoom));
        if missing {
            self.notice_missing_executable(app, &game.title);
        }
        Ok(Outcome::Done)
    }

    /// Record the staging folder so pause/cancel/restart can find it.
    async fn begin_pending(
        &self,
        app: &str,
        staging: &Path,
        install_path: &Path,
        platform: InstallPlatform,
    ) -> Result<()> {
        let stale = {
            let mut st = self.state.write();
            let prev = st.pending.get(app).cloned();
            let same = prev.as_ref().filter(|p| p.staging_dir == staging);
            let created = match same {
                Some(p) => p.created_install_dir,
                None => !install_path.exists(),
            };
            st.pending.insert(
                app.to_string(),
                PendingInstall {
                    staging_dir: staging.to_path_buf(),
                    install_path: install_path.to_path_buf(),
                    platform,
                    created_install_dir: created,
                },
            );
            self.save_pending(&st)?;
            prev.filter(|p| p.staging_dir != staging || p.platform != platform)
        };
        if let Some(old) = stale {
            // Another folder, or the other platform's files: start clean.
            self.remove_dir_if_safe(&old.staging_dir).await;
        }
        tokio::fs::create_dir_all(staging)
            .await
            .with_context(|| format!("creating {}", staging.display()))
    }

    async fn extract_native(
        &self,
        archive: &Path,
        kind: ArchiveKind,
        dest: &Path,
        ctx: &OpContext,
    ) -> Result<Phase<()>> {
        let name = archive
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        (ctx.step)(&format!("Extracting {name}"));
        log_line(ctx, &format!("extracting {name}"));
        let (a, d) = (archive.to_path_buf(), dest.to_path_buf());
        let cancel = ctx.cancel.clone();
        let sink = ctx.progress.clone();
        let ok = tokio::task::spawn_blocking(move || {
            let mut last = 0f32;
            extract::extract(&a, kind, &d, &cancel, &mut |done, total| {
                let pct = if total > 0 {
                    (done as f64 * 100.0 / total as f64) as f32
                } else {
                    0.0
                };
                if pct - last >= 0.5 || done == total {
                    last = pct;
                    sink(InstallProgress {
                        percent: Some(pct.min(100.0)),
                        bytes_done: Some(done),
                        bytes_total: Some(total),
                        ..Default::default()
                    });
                }
            })
        })
        .await
        .context("extraction task failed")??;
        Ok(if ok { Phase::Done(()) } else { Phase::Aborted })
    }

    async fn run_tool(&self, spec: CommandSpec, what: &str, ctx: &OpContext) -> Result<Phase<()>> {
        let mut opts = RunOptions::default().cancel(ctx.cancel.clone());
        if let Some(log) = &ctx.log_file {
            opts = opts.log_file(log);
        }
        let res = process::run(&spec, opts).await?;
        if res.aborted {
            return Ok(Phase::Aborted);
        }
        res.check().with_context(|| format!("{what} failed"))?;
        Ok(Phase::Done(()))
    }

    async fn install_linux(
        &self,
        game: &GameInfo,
        local: &[PathBuf],
        install_path: &Path,
        ctx: &OpContext,
    ) -> Result<Phase<Detected>> {
        tokio::fs::create_dir_all(install_path)
            .await
            .with_context(|| format!("creating {}", install_path.display()))?;
        for file in local {
            let f = file.clone();
            let kind = tokio::task::spawn_blocking(move || extract::classify(&f))
                .await
                .context("inspecting download")?;
            let phase = match kind {
                ArchiveKind::Tar
                | ArchiveKind::TarGz
                | ArchiveKind::TarXz
                | ArchiveKind::TarZst
                | ArchiveKind::TarBz2
                | ArchiveKind::TarLzma
                | ArchiveKind::Zip
                | ArchiveKind::MojoSetup
                | ArchiveKind::Makeself => {
                    self.extract_native(file, kind, install_path, ctx).await?
                }
                ArchiveKind::TarOther => {
                    (ctx.step)("Extracting");
                    let spec = CommandSpec::new("tar")
                        .arg("-xf")
                        .arg(file.as_os_str())
                        .arg("-C")
                        .arg(install_path.as_os_str());
                    self.run_tool(spec, "Extracting the archive", ctx).await?
                }
                ArchiveKind::WindowsExe => {
                    log_line(ctx, &format!("ignoring Windows file {}", file.display()));
                    Phase::Done(())
                }
                ArchiveKind::Other => {
                    let name = file.file_name().context("invalid download name")?;
                    let dest = install_path.join(name);
                    tokio::fs::copy(file, &dest)
                        .await
                        .with_context(|| format!("copying {}", dest.display()))?;
                    make_executable(&dest);
                    Phase::Done(())
                }
            };
            if let Phase::Aborted = phase {
                return Ok(Phase::Aborted);
            }
        }
        (ctx.step)("Finishing");
        let root = install_path.to_path_buf();
        let title = game.title.clone();
        let detected = tokio::task::spawn_blocking(move || {
            detect_installed(&root, &title, InstallPlatform::Linux, &[])
        })
        .await
        .context("scanning the installed game")?;
        Ok(Phase::Done(detected))
    }

    async fn install_windows(
        &self,
        game: &GameInfo,
        local: &[PathBuf],
        staging: &Path,
        install_path: &Path,
        ctx: &OpContext,
    ) -> Result<Phase<Detected>> {
        tokio::fs::create_dir_all(install_path)
            .await
            .with_context(|| format!("creating {}", install_path.display()))?;
        let root = install_path.to_path_buf();
        let confs_before = tokio::task::spawn_blocking(move || detect::conf_files(&root))
            .await
            .context("scanning the install folder")?;

        let mut installers = Vec::new();
        let mut extracted_any = false;
        for file in local {
            let f = file.clone();
            let kind = tokio::task::spawn_blocking(move || extract::classify(&f))
                .await
                .context("inspecting download")?;
            match kind {
                ArchiveKind::WindowsExe => installers.push(file.clone()),
                ArchiveKind::Zip
                | ArchiveKind::Tar
                | ArchiveKind::TarGz
                | ArchiveKind::TarXz
                | ArchiveKind::TarZst => {
                    if let Phase::Aborted =
                        self.extract_native(file, kind, install_path, ctx).await?
                    {
                        return Ok(Phase::Aborted);
                    }
                    extracted_any = true;
                }
                // Installer data parts (`setup-1.bin`) stay next to the exe.
                _ => {}
            }
        }

        if let Some(exe) = installers.first() {
            let e = exe.clone();
            let kind = tokio::task::spawn_blocking(move || detect::installer_kind(&e))
                .await
                .unwrap_or(InstallerKind::Unknown);
            let inf = staging.join("zoom_installer.inf");
            let installer_log = staging.join("installer.log");
            tokio::fs::write(&inf, INNO_INF)
                .await
                .with_context(|| format!("writing {}", inf.display()))?;
            let _ = tokio::fs::remove_file(&installer_log).await;
            let argv = installer_argv(kind, exe, install_path, &inf, &installer_log);
            (ctx.step)("Running installer");
            (ctx.progress)(InstallProgress::default());
            log_line(
                ctx,
                &format!("running {kind:?} installer: {}", argv.join(" ")),
            );

            let app_settings = (self.settings)();
            let id = GameId::new(Runner::Zoom, game.app_name.as_str());
            let settings = game_settings(&id, &app_settings, ctx);
            let target = ember_runtime::Target {
                id: &id,
                game,
                platform: InstallPlatform::Windows,
            };
            let res = if cfg!(windows) {
                let spec = ember_core::process::CommandSpec::new(&argv[0])
                    .args(&argv[1..])
                    .cwd(staging);
                ember_core::process::run(
                    &spec,
                    ember_core::process::RunOptions::default()
                        .cancel(ctx.cancel.clone())
                        .log_file(&installer_log),
                )
                .await
            } else {
                ember_runtime::run_in_prefix(
                    &app_settings,
                    &settings,
                    target,
                    argv,
                    true,
                    ctx.clone(),
                )
                .await
            }
            .context("Could not run the game installer")?;
            if let Ok(text) = tokio::fs::read(&installer_log).await {
                log_line(ctx, &String::from_utf8_lossy(&text));
            }
            if res.aborted || ctx.cancel.is_cancelled() {
                return Ok(Phase::Aborted);
            }
            if !res.success() {
                bail!(
                    "The installer failed (exit code {}): {}",
                    res.code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "none".into()),
                    res.error_summary()
                );
            }
        } else if !extracted_any {
            bail!("No Windows installer was found among the downloaded files");
        }

        (ctx.step)("Finishing");
        let root = install_path.to_path_buf();
        let title = game.title.clone();
        let detected = tokio::task::spawn_blocking(move || {
            detect_installed(&root, &title, InstallPlatform::Windows, &confs_before)
        })
        .await
        .context("scanning the installed game")?;
        Ok(Phase::Done(detected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inno_setup_arguments() {
        let argv = installer_argv(
            InstallerKind::InnoSetup,
            Path::new("/g/.zoom-download/1/setup.exe"),
            Path::new("/g/My Game"),
            Path::new("/g/.zoom-download/1/zoom_installer.inf"),
            Path::new("/g/.zoom-download/1/installer.log"),
        );
        assert_eq!(argv[0], "/g/.zoom-download/1/setup.exe");
        assert!(argv.contains(&"/VERYSILENT".to_string()));
        assert!(argv.contains(&"/SP-".to_string()));
        assert!(
            argv.contains(
                &(if cfg!(windows) {
                    r"/DIR=\g\My Game"
                } else {
                    r"/DIR=Z:\g\My Game"
                })
                .to_string()
            )
        );
        assert!(
            argv.contains(
                &(if cfg!(windows) {
                    r"/LOADINF=\g\.zoom-download\1\zoom_installer.inf"
                } else {
                    r"/LOADINF=Z:\g\.zoom-download\1\zoom_installer.inf"
                })
                .to_string()
            )
        );
        let nsis = installer_argv(
            InstallerKind::Nsis,
            Path::new("/s.exe"),
            Path::new("/g/x"),
            Path::new("/i"),
            Path::new("/l"),
        );
        assert_eq!(
            nsis,
            vec![
                "/s.exe",
                "/S",
                if cfg!(windows) {
                    r"/D=\g\x"
                } else {
                    r"/D=Z:\g\x"
                }
            ]
        );
    }

    #[test]
    fn detects_new_dosbox_configs_only() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        std::fs::create_dir_all(r.join("DOSBox")).unwrap();
        std::fs::write(r.join("DOSBox/dosbox.exe"), b"MZ").unwrap();
        std::fs::write(r.join("old.conf"), b"").unwrap();
        std::fs::write(r.join("Game.exe"), b"MZ").unwrap();
        let before = detect::conf_files(r);
        let det = detect_installed(r, "Game", InstallPlatform::Windows, &before);
        assert!(det.dosbox.is_none());
        assert_eq!(det.executable, "Game.exe");

        std::fs::write(r.join("game.conf"), b"").unwrap();
        let det = detect_installed(r, "Game", InstallPlatform::Windows, &before);
        let dos = det.dosbox.unwrap();
        assert_eq!(dos.confs, vec!["game.conf"]);
        assert_eq!(dos.dir, "");
        assert_eq!(Path::new(&det.executable), Path::new("DOSBox/dosbox.exe"));
    }
}
