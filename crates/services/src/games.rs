//! Game operations that span stores and runtime: launching and tracking
//! sessions, uninstall, move, repair, import, cloud saves and Wine tools.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, Result, bail};
use irori_core::log_cap;
use irori_core::process::{self, CancellationToken, CommandSpec, ExecResult, RunOptions};
use irori_core::{
    Event, GameId, GameInfo, InstallPlatform, InstallProgress, LaunchOption, LaunchRequest, Notice,
    NoticeLevel, Outcome, Paths, RemoveArgs, SaveConflict, StatusKind, SyncDirection, util,
};
use irori_runtime::Target;
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

/// A game process Irori started and is tracking.
#[derive(Debug, Clone)]
pub struct RunningGame {
    pub pid: u32,
    pub started: Instant,
    /// The user asked to stop it, so a quick non-zero exit is not a crash.
    pub stopping: bool,
    /// Follows a game another program started (Steam); stopping goes through it.
    pub watch: Option<Arc<dyn irori_core::GameWatch>>,
}

/// Whether a session counts as a failed start: the game exited with an error
/// within seconds of starting and nobody asked it to stop. A clean exit is
/// never one, since launchers (EA app, Ubisoft Connect) hand off and exit 0.
pub(crate) fn failed_start(
    status: Option<std::process::ExitStatus>,
    seconds: u64,
    stopped: bool,
) -> bool {
    if stopped {
        return false;
    }
    status.is_none_or(|s| !s.success() && seconds < FAILED_START_SECONDS)
}

