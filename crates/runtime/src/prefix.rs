//! Wine prefixes and the generic Wine/Proton command runner.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use ember_core::log_cap;
use ember_core::process::{self, CommandSpec, ExecResult, RunOptions};
use ember_core::{AppSettings, GameSettings, OpContext, WineInstallation, WineKind};

use crate::discovery;
use crate::env::{self, EnvMap, HostEnv, WineEnvInput};
use crate::locations;
use crate::runtimes::{self, RuntimeName};

/// The Wine layer resolved for one game: which build, which prefix, and
/// whether umu runs it.
#[derive(Debug, Clone)]
pub struct WineContext {
    /// The build (single quotes already removed from `bin`).
    pub wine: WineInstallation,
    pub prefix: PathBuf,
    pub uses_umu: bool,
    /// umu-run path (meaningful when `uses_umu`).
    pub umu: PathBuf,
}

impl WineContext {
    /// Build a context, normalising the binary path.
    pub fn new(mut wine: WineInstallation, prefix: PathBuf, uses_umu: bool, umu: PathBuf) -> Self {
        wine.bin = discovery::unquote(&wine.bin);
        Self {
            wine,
            prefix,
            uses_umu,
            umu,
        }
    }

    pub fn is_proton(&self) -> bool {
        self.wine.kind == WineKind::Proton
    }

    /// Program that runs Windows binaries: umu-run, the proton script or wine.
    pub fn runner_bin(&self) -> PathBuf {
        if self.uses_umu {
            self.umu.clone()
        } else {
            PathBuf::from(&self.wine.bin)
        }
    }

    /// The real Wine prefix (`<prefix>/pfx` for Proton).
    pub fn wine_prefix_dir(&self) -> PathBuf {
        if self.is_proton() {
            self.prefix.join("pfx")
        } else {
            self.prefix.clone()
        }
    }
}

/// A command to run inside a prefix.
#[derive(Debug, Clone)]
pub struct WineCommand {
    pub argv: Vec<String>,
    /// Proton verb (`run`, `waitforexitandrun`, `runinprefix`, …).
    pub verb: &'static str,
    /// Wait for the prefix's wineserver to exit afterwards (Wine builds).
    pub wait: bool,
    pub cwd: Option<PathBuf>,
    /// Base variables (lowest precedence).
    pub env: EnvMap,
    pub install_path: Option<PathBuf>,
    /// Identifier for `SteamGameId` (`heroic-<id>`).
    pub game_id: String,
    pub skip_prefix_check: bool,
    /// Start the process and return immediately.
    pub detach: bool,
}

impl WineCommand {
    pub fn new<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            verb: "run",
            wait: false,
            cwd: None,
            env: EnvMap::new(),
            install_path: None,
            game_id: "0".into(),
            skip_prefix_check: false,
            detach: false,
        }
    }
}

/// Inputs for [`env::wine_env`] from a context and settings.
pub fn wine_env_for(
    app: &AppSettings,
    settings: &GameSettings,
    wc: &WineContext,
    game_id: &str,
    host: &HostEnv,
) -> EnvMap {
    let steam = locations::steam_client_install_path(app);
    let log_dir = locations::flatpak_home();
    let eac = runtimes::effective_runtime_dir(RuntimeName::EacRuntime);
    let be = runtimes::effective_runtime_dir(RuntimeName::BattlEyeRuntime);
    env::wine_env(&WineEnvInput {
        settings,
        wine: &wc.wine,
        prefix: &wc.prefix,
        game_id,
        steam_client_path: &steam,
        proton_log_dir: &log_dir,
        eac_runtime: &eac,
        battleye_runtime: &be,
        host,
    })
}

/// Files that must exist in an initialised prefix of this kind.
fn required_files(kind: WineKind) -> &'static [&'static str] {
    match kind {
        WineKind::Proton => &[
            "pfx.lock",
            "tracked_files",
            "version",
            "config_info",
            "pfx/dosdevices",
            "pfx/drive_c",
            "pfx/system.reg",
            "pfx/user.reg",
            "pfx/userdef.reg",
        ],
        _ => &[
            "dosdevices",
            "drive_c",
            "system.reg",
            "user.reg",
            "userdef.reg",
        ],
    }
}

/// Whether the prefix looks complete for this build.
pub fn prefix_complete(wc: &WineContext) -> bool {
    wc.prefix.is_dir()
        && required_files(wc.wine.kind)
            .iter()
            .all(|f| wc.prefix.join(f).exists())
}

