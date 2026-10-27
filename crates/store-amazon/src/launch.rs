//! Building the game command from `fuel.json`, and the post-install
//! commands run inside the game's prefix.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use irori_amazon_api::fuel::{FuelAction, LaunchInfo};
use irori_amazon_api::sdk;
use irori_core::process::{CommandSpec, split_args};
use irori_core::{GameSettings, LaunchEnvironment, LaunchOption, LaunchPlan, WineKind, WineLaunch};

/// Everything a game command depends on.
pub struct LaunchInputs<'a> {
    pub install_dir: &'a Path,
    pub info: &'a LaunchInfo,
    pub app_name: &'a str,
    pub entitlement_id: &'a str,
    pub sku: &'a str,
    pub display_name: &'a str,
    /// Folder holding the Amazon Games SDK, when available.
    pub sdk_dir: Option<&'a Path>,
    pub settings: &'a GameSettings,
    pub environment: &'a LaunchEnvironment,
    pub option: Option<&'a LaunchOption>,
    pub extra_args: &'a [String],
}

/// Older configs stored binaries wrapped in single quotes.
fn unquote(path: &Path) -> String {
    let s = path.to_string_lossy();
    match s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        Some(inner) => inner.to_string(),
        None => s.into_owned(),
    }
}

/// Variables identifying the game to wrappers and scripts. Values the
/// runtime already set win.
pub fn launch_env(environment: &LaunchEnvironment, app_name: &str) -> BTreeMap<String, String> {
    let mut env = environment.env.clone();
    for (k, v) in [
        ("HEROIC_APP_NAME", app_name),
        ("HEROIC_APP_RUNNER", "nile"),
        ("HEROIC_APP_SOURCE", "amazon"),
        ("STORE", "amazon"),
        ("GAMEID", "umu-0"),
    ] {
        env.entry(k.to_string()).or_insert_with(|| v.to_string());
    }
    env
}

/// The executable to start: the launch option's, the game settings'
/// override, or fuel.json's. Relative overrides are inside the install.
fn executable(i: &LaunchInputs<'_>) -> PathBuf {
    let chosen = match i.option {
        Some(LaunchOption::AltExe { executable }) if !executable.is_empty() => {
            Some(executable.as_str())
        }
        _ => Some(i.settings.target_exe.as_str()).filter(|t| !t.is_empty()),
    };
    match chosen {
        Some(exe) if Path::new(exe).is_absolute() => PathBuf::from(exe),
        Some(exe) => irori_amazon_api::fuel::resolve(i.install_dir, exe),
        None => i.info.executable.clone(),
    }
}

/// wrappers + Wine exec prefix + exe + fuel args + option params + launcher
/// args + extra args, started in fuel.json's working folder with the Fuel
/// SDK variables set.
pub fn build_launch_plan(i: &LaunchInputs<'_>) -> Result<LaunchPlan> {
    let exe = executable(i);
    let mut args = i.info.args.clone();
    if let Some(LaunchOption::Basic { parameters, .. }) = i.option {
        args.extend(split_args(parameters));
    }
    args.extend(split_args(&i.settings.launcher_args));
    args.extend(i.extra_args.iter().cloned());

    let mut env = launch_env(i.environment, i.app_name);
    if let Some(dir) = i.sdk_dir {
        env.insert(
            "FUEL_DIR".into(),
            sdk::fuel_dir(dir).to_string_lossy().into_owned(),
        );
        env.insert(
            "AMAZON_GAMES_SDK_PATH".into(),
            sdk::games_sdk_dir(dir).to_string_lossy().into_owned(),
        );
    }
    env.insert(
        "AMAZON_GAMES_FUEL_ENTITLEMENT_ID".into(),
        i.entitlement_id.into(),
    );
    env.insert("AMAZON_GAMES_FUEL_PRODUCT_SKU".into(), i.sku.into());
    env.insert(
        "AMAZON_GAMES_FUEL_DISPLAY_NAME".into(),
        i.display_name.into(),
    );

    let (command, track_wineserver) = match &i.environment.wine {
        Some(wine) => {
            let mut prefix = wine.exec_prefix().into_iter();
            let program = prefix.next().unwrap_or_else(|| unquote(&wine.bin));
            let cmd = CommandSpec::new(unquote(Path::new(&program)))
                .args(prefix)
                .arg(exe.as_os_str())
                .args(args);
            (cmd, true)
        }
        None if cfg!(windows) => (CommandSpec::new(&exe).args(args), false),
        None => bail!("Choose a Wine or Proton version in the game's settings to run Amazon games"),
    };
    let command = command
        .envs(env)
        .cwd(&i.info.working_dir)
        .wrapped(&i.environment.wrappers);
    Ok(LaunchPlan {
        command,
        track_wineserver,
    })
}

