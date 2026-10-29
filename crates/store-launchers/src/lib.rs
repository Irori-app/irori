//! Official third-party launchers ([`Runner::Launchers`]): Battle.net, the
//! EA app, Ubisoft Connect and the Rockstar Games Launcher.
//!
//! Irori never handles these launchers' accounts. It sets each launcher up
//! once (a prefix of its own with a Wine build the user picks, and the
//! vendor's official installer; on Windows the installer runs natively), and
//! lists the games the launcher installed by reading its local data
//! (`irori-launchers-data`). Games start through the launcher (its URI or
//! flags); the launcher installs, updates and removes them, so Irori doesn't.
//!
//! Everything is behind `iroriThirdPartyLaunchers`: while it is off the store
//! lists nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use irori_core::process::{self, CommandSpec};
use irori_core::{
    Event, EventBus, ExtraInfo, GameInfo, GameSettings, InstallInfo, InstallInfoQuery,
    InstallPlatform, InstallRequest, InstalledInfo, LaunchPlan, LaunchRequest, LoginFlow,
    OpContext, Outcome, Paths, RemoveArgs, Runner, SettingsFn, Store, UpdateRequest, UserInfo,
    WineInstallation, json_store, util,
};
use irori_launchers_data::{InstalledGame, Launcher, Registry, ScanReport};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

pub use irori_launchers_data::Launcher as LauncherKind;

const MANAGED_BY_LAUNCHER: &str =
    "This game belongs to its launcher; install, update and remove it there";

/// A launcher Irori set up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Setup {
    /// The launcher's own Wine prefix (empty on Windows).
    pub prefix: PathBuf,
    /// The Wine build it runs with (unset on Windows).
    #[serde(default)]
    pub wine: WineInstallation,
    /// RFC 3339.
    #[serde(default)]
    pub set_up_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct StateFile {
    setups: BTreeMap<Launcher, Setup>,
}

#[derive(Default)]
struct State {
    setups: BTreeMap<Launcher, Setup>,
    found: Vec<InstalledGame>,
    games: Vec<Arc<GameInfo>>,
    warnings: Vec<String>,
}

/// What the launchers page shows for one launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherStatus {
    pub launcher: Launcher,
    pub setup: Option<Setup>,
    /// The launcher's program exists.
    pub installed: bool,
    /// Games found for it.
    pub games: usize,
}

pub struct LaunchersStore {
    bus: EventBus,
    settings: SettingsFn,
    state_file: PathBuf,
    downloads: PathBuf,
    state: RwLock<State>,
    /// One setup or scan at a time.
    busy: tokio::sync::Mutex<()>,
}

impl LaunchersStore {
    pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self> {
        let paths = Paths::get();
        Self::with_dirs(
            bus,
            settings,
            paths.store_config_dir(Runner::Launchers),
            paths.store_cache_dir(Runner::Launchers),
        )
    }

