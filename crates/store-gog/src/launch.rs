//! Launching: launch options and play tasks from `goggame-*.info`, the game
//! command (native, Wine, Proton or umu with the wrapper chain), Cyberpunk
//! 2077 REDmod deployment and the in-process Galaxy service.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use irori_core::process::{CommandSpec, split_args};
use irori_core::{
    InstallPlatform, LaunchEnvironment, LaunchOption, LaunchPlan, LaunchRequest, OpContext,
};

use crate::installed::{CyberpunkConfig, InstalledRecord};
use crate::manifest::{self, GameInfoFile};
use crate::wine::{WineContext, to_windows_path};
use crate::{GogStore, load_json, save_json};

/// Cyberpunk 2077.
pub const CYBERPUNK_ID: &str = "1423049311";
/// REDmod DLC; its play task deploys mods before starting the game.
pub const REDMOD_ID: &str = "1597316373";

/// Launch options from the info file: every visible, non-document file task
/// (`--prefer-task <index>`). Empty when there is only one.
pub fn launch_options_from_info(info: &GameInfoFile) -> Vec<LaunchOption> {
    let opts: Vec<LaunchOption> = info
        .play_tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            t.kind == "FileTask" && !t.is_hidden && t.category.as_deref() != Some("document")
        })
        .map(|(i, t)| LaunchOption::Basic {
            name: t
                .name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| info.name.clone()),
            parameters: format!("--prefer-task {i}"),
        })
        .collect();
    if opts.len() < 2 { Vec::new() } else { opts }
}

/// `--prefer-task <index>` of a launch option, and the remaining arguments.
pub fn split_option(parameters: &str) -> (Option<usize>, Vec<String>) {
    let mut args = split_args(parameters);
    let mut task = None;
    if let Some(i) = args.iter().position(|a| a == "--prefer-task")
        && i + 1 < args.len()
    {
        task = args[i + 1].parse().ok();
        args.drain(i..i + 2);
    }
    (task, args)
}

/// `path` with each missing component matched case-insensitively.
fn case_insensitive(path: &Path) -> PathBuf {
    if path.exists() {
        return path.to_path_buf();
    }
    let mut out = PathBuf::new();
    for c in path.components() {
        let next = out.join(c);
        if next.exists() || out.as_os_str().is_empty() {
            out = next;
            continue;
        }
        let name = c.as_os_str().to_string_lossy().to_lowercase();
        let found = std::fs::read_dir(&out).ok().and_then(|it| {
            it.flatten()
                .find(|e| e.file_name().to_string_lossy().to_lowercase() == name)
                .map(|e| e.path())
        });
        out = found.unwrap_or(next);
    }
    out
}

/// What a play task runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCommand {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// The command of the play task at `prefer` (when it exists) or the primary
/// one. Linux installs start through `start.sh`.
pub fn task_command(
    install: &Path,
    platform: InstallPlatform,
    info: Option<&GameInfoFile>,
    prefer: Option<usize>,
) -> Result<TaskCommand> {
    if platform == InstallPlatform::Mac {
        let bundle = if install.extension().is_some_and(|e| e == "app") {
            install.to_path_buf()
        } else {
            let mut bundles: Vec<_> = std::fs::read_dir(install)?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "app"))
                .collect();
            bundles.sort();
            anyhow::ensure!(
                bundles.len() == 1,
                "Select the macOS app in the game's alternative executable setting"
            );
            bundles.remove(0)
        };
        let exe = irori_core::platform::bundle_executable(&bundle)?;
        let resources = bundle.join("Contents/Resources");
        let cwd = if resources.is_dir() {
            resources
        } else {
            exe.parent()
                .context("The app executable has no parent directory")?
                .to_path_buf()
        };
        return Ok(TaskCommand {
            exe,
            args: Vec::new(),
            cwd,
        });
    }
    if platform == InstallPlatform::Linux {
        return Ok(TaskCommand {
            exe: install.join("start.sh"),
            args: Vec::new(),
            cwd: install.to_path_buf(),
        });
    }
    let info = info.context("The game's goggame info file was not found")?;
    let task = prefer
        .and_then(|i| info.play_tasks.get(i))
        .or_else(|| info.play_tasks.iter().find(|t| t.is_primary))
        .context("The game has no primary play task")?;
    let rel = task
        .path
        .as_deref()
        .context("The game's play task has no executable")?
        .replace('\\', "/");
    let args = match &task.arguments {
        Some(Value::String(s)) => {
            if cfg!(windows) {
                split_args(s)
            } else {
                split_args(&s.replace('\\', "/"))
            }
        }
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let cwd = task
        .working_dir
        .as_deref()
        .filter(|w| !w.is_empty())
        .map(|w| install.join(w.replace('\\', "/")))
        .unwrap_or_else(|| install.to_path_buf());
    Ok(TaskCommand {
        exe: case_insensitive(&install.join(rel)),
        args,
        cwd: case_insensitive(&cwd),
    })
}