/// Run a command in the prefix (Heroic's `runWineCommand`).
///
/// Environment: `cmd.env` < generic variables < Wine variables <
/// `PROTON_VERB`. umu runs the command when enabled; Proton without umu gets
/// the verb as its first argument. Unlike Heroic, a spawn failure is
/// returned as an error instead of hanging.
pub async fn run_wine(
    app: &AppSettings,
    settings: &GameSettings,
    wc: &WineContext,
    cmd: WineCommand,
    ctx: &OpContext,
) -> Result<ExecResult> {
    if !discovery::is_valid(&wc.wine) {
        bail!(
            "The Wine version \"{}\" was not found at {}",
            wc.wine.name,
            wc.wine.bin
        );
    }
    if !cmd.skip_prefix_check && wc.wine.kind != WineKind::Crossover && !prefix_complete(wc) {
        tracing::warn!(
            "prefix {} is incomplete; initialising it",
            wc.prefix.display()
        );
        if !wc.uses_umu {
            tokio::fs::create_dir_all(&wc.prefix)
                .await
                .with_context(|| format!("creating {}", wc.prefix.display()))?;
        }
        Box::pin(verify_prefix(app, settings, wc, ctx)).await?;
    }
    let spec = wine_spec(app, settings, wc, &cmd, &HostEnv::current());
    if cmd.detach {
        let keep = ctx
            .log_file
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len());
        let mut child = process::spawn(&spec, ctx.log_file.as_deref())?;
        // A program left running in the prefix writes its own output, like a
        // game; keep its log bounded the same way.
        let bound = ctx
            .log_file
            .clone()
            .map(|log| tokio::spawn(log_cap::bound_log(log, keep, log_cap::OPERATION_LOG_CAP)));
        tokio::spawn(async move {
            let _ = child.wait().await;
            if let Some(bound) = bound {
                bound.abort();
            }
        });
        return Ok(ExecResult {
            code: Some(0),
            ..Default::default()
        });
    }
    let mut opts = RunOptions::default().cancel(ctx.cancel.clone());
    if let Some(log) = &ctx.log_file {
        opts = opts.log_file(log);
    }
    let res = process::run(&spec, opts).await?;
    if cmd.wait && !res.aborted {
        wait_wineserver(wc, &spec.env).await;
    }
    Ok(res)
}

/// The full command line (program, args, env, cwd) for a Wine command.
pub fn wine_spec(
    app: &AppSettings,
    settings: &GameSettings,
    wc: &WineContext,
    cmd: &WineCommand,
    host: &HostEnv,
) -> CommandSpec {
    let mut vars = cmd.env.clone();
    env::layer(
        &mut vars,
        env::generic_env(settings, cmd.install_path.as_deref(), None, host),
    );
    env::layer(
        &mut vars,
        wine_env_for(app, settings, wc, &cmd.game_id, host),
    );
    if wc.is_proton() {
        vars.insert("PROTON_VERB".into(), cmd.verb.into());
    }
    if wc.uses_umu {
        vars.entry("GAMEID".into())
            .or_insert_with(|| "umu-0".into());
    }
    let mut args: Vec<String> = Vec::new();
    if wc.is_proton() && !wc.uses_umu {
        args.push(cmd.verb.to_string());
    }
    args.extend(cmd.argv.iter().cloned());
    let mut spec = CommandSpec::new(wc.runner_bin()).args(args).envs(vars);
    if let Some(dir) = &cmd.cwd {
        spec = spec.cwd(dir);
    }
    spec
}

/// Wait for the prefix's wineserver (Wine builds only; Proton manages its
/// own).
async fn wait_wineserver(wc: &WineContext, env: &EnvMap) {
    if wc.wine.kind == WineKind::Proton {
        return;
    }
    let Some(server) = wc.wine.wineserver.as_deref().filter(|s| !s.is_empty()) else {
        return;
    };
    let spec = CommandSpec::new(server).arg("--wait").envs(env.clone());
    if let Err(e) = process::run(&spec, RunOptions::default()).await {
        tracing::debug!("wineserver --wait: {e:#}");
    }
}

/// Create the prefix when it does not exist yet (Heroic's
/// `verifyWinePrefix`): `umu-run createprefix`, `proton run wineboot --init`
/// or `wine wineboot --init`. Returns whether a prefix was created.
///
/// Heroic runs `wineboot --init` on every launch; Wine and Proton already
/// upgrade an existing prefix themselves when the build changes, so Ember
/// only runs it when the prefix's registry is missing.
pub async fn verify_prefix(
    app: &AppSettings,
    settings: &GameSettings,
    wc: &WineContext,
    ctx: &OpContext,
) -> Result<bool> {
    if !discovery::is_valid(&wc.wine) || wc.wine.kind == WineKind::Crossover {
        return Ok(false);
    }
    if !wc.prefix.exists() && !wc.uses_umu {
        tokio::fs::create_dir_all(&wc.prefix)
            .await
            .with_context(|| format!("creating {}", wc.prefix.display()))?;
    }
    let system_reg = wc.wine_prefix_dir().join("system.reg");
    if system_reg.exists() {
        return Ok(false);
    }
    (ctx.step)("Creating Wine prefix");
    let argv: Vec<&str> = if wc.uses_umu {
        vec!["createprefix"]
    } else {
        vec!["wineboot", "--init"]
    };
    let mut cmd = WineCommand::new(argv);
    cmd.wait = true;
    cmd.skip_prefix_check = true;
    let res = run_wine(app, settings, wc, cmd, ctx)
        .await
        .context("Unable to create Wineprefix")?;
    if res.aborted {
        return Err(crate::download::Cancelled.into());
    }
    if !system_reg.exists() {
        bail!(
            "Unable to create Wineprefix at {}: {}",
            wc.prefix.display(),
            res.error_summary()
        );
    }
    tracing::info!("Created Wineprefix at {}", wc.prefix.display());
    Ok(true)
}

