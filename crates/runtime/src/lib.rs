//! Running Windows games on Linux: Wine/Proton discovery and downloads, prefix
//! management, DXVK/VKD3D, umu and the Steam runtime, and assembly of the
//! launch environment (env vars and wrapper chain) handed to stores.
//!
//! The top-level functions are the contract used by `ember-services`; the
//! modules expose the building blocks (Wine manager, tools, system info) for
//! the settings UI.

pub mod discovery;
pub mod download;
pub mod env;
pub mod hooks;
pub mod locations;
pub mod manager;
pub mod prefix;
pub mod runtimes;
pub mod sysinfo;
pub mod tools;
pub mod vdf;
pub mod wrappers;

use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use ember_core::process::{CommandSpec, ExecResult};
use ember_core::{
    AppSettings, GameId, GameInfo, GameSettings, InstallPlatform, LaunchEnvironment, OpContext,
    WineInstallation, WineKind, WineLaunch,
};
use once_cell::sync::Lazy;
use parking_lot::Mutex;

pub use discovery::{DiscoveredWine, WineSource, recommended};
pub use download::{Cancelled, is_cancelled};
pub use hooks::{LaunchHooks, set_hooks};
pub use manager::{Release, ReleaseKind, available_releases};
pub use prefix::WineContext;
pub use tools::DxTool;

use env::HostEnv;
use prefix::WineCommand;
use runtimes::RuntimeName;
use wrappers::WrapperParts;

/// The game being launched or operated on.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    pub id: &'a GameId,
    pub game: &'a GameInfo,
    /// Platform of the installed build.
    pub platform: InstallPlatform,
}

impl Target<'_> {
    /// Identifier used for `SteamGameId` (`heroic-<folder>`).
    fn folder_id(&self) -> String {
        self.game
            .folder_name
            .clone()
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| "0".into())
    }
}

/// Prefix and build of recent launches, keyed by a fingerprint of the
/// settings they used, so [`kill_prefix`]/[`wait_prefix`] (which only get the
/// settings) can find a default per-game prefix.
static ACTIVE: Lazy<Mutex<HashMap<u64, (WineInstallation, PathBuf)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn fingerprint(settings: &GameSettings) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(settings)
        .unwrap_or_default()
        .hash(&mut h);
    h.finish()
}

fn note(ctx: &OpContext, msg: &str) {
    tracing::warn!("{msg}");
    (ctx.step)(msg);
}

/// Pick the Wine/Proton build for a game: its own setting when valid, else
/// the global default, else the recommended discovered build, else (when
/// `allow_download`) a freshly downloaded Proton. Heroic asks the user in a
/// dialog at this point; Ember falls back and says so in the status line.
pub async fn resolve_wine(
    app: &AppSettings,
    settings: &GameSettings,
    ctx: &OpContext,
    allow_download: bool,
) -> Result<WineInstallation> {
    anyhow::ensure!(!cfg!(windows), "Windows games run natively on Windows");
    let wanted = &settings.wine_version;
    if discovery::is_usable(wanted) {
        return Ok(wanted.clone());
    }
    let fallback_note = |found: &WineInstallation| {
        if wanted.is_set() {
            note(
                ctx,
                &format!(
                    "Wine version {} is not valid, using {} instead",
                    wanted.name, found.name
                ),
            );
        }
    };
    if discovery::is_usable(&app.game.wine_version) {
        fallback_note(&app.game.wine_version);
        return Ok(app.game.wine_version.clone());
    }
    let found: Vec<WineInstallation> = discovery::discover(app)
        .await
        .into_iter()
        .map(|d| d.installation)
        .collect();
    if let Some(best) = recommended(&found) {
        fallback_note(best);
        return Ok(best.clone());
    }
    if allow_download {
        note(ctx, "No Wine or Proton found; downloading Proton");
        return manager::download_default_wine(app, ctx)
            .await
            .context("No Wine or Proton version is available and none could be downloaded");
    }
    bail!("No Wine or Proton version was found. Download one in the Wine manager or install Wine.")
}

