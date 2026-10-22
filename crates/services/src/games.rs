//! Game operations that span stores and runtime: launching and tracking
//! sessions, uninstall, move, repair, import, cloud saves and Wine tools.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, Result, bail};
use ember_core::log_cap;
use ember_core::process::{self, CancellationToken, CommandSpec, ExecResult, RunOptions};
use ember_core::{
    Event, GameId, GameInfo, InstallPlatform, InstallProgress, LaunchOption, LaunchRequest, Notice,
    NoticeLevel, Outcome, Paths, RemoveArgs, StatusKind, SyncDirection, util,
};
use ember_runtime::Target;
use serde_json::Value;

use crate::Backend;
use crate::inhibit::Inhibitor;

/// One-off launch choices.
#[derive(Debug, Clone, Default)]
pub struct LaunchParams {
    pub option: Option<LaunchOption>,
    pub extra_args: Vec<String>,
    /// Start an outdated install without updating first.
    pub skip_version_check: bool,
    /// Verbose Wine/Proton logging for this session only.
    pub verbose: bool,
}

/// A game process Ember started and is tracking.
#[derive(Debug, Clone)]
pub struct RunningGame {
    pub pid: u32,
    pub started: Instant,
}

impl Backend {
    pub fn is_running(&self, id: &GameId) -> bool {
        self.running.lock().contains_key(id)
    }

    pub fn running_games(&self) -> Vec<GameId> {
        self.running.lock().keys().cloned().collect()
    }

    /// How long a running game has been running.
    pub fn playing_for(&self, id: &GameId) -> Option<std::time::Duration> {
        self.running.lock().get(id).map(|r| r.started.elapsed())
    }

    /// Every recorded play session, oldest first. Reads the history file;
    /// call off the UI thread.
    pub fn sessions(&self) -> Vec<crate::sessions::Session> {
        crate::sessions::read_all()
    }

    fn installed_game(&self, id: &GameId) -> Result<Arc<GameInfo>> {
        let game = self
            .game(id)
            .with_context(|| format!("{} is not in the library", id.app_name))?;
        if !game.is_installed {
            bail!("{} is not installed", game.title);
        }
        Ok(game)
    }

    fn target_platform(game: &GameInfo) -> InstallPlatform {
        game.installed_platform()
            .unwrap_or(InstallPlatform::Windows)
    }

    /// Launch a game and track it until it exits.
    pub async fn launch(
        &self,
        id: &GameId,
        option: Option<LaunchOption>,
        extra_args: Vec<String>,
    ) -> Result<()> {
        self.launch_with(
            id,
            LaunchParams {
                option,
                extra_args,
                ..Default::default()
            },
        )
        .await
    }