/// Record `app_name` in `<prefix>/installed_games`. Returns true when the
/// game was not listed yet (first launch in this prefix).
pub async fn register_game(prefix: &Path, app_name: &str) -> Result<bool> {
    let file = prefix.join("installed_games");
    let mut games: Vec<String> = match tokio::fs::read(&file).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    if games.iter().any(|g| g == app_name) {
        return Ok(false);
    }
    games.push(app_name.to_string());
    tokio::fs::create_dir_all(prefix).await?;
    tokio::fs::write(&file, serde_json::to_vec(&games)?)
        .await
        .with_context(|| format!("writing {}", file.display()))?;
    Ok(true)
}

/// The Wine binaries inside a Proton build (`files/` for GE and Experimental,
/// `dist/` for old Valve builds).
pub fn wine_inside_proton(proton: &WineInstallation) -> Option<WineInstallation> {
    let dir = Path::new(&proton.bin).parent()?;
    ["files", "dist"].iter().find_map(|d| {
        let bin = dir.join(d).join("bin/wine");
        bin.is_file().then(|| {
            let server = dir.join(d).join("bin/wineserver");
            WineInstallation {
                bin: bin.to_string_lossy().into_owned(),
                name: proton.name.clone(),
                kind: WineKind::Wine,
                lib: None,
                lib32: None,
                wineserver: server
                    .is_file()
                    .then(|| server.to_string_lossy().into_owned()),
            }
        })
    })
}

/// Heroic's `getWineFromProton`: for Proton without umu, the inner Wine and
/// `<prefix>/pfx`; otherwise the inputs unchanged.
pub fn wine_from_proton(wc: &WineContext) -> (WineInstallation, PathBuf) {
    if !wc.is_proton() || wc.uses_umu {
        return (wc.wine.clone(), wc.prefix.clone());
    }
    match wine_inside_proton(&wc.wine) {
        Some(w) => (w, wc.prefix.join("pfx")),
        None => {
            tracing::warn!(
                "Proton {} has an abnormal structure; no inner Wine found",
                wc.wine.name
            );
            (wc.wine.clone(), wc.prefix.clone())
        }
    }
}

/// The wineserver and real prefix to address processes of `prefix`.
fn server_for(wine: &WineInstallation, prefix: &Path) -> (Option<PathBuf>, PathBuf) {
    match wine.kind {
        WineKind::Proton => {
            let server = wine_inside_proton(wine)
                .and_then(|w| w.wineserver)
                .map(PathBuf::from);
            (server, prefix.join("pfx"))
        }
        _ => {
            let server = wine
                .wineserver
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_file())
                .or_else(|| discovery::wineserver_for(Path::new(&discovery::unquote(&wine.bin))))
                .or_else(|| which::which("wineserver").ok());
            (server, prefix.to_path_buf())
        }
    }
}

fn server_command(server: PathBuf, kind: WineKind, prefix: &Path) -> CommandSpec {
    if kind == WineKind::Crossover {
        CommandSpec::new(server).env(
            "CX_BOTTLE",
            prefix.file_name().unwrap_or_default().to_string_lossy(),
        )
    } else {
        CommandSpec::new(server).env("WINEPREFIX", prefix.to_string_lossy())
    }
}

/// Kill every process of a prefix (`wineserver -k`).
pub async fn kill(wine: &WineInstallation, prefix: &Path) -> Result<()> {
    let (server, real_prefix) = server_for(wine, prefix);
    let Some(server) = server else {
        bail!("no wineserver found for {}", wine.name);
    };
    let spec = server_command(server, wine.kind, &real_prefix).arg("-k");
    match discovery::run_with_timeout(&spec, Duration::from_secs(20)).await {
        Some(_) => Ok(()),
        None => bail!("wineserver -k did not finish"),
    }
}