    /// A store keeping its files in the given folders (tests).
    pub fn with_dirs(
        bus: EventBus,
        settings: SettingsFn,
        config_dir: PathBuf,
        cache_dir: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            bus,
            settings,
            state_file: config_dir.join("launchers.json"),
            downloads: cache_dir.join("installers"),
            state: RwLock::new(State::default()),
            busy: tokio::sync::Mutex::new(()),
        })
    }

    fn enabled(&self) -> bool {
        (self.settings)().third_party_launchers
    }

    /// Where a launcher's prefix goes: beside the other prefixes.
    pub fn default_prefix(&self, launcher: Launcher) -> PathBuf {
        let app = (self.settings)();
        irori_core::paths::expand_home(&app.default_wine_prefix_dir).join(launcher.display_name())
    }

    /// Every launcher with its setup and game count.
    pub fn statuses(&self) -> Vec<LauncherStatus> {
        let s = self.state.read();
        Launcher::ALL
            .into_iter()
            .map(|launcher| {
                let setup = s.setups.get(&launcher).cloned();
                LauncherStatus {
                    launcher,
                    installed: launcher_program(launcher, setup.as_ref())
                        .is_some_and(|p| p.exists()),
                    games: s.found.iter().filter(|g| g.launcher == launcher).count(),
                    setup,
                }
            })
            .collect()
    }

    /// Files Irori couldn't read during the last scan.
    pub fn warnings(&self) -> Vec<String> {
        self.state.read().warnings.clone()
    }

    async fn save(&self) -> Result<()> {
        let file = StateFile {
            setups: self.state.read().setups.clone(),
        };
        let path = self.state_file.clone();
        tokio::task::spawn_blocking(move || json_store::write(&path, &file))
            .await
            .context("saving stopped")?
            .context("Could not save the launchers")
    }

    /// Download the launcher's official installer and run it: inside a new
    /// prefix with `wine` on Linux and macOS, natively on Windows. Waits for
    /// the installer to finish, then lists the launcher's games.
    pub async fn set_up(
        &self,
        launcher: Launcher,
        wine: Option<WineInstallation>,
        ctx: &OpContext,
    ) -> Result<()> {
        let _one = self.busy.lock().await;
        (ctx.step)("Downloading");
        let installer = self.downloads.join(format!("{}-setup.exe", launcher.id()));
        irori_runtime::download::download(
            launcher.installer_url(),
            &installer,
            &ctx.progress,
            &ctx.cancel,
        )
        .await
        .with_context(|| {
            format!(
                "Could not download the {} installer",
                launcher.display_name()
            )
        })?;
        (ctx.step)("Installing");
        let setup = if cfg!(windows) {
            let result = process::run(
                &CommandSpec::new(&installer),
                process::RunOptions::default().cancel(ctx.cancel.clone()),
            )
            .await?;
            if result.aborted {
                bail!("The installer was stopped");
            }
            Setup {
                prefix: PathBuf::new(),
                wine: WineInstallation::default(),
                set_up_at: util::now_rfc3339(),
            }
        } else {
            let wine = wine
                .filter(WineInstallation::is_set)
                .context("Choose a Wine build for the launcher")?;
            let prefix = self.default_prefix(launcher);
            let app = (self.settings)();
            let settings = game_settings(&app.game, &wine, &prefix);
            let wc = irori_runtime::WineContext::new(
                wine.clone(),
                prefix.clone(),
                false,
                irori_runtime::runtimes::umu_path(),
            );
            let mut cmd =
                irori_runtime::prefix::WineCommand::new([installer.display().to_string()]);
            cmd.wait = true;
            cmd.initialize_prefix = true;
            irori_runtime::prefix::run_wine(&app, &settings, &wc, cmd, ctx)
                .await
                .with_context(|| format!("The {} installer failed", launcher.display_name()))?;
            Setup {
                prefix,
                wine,
                set_up_at: util::now_rfc3339(),
            }
        };
        self.state.write().setups.insert(launcher, setup);
        self.save().await?;
        let _ = tokio::fs::remove_file(&installer).await;
        drop(_one);
        self.rescan().await;
        Ok(())
    }

    /// Start the launcher itself (to sign in or install games there).
    pub async fn open(&self, launcher: Launcher) -> Result<()> {
        let setup = self.state.read().setups.get(&launcher).cloned();
        let program = launcher_program(launcher, setup.as_ref())
            .filter(|p| p.exists())
            .with_context(|| format!("{} isn't set up yet", launcher.display_name()))?;
        let spec = self.command(launcher, setup.as_ref(), &program, Vec::new())?;
        process::spawn(&spec, None)?;
        Ok(())
    }

    /// Forget a launcher's setup. Its prefix and games stay on disk.
    pub async fn forget(&self, launcher: Launcher) -> Result<()> {
        self.state.write().setups.remove(&launcher);
        self.save().await?;
        self.rescan().await;
        Ok(())
    }

    /// The command that runs `program` of `launcher` with `args`.
    fn command(
        &self,
        launcher: Launcher,
        setup: Option<&Setup>,
        program: &Path,
        args: Vec<String>,
    ) -> Result<CommandSpec> {
        if cfg!(windows) {
            return Ok(CommandSpec::new(program).args(args));
        }
        let setup =
            setup.with_context(|| format!("{} isn't set up yet", launcher.display_name()))?;
        let app = (self.settings)();
        let settings = game_settings(&app.game, &setup.wine, &setup.prefix);
        let wc = irori_runtime::WineContext::new(
            setup.wine.clone(),
            setup.prefix.clone(),
            false,
            irori_runtime::runtimes::umu_path(),
        );
        let mut argv = vec![program.display().to_string()];
        argv.extend(args);
        let mut cmd = irori_runtime::prefix::WineCommand::new(argv);
        cmd.skip_prefix_check = true;
        cmd.cwd = program.parent().map(Path::to_path_buf);
        Ok(irori_runtime::prefix::wine_spec(
            &app,
            &settings,
            &wc,
            &cmd,
            &irori_runtime::env::HostEnv::current(),
        ))
    }

    /// Read every set-up launcher's local data again.
    async fn rescan(&self) {
        let setups = self.state.read().setups.clone();
        let report = tokio::task::spawn_blocking(move || scan(&setups))
            .await
            .unwrap_or_default();
        let games = report
            .games
            .iter()
            .map(|g| Arc::new(game_info(g)))
            .collect();
        {
            let mut s = self.state.write();
            s.found = report.games;
            s.games = games;
            s.warnings = report.warnings;
        }
        self.bus.emit(Event::LibraryChanged(Runner::Launchers));
    }
}