/// Sessions shorter than this that end in an error count as failed starts.
const FAILED_START_SECONDS: u64 = 10;

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

    /// Refuse to touch the files of a game another launcher manages (Steam),
    /// before anything moves or is deleted.
    fn ensure_managed(id: &GameId) -> Result<()> {
        anyhow::ensure!(
            id.runner.manages_installs(),
            "{} installs, updates and removes its own games. Use {} for that.",
            id.runner.store_name(),
            id.runner.store_name()
        );
        Ok(())
    }

    /// Ask the client of a store that installs its own games (Steam) to
    /// install, remove, verify or show `id`. Irori's files are untouched.
    pub async fn client_action(&self, id: &GameId, action: irori_core::ClientAction) -> Result<()> {
        anyhow::ensure!(
            !id.runner.manages_installs(),
            "Irori installs {} games itself",
            id.runner.store_name()
        );
        let store = self
            .store(id.runner)
            .with_context(|| format!("{} is not available", id.runner.store_name()))?;
        store.client_action(&id.app_name, action).await
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
                // Playing on would build on saves the next upload overwrites,
                // or that overwrite the cloud's; the caller asks the user.
                if SaveConflict::of(&e).is_some() {
                    return Err(e);
                }
                tracing::warn!("pre-launch save sync of {id}: {e:#}");
            }
        }

        ctx.step.as_ref()("Preparing");
        // Another launcher starts its games with its own settings; Irori's
        // Wine, wrappers and variables would only apply to the opener.
        let environment = if id.runner.manages_installs() {
            irori_runtime::prepare_launch(&app, &settings, target, &ctx).await?
        } else {
            Default::default()
        };

        // "Turn off logs" sends everything to /dev/null; stores and scripts
        // still get a path to write to.
        let log_file = if app.disable_logs {
            PathBuf::from(irori_core::platform::null_device())
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
        self.running.lock().insert(
            id.clone(),
            RunningGame {
                pid,
                started,
                stopping: false,
                watch: plan.watch.clone(),
            },
        );
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

        // A third-party launcher's games are followed by their own processes;
        // a store whose client starts the game says how to follow it.
        let probe: Option<crate::follow::Probe> = match &plan.watch {
            Some(watch) => {
                let watch = watch.clone();
                Some(Arc::new(move || watch.running()))
            }
            None => (id.runner == irori_core::Runner::Launchers)
                .then(|| game.install.as_ref().map(|i| i.install_path.clone()))
                .flatten()
                .filter(|p| !p.is_empty())
                .map(crate::follow::folder_probe),
        };
        let b = self.clone();
        let id = id.clone();
        self.spawn(async move {
            let status = match probe {
                Some(probe) => crate::follow::follow(&mut child, probe).await,
                None => child.wait().await.ok(),
            };
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
                let _ = irori_runtime::wait_prefix(&settings).await;
            }
            let seconds = started.elapsed().as_secs();
            let stopped = b.running.lock().remove(&id).is_some_and(|r| r.stopping);
            let crashed = failed_start(status, seconds, stopped);
            let exit_code = status.and_then(|s| s.code());
            b.statuses.clear(&id);
            b.userdata.record_session(&id, seconds);
            let session = crate::sessions::Session {
                game: id.key(),
                started: started_at,
                seconds,
                crashed,
                exit_code: exit_code.filter(|_| crashed),
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
                if SaveConflict::of(&e).is_some() {
                    b.bus.notice(
                        Notice::new(
                            NoticeLevel::Warning,
                            format!("Saves of {} weren't uploaded", game.title),
                        )
                        .message(format!("{e:#}"))
                        .keys(
                            "shell.notice.saves_conflict",
                            Some("shell.notice.saves_conflict_message"),
                            &[("title", &game.title)],
                        )
                        .game(id.clone()),
                    );
                }
            }
            b.bus.emit(Event::GameExited {
                id,
                seconds,
                crashed,
                exit_code,
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
        if !id.runner.manages_installs() {
            return self
                .client_action(id, irori_core::ClientAction::Install)
                .await;
        }
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
                &irori_core::InstallInfoQuery::default(),
            )
            .await
            .with_context(|| format!("Could not read the install options of {}", game.title))?;
        if let Some(launcher) = &info.third_party_managed_app {
            bail!("{} is installed through {launcher}", game.title);
        }
        let dir = irori_core::paths::expand_home(&self.settings.app().default_install_path);
        let request = irori_core::InstallRequest::with_defaults(&info, &dir, platform);
        self.enqueue_install(id.clone(), game.title.clone(), request, info.download_size);
        Ok(())
    }

    /// Stop a running game and everything in its prefix.
    pub async fn stop(&self, id: &GameId) -> Result<()> {
        let (pid, watch) = self
            .running
            .lock()
            .get_mut(id)
            .map(|r| {
                r.stopping = true;
                (Some(r.pid), r.watch.clone())
            })
            .unwrap_or_default();
        if let Some(pid) = pid {
            process::kill_group(pid, false);
        }
        if let Some(watch) = &watch {
            let w = watch.clone();
            let _ = tokio::task::spawn_blocking(move || w.stop(false)).await;
        }
        if let Some(game) = self.game(id)
            && Self::target_platform(&game).needs_wine()
        {
            let app = self.settings.app();
            let _ = irori_runtime::kill_game_prefix(&app, &self.settings.game(id), &game).await;
        }
        if let Some(pid) = pid {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if self.is_running(id) {
                process::kill_group(pid, true);
            }
        }
        if let Some(watch) = watch {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if self.is_running(id) {
                let _ = tokio::task::spawn_blocking(move || watch.stop(true)).await;
            }
        }
        Ok(())
    }

    pub(crate) async fn after_install(&self, id: &GameId) {
        let Some(game) = self.game(id) else { return };
        // Native Linux GOG builds expect the Steam "scout" runtime (via umu).
        if id.runner == irori_core::Runner::Gog
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
        let targets = irori_extras::shortcuts::ShortcutTargets::from_settings(&app);
        if (targets.desktop || targets.app_menu || targets.steam)
            && let Err(e) = irori_extras::shortcuts::add(&game, &app, targets).await
        {
            tracing::warn!("adding shortcuts for {id}: {e:#}");
        }
    }

    pub async fn uninstall(&self, id: &GameId, args: RemoveArgs) -> Result<()> {
        Self::ensure_managed(id)?;
        let game = self.installed_game(id)?;
        let store = self.store(id.runner).context("store not available")?;
        let _guard = self.statuses.guard(id, StatusKind::Uninstalling);
        let app = self.settings.app();
        let settings = self.settings.game(id);
        if args.remove_prefix && Self::target_platform(&game).needs_wine() {
            let prefix = irori_runtime::prefix_path(&app, &settings, &game);
            anyhow::ensure!(
                !prefix.join("cxbottle.conf").is_file(),
                "This CrossOver bottle may be shared by other games. Keep the prefix when uninstalling, and manage bottles in CrossOver."
            );
            anyhow::ensure!(
                !irori_runtime::prefix_is_shared(&app, &prefix),
                "{} is shared by other games. Keep the prefix when uninstalling.",
                prefix.display()
            );
        }
        store.uninstall(&id.app_name, args).await?;
        if args.remove_prefix && Self::target_platform(&game).needs_wine() {
            let prefix = irori_runtime::prefix_path(&app, &settings, &game);
            remove_prefix_dir(&prefix, &app).await?;
        }
        let _ = irori_extras::shortcuts::remove(&game, &app).await;
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
        Self::ensure_managed(id)?;
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
        let moved = self
            .spawn_blocking(move || move_dir(&s, &d, &|p| statuses.progress(&pid, p)))
            .await??;
        // Point the install at the complete copy before the original goes,
        // so no failure from here on leaves the game pointing at a half
        // deleted folder.
        store.set_install_path(&id.app_name, &dst).await?;
        if moved == Moved::Copied {
            let app = self.settings.app();
            let old = src.clone();
            let removed = self
                .spawn_blocking(move || {
                    irori_core::fs_guard::ensure_deletable(&old, &app)?;
                    std::fs::remove_dir_all(&old)
                        .with_context(|| format!("Could not delete {}", old.display()))
                })
                .await?;
            if let Err(e) = removed {
                tracing::warn!("removing the old copy of {id}: {e:#}");
                let old = src.display().to_string();
                self.bus.notice(
                    Notice::new(
                        NoticeLevel::Warning,
                        format!("The old copy of {} is still at {old}", game.title),
                    )
                    .message(format!("{e:#}"))
                    .keys(
                        "shell.notice.move_leftover",
                        None,
                        &[("title", &game.title), ("path", &old)],
                    ),
                );
            }
        }
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
        Self::ensure_managed(id)?;
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
        Self::ensure_managed(id)?;
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
            let prefix = irori_runtime::prefix_path(&app, &settings, &game);
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
        irori_runtime::run_in_prefix(&app, &settings, target, argv, false, ctx).await
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
        irori_runtime::winetricks(&app, &settings, target, verbs, ctx)
            .await?
            .check()
    }

    /// Run protontricks on the Proton prefix Steam made for one of its games
    /// (Linux): its window when `verbs` is empty, else the verbs unattended.
    pub async fn protontricks(&self, id: &GameId, verbs: Vec<String>) -> Result<ExecResult> {
        anyhow::ensure!(cfg!(target_os = "linux"), "Protontricks runs on Linux only");
        let game = self.installed_game(id)?;
        anyhow::ensure!(
            id.runner == irori_core::Runner::Steam,
            "Protontricks works with Steam games. Use Winetricks for {}.",
            game.title
        );
        let install = game.install_path().context("unknown install location")?;
        let prefix = irori_runtime::protontricks::prefix_of(&install, &id.app_name)
            .context("unknown install location")?;
        if !tokio::fs::try_exists(&prefix).await.unwrap_or(false) {
            bail!(
                "Steam hasn't made a Proton prefix for {} yet. Play it once with Proton in Steam first.",
                game.title
            );
        }
        let steam_dir = irori_runtime::locations::steam_path(&self.settings.app());
        let _guard = self.statuses.guard(id, StatusKind::Winetricks);
        let ctx = self.op_context(id, CancellationToken::new());
        irori_runtime::protontricks::run(&steam_dir, &id.app_name, &verbs, &ctx)
            .await?
            .check()
    }

    /// Record that a game's files now live at `path` (files already moved).
    pub async fn set_install_path(&self, id: &GameId, path: &Path) -> Result<()> {
        Self::ensure_managed(id)?;
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
    settings: &irori_core::GameSettings,
) -> Result<()> {
    if path == Path::new(irori_core::platform::null_device()) {
        return Ok(());
    }
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut header = format!(
        "Irori {} — {} ({})\nStarted: {}\n",
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
        .env("IRORI_GAME_TITLE", game.title.clone())
        .env("IRORI_APP_NAME", game.app_name.clone())
        .env("IRORI_RUNNER", game.runner().id())
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
async fn remove_prefix_dir(prefix: &Path, app: &irori_core::AppSettings) -> Result<()> {
    anyhow::ensure!(
        !prefix.join("cxbottle.conf").is_file(),
        "Manage shared CrossOver bottles in CrossOver"
    );
    if !irori_runtime::is_prefix(prefix) {
        tracing::warn!("not deleting {}: not a Wine prefix", prefix.display());
        return Ok(());
    }
    irori_core::fs_guard::ensure_deletable(prefix, app)?;
    tokio::fs::remove_dir_all(prefix)
        .await
        .with_context(|| format!("Could not delete {}", prefix.display()))
}

/// How [`move_dir`] moved a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Moved {
    /// Renamed in place: same filesystem, nothing left behind.
    Renamed,
    /// Copied to another filesystem; the source is still there for the
    /// caller to delete once the copy is in use.
    Copied,
}

/// Move a directory: rename it on the same filesystem, else copy it with
/// progress. A failed copy removes what it wrote and leaves the source as
/// it was. Any rename error other than crossing filesystems (files in use,
/// permissions) is reported rather than worked around with a copy.
fn move_dir(src: &Path, dst: &Path, progress: &dyn Fn(InstallProgress)) -> Result<Moved> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Could not create {}", parent.display()))?;
    }
    match std::fs::rename(src, dst) {
        Ok(()) => return Ok(Moved::Renamed),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {}
        Err(e) => {
            return Err(e)
                .with_context(|| format!("Could not move {} to {}", src.display(), dst.display()));
        }
    }
    copy_dir(src, dst, progress)?;
    Ok(Moved::Copied)
}

/// Copy `src` to `dst`, which must not exist yet, with progress. On failure
/// remove what was written; `src` is never touched.
fn copy_dir(src: &Path, dst: &Path, progress: &dyn Fn(InstallProgress)) -> Result<()> {
    let total = dir_size(src);
    if let Some(free) = free_space(dst)
        && free < total
    {
        bail!(
            "Not enough space: moving needs {} but only {} is free",
            util::format_bytes(total),
            util::format_bytes(free)
        );
    }
    let mut done = 0u64;
    let started = Instant::now();
    let copied = copy_tree(src, dst, &mut |n| {
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
    });
    if let Err(e) = copied {
        // `dst` did not exist before the copy, so everything in it is ours.
        if let Err(cleanup) = std::fs::remove_dir_all(dst) {
            tracing::warn!("removing the partial copy {}: {cleanup}", dst.display());
        }
        return Err(e);
    }
    Ok(())
}

/// Free bytes on the filesystem that will hold `path`.
pub(crate) fn free_space(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    fs4::available_space(existing).ok()
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
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &to)?;
            #[cfg(windows)]
            if entry.path().is_dir() {
                std::os::windows::fs::symlink_dir(target, &to)?;
            } else {
                std::os::windows::fs::symlink_file(target, &to)?;
            }
        } else {
            let n = std::fs::copy(entry.path(), &to)
                .with_context(|| format!("copying {}", entry.path().display()))?;
            on_bytes(n);
        }
    }
    Ok(())
}