/// Resolve the build, prefix and umu use for a game. umu-run is downloaded
/// when Proton needs it; if that fails, Proton runs directly.
pub async fn wine_context(
    app: &AppSettings,
    settings: &GameSettings,
    game: &GameInfo,
    ctx: &OpContext,
    allow_download: bool,
) -> Result<WineContext> {
    let wine = resolve_wine(app, settings, ctx, allow_download).await?;
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        // These macOS Wine distributions execute Intel Windows code through
        // Rosetta. Test its availability without installing anything silently.
        let rosetta = ember_core::process::run(
            &CommandSpec::new("/usr/bin/arch").args(["-x86_64", "/usr/bin/true"]),
            ember_core::process::RunOptions::default(),
        )
        .await;
        anyhow::ensure!(
            rosetta.is_ok_and(|r| r.success()),
            "This Wine runtime needs Rosetta 2. Install Rosetta in macOS, then try again."
        );
    }
    let prefix = if wine.kind == WineKind::Crossover {
        let bottle = settings.wine_crossover_bottle.trim();
        anyhow::ensure!(
            !bottle.is_empty() && !bottle.contains(['/', '\\']) && bottle != "." && bottle != "..",
            "Choose a valid CrossOver bottle name"
        );
        let prefix = locations::home()
            .join("Library/Application Support/CrossOver/Bottles")
            .join(bottle);
        anyhow::ensure!(
            prefix.join("cxbottle.conf").is_file(),
            "CrossOver bottle {bottle} was not found. Create it in CrossOver first."
        );
        prefix
    } else {
        prefix_path(app, settings, game)
    };
    let effective = with_wine(settings, &wine);
    let mut uses_umu = runtimes::is_umu_supported(&effective, false);
    let mut umu = PathBuf::new();
    if uses_umu {
        match runtimes::ensure_umu(ctx).await {
            Ok(p) => umu = p,
            Err(e) => {
                note(ctx, &format!("{e:#}; running Proton directly"));
                uses_umu = false;
            }
        }
    }
    Ok(WineContext::new(wine, prefix, uses_umu, umu))
}

fn with_wine(settings: &GameSettings, wine: &WineInstallation) -> GameSettings {
    GameSettings {
        wine_version: wine.clone(),
        ..settings.clone()
    }
}

/// Cached result of the gamescope CLI probe per binary.
static GAMESCOPE_NEW_CLI: Lazy<Mutex<HashMap<PathBuf, bool>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Whether this gamescope uses the 3.12+ CLI (`-F, --filter` in `--help`).
async fn gamescope_new_cli(bin: &Path) -> bool {
    if let Some(v) = GAMESCOPE_NEW_CLI.lock().get(bin) {
        return *v;
    }
    let spec = CommandSpec::new(bin).arg("--help");
    let new = discovery::run_with_timeout(&spec, Duration::from_secs(10))
        .await
        .is_some_and(|r| r.stderr.contains("-F, --filter") || r.stdout.contains("-F, --filter"));
    GAMESCOPE_NEW_CLI.lock().insert(bin.to_path_buf(), new);
    new
}