/// Launch settings for a launcher's prefix and Wine build.
fn game_settings(defaults: &GameSettings, wine: &WineInstallation, prefix: &Path) -> GameSettings {
    GameSettings {
        wine_version: wine.clone(),
        wine_prefix: prefix.display().to_string(),
        ..defaults.clone()
    }
}

/// The launcher's program on this computer.
fn launcher_program(launcher: Launcher, setup: Option<&Setup>) -> Option<PathBuf> {
    if cfg!(windows) {
        return Some(PathBuf::from(launcher.windows_exe_path()));
    }
    setup.map(|s| launcher.exe_in_prefix(&s.prefix))
}

/// Read the launchers' data. Blocking.
fn scan(setups: &BTreeMap<Launcher, Setup>) -> ScanReport {
    let mut report = ScanReport::default();
    if cfg!(windows) {
        let registry = windows_registry();
        for launcher in Launcher::ALL {
            report.merge(launcher.scan_drive(Path::new("C:\\"), &registry));
        }
    } else {
        for (launcher, setup) in setups {
            report.merge(launcher.scan_prefix(&setup.prefix));
        }
    }
    report
}

/// The machine-wide registry keys launchers write, exported with `reg`
/// (Windows only; empty elsewhere or when `reg` fails).
fn windows_registry() -> Registry {
    #[cfg(windows)]
    {
        let out = std::env::temp_dir().join("irori-launchers.reg");
        let mut text = String::new();
        for key in [
            r"HKLM\SOFTWARE\WOW6432Node\Ubisoft",
            r"HKLM\SOFTWARE\WOW6432Node\Rockstar Games",
            r"HKLM\SOFTWARE\Rockstar Games",
        ] {
            // A short system tool on the blocking scan thread, not a game or
            // a long-running tool, so `irori_core::process` isn't needed.
            let ok = std::process::Command::new("reg")
                .args(["export", key])
                .arg(&out)
                .arg("/y")
                .status()
                .is_ok_and(|s| s.success());
            if ok && let Ok(bytes) = std::fs::read(&out) {
                text.push_str(&decode_reg(&bytes));
                text.push('\n');
            }
        }
        let _ = std::fs::remove_file(&out);
        Registry::parse(&text)
    }
    #[cfg(not(windows))]
    {
        Registry::default()
    }
}

