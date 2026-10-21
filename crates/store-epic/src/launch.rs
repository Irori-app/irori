//! Building game commands from Epic launch parameters and the runtime's
//! launch environment.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ember_core::process::{CommandSpec, split_args};
use ember_core::{LaunchEnvironment, LaunchOption, WineKind, WineLaunch};
use ember_epic_api::launch::LaunchParams;

fn strip_quotes(bin: &Path) -> String {
    bin.to_string_lossy().trim_matches('\'').to_string()
}

/// Arguments chosen by the user: the launch option's parameters, the game
/// settings' launcher arguments and extra arguments of the request.
pub(crate) fn user_args(
    option: Option<&LaunchOption>,
    launcher_args: &str,
    extra_args: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(LaunchOption::Basic { parameters, .. }) = option {
        out.extend(split_args(parameters));
    }
    out.extend(split_args(launcher_args));
    out.extend(extra_args.iter().cloned());
    out
}

/// The executable override chosen for this launch, if any.
pub(crate) fn override_exe<'a>(
    option: Option<&'a LaunchOption>,
    target_exe: &'a str,
) -> Option<&'a str> {
    match option {
        Some(LaunchOption::AltExe { executable }) if !executable.is_empty() => Some(executable),
        _ => Some(target_exe).filter(|t| !t.is_empty()),
    }
}

/// Inputs of [`build_command`].
pub(crate) struct CommandInput<'a> {
    pub params: &'a LaunchParams,
    pub user_args: &'a [String],
    pub environment: &'a LaunchEnvironment,
    pub env: BTreeMap<String, String>,
    /// Windows program started in front of the game (the fake
    /// `EpicGamesLauncher.exe`).
    pub wrapper_exe: Option<&'a str>,
    pub needs_wine: bool,
}

/// `wrappers + [wine prefix] + [wrapper exe] + exe + game args + user args
/// + Epic args`, in the executable's folder.
pub(crate) fn build_command(i: &CommandInput<'_>) -> Result<CommandSpec> {
    let mut argv: Vec<String> = Vec::new();
    if i.needs_wine {
        let wine = i
            .environment
            .wine
            .as_ref()
            .context("No Wine or Proton version is configured for this game")?;
        argv.extend(
            wine.exec_prefix()
                .into_iter()
                .map(|p| strip_quotes(Path::new(&p))),
        );
        if let Some(w) = i.wrapper_exe {
            argv.push(w.to_string());
        }
    }
    argv.push(i.params.executable.to_string_lossy().into_owned());
    argv.extend(i.params.game_args.iter().cloned());
    argv.extend(i.user_args.iter().cloned());
    argv.extend(i.params.egl_args.iter().cloned());
    let program = argv.remove(0);
    Ok(CommandSpec::new(program)
        .args(argv)
        .envs(i.env.clone())
        .cwd(&i.params.working_dir)
        .wrapped(&i.environment.wrappers))
}

/// Command prefix for running a Windows program during prefix setup:
/// `[wine]`, `[proton, run]` or `[umu-run]`.
pub(crate) fn setup_exec_prefix(wine: &WineLaunch) -> Vec<String> {
    let bin = strip_quotes(&wine.bin);
    if wine.kind == WineKind::Proton && !wine.uses_umu {
        vec![bin, "run".into()]
    } else {
        vec![bin]
    }
}

/// Whether an environment value enables an option (`USE_FAKE_EPIC_EXE`).
pub(crate) fn env_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0" && v != "false")
}

/// Windows path legendary passes as `LEGENDARY_WRAPPER_EXE`.
pub(crate) const FAKE_EXE_WIN_PATH: &str = r"C:\windows\command\EpicGamesLauncher.exe";

/// Location of [`FAKE_EXE_WIN_PATH`] inside a prefix's `drive_c`.
pub(crate) fn fake_exe_in_prefix(real_prefix: &Path) -> PathBuf {
    real_prefix
        .join("drive_c")
        .join("windows")
        .join("command")
        .join("EpicGamesLauncher.exe")
}

/// The bundled fake `EpicGamesLauncher.exe`: Ember's bin dir, then Heroic's
/// resources.
pub(crate) fn find_fake_exe(ember_bin: &Path) -> Option<PathBuf> {
    const HEROIC: &str = "resources/app.asar.unpacked/build/bin/x64/win32/EpicGamesLauncher.exe";
    std::iter::once(ember_bin.join("EpicGamesLauncher.exe"))
        .chain(
            [
                "/usr/lib64/heroic",
                "/usr/lib/heroic",
                "/opt/Heroic",
                "/opt/heroic",
            ]
            .iter()
            .map(|d| Path::new(d).join(HEROIC)),
        )
        .find(|p| p.is_file())
}