    /// Launch with one-off choices (skip the store's version check, verbose
    /// logs for this session).
    pub async fn launch_with(&self, id: &GameId, params: LaunchParams) -> Result<()> {
        let LaunchParams {
            option,
            extra_args,
            skip_version_check,
            verbose,
        } = params;
        if self.is_running(id) {
            bail!("already running");
        }
        if self.statuses.is_busy(id) {
            bail!("another operation is in progress");
        }
        let game = self.installed_game(id)?;
        let store = self
            .store(id.runner)
            .with_context(|| format!("{} is not available", id.runner))?;
        let guard = self.statuses.guard(id, StatusKind::Launching);

        if game.browser_url.is_none() && !store.is_available(&id.app_name).await {
            bail!(
                "The game files of {} were not found{}",
                game.title,
                game.install_path()
                    .map(|p| format!(" at {}", p.display()))
                    .unwrap_or_default()
            );
        }

        let app = self.settings.app();
        if let Some(o) = &option {
            self.settings.set_game_value(
                id,
                "lastUsedLaunchOption",
                Some(serde_json::to_value(o).unwrap_or(Value::Null)),
            );
        }
        let mut settings = self.settings.game(id);
        if verbose {
            settings.verbose_logs = true;
        }
        let ctx = self.op_context(id, CancellationToken::new());
        let target = Target {
            id,
            game: &game,
            platform: Self::target_platform(&game),
        };

        if settings.auto_sync_saves && store.supports_cloud_saves() && self.is_online() {
            ctx.step.as_ref()("Syncing saves");
            if let Err(e) = self.sync_saves_inner(id, SyncDirection::Download).await {
                tracing::warn!("pre-launch save sync of {id}: {e:#}");
            }
        }

        ctx.step.as_ref()("Preparing");
        let environment = ember_runtime::prepare_launch(&app, &settings, target, &ctx).await?;

        // "Turn off logs" sends everything to /dev/null; stores and scripts
        // still get a path to write to.
        let log_file = if app.disable_logs {
            PathBuf::from("/dev/null")
        } else {
            Paths::get().game_log_file(id)
        };
        write_log_header(&log_file, &game, &settings)?;

        if !settings.before_launch_script_path.is_empty() {
            ctx.step.as_ref()("Running script");
            run_script(&settings.before_launch_script_path, &game, &log_file).await?;
        }

        let request = LaunchRequest {
            option,
            extra_args,
            offline: settings.offline_mode || !self.is_online(),
            skip_version_check,
            log_file: log_file.clone(),
            environment,
            settings: settings.clone(),
        };
        let plan = store.launch(&id.app_name, &request, ctx).await?;
        append_log(&log_file, &format!("Launch command: {}\n\n", plan.command));

        // The game writes its own output; past the cap, cut the log back to
        // the details above (Wine warnings in a loop can add 100 MiB/s).
        let header = std::fs::metadata(&log_file).map(|m| m.len()).unwrap_or(0);
        let mut child = process::spawn(&plan.command, Some(&log_file))?;
        let pid = child.id().context("game exited immediately")?;
        let bound = (!app.disable_logs).then(|| {
            self.spawn(log_cap::bound_log(
                log_file.clone(),
                header,
                log_cap::GAME_LOG_CAP,
            ))
        });
        let awake = self.spawn(Inhibitor::acquire_owned(format!("Playing {}", game.title)));
        let started = Instant::now();
        let started_at = util::now_rfc3339();
        self.running
            .lock()
            .insert(id.clone(), RunningGame { pid, started });
        drop(guard);
        self.statuses.set(id, StatusKind::Playing);
        self.userdata
            .record_launch(id, app.max_recent_games as usize);

        let discord = (app.discord_rpc && !app.discord_client_id.is_empty())
            .then(|| app.discord_client_id.clone());
        if let Some(client_id) = discord.clone() {
            let presence = self.presence.clone();
            let (title, store) = (game.title.clone(), id.runner.store_name());
            let started = util::unix_now() as i64;
            self.spawn(async move { presence.playing(client_id, title, store, started).await });
        }

        let b = self.clone();
        let id = id.clone();
        self.spawn(async move {
            let status = child.wait().await;
            if let Some(bound) = bound {
                bound.abort();
                let _ = log_cap::cut_back(&log_file, header, log_cap::GAME_LOG_CAP);
            }
            if let Ok(Some(awake)) = awake.await {
                awake.release().await;
            }
            if discord.is_some() {
                b.presence.clear().await;
            }
            if plan.track_wineserver && Self::target_platform(&game).needs_wine() {
                let _ = ember_runtime::wait_prefix(&settings).await;
            }
            let seconds = started.elapsed().as_secs();
            let crashed = status.map(|s| !s.success() && seconds < 10).unwrap_or(true);
            b.running.lock().remove(&id);
            b.statuses.clear(&id);
            b.userdata.record_session(&id, seconds);
            let session = crate::sessions::Session {
                game: id.key(),
                started: started_at,
                seconds,
                crashed,
            };
            if let Err(e) = crate::sessions::append(&session) {
                tracing::warn!("recording the session of {id}: {e:#}");
            }
            if let Some(store) = b.store(id.runner)
                && let Err(e) = store.after_exit(&id.app_name, seconds).await
            {
                tracing::warn!("after-exit hook of {id}: {e:#}");
            }
            if !settings.after_launch_script_path.is_empty() {
                let _ = run_script(&settings.after_launch_script_path, &game, &log_file).await;
            }
            if settings.auto_sync_saves
                && b.is_online()
                && let Err(e) = b.sync_saves(&id, SyncDirection::Upload).await
            {
                tracing::warn!("post-exit save sync of {id}: {e:#}");
            }
            if crashed {
                b.bus.notice(
                    Notice::new(
                        NoticeLevel::Error,
                        format!("{} closed unexpectedly", game.title),
                    )
                    .message("Check the game log for details.")
                    .keys(
                        "shell.notice.crashed",
                        Some("shell.notice.crashed_message"),
                        &[("title", &game.title)],
                    )
                    .game(id.clone()),
                );
            }
            b.bus.emit(Event::GameExited {
                id,
                seconds,
                crashed,
            });
        });
        Ok(())
    }