/// `wrappers + [wine prefix] + exe + task args + user args` in the task's
/// working folder.
pub fn build_command(
    task: &TaskCommand,
    user_args: &[String],
    environment: &LaunchEnvironment,
    needs_wine: bool,
    env: BTreeMap<String, String>,
) -> Result<CommandSpec> {
    let mut argv: Vec<String> = Vec::new();
    if needs_wine {
        let wine = environment
            .wine
            .as_ref()
            .context("No Wine or Proton version is set up for this game")?;
        argv.extend(
            wine.exec_prefix()
                .into_iter()
                .map(|p| p.trim_matches('\'').to_string()),
        );
    }
    argv.push(task.exe.to_string_lossy().into_owned());
    argv.extend(task.args.iter().cloned());
    argv.extend(user_args.iter().cloned());
    let program = argv.remove(0);
    Ok(CommandSpec::new(program)
        .args(argv)
        .envs(env)
        .cwd(&task.cwd)
        .wrapped(&environment.wrappers))
}

/// Environment wrappers (umu, Heroic-aware tools) use to identify the game.
pub fn identity_env(app_name: &str) -> [(&'static str, String); 5] {
    [
        ("HEROIC_APP_NAME", app_name.to_string()),
        ("HEROIC_APP_RUNNER", "gog".into()),
        ("HEROIC_APP_SOURCE", "gog".into()),
        ("STORE", "gog".into()),
        ("GAMEID", "umu-0".into()),
    ]
}

/// REDmod mods available in a Cyberpunk install (folders with `info.json`), sorted.
pub fn available_mods(install_path: &Path) -> Vec<String> {
    let mut mods: Vec<String> = std::fs::read_dir(install_path.join("mods"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("info.json").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    mods.sort_by_key(|m| m.to_lowercase());
    mods
}

/// Mods to deploy: the selected ones that exist, or all when none are selected.
pub fn mods_to_load(available: &[String], selected: &[String]) -> Vec<String> {
    let chosen: Vec<String> = selected
        .iter()
        .filter(|m| available.contains(m))
        .cloned()
        .collect();
    if chosen.is_empty() {
        available.to_vec()
    } else {
        chosen
    }
}

/// The in-process Galaxy service and the games using it.
#[derive(Default)]
pub(crate) struct Galaxy {
    service: Option<irori_gog_galaxy::Service>,
    sessions: HashSet<String>,
}

impl GogStore {
    /// The Wine layer remembered from the game's last launch.
    pub(crate) async fn wine_context(&self, app_name: &str) -> Option<WineContext> {
        let all: HashMap<String, WineContext> = load_json(self.paths.wine_contexts_file()).await;
        all.get(app_name).cloned()
    }

    async fn remember_wine_context(&self, app_name: &str, ctx: &WineContext) {
        let path = self.paths.wine_contexts_file();
        let mut all: HashMap<String, WineContext> = load_json(path.clone()).await;
        if all.get(app_name) != Some(ctx) {
            all.insert(app_name.to_string(), ctx.clone());
            if let Err(e) = save_json(path, all).await {
                tracing::warn!("{e:#}");
            }
        }
    }

    pub(crate) async fn game_launch_options(&self, app_name: &str) -> Result<Vec<LaunchOption>> {
        let Some(rec) = self.record(app_name) else {
            return Ok(Vec::new());
        };
        let app = app_name.to_string();
        Ok(tokio::task::spawn_blocking(move || {
            manifest::read_info_file(&rec.path(), &app, rec.platform)
                .map(|i| launch_options_from_info(&i))
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default())
    }

    /// REDmod mods available in an installed Cyberpunk 2077.
    pub async fn cyberpunk_mods(&self, app_name: &str) -> Result<Vec<String>> {
        let rec = self.installed_record(app_name)?;
        Ok(tokio::task::spawn_blocking(move || available_mods(&rec.path())).await?)
    }

    /// Enable or disable REDmod and choose the mods to deploy (empty = all).
    pub async fn set_cyberpunk_mod_config(
        &self,
        app_name: &str,
        config: CyberpunkConfig,
    ) -> Result<()> {
        self.modify_record(app_name, |r| r.cyberpunk = Some(config))
            .await
    }

    /// Deploy REDmod mods. Returns true when the REDmod task should be used.
    async fn deploy_redmod(
        &self,
        rec: &InstalledRecord,
        wine: Option<&WineContext>,
        log_file: &Path,
        ctx: &OpContext,
    ) -> Result<bool> {
        let Some(cfg) = rec.cyberpunk.as_ref().filter(|c| c.mods_enabled) else {
            return Ok(false);
        };
        let bin_dir = rec.path().join("tools").join("redmod").join("bin");
        if !bin_dir.is_dir() {
            return Ok(false);
        }
        let install = rec.path();
        let selected = cfg.mods_to_load.clone();
        let mods =
            tokio::task::spawn_blocking(move || mods_to_load(&available_mods(&install), &selected))
                .await?;
        (ctx.step)("Deploying mods");
        let mut args = vec![
            "deploy".to_string(),
            "-reportProgress".into(),
            "-root".into(),
            to_windows_path(&rec.path()),
        ];
        for m in &mods {
            args.push("-mod".into());
            args.push(m.clone());
        }
        let exe = bin_dir.join("redMod.exe");
        let res = crate::wine::run_windows(
            wine,
            &exe.to_string_lossy(),
            &args,
            Some(&bin_dir),
            Some(ctx.cancel.clone()),
            Some(log_file),
        )
        .await?;
        if res.aborted {
            bail!("aborted");
        }
        let succeeded = res.success()
            || res.stdout.contains("deploy has succeeded")
            || res.stderr.contains("deploy has succeeded");
        if !succeeded {
            bail!("Mod deployment failed: {}", res.error_summary());
        }
        Ok(true)
    }

    pub(crate) async fn prepare_game_launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        ctx: OpContext,
    ) -> Result<LaunchPlan> {
        let rec = self.installed_record(app_name)?;
        let install = rec.path();
        if !install.is_dir() {
            bail!(
                "The game files of {} were not found at {}",
                self.title_of(app_name),
                install.display()
            );
        }
        let native = !rec.platform.needs_wine();
        let wine = if native {
            if rec.platform == irori_core::InstallPlatform::Windows {
                self.prepare_prefix(&rec, None, &ctx, &request.log_file)
                    .await?;
            }
            None
        } else {
            let w = WineContext::from_launch(&request.environment)
                .context("No Wine or Proton version is set up for this game")?;
            self.remember_wine_context(app_name, &w).await;
            self.prepare_prefix(&rec, Some(&w), &ctx, &request.log_file)
                .await?;
            Some(w)
        };

        let (mut prefer, mut user_args) = match &request.option {
            Some(LaunchOption::Basic { parameters, .. }) => split_option(parameters),
            _ => (None, Vec::new()),
        };
        user_args.extend(split_args(&request.settings.launcher_args));
        user_args.extend(request.extra_args.iter().cloned());
        let mut task_app = app_name.to_string();
        if app_name == CYBERPUNK_ID
            && self
                .deploy_redmod(&rec, wine.as_ref(), &request.log_file, &ctx)
                .await?
        {
            task_app = REDMOD_ID.into();
            prefer = Some(0);
        }
        let override_exe = match &request.option {
            Some(LaunchOption::AltExe { executable }) => Some(executable.clone()),
            _ => Some(request.settings.target_exe.clone()),
        }
        .filter(|e| !e.is_empty());
        let task = match override_exe {
            Some(exe) => {
                let exe = case_insensitive(&install.join(exe));
                TaskCommand {
                    cwd: exe
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or(install.clone()),
                    exe,
                    args: Vec::new(),
                }
            }
            None => {
                let (dir, platform) = (install.clone(), rec.platform);
                let info = tokio::task::spawn_blocking(move || {
                    manifest::read_info_file(&dir, &task_app, platform)
                })
                .await?;
                task_command(&install, rec.platform, info.as_ref(), prefer)?
            }
        };
        let mut env = request.environment.env.clone();
        for (k, v) in identity_env(app_name) {
            env.entry(k.to_string()).or_insert(v);
        }
        let command = build_command(&task, &user_args, &request.environment, !native, env)?;

        if (self.settings)().comet_support && !request.offline {
            self.acquire_galaxy(app_name).await;
        }
        if let Some(me) = self.arc() {
            let app = app_name.to_string();
            tokio::spawn(async move { me.presence_set_game(Some(app)).await });
        }
        Ok(LaunchPlan {
            command,
            track_wineserver: false,
        })
    }

    /// Start the Galaxy service for a game session (shared by all sessions).
    async fn acquire_galaxy(&self, app_name: &str) {
        let mut g = self.galaxy.lock().await;
        g.sessions.insert(app_name.to_string());
        if g.service.is_some() {
            return;
        }
        let Some(user) = self.user_data().filter(|u| !u.username.is_empty()) else {
            return;
        };
        let creds = match self.auth.credentials().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Galaxy service: {e:#}");
                return;
            }
        };
        let account = irori_gog_galaxy::Account {
            access_token: creds.access_token,
            refresh_token: creds.refresh_token,
            user_id: creds.user_id,
            username: user.username,
        };
        match irori_gog_galaxy::start(account).await {
            Ok(service) => {
                tracing::info!("Galaxy service started for {app_name}");
                g.service = Some(service);
            }
            Err(e) => tracing::warn!("starting the Galaxy service: {e:#}"),
        }
    }

    /// End a game session; the service stops with the last one.
    pub(crate) async fn release_galaxy(&self, app_name: &str) {
        let mut g = self.galaxy.lock().await;
        g.sessions.remove(app_name);
        if g.sessions.is_empty()
            && let Some(service) = g.service.take()
        {
            service.stop();
            tracing::info!("Galaxy service stopped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_core::{WineKind, WineLaunch};

    #[test]
    fn builds_task_commands() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("Bin/x64")).unwrap();
        std::fs::write(d.path().join("Bin/x64/Game.exe"), b"MZ").unwrap();
        let info: GameInfoFile = serde_json::from_str(
            r#"{"name": "G", "playTasks": [
                {"type": "FileTask", "path": "bin\\X64\\game.exe", "isPrimary": true,
                 "arguments": "-path \"C:\\a b\" -x", "workingDir": "bin\\x64"},
                {"type": "FileTask", "path": "cfg.exe", "arguments": ["-a", "b c"]}]}"#,
        )
        .unwrap();
        let t = task_command(d.path(), InstallPlatform::Windows, Some(&info), None).unwrap();
        assert_eq!(
            std::fs::canonicalize(t.exe).unwrap(),
            std::fs::canonicalize(d.path().join("Bin/x64/Game.exe")).unwrap()
        );
        assert_eq!(
            t.args,
            [
                "-path",
                if cfg!(windows) { r"C:\a b" } else { "C:/a b" },
                "-x"
            ]
        );
        assert_eq!(
            std::fs::canonicalize(t.cwd).unwrap(),
            std::fs::canonicalize(d.path().join("Bin/x64")).unwrap()
        );
        let t = task_command(d.path(), InstallPlatform::Windows, Some(&info), Some(1)).unwrap();
        assert_eq!(t.exe, d.path().join("cfg.exe"));
        assert_eq!(t.args, ["-a", "b c"]);
        let t = task_command(d.path(), InstallPlatform::Linux, None, None).unwrap();
        assert_eq!(t.exe, d.path().join("start.sh"));
        assert!(task_command(d.path(), InstallPlatform::Windows, None, None).is_err());

        assert_eq!(
            split_option("--prefer-task 3 -dx11"),
            (Some(3), vec!["-dx11".to_string()])
        );
        assert_eq!(split_option("-safe"), (None, vec!["-safe".to_string()]));
    }

    #[test]
    fn wraps_wine_commands() {
        let t = TaskCommand {
            exe: "/g/game.exe".into(),
            args: vec!["-x".into()],
            cwd: "/g".into(),
        };
        let env = LaunchEnvironment {
            env: BTreeMap::new(),
            wrappers: vec![vec!["gamemoderun".into()]],
            wine: Some(WineLaunch {
                bin: "/p/GE-Proton9-20/proton".into(),
                build_bin: "/p/GE-Proton9-20/proton".into(),
                kind: WineKind::Proton,
                prefix: "/pfx".into(),
                uses_umu: false,
                proton_verb: "waitforexitandrun",
            }),
        };
        let c = build_command(&t, &["-user".into()], &env, true, BTreeMap::new()).unwrap();
        assert_eq!(c.program, PathBuf::from("gamemoderun"));
        let args: Vec<String> = c
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "/p/GE-Proton9-20/proton",
                "waitforexitandrun",
                "/g/game.exe",
                "-x",
                "-user"
            ]
        );
        let native = build_command(
            &t,
            &[],
            &LaunchEnvironment::default(),
            false,
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(native.program, PathBuf::from("/g/game.exe"));
        assert!(
            build_command(
                &t,
                &[],
                &LaunchEnvironment::default(),
                true,
                BTreeMap::new()
            )
            .is_err()
        );
    }

    #[test]
    fn launch_options_from_tasks() {
        let info: GameInfoFile = serde_json::from_str(
            r#"{"name": "Witcher", "playTasks": [
                {"type": "FileTask", "path": "w.exe", "isPrimary": true},
                {"type": "FileTask", "path": "manual.pdf", "category": "document", "name": "Manual"},
                {"type": "URLTask", "link": "https://x", "name": "Site"},
                {"type": "FileTask", "path": "cfg.exe", "name": "Config"},
                {"type": "FileTask", "path": "h.exe", "name": "Hidden", "isHidden": true}]}"#,
        )
        .unwrap();
        let o = launch_options_from_info(&info);
        assert_eq!(
            o,
            vec![
                LaunchOption::Basic {
                    name: "Witcher".into(),
                    parameters: "--prefer-task 0".into()
                },
                LaunchOption::Basic {
                    name: "Config".into(),
                    parameters: "--prefer-task 3".into()
                },
            ]
        );
        let single: GameInfoFile =
            serde_json::from_str(r#"{"playTasks": [{"type": "FileTask", "path": "a.exe"}]}"#)
                .unwrap();
        assert!(launch_options_from_info(&single).is_empty());
    }

    #[test]
    fn redmod_selection() {
        let d = tempfile::tempdir().unwrap();
        for m in ["b", "A", "nomod"] {
            std::fs::create_dir_all(d.path().join("mods").join(m)).unwrap();
        }
        std::fs::write(d.path().join("mods/b/info.json"), "{}").unwrap();
        std::fs::write(d.path().join("mods/A/info.json"), "{}").unwrap();
        let avail = available_mods(d.path());
        assert_eq!(avail, vec!["A", "b"]);
        assert_eq!(mods_to_load(&avail, &[]), vec!["A", "b"]);
        assert_eq!(
            mods_to_load(&avail, &["b".into(), "gone".into()]),
            vec!["b"]
        );
    }

    #[test]
    fn identity_defaults() {
        let e = identity_env("1");
        assert!(e.contains(&("STORE", "gog".to_string())));
        assert!(e.contains(&("GAMEID", "umu-0".to_string())));
    }
}