#[cfg(test)]
mod platform_tests {
    use super::*;

    #[test]
    fn moves_by_renaming_on_the_same_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("Game"), dir.path().join("to/Game"));
        std::fs::create_dir_all(src.join("data")).unwrap();
        std::fs::write(src.join("data/save.dat"), "x").unwrap();
        assert_eq!(move_dir(&src, &dst, &|_| {}).unwrap(), Moved::Renamed);
        assert!(!src.exists());
        assert_eq!(std::fs::read(dst.join("data/save.dat")).unwrap(), b"x");
    }

    #[test]
    fn a_failed_move_leaves_the_source_whole() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("Game");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.bin"), "a").unwrap();
        // The destination's parent is a file, so the move cannot even start.
        let file = dir.path().join("a-file");
        std::fs::write(&file, "").unwrap();
        assert!(move_dir(&src, &file.join("Game"), &|_| {}).is_err());
        assert!(src.join("a.bin").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_copy_removes_what_it_wrote() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("Game"), dir.path().join("to/Game"));
        std::fs::create_dir_all(src.join("data")).unwrap();
        std::fs::write(src.join("data/a.bin"), "a").unwrap();
        let locked = src.join("data/z.bin");
        std::fs::write(&locked, "z").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&locked).is_ok() {
            return; // running as root: nothing is unreadable
        }
        assert!(copy_dir(&src, &dst, &|_| {}).is_err());
        assert!(!dst.exists(), "the partial copy is gone");
        assert!(src.join("data/a.bin").is_file(), "the source is untouched");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[tokio::test]
    async fn uninstall_never_removes_shared_crossover_bottles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cxbottle.conf"), "[Bottle]").unwrap();
        std::fs::create_dir_all(dir.path().join("drive_c")).unwrap();
        let error = remove_prefix_dir(dir.path(), &irori_core::AppSettings::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("CrossOver"));
        assert!(dir.path().join("cxbottle.conf").exists());
    }
}

#[cfg(test)]
mod tests {
    use super::failed_start;
    use std::process::ExitStatus;

    fn exit(code: i32) -> ExitStatus {
        #[cfg(unix)]
        {
            std::os::unix::process::ExitStatusExt::from_raw(code << 8)
        }
        #[cfg(windows)]
        {
            std::os::windows::process::ExitStatusExt::from_raw(code as u32)
        }
    }

    #[test]
    fn counts_only_unasked_quick_errors_as_failed_starts() {
        assert!(failed_start(Some(exit(1)), 2, false));
        assert!(failed_start(None, 30, false), "lost track of the process");
        assert!(!failed_start(Some(exit(0)), 1, false), "launchers exit 0");
        assert!(
            !failed_start(Some(exit(1)), 600, false),
            "played, then failed"
        );
        assert!(!failed_start(Some(exit(1)), 2, true), "the user stopped it");
        assert!(!failed_start(None, 2, true));
    }
}