    /// Queue an install of `id` with default options into the default install
    /// folder, as Big Picture and bulk installs do (no install sheet).
    pub async fn install_with_defaults(&self, id: &GameId) -> Result<()> {
        let game = self
            .game(id)
            .with_context(|| format!("{} is not in the library", id.app_name))?;
        if let Some(launcher) = &game.third_party_managed_app {
            bail!("{} is installed through {launcher}", game.title);
        }
        let store = self
            .store(id.runner)
            .with_context(|| format!("{} is not available", id.runner.store_name()))?;
        let platform = game.default_platform();
        let info = store
            .install_info(
                &id.app_name,
                platform,
                &ember_core::InstallInfoQuery::default(),
            )
            .await
            .with_context(|| format!("Could not read the install options of {}", game.title))?;
        if let Some(launcher) = &info.third_party_managed_app {
            bail!("{} is installed through {launcher}", game.title);
        }
        let dir = ember_core::paths::expand_home(&self.settings.app().default_install_path);
        let request = ember_core::InstallRequest::with_defaults(&info, &dir, platform);
        self.enqueue_install(id.clone(), game.title.clone(), request, info.download_size);
        Ok(())
    }

    /// Stop a running game and everything in its prefix.
    pub async fn stop(&self, id: &GameId) -> Result<()> {
        let pid = self.running.lock().get(id).map(|r| r.pid);
        if let Some(pid) = pid {
            process::kill_group(pid, false);
        }
        if let Some(game) = self.game(id)
            && Self::target_platform(&game).needs_wine()
        {
            let app = self.settings.app();
            let _ = ember_runtime::kill_game_prefix(&app, &self.settings.game(id), &game).await;
        }
        if let Some(pid) = pid {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if self.is_running(id) {
                process::kill_group(pid, true);
            }
        }
        Ok(())
    }

    pub(crate) async fn after_install(&self, id: &GameId) {
        let Some(game) = self.game(id) else { return };
        // Native Linux GOG builds expect the Steam "scout" runtime (via umu).
        if id.runner == ember_core::Runner::Gog
            && game.installed_platform() == Some(InstallPlatform::Linux)
            && self.settings.game(id).steam_runtime.is_none()
        {
            self.settings.set_game_value(
                id,
                "steamRuntime",
                Some(Value::String("umu-scout".into())),
            );
        }
        let app = self.settings.app();
        let targets = ember_extras::shortcuts::ShortcutTargets::from_settings(&app);
        if (targets.desktop || targets.app_menu || targets.steam)
            && let Err(e) = ember_extras::shortcuts::add(&game, &app, targets).await
        {
            tracing::warn!("adding shortcuts for {id}: {e:#}");
        }
    }

    pub async fn uninstall(&self, id: &GameId, args: RemoveArgs) -> Result<()> {
        let game = self.installed_game(id)?;
        let store = self.store(id.runner).context("store not available")?;
        let _guard = self.statuses.guard(id, StatusKind::Uninstalling);
        let app = self.settings.app();
        let settings = self.settings.game(id);
        store.uninstall(&id.app_name, args).await?;
        if args.remove_prefix && Self::target_platform(&game).needs_wine() {
            let prefix = ember_runtime::prefix_path(&app, &settings, &game);
            remove_prefix_dir(&prefix, &app).await?;
        }
        let _ = ember_extras::shortcuts::remove(&game, &app).await;
        if args.remove_settings {
            self.settings.remove_game(id);
        }
        self.userdata.forget(id);
        self.bus.emit(Event::LibraryChanged(id.runner));
        self.bus.notice(
            Notice::new(NoticeLevel::Success, format!("{} uninstalled", game.title)).keys(
                "shell.notice.uninstalled",
                None,
                &[("title", &game.title)],
            ),
        );
        Ok(())
    }