fn find_on_path(name: &str) -> Option<String> {
    which::which(name)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Resolve MangoHud, GameMode and gamescope (Heroic's `prepareLaunch` steps
/// 7–9). A missing MangoHud or GameMode fails the launch; a missing
/// gamescope only logs a warning.
async fn wrapper_parts(settings: &GameSettings, ctx: &OpContext) -> Result<WrapperParts> {
    if !cfg!(target_os = "linux") {
        return Ok(WrapperParts::default());
    }
    let deck_game_mode = locations::is_steam_deck_game_mode();
    let flatpak_hint = if locations::is_flatpak() {
        ". Make sure to install the MangoHud Flatpak extension matching Ember's runtime and restart Ember."
    } else {
        ""
    };
    let mut parts = WrapperParts::default();
    if settings.show_mangohud && !deck_game_mode {
        let bin = find_on_path("mangohud").ok_or_else(|| {
            anyhow!("Mangohud is enabled, but `mangohud` executable could not be found on $PATH{flatpak_hint}")
        })?;
        parts.mangohud = vec![bin, "--dlsym".into()];
    }
    if settings.use_game_mode {
        let bin = find_on_path("gamemoderun").ok_or_else(|| {
            anyhow!("GameMode is enabled, but `gamemoderun` executable could not be found on $PATH")
        })?;
        parts.gamemode = Some(bin);
    }
    if wrappers::gamescope_enabled(&settings.gamescope) && !deck_game_mode {
        match which::which("gamescope") {
            Ok(bin) => {
                let new_cli = gamescope_new_cli(&bin).await;
                let mut cmd = vec![bin.to_string_lossy().into_owned()];
                cmd.extend(wrappers::gamescope_args(
                    &settings.gamescope,
                    settings.show_mangohud,
                    new_cli,
                ));
                parts.gamescope = cmd;
            }
            Err(_) => note(
                ctx,
                "Gamescope is enabled, but `gamescope` executable could not be found on $PATH",
            ),
        }
    }
    Ok(parts)
}

/// Prepare everything needed to start `target`: create/upgrade the prefix,
/// auto-install DXVK/VKD3D/NVAPI and anticheat runtimes when enabled, download
/// umu/Steam runtime when needed, and compute env vars and wrappers.
///
/// Environment precedence (lowest first): game identity (`HEROIC_APP_*`,
/// `STORE`, `GAMEID=umu-0`), generic variables and the user's variables,
/// known-fixes variables, Wine/Proton variables, then the umu database
/// `GAMEID`. Wrappers, outermost first: gamescope, user wrappers, MangoHud
/// (without gamescope), gamemoderun, and umu-run for native games on the
/// Steam runtime. For Windows games umu-run is the Wine layer's program
/// ([`WineLaunch::bin`]), never a wrapper.
pub async fn prepare_launch(
    app: &AppSettings,
    settings: &GameSettings,
    target: Target<'_>,
    ctx: &OpContext,
) -> Result<LaunchEnvironment> {
    anyhow::ensure!(
        ember_core::HostPlatform::current().supports(target.platform),
        "{} builds cannot run on this operating system. Install a compatible build instead.",
        target.platform.label()
    );
    let host = HostEnv::current();
    let mut vars = env::wrapper_env(target.id);
    let install_path = target.game.install_path();

    if target.platform == InstallPlatform::Browser {
        return Ok(LaunchEnvironment {
            env: vars,
            ..Default::default()
        });
    }

    let mut parts = wrapper_parts(settings, ctx).await?;
    let fixes = if cfg!(target_os = "linux") {
        hooks::hooks().known_fixes_env(target.id).await
    } else {
        Default::default()
    };

    if !target.platform.needs_wine() {
        let runtime = settings
            .steam_runtime
            .as_deref()
            .filter(|r| cfg!(target_os = "linux") && !r.trim().is_empty())
            .map(runtimes::steam_runtime_protonpath);
        if runtime.is_some() {
            let umu = runtimes::ensure_umu(ctx)
                .await
                .context("The Steam runtime needs umu-run")?;
            parts.steam_runtime = vec![umu.to_string_lossy().into_owned()];
        }
        env::layer(
            &mut vars,
            env::generic_env(settings, install_path.as_deref(), runtime.as_deref(), &host),
        );
        env::layer(&mut vars, fixes);
        return Ok(LaunchEnvironment {
            env: vars,
            wrappers: wrappers::setup_wrappers(settings, &parts),
            wine: None,
        });
    }

    let wc = wine_context(app, settings, target.game, ctx, true).await?;
    let effective = with_wine(settings, &wc.wine);
    ACTIVE
        .lock()
        .insert(fingerprint(settings), (wc.wine.clone(), wc.prefix.clone()));

    prefix::verify_prefix(app, &effective, &wc, ctx).await?;
    match prefix::register_game(&wc.prefix, &target.id.app_name).await {
        Ok(true) => {
            tracing::info!("Created/Updated Wineprefix at {}", wc.prefix.display());
            if let Err(e) = hooks::hooks()
                .prefix_initialized(target.id, target.game, &wc.prefix, ctx)
                .await
            {
                tracing::warn!("prefix setup for {}: {e:#}", target.id);
            }
        }
        Ok(false) => {}
        Err(e) => tracing::warn!("{e:#}"),
    }

    if cfg!(target_os = "linux") && wc.wine.kind == WineKind::Wine {
        let mut wanted = Vec::new();
        if settings.auto_install_dxvk {
            wanted.push(DxTool::Dxvk);
        }
        if settings.auto_install_dxvk_nvapi {
            wanted.push(DxTool::DxvkNvapi);
        }
        if settings.auto_install_vkd3d {
            wanted.push(DxTool::Vkd3d);
        }
        if let Err(e) = tools::install_tools(app, &effective, &wc, &wanted, ctx).await {
            if is_cancelled(&e) {
                return Err(e);
            }
            note(ctx, &format!("Could not install DXVK/VKD3D: {e:#}"));
        }
    }

    for (enabled, runtime) in [
        (settings.eac_runtime, RuntimeName::EacRuntime),
        (settings.battl_eye_runtime, RuntimeName::BattlEyeRuntime),
    ] {
        if !cfg!(target_os = "linux") || !enabled {
            continue;
        }
        if let Err(e) = runtimes::ensure(runtime, ctx).await {
            if is_cancelled(&e) {
                return Err(e);
            }
            note(
                ctx,
                &format!("Could not download the {}: {e:#}", runtime.label()),
            );
        }
    }

    env::layer(
        &mut vars,
        env::generic_env(&effective, install_path.as_deref(), None, &host),
    );
    env::layer(&mut vars, fixes);
    env::layer(
        &mut vars,
        prefix::wine_env_for(app, &effective, &wc, &target.folder_id(), &host),
    );
    if wc.uses_umu
        && let Some(id) = hooks::hooks()
            .umu_id(target.id)
            .await
            .filter(|i| !i.is_empty())
    {
        vars.insert("GAMEID".into(), id);
    }

    Ok(LaunchEnvironment {
        env: vars,
        wrappers: wrappers::setup_wrappers(settings, &parts),
        wine: Some(WineLaunch {
            bin: wc.runner_bin(),
            build_bin: PathBuf::from(&wc.wine.bin),
            kind: wc.wine.kind,
            prefix: wc.prefix.clone(),
            uses_umu: wc.uses_umu,
            proton_verb: "waitforexitandrun",
        }),
    })
}

/// Run a Windows command inside the game's prefix (winecfg, regedit, an exe).
/// When `wait` is false, returns as soon as the process started.
pub async fn run_in_prefix(
    app: &AppSettings,
    settings: &GameSettings,
    target: Target<'_>,
    argv: Vec<String>,
    wait: bool,
    ctx: OpContext,
) -> Result<ExecResult> {
    if argv.is_empty() {
        bail!("nothing to run");
    }
    let wc = wine_context(app, settings, target.game, &ctx, false).await?;
    let effective = with_wine(settings, &wc.wine);
    let mut cmd = WineCommand::new(argv);
    cmd.env = env::wrapper_env(target.id);
    cmd.install_path = target.game.install_path();
    cmd.game_id = target.folder_id();
    if let Some(first) = cmd.argv.first() {
        let p = Path::new(first);
        if p.is_absolute() && p.is_file() {
            cmd.cwd = p.parent().map(Path::to_path_buf);
        }
    }
    if wait {
        cmd.wait = true;
        cmd.verb = "waitforexitandrun";
    } else {
        cmd.detach = true;
    }
    prefix::run_wine(app, &effective, &wc, cmd, &ctx).await
}

/// Settings-only lookup of the build and prefix of a game (explicit prefix
/// setting, else the prefix its last launch used).
fn resolve_for_settings(settings: &GameSettings) -> Option<(WineInstallation, PathBuf)> {
    let recorded = ACTIVE.lock().get(&fingerprint(settings)).cloned();
    let prefix =
        if settings.wine_prefix.is_empty() || settings.wine_version.kind == WineKind::Crossover {
            recorded.as_ref().map(|(_, p)| p.clone())?
        } else {
            ember_core::paths::expand_home(&settings.wine_prefix)
        };
    let wine = if discovery::is_usable(&settings.wine_version) {
        settings.wine_version.clone()
    } else {
        recorded.map(|(w, _)| w)?
    };
    Some((wine, prefix))
}

/// Kill every process in the prefix (`wineserver -k`).
pub async fn kill_prefix(settings: &GameSettings) -> Result<()> {
    match resolve_for_settings(settings) {
        Some((wine, prefix)) => prefix::kill(&wine, &prefix).await,
        None => {
            tracing::debug!("kill_prefix: prefix unknown for these settings");
            Ok(())
        }
    }
}

/// Kill every process in a game's prefix, resolving the default prefix from
/// the game (preferred over [`kill_prefix`]).
pub async fn kill_game_prefix(
    app: &AppSettings,
    settings: &GameSettings,
    game: &GameInfo,
) -> Result<()> {
    let prefix = prefix_path(app, settings, game);
    let wine = if discovery::is_usable(&settings.wine_version) {
        settings.wine_version.clone()
    } else {
        app.game.wine_version.clone()
    };
    prefix::kill(&wine, &prefix).await
}

/// Default prefix path for a game when its settings name none.
pub fn default_prefix(app: &AppSettings, game: &GameInfo) -> PathBuf {
    let base = if app.default_wine_prefix_dir.is_empty() {
        PathBuf::from(&app.default_install_path).join("Prefixes")
    } else {
        PathBuf::from(&app.default_wine_prefix_dir)
    };
    base.join(ember_core::paths::sanitize(&game.title))
}

/// Every Wine/Proton build usable on this system (system Wine, downloaded
/// builds, Steam Proton, Lutris/Bottles builds, custom paths).
pub async fn wine_installations(app: &AppSettings) -> Vec<WineInstallation> {
    discovery::discover(app)
        .await
        .into_iter()
        .map(|d| d.installation)
        .collect()
}

/// Run winetricks for the game's prefix: GUI when `verbs` is empty.
pub async fn winetricks(
    app: &AppSettings,
    settings: &GameSettings,
    target: Target<'_>,
    verbs: Vec<String>,
    ctx: OpContext,
) -> Result<ExecResult> {
    let wc = wine_context(app, settings, target.game, &ctx, false).await?;
    let effective = with_wine(settings, &wc.wine);
    prefix::verify_prefix(app, &effective, &wc, &ctx).await?;
    tools::run_winetricks(app, &effective, &wc, &target.id.app_name, &verbs, &ctx).await
}

/// Whether a path holds a valid initialised prefix.
pub fn is_prefix(path: &Path) -> bool {
    path.join("system.reg").is_file() || path.join("pfx/system.reg").is_file()
}

/// Wait until every process in the game's prefix has exited (`wineserver -w`).
/// Used to keep tracking games whose launcher process exits early.
pub async fn wait_prefix(settings: &GameSettings) -> Result<()> {
    match resolve_for_settings(settings) {
        Some((wine, prefix)) => prefix::wait(&wine, &prefix).await,
        None => Ok(()),
    }
}

/// Resolve the prefix path a game uses (settings value, or the default).
pub fn prefix_path(app: &AppSettings, settings: &GameSettings, game: &GameInfo) -> PathBuf {
    let wine = if settings.wine_version.is_set() {
        &settings.wine_version
    } else {
        &app.game.wine_version
    };
    if cfg!(target_os = "macos") && wine.kind == WineKind::Crossover {
        let bottle = settings.wine_crossover_bottle.trim();
        if !bottle.is_empty() && !bottle.contains(['/', '\\']) && bottle != "." && bottle != ".." {
            return locations::home()
                .join("Library/Application Support/CrossOver/Bottles")
                .join(bottle);
        }
    }
    if settings.wine_prefix.is_empty() {
        default_prefix(app, game)
    } else {
        ember_core::paths::expand_home(&settings.wine_prefix)
    }
}

/// Whether `prefix` is shared by several games: the root that holds every
/// game's own prefix, or the prefix set for all games in the defaults.
/// Deleting it with one game would take the others' saves with it.
pub fn prefix_is_shared(app: &AppSettings, prefix: &Path) -> bool {
    let same = |setting: &str| {
        !setting.trim().is_empty() && ember_core::paths::expand_home(setting) == prefix
    };
    same(&app.default_wine_prefix_dir) || same(&app.game.wine_prefix)
}

/// Install (`enabled`) or remove a DLL tool in the game's prefix (the DXVK /
/// VKD3D / NVAPI toggles). Only Wine prefixes are affected; Proton ships its
/// own.
pub async fn set_dx_tool(
    app: &AppSettings,
    settings: &GameSettings,
    target: Target<'_>,
    tool: DxTool,
    enabled: bool,
    ctx: OpContext,
) -> Result<()> {
    anyhow::ensure!(
        cfg!(target_os = "linux"),
        "DXVK management is available on Linux. macOS runtimes manage their own graphics libraries."
    );
    let wc = wine_context(app, settings, target.game, &ctx, false).await?;
    let effective = with_wine(settings, &wc.wine);
    prefix::verify_prefix(app, &effective, &wc, &ctx).await?;
    if enabled {
        tools::install_tools(app, &effective, &wc, &[tool], &ctx).await
    } else {
        tools::remove_tool(app, &effective, &wc, tool, &ctx).await
    }
}

/// Open `winecfg` for the game's prefix.
pub async fn winecfg(
    app: &AppSettings,
    settings: &GameSettings,
    target: Target<'_>,
    ctx: OpContext,
) -> Result<ExecResult> {
    run_in_prefix(app, settings, target, vec!["winecfg".into()], false, ctx).await
}

/// Open the registry editor for the game's prefix.
pub async fn regedit(
    app: &AppSettings,
    settings: &GameSettings,
    target: Target<'_>,
    ctx: OpContext,
) -> Result<ExecResult> {
    run_in_prefix(app, settings, target, vec!["regedit".into()], false, ctx).await
}

/// Start a Windows executable in the game's prefix (from its own folder).
pub async fn run_exe(
    app: &AppSettings,
    settings: &GameSettings,
    target: Target<'_>,
    exe: &Path,
    ctx: OpContext,
) -> Result<ExecResult> {
    run_in_prefix(
        app,
        settings,
        target,
        vec![exe.to_string_lossy().into_owned()],
        false,
        ctx,
    )
    .await
}

/// Open the game's prefix folder in the file manager.
pub fn open_prefix_folder(
    app: &AppSettings,
    settings: &GameSettings,
    game: &GameInfo,
) -> Result<()> {
    let prefix = prefix_path(app, settings, game);
    let wc = WineContext::new(settings.wine_version.clone(), prefix, false, PathBuf::new());
    tools::open_prefix_folder(&wc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_core::Runner;

    fn game() -> GameInfo {
        GameInfo {
            app_name: "app".into(),
            title: "My: Game".into(),
            install: Some(ember_core::InstalledInfo {
                install_path: "/games/my".into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn knows_shared_prefixes() {
        let root = ember_core::paths::expand_home("~/Games/Prefixes");
        let mut app = AppSettings {
            default_wine_prefix_dir: "~/Games/Prefixes".into(),
            ..Default::default()
        };
        assert!(prefix_is_shared(&app, &root));
        assert!(!prefix_is_shared(&app, &root.join("My Game")));
        app.game.wine_prefix = "/data/shared".into();
        assert!(prefix_is_shared(&app, Path::new("/data/shared")));
        app.game.wine_prefix.clear();
        assert!(
            !prefix_is_shared(&app, Path::new("")),
            "no setting, no match"
        );
    }

    #[test]
    fn prefixes() {
        let app = AppSettings {
            default_wine_prefix_dir: "/p".into(),
            ..Default::default()
        };
        let mut gs = GameSettings::default();
        assert_eq!(
            prefix_path(&app, &gs, &game()),
            PathBuf::from("/p/My_ Game")
        );
        gs.wine_prefix = "/x/y".into();
        assert_eq!(prefix_path(&app, &gs, &game()), PathBuf::from("/x/y"));
    }

    #[tokio::test]
    async fn native_launch_environment() {
        let id = GameId::new(Runner::Gog, "app");
        let g = game();
        let gs = GameSettings {
            wrapper_options: vec![ember_core::Wrapper {
                exe: "env".into(),
                args: "-u X".into(),
            }],
            environment_options: vec![ember_core::EnvVar {
                key: "A".into(),
                value: "1".into(),
            }],
            ..Default::default()
        };
        let target = Target {
            id: &id,
            game: &g,
            platform: ember_core::HostPlatform::current().native(),
        };
        let le = prepare_launch(&AppSettings::default(), &gs, target, &OpContext::detached())
            .await
            .unwrap();
        assert!(le.wine.is_none());
        assert_eq!(le.wrappers, vec![vec!["env", "-u", "X"]]);
        assert_eq!(le.env["A"], "1");
        assert_eq!(le.env["HEROIC_APP_RUNNER"], "gog");
        if cfg!(target_os = "linux") {
            assert_eq!(le.env["STEAM_COMPAT_INSTALL_PATH"], "/games/my");
        }
        assert!(!le.env.contains_key("WINEPREFIX"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn missing_gamemode_fails() {
        if which::which("gamemoderun").is_ok() {
            return;
        }
        let gs = GameSettings {
            use_game_mode: true,
            ..Default::default()
        };
        let err = wrapper_parts(&gs, &OpContext::detached())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("gamemoderun"));
    }

    #[test]
    fn settings_lookup_uses_recorded_prefix() {
        let gs = GameSettings {
            launcher_args: "unique-for-test".into(),
            ..Default::default()
        };
        assert!(resolve_for_settings(&gs).is_none());
        let wine = WineInstallation {
            bin: "/w/bin/wine".into(),
            ..Default::default()
        };
        ACTIVE
            .lock()
            .insert(fingerprint(&gs), (wine.clone(), "/pfx/a".into()));
        let (w, p) = resolve_for_settings(&gs).unwrap();
        assert_eq!(w, wine);
        assert_eq!(p, PathBuf::from("/pfx/a"));
    }
}