/// Wait until every process of a prefix exited (`wineserver -w`).
pub async fn wait(wine: &WineInstallation, prefix: &Path) -> Result<()> {
    let (server, real_prefix) = server_for(wine, prefix);
    let Some(server) = server else {
        return Ok(());
    };
    if !real_prefix.is_dir() {
        return Ok(());
    }
    let spec = server_command(server, wine.kind, &real_prefix).arg("-w");
    process::run(&spec, RunOptions::default()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_for(kind: WineKind, umu: bool) -> WineContext {
        WineContext::new(
            WineInstallation {
                bin: "'/p/GE/proton'".into(),
                name: "GE".into(),
                kind,
                ..Default::default()
            },
            "/pfx".into(),
            umu,
            "/usr/bin/umu-run".into(),
        )
    }

    #[test]
    fn command_lines() {
        let app = AppSettings::default();
        let gs = GameSettings::default();
        let host = HostEnv::default();
        let mut cmd = WineCommand::new(["winecfg"]);
        cmd.verb = "run";
        cmd.cwd = Some("/g".into());
        cmd.env.insert("PROTON_VERB".into(), "x".into());

        let wc = ctx_for(WineKind::Proton, false);
        let spec = wine_spec(&app, &gs, &wc, &cmd, &host);
        assert_eq!(spec.program, PathBuf::from("/p/GE/proton"));
        assert_eq!(spec.args, vec!["run", "winecfg"]);
        assert_eq!(spec.env["PROTON_VERB"], "run");
        if cfg!(target_os = "linux") {
            assert_eq!(spec.env["STEAM_COMPAT_DATA_PATH"], "/pfx");
        } else {
            assert!(!spec.env.contains_key("STEAM_COMPAT_DATA_PATH"));
        }
        assert_eq!(spec.cwd.as_deref(), Some(Path::new("/g")));
        assert!(!spec.env.contains_key("GAMEID"));

        let wc = ctx_for(WineKind::Proton, true);
        let spec = wine_spec(&app, &gs, &wc, &cmd, &host);
        assert_eq!(spec.program, PathBuf::from("/usr/bin/umu-run"));
        assert_eq!(spec.args, vec!["winecfg"]);
        assert_eq!(spec.env["GAMEID"], "umu-0");
        if cfg!(target_os = "linux") {
            assert_eq!(spec.env["PROTONPATH"], "/p/GE");
        }

        let mut wc = ctx_for(WineKind::Wine, false);
        wc.wine.bin = "/w/bin/wine".into();
        let spec = wine_spec(&app, &gs, &wc, &cmd, &host);
        assert_eq!(spec.program, PathBuf::from("/w/bin/wine"));
        assert_eq!(spec.args, vec!["winecfg"]);
        if cfg!(windows) {
            assert!(!spec.env.contains_key("WINEPREFIX"));
        } else {
            assert_eq!(spec.env["WINEPREFIX"], "/pfx");
        }
    }

    #[tokio::test]
    async fn installed_games_bookkeeping() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("pfx");
        assert!(register_game(&prefix, "a").await.unwrap());
        assert!(!register_game(&prefix, "a").await.unwrap());
        assert!(register_game(&prefix, "b").await.unwrap());
        let text = std::fs::read_to_string(prefix.join("installed_games")).unwrap();
        assert_eq!(text, r#"["a","b"]"#);
    }

    #[tokio::test]
    async fn spawn_failure_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin/wine");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"not executable").unwrap();
        std::fs::write(dir.path().join("bin/wineserver"), b"").unwrap();
        let wc = WineContext::new(
            discovery::wine_build(dir.path(), "broken"),
            dir.path().join("prefix"),
            false,
            PathBuf::new(),
        );
        let mut cmd = WineCommand::new(["winecfg"]);
        cmd.skip_prefix_check = true;
        let res = run_wine(
            &AppSettings::default(),
            &GameSettings::default(),
            &wc,
            cmd,
            &OpContext::detached(),
        )
        .await;
        assert!(res.is_err());
    }

    #[test]
    fn proton_inner_wine() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("files/bin")).unwrap();
        std::fs::write(dir.path().join("proton"), b"").unwrap();
        std::fs::write(dir.path().join("files/bin/wine"), b"").unwrap();
        std::fs::write(dir.path().join("files/bin/wineserver"), b"").unwrap();
        let wc = WineContext::new(
            discovery::proton_build(dir.path(), "GE"),
            "/games/pfx".into(),
            false,
            PathBuf::new(),
        );
        let (w, p) = wine_from_proton(&wc);
        assert_eq!(w.kind, WineKind::Wine);
        assert!(Path::new(&w.bin).ends_with("files/bin/wine"));
        assert!(Path::new(&w.wineserver.unwrap()).ends_with("files/bin/wineserver"));
        assert_eq!(p, PathBuf::from("/games/pfx/pfx"));
        let umu = WineContext {
            uses_umu: true,
            ..wc
        };
        assert_eq!(wine_from_proton(&umu).1, PathBuf::from("/games/pfx"));
    }
}