    /// Move an installed game to another directory (`new_base/<folder>`).
    pub async fn move_install(&self, id: &GameId, new_base: PathBuf) -> Result<()> {
        let game = self.installed_game(id)?;
        let store = self.store(id.runner).context("store not available")?;
        let src = game.install_path().context("unknown install location")?;
        let folder = src.file_name().context("invalid install path")?.to_owned();
        let dst = new_base.join(folder);
        if dst == src {
            return Ok(());
        }
        if dst.exists() {
            bail!("{} already exists", dst.display());
        }
        let _guard = self.statuses.guard(id, StatusKind::Moving);
        let statuses = self.statuses.clone();
        let pid = id.clone();
        let (s, d) = (src.clone(), dst.clone());
        self.spawn_blocking(move || move_dir(&s, &d, &|p| statuses.progress(&pid, p)))
            .await??;
        store.set_install_path(&id.app_name, &dst).await?;
        self.bus.emit(Event::LibraryChanged(id.runner));
        let dest = new_base.display().to_string();
        self.bus.notice(
            Notice::new(
                NoticeLevel::Success,
                format!("{} moved to {dest}", game.title),
            )
            .keys(
                "shell.notice.moved",
                None,
                &[("title", &game.title), ("path", &dest)],
            ),
        );
        Ok(())
    }

    pub async fn repair(&self, id: &GameId) -> Result<()> {
        let game = self.installed_game(id)?;
        let store = self.store(id.runner).context("store not available")?;
        let _guard = self.statuses.guard(id, StatusKind::Repairing);
        let ctx = self.op_context(id, CancellationToken::new());
        match store.repair(&id.app_name, ctx).await? {
            Outcome::Done => {
                self.bus.emit(Event::LibraryChanged(id.runner));
                self.bus.notice(
                    Notice::new(
                        NoticeLevel::Success,
                        format!("{} verified and repaired", game.title),
                    )
                    .keys(
                        "shell.notice.repaired",
                        None,
                        &[("title", &game.title)],
                    ),
                );
                Ok(())
            }
            Outcome::Abort => Ok(()),
            Outcome::Error(e) => bail!(e),
        }
    }

    pub async fn import(&self, id: &GameId, path: &Path, platform: InstallPlatform) -> Result<()> {
        let store = self.store(id.runner).context("store not available")?;
        let _guard = self.statuses.guard(id, StatusKind::Importing);
        store.import(&id.app_name, path, platform).await?;
        self.bus.emit(Event::LibraryChanged(id.runner));
        Ok(())
    }

    /// Resolve the save folder (settings, else the store default) and sync.
    pub async fn sync_saves(&self, id: &GameId, direction: SyncDirection) -> Result<String> {
        let _guard = self.statuses.guard(id, StatusKind::SyncingSaves);
        self.sync_saves_inner(id, direction).await
    }

    async fn sync_saves_inner(&self, id: &GameId, direction: SyncDirection) -> Result<String> {
        let game = self.installed_game(id)?;
        let store = self.store(id.runner).context("store not available")?;
        if !store.supports_cloud_saves() || !game.cloud_save_enabled {
            bail!("{} has no cloud saves", game.title);
        }
        let mut settings = self.settings.game(id);
        if settings.saves_path.is_empty() && settings.gog_saves.is_empty() {
            let app = self.settings.app();
            let prefix = ember_runtime::prefix_path(&app, &settings, &game);
            let path = store.default_save_path(&id.app_name, Some(&prefix)).await?;
            self.settings
                .set_game_value(id, "savesPath", Some(Value::String(path.clone())));
            settings.saves_path = path;
        }
        // GOG games can have several named save locations; the GOG store
        // takes them as `name=path` lines.
        let saves = if settings.gog_saves.is_empty() {
            settings.saves_path.clone()
        } else {
            settings
                .gog_saves
                .iter()
                .map(|l| format!("{}={}", l.name, l.location))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let ctx = self.op_context(id, CancellationToken::new());
        store.sync_saves(&id.app_name, direction, &saves, ctx).await
    }

    /// Run a Windows program (or `winecfg`, `regedit`…) in the game's prefix.
    pub async fn run_in_prefix(&self, id: &GameId, argv: Vec<String>) -> Result<ExecResult> {
        let game = self.game(id).context("unknown game")?;
        let app = self.settings.app();
        let settings = self.settings.game(id);
        let target = Target {
            id,
            game: &game,
            platform: InstallPlatform::Windows,
        };
        let ctx = self.op_context(id, CancellationToken::new());
        ember_runtime::run_in_prefix(&app, &settings, target, argv, false, ctx).await
    }

    pub async fn winetricks(&self, id: &GameId, verbs: Vec<String>) -> Result<ExecResult> {
        let game = self.game(id).context("unknown game")?;
        let app = self.settings.app();
        let settings = self.settings.game(id);
        let _guard = self.statuses.guard(id, StatusKind::Winetricks);
        let target = Target {
            id,
            game: &game,
            platform: InstallPlatform::Windows,
        };
        let ctx = self.op_context(id, CancellationToken::new());
        ember_runtime::winetricks(&app, &settings, target, verbs, ctx)
            .await?
            .check()
    }

    /// Record that a game's files now live at `path` (files already moved).
    pub async fn set_install_path(&self, id: &GameId, path: &Path) -> Result<()> {
        let store = self.store(id.runner).context("store not available")?;
        store.set_install_path(&id.app_name, path).await?;
        self.bus.emit(Event::LibraryChanged(id.runner));
        Ok(())
    }

    /// Path of the last session log of a game.
    pub fn game_log(&self, id: &GameId) -> PathBuf {
        Paths::get().game_log_file(id)
    }
}

fn write_log_header(
    path: &Path,
    game: &GameInfo,
    settings: &ember_core::GameSettings,
) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut header = format!(
        "Ember {} — {} ({})\nStarted: {}\n",
        env!("CARGO_PKG_VERSION"),
        game.title,
        game.id(),
        util::now_rfc3339()
    );
    if let Some(p) = game.install_path() {
        header.push_str(&format!("Install path: {}\n", p.display()));
    }
    if settings.wine_version.is_set() {
        header.push_str(&format!(
            "Wine: {} ({})\nPrefix: {}\n",
            settings.wine_version.name, settings.wine_version.bin, settings.wine_prefix
        ));
    }
    header.push_str(&format!(
        "Settings: {}\n\n",
        serde_json::to_string(settings).unwrap_or_default()
    ));
    std::fs::write(path, header)?;
    Ok(())
}