/// Fail early when a Windows game has no Wine layer.
pub(crate) fn require_wine(env: &LaunchEnvironment, needs_wine: bool) -> Result<()> {
    if needs_wine && env.wine.is_none() {
        bail!(
            "No Wine or Proton version is configured for this game. Choose one in the game settings."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn wine(kind: WineKind, bin: &str, umu: bool) -> WineLaunch {
        WineLaunch {
            bin: bin.into(),
            build_bin: "/proton/GE-Proton9-20/proton".into(),
            kind,
            prefix: "/pfx".into(),
            uses_umu: umu,
            proton_verb: "waitforexitandrun",
        }
    }

    fn env(wrappers: Vec<Vec<&str>>, w: Option<WineLaunch>) -> LaunchEnvironment {
        LaunchEnvironment {
            env: Default::default(),
            wrappers: wrappers
                .into_iter()
                .map(|v| v.into_iter().map(String::from).collect())
                .collect(),
            wine: w,
        }
    }

    fn params() -> LaunchParams {
        LaunchParams {
            executable: "/g/Bin/Game.exe".into(),
            working_dir: "/g/Bin".into(),
            game_args: vec!["-nosplash".into()],
            egl_args: vec!["-epicapp=Sugar".into()],
        }
    }

    fn args(spec: &CommandSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|a: &OsString| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn builds_wine_and_native_commands() {
        let p = params();
        let user = user_args(
            Some(&LaunchOption::Basic {
                name: "Safe".into(),
                parameters: "-safe \"-name=a b\"".into(),
            }),
            "-dx11",
            &["-proto".into()],
        );
        assert_eq!(user, ["-safe", "-name=a b", "-dx11", "-proto"]);
        let e = env(
            vec![vec!["gamemoderun"]],
            Some(wine(WineKind::Proton, "'/p/proton'", false)),
        );
        let spec = build_command(&CommandInput {
            params: &p,
            user_args: &user,
            environment: &e,
            env: BTreeMap::from([("STORE".into(), "egs".into())]),
            wrapper_exe: Some(FAKE_EXE_WIN_PATH),
            needs_wine: true,
        })
        .unwrap();
        assert_eq!(spec.program, PathBuf::from("gamemoderun"));
        assert_eq!(
            args(&spec),
            [
                "/p/proton",
                "waitforexitandrun",
                FAKE_EXE_WIN_PATH,
                "/g/Bin/Game.exe",
                "-nosplash",
                "-safe",
                "-name=a b",
                "-dx11",
                "-proto",
                "-epicapp=Sugar"
            ]
        );
        assert_eq!(spec.cwd.as_deref(), Some(Path::new("/g/Bin")));
        assert_eq!(spec.env["STORE"], "egs");

        let native = build_command(&CommandInput {
            params: &p,
            user_args: &[],
            environment: &env(vec![], None),
            env: BTreeMap::new(),
            wrapper_exe: Some("ignored"),
            needs_wine: false,
        })
        .unwrap();
        assert_eq!(native.program, PathBuf::from("/g/Bin/Game.exe"));
        assert_eq!(args(&native), ["-nosplash", "-epicapp=Sugar"]);
        assert!(
            build_command(&CommandInput {
                params: &p,
                user_args: &[],
                environment: &env(vec![], None),
                env: BTreeMap::new(),
                wrapper_exe: None,
                needs_wine: true,
            })
            .is_err()
        );
    }

    #[test]
    fn picks_executable_overrides() {
        let alt = LaunchOption::AltExe {
            executable: "alt.exe".into(),
        };
        assert_eq!(override_exe(Some(&alt), "target.exe"), Some("alt.exe"));
        assert_eq!(override_exe(None, "target.exe"), Some("target.exe"));
        assert_eq!(override_exe(None, ""), None);
    }

    #[test]
    fn helpers() {
        assert!(env_enabled(Some("1")));
        assert!(!env_enabled(Some("false")));
        assert!(!env_enabled(Some("0")));
        assert!(!env_enabled(None));
        assert_eq!(
            setup_exec_prefix(&wine(WineKind::Proton, "/p/proton", false)),
            vec!["/p/proton", "run"]
        );
        assert_eq!(
            setup_exec_prefix(&wine(WineKind::Wine, "/w/wine", false)),
            vec!["/w/wine"]
        );
        assert!(require_wine(&env(vec![], None), true).is_err());
    }
}