/// Resolve a fuel.json command against the install directory: relative
/// Windows-style paths become absolute paths when the file exists.
fn resolve_command(command: &str, install_dir: &Path) -> String {
    let candidate = irori_amazon_api::fuel::resolve(install_dir, command);
    let is_relative = !command.starts_with(['\\', '/']) && !command.contains(':');
    if is_relative && candidate.is_file() {
        candidate.to_string_lossy().into_owned()
    } else {
        command.to_string()
    }
}

/// Command that runs one post-install action with the game's Wine layer:
/// `wine <cmd> <args>`, `proton run <cmd> <args>` or `umu-run <cmd> <args>`,
/// started in the install directory.
pub fn post_install_command(
    wine: Option<&WineLaunch>,
    env: &BTreeMap<String, String>,
    action: &FuelAction,
    install_dir: &Path,
) -> CommandSpec {
    let Some(wine) = wine else {
        return CommandSpec::new(resolve_command(&action.command, install_dir))
            .args(action.args.iter().cloned())
            .cwd(install_dir)
            .envs(env.clone());
    };
    let mut argv: Vec<String> = vec![unquote(&wine.bin)];
    if wine.kind == WineKind::Proton && !wine.uses_umu {
        argv.push("run".into());
    }
    argv.push(resolve_command(&action.command, install_dir));
    argv.extend(action.args.iter().cloned());

    let program = PathBuf::from(argv.remove(0));
    let mut spec = CommandSpec::new(program).args(argv).cwd(install_dir);
    spec.env = env.clone();
    let prefix = wine.prefix.to_string_lossy().into_owned();
    if wine.kind == WineKind::Proton {
        spec.env
            .entry("STEAM_COMPAT_DATA_PATH".into())
            .or_insert_with(|| prefix.clone());
        if wine.uses_umu {
            spec.env
                .entry("WINEPREFIX".into())
                .or_insert_with(|| prefix.clone());
            if let Some(dir) = wine.build_bin.parent() {
                spec.env
                    .entry("PROTONPATH".into())
                    .or_insert_with(|| dir.to_string_lossy().into_owned());
            }
        }
    } else if wine.kind != WineKind::Crossover {
        spec.env.entry("WINEPREFIX".into()).or_insert(prefix);
    }
    spec
}