fn append_log(path: &Path, text: &str) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = f.write_all(text.as_bytes());
    }
}

async fn run_script(script: &str, game: &GameInfo, log_file: &Path) -> Result<()> {
    let spec = CommandSpec::new(script)
        .env("EMBER_GAME_TITLE", game.title.clone())
        .env("EMBER_APP_NAME", game.app_name.clone())
        .env("EMBER_RUNNER", game.runner().id())
        .env("HEROIC_GAME_TITLE", game.title.clone())
        .env("HEROIC_APP_NAME", game.app_name.clone())
        .env("HEROIC_APP_RUNNER", game.runner().id());
    let res = process::run(&spec, RunOptions::default().log_file(log_file)).await?;
    if !res.success() {
        tracing::warn!("script {script} exited with {:?}", res.code);
    }
    Ok(())
}

/// Delete a prefix after sanity checks: never the shared prefix root, a
/// protected folder, or a directory that is not actually a Wine prefix.
async fn remove_prefix_dir(prefix: &Path, app: &ember_core::AppSettings) -> Result<()> {
    if !ember_runtime::is_prefix(prefix) {
        tracing::warn!("not deleting {}: not a Wine prefix", prefix.display());
        return Ok(());
    }
    ember_core::fs_guard::ensure_deletable(prefix, app)?;
    tokio::fs::remove_dir_all(prefix)
        .await
        .with_context(|| format!("Could not delete {}", prefix.display()))
}

/// Move a directory: rename when on the same filesystem, else copy with
/// progress and delete the source.
fn move_dir(src: &Path, dst: &Path, progress: &dyn Fn(InstallProgress)) -> Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    let total = dir_size(src);
    let mut done = 0u64;
    let started = Instant::now();
    copy_tree(src, dst, &mut |n| {
        done += n;
        let secs = started.elapsed().as_secs_f64().max(0.001);
        let speed = (done as f64 / secs) as u64;
        progress(InstallProgress {
            percent: (total > 0).then(|| done as f32 * 100.0 / total as f32),
            bytes_done: Some(done),
            bytes_total: Some(total),
            eta_secs: (speed > 0).then(|| total.saturating_sub(done) / speed),
            disk_speed: Some(speed),
            ..Default::default()
        });
    })?;
    std::fs::remove_dir_all(src)?;
    Ok(())
}

fn dir_size(path: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(path) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn copy_tree(src: &Path, dst: &Path, on_bytes: &mut dyn FnMut(u64)) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_tree(&entry.path(), &to, on_bytes)?;
        } else if ty.is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            std::os::unix::fs::symlink(target, &to)?;
        } else {
            let n = std::fs::copy(entry.path(), &to)
                .with_context(|| format!("copying {}", entry.path().display()))?;
            on_bytes(n);
        }
    }
    Ok(())
}