/// `reg export` writes UTF-16 with a byte-order mark.
#[cfg(windows)]
fn decode_reg(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xff, 0xfe]) {
        let (pairs, _) = bytes[2..].as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// What the library shows for a launcher's game.
fn game_info(g: &InstalledGame) -> GameInfo {
    GameInfo {
        runner: Some(Runner::Launchers),
        app_name: g.app_name(),
        title: g.title.clone(),
        publisher: Some(g.launcher.display_name().to_string()),
        is_installed: true,
        install: Some(InstalledInfo {
            install_path: g.install_dir.display().to_string(),
            executable: g.executable.clone().unwrap_or_default(),
            platform: InstallPlatform::Windows,
            ..Default::default()
        }),
        installable: false,
        third_party_managed_app: Some(g.launcher.display_name().to_string()),
        is_ea_managed: g.launcher == Launcher::EaApp,
        is_ubisoft_managed: g.launcher == Launcher::Ubisoft,
        platforms: vec![InstallPlatform::Windows],
        folder_name: g
            .install_dir
            .to_string_lossy()
            .rsplit(['\\', '/'])
            .find(|s| !s.is_empty())
            .map(str::to_string),
        ..Default::default()
    }
    .finalize()
}

#[async_trait]
impl Store for LaunchersStore {
    fn runner(&self) -> Runner {
        Runner::Launchers
    }

    async fn init(&self) -> Result<()> {
        let path = self.state_file.clone();
        let file: StateFile =
            tokio::task::spawn_blocking(move || json_store::read_or_default(&path))
                .await
                .context("loading the launchers stopped")?;
        self.state.write().setups = file.setups;
        self.rescan().await;
        Ok(())
    }

    fn user(&self) -> Option<UserInfo> {
        Some(UserInfo {
            account_id: "local".into(),
            display_name: "Local".into(),
        })
    }

    fn login_flow(&self) -> LoginFlow {
        LoginFlow::None
    }

    async fn login(&self, _input: &str) -> Result<UserInfo> {
        self.user().context("no account")
    }

    async fn logout(&self) -> Result<()> {
        Ok(())
    }

    async fn refresh(&self, _full: bool) -> Result<()> {
        self.rescan().await;
        Ok(())
    }

    fn games(&self) -> Vec<Arc<GameInfo>> {
        if !self.enabled() {
            return Vec::new();
        }
        self.state.read().games.clone()
    }

    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        self.games().into_iter().find(|g| g.app_name == app_name)
    }

    async fn extra_info(&self, _app_name: &str) -> Result<ExtraInfo> {
        Ok(ExtraInfo::default())
    }

    async fn install_info(
        &self,
        _app_name: &str,
        _platform: InstallPlatform,
        _query: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        bail!(MANAGED_BY_LAUNCHER)
    }

    async fn updatable_games(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn install(&self, _: &str, _: &InstallRequest, _: OpContext) -> Result<Outcome> {
        Ok(Outcome::Error(MANAGED_BY_LAUNCHER.into()))
    }

    async fn update(&self, _: &str, _: &UpdateRequest, _: OpContext) -> Result<Outcome> {
        Ok(Outcome::Error(MANAGED_BY_LAUNCHER.into()))
    }

    async fn repair(&self, _: &str, _: OpContext) -> Result<Outcome> {
        Ok(Outcome::Error(MANAGED_BY_LAUNCHER.into()))
    }

    async fn uninstall(&self, _: &str, _: RemoveArgs) -> Result<()> {
        bail!(MANAGED_BY_LAUNCHER)
    }

    async fn import(&self, _: &str, _: &Path, _: InstallPlatform) -> Result<()> {
        bail!(MANAGED_BY_LAUNCHER)
    }

    async fn set_install_path(&self, _: &str, _: &Path) -> Result<()> {
        bail!(MANAGED_BY_LAUNCHER)
    }

    async fn is_available(&self, app_name: &str) -> bool {
        self.game(app_name).is_some()
    }

    /// Start the game through its launcher. The session follows the game's
    /// own processes (see the services layer), not the launcher's.
    async fn launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        _ctx: OpContext,
    ) -> Result<LaunchPlan> {
        let game = self
            .state
            .read()
            .found
            .iter()
            .find(|g| g.app_name() == app_name)
            .cloned()
            .context("This game is no longer listed by its launcher")?;
        let setup = self.state.read().setups.get(&game.launcher).cloned();
        let program = launcher_program(game.launcher, setup.as_ref())
            .filter(|p| p.exists())
            .with_context(|| {
                format!(
                    "{} isn't installed; set it up again on the Stores page",
                    game.launcher.display_name()
                )
            })?;
        let mut args = game.launch.launcher_args();
        args.extend(request.extra_args.iter().cloned());
        let command = self.command(game.launcher, setup.as_ref(), &program, args)?;
        Ok(LaunchPlan {
            command,
            track_wineserver: false,
            watch: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_core::AppSettings;
    use irori_launchers_data::LaunchSpec;

    fn store(dir: &Path, enabled: bool) -> Arc<LaunchersStore> {
        let app = Arc::new(AppSettings {
            third_party_launchers: enabled,
            default_wine_prefix_dir: dir.join("prefixes").display().to_string(),
            ..Default::default()
        });
        LaunchersStore::with_dirs(
            EventBus::new(),
            Arc::new(move || app.clone()),
            dir.join("config"),
            dir.join("cache"),
        )
    }

    #[test]
    fn games_show_their_launcher_and_folder() {
        let g = InstalledGame {
            launcher: Launcher::BattleNet,
            id: "fenris".into(),
            title: "Diablo IV".into(),
            install_dir: PathBuf::from("C:\\Program Files (x86)\\Diablo IV"),
            executable: None,
            launch: LaunchSpec::LauncherArgs(vec!["--exec=launch Fen".into()]),
        };
        let info = game_info(&g);
        assert_eq!(info.app_name, "battlenet:fenris");
        assert_eq!(info.third_party_managed_app.as_deref(), Some("Battle.net"));
        assert_eq!(info.folder_name.as_deref(), Some("Diablo IV"));
        assert_eq!(info.runner(), Runner::Launchers);
        assert!(!info.installable);
    }

    #[tokio::test]
    async fn lists_nothing_until_turned_on_and_set_up() {
        let dir = tempfile::tempdir().unwrap();
        let off = store(dir.path(), false);
        off.init().await.unwrap();
        assert!(off.games().is_empty());
        let on = store(dir.path(), true);
        on.init().await.unwrap();
        assert!(on.games().is_empty());
        let statuses = on.statuses();
        assert_eq!(statuses.len(), 4);
        assert!(statuses.iter().all(|s| s.setup.is_none() && s.games == 0));
        assert!(
            on.default_prefix(Launcher::EaApp)
                .ends_with("prefixes/EA app")
        );
        assert!(
            on.launch(
                "battlenet:x",
                &dummy_request(dir.path()),
                OpContext::detached()
            )
            .await
            .is_err()
        );
    }

    fn dummy_request(dir: &Path) -> LaunchRequest {
        LaunchRequest {
            settings: GameSettings::default(),
            environment: Default::default(),
            option: None,
            extra_args: Vec::new(),
            offline: false,
            skip_version_check: false,
            log_file: dir.join("log"),
        }
    }
}