/// Path of the list of games already set up in a prefix (Heroic's
/// `installed_games`, shared so imported prefixes are not set up twice).
pub fn prefix_games_file(prefix: &Path) -> PathBuf {
    prefix.join("installed_games")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn env_with(wine: Option<WineLaunch>, wrappers: Vec<Vec<String>>) -> LaunchEnvironment {
        LaunchEnvironment {
            env: BTreeMap::from([("WINEPREFIX".to_string(), "/p".to_string())]),
            wrappers,
            wine,
        }
    }

    fn wine(kind: WineKind, bin: &str, umu: bool) -> WineLaunch {
        WineLaunch {
            bin: bin.into(),
            build_bin: "/proton/GE/proton".into(),
            kind,
            prefix: "/prefixes/Foo".into(),
            uses_umu: umu,
            proton_verb: "waitforexitandrun",
        }
    }

    fn args(spec: &CommandSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|a: &OsString| a.to_string_lossy().into_owned())
            .collect()
    }

    fn info() -> LaunchInfo {
        LaunchInfo {
            executable: "/g/Bin/Game.exe".into(),
            args: vec!["-fuel".into()],
            working_dir: "/g/Bin".into(),
        }
    }

    #[test]
    fn builds_launch_plans() {
        let settings = GameSettings {
            launcher_args: "-windowed \"-name X\"".into(),
            ..Default::default()
        };
        let environment = env_with(
            Some(wine(WineKind::Proton, "/proton/proton", false)),
            vec![vec!["gamemoderun".into()]],
        );
        let basic = LaunchOption::Basic {
            name: "Safe".into(),
            parameters: "-safe".into(),
        };
        let extra = vec!["--extra".to_string()];
        let info = info();
        let mut inputs = LaunchInputs {
            install_dir: Path::new("/g"),
            info: &info,
            app_name: "amzn1.adg.product.abc",
            entitlement_id: "ent-1",
            sku: "SKU",
            display_name: "Radu",
            sdk_dir: Some(Path::new("/sdk")),
            settings: &settings,
            environment: &environment,
            option: Some(&basic),
            extra_args: &extra,
        };
        let plan = build_launch_plan(&inputs).unwrap();
        let spec = &plan.command;
        assert!(plan.track_wineserver);
        assert_eq!(spec.program, PathBuf::from("gamemoderun"));
        assert_eq!(
            args(spec),
            [
                "/proton/proton",
                "waitforexitandrun",
                "/g/Bin/Game.exe",
                "-fuel",
                "-safe",
                "-windowed",
                "-name X",
                "--extra"
            ]
        );
        assert_eq!(spec.cwd.as_deref(), Some(Path::new("/g/Bin")));
        assert_eq!(
            Path::new(&spec.env["FUEL_DIR"]),
            Path::new("/sdk/Amazon Games Services/Legacy")
        );
        assert_eq!(
            Path::new(&spec.env["AMAZON_GAMES_SDK_PATH"]),
            Path::new("/sdk/Amazon Games Services/AmazonGamesSDK")
        );
        assert_eq!(spec.env["AMAZON_GAMES_FUEL_ENTITLEMENT_ID"], "ent-1");
        assert_eq!(spec.env["AMAZON_GAMES_FUEL_PRODUCT_SKU"], "SKU");
        assert_eq!(spec.env["AMAZON_GAMES_FUEL_DISPLAY_NAME"], "Radu");
        assert_eq!(spec.env["HEROIC_APP_SOURCE"], "amazon");
        assert_eq!(spec.env["WINEPREFIX"], "/p");

        // Executable overrides: launch option, then settings (relative to
        // the install folder).
        let absolute = if cfg!(windows) {
            "C:/abs/Other.exe"
        } else {
            "/abs/Other.exe"
        };
        let alt = LaunchOption::AltExe {
            executable: absolute.into(),
        };
        inputs.option = Some(&alt);
        inputs.extra_args = &[];
        inputs.sdk_dir = None;
        let plan = build_launch_plan(&inputs).unwrap();
        assert_eq!(Path::new(&args(&plan.command)[2]), Path::new(absolute));
        assert!(!plan.command.env.contains_key("FUEL_DIR"));
        let settings = GameSettings {
            target_exe: "Tools\\Editor.exe".into(),
            ..Default::default()
        };
        inputs.settings = &settings;
        inputs.option = None;
        assert_eq!(
            Path::new(&args(&build_launch_plan(&inputs).unwrap().command)[2]),
            Path::new("/g/Tools/Editor.exe")
        );

        let umu = env_with(Some(wine(WineKind::Proton, "/bin/umu-run", true)), vec![]);
        inputs.environment = &umu;
        let plan = build_launch_plan(&inputs).unwrap();
        assert_eq!(plan.command.program, PathBuf::from("/bin/umu-run"));
        assert_eq!(
            Path::new(&args(&plan.command)[0]),
            Path::new("/g/Tools/Editor.exe")
        );

        let none = env_with(None, vec![]);
        inputs.environment = &none;
        assert_eq!(build_launch_plan(&inputs).is_err(), !cfg!(windows));
    }

    #[test]
    fn post_install_uses_wine_layer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("__Installer")).unwrap();
        std::fs::write(dir.path().join("__Installer/vc.exe"), b"").unwrap();
        let action = FuelAction {
            command: "__Installer\\vc.exe".into(),
            args: vec!["/quiet".into()],
        };
        let env = BTreeMap::new();

        let proton = wine(WineKind::Proton, "/proton/proton", false);
        let spec = post_install_command(Some(&proton), &env, &action, dir.path());
        assert_eq!(spec.program, PathBuf::from("/proton/proton"));
        let a = args(&spec);
        assert_eq!(a[0], "run");
        assert_eq!(Path::new(&a[1]), dir.path().join("__Installer/vc.exe"));
        assert_eq!(a[2], "/quiet");
        assert_eq!(spec.env["STEAM_COMPAT_DATA_PATH"], "/prefixes/Foo");
        assert_eq!(spec.cwd.as_deref(), Some(dir.path()));

        let w = wine(WineKind::Wine, "/usr/bin/wine", false);
        let missing = FuelAction {
            command: "C:\\x.exe".into(),
            args: vec![],
        };
        let spec = post_install_command(Some(&w), &env, &missing, dir.path());
        assert_eq!(args(&spec), vec!["C:\\x.exe"]);
        assert_eq!(spec.env["WINEPREFIX"], "/prefixes/Foo");

        let umu = wine(WineKind::Proton, "/bin/umu-run", true);
        let spec = post_install_command(Some(&umu), &env, &missing, dir.path());
        assert_eq!(spec.program, PathBuf::from("/bin/umu-run"));
        assert_eq!(spec.env["PROTONPATH"], "/proton/GE");
    }
}
