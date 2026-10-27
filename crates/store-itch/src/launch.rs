//! Building the launch command of an installed itch.io game: native
//! builds run directly (macOS `.app` bundles through their executable),
//! Windows builds through the Wine layer the runtime prepared. The wrapper
//! chain and environment from [`LaunchRequest::environment`] apply in every
//! case.

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use irori_core::process::{CommandSpec, split_args};
use irori_core::{LaunchOption, LaunchPlan, LaunchRequest, WineKind};

use crate::records::InstalledRecord;

pub(crate) fn build_plan(record: &InstalledRecord, request: &LaunchRequest) -> Result<LaunchPlan> {
    let title = &record.title;
    let install = Path::new(&record.install_path);
    if !install.is_dir() {
        bail!(
            "The files of {title} appear to be deleted (expected at {})",
            install.display()
        );
    }
    let mut args = split_args(&request.settings.launcher_args);
    let mut exe_override = None;
    match &request.option {
        Some(LaunchOption::Basic { parameters, .. }) => args.extend(split_args(parameters)),
        Some(LaunchOption::AltExe { executable }) if !executable.is_empty() => {
            exe_override = Some(executable.clone());
        }
        _ => {}
    }
    let target = request.settings.target_exe.trim();
    if exe_override.is_none() && !target.is_empty() {
        exe_override = Some(target.to_string());
    }
    args.extend(request.extra_args.iter().cloned());

    let exe = match &exe_override {
        Some(e) => record.resolve(e),
        None => record.executable_path().with_context(|| {
            format!(
                "No program to start was found in {title}. Choose one in the game settings (target executable)."
            )
        })?,
    };
    if !exe.exists() {
        bail!("The program of {title} was not found at {}", exe.display());
    }
    let cwd = if exe.is_dir() {
        exe.clone()
    } else {
        exe.parent().unwrap_or(install).to_path_buf()
    };
    let windows_exe = exe
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe") || e.eq_ignore_ascii_case("bat"));
    let uses_wine = record.platform.needs_wine() || (windows_exe && !cfg!(windows));
    let env = &request.environment;
    let mut command = if uses_wine {
        let wine = env.wine.as_ref().with_context(|| {
            format!("{title} is a Windows game, but no Wine or Proton version is configured")
        })?;
        let mut prefix = wine.exec_prefix();
        let program = prefix.remove(0);
        let prefix_dir = wine.prefix.to_string_lossy().into_owned();
        let command = CommandSpec::new(program)
            .args(prefix)
            .arg(exe.as_os_str())
            .args(args)
            .cwd(cwd);
        // Defaults for a bare environment; the runtime's values win.
        if wine.kind == WineKind::Proton {
            command.env("STEAM_COMPAT_DATA_PATH", prefix_dir)
        } else {
            command.env("WINEPREFIX", prefix_dir)
        }
    } else {
        ensure_executable(&exe);
        CommandSpec::new(&exe).args(args).cwd(cwd)
    };
    command = command
        .envs(env.env.iter().map(|(k, v)| (k.clone(), v.clone())))
        .env("HEROIC_APP_SOURCE", "itch")
        .env("STORE", "itchio")
        .wrapped(&env.wrappers);
    Ok(LaunchPlan {
        command,
        track_wineserver: uses_wine,
    })
}

#[cfg(not(unix))]
fn ensure_executable(_path: &Path) {}

/// Archives made on Windows often lose the executable bit of Linux builds.
#[cfg(unix)]
fn ensure_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Ok(meta) = std::fs::metadata(path)
        && meta.is_file()
    {
        let mode = meta.permissions().mode();
        if mode & 0o111 == 0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o755));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use irori_core::{GameSettings, InstallPlatform, LaunchEnvironment, WineLaunch};

    use super::*;

    fn request(wine: Option<WineLaunch>) -> LaunchRequest {
        LaunchRequest {
            settings: GameSettings {
                launcher_args: "-windowed".into(),
                ..Default::default()
            },
            environment: LaunchEnvironment {
                env: [("MY_VAR".to_string(), "1".to_string())].into(),
                wrappers: vec![vec!["gamemoderun".into()]],
                wine,
            },
            option: None,
            extra_args: vec!["--extra".into()],
            offline: false,
            skip_version_check: false,
            log_file: PathBuf::from("/tmp/log"),
        }
    }

    fn args(plan: &LaunchPlan) -> Vec<String> {
        plan.command
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn windows_builds_run_through_wine_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Game")).unwrap();
        std::fs::write(dir.path().join("Game/game.exe"), b"MZ").unwrap();
        let record = InstalledRecord {
            title: "Game".into(),
            install_path: dir.path().to_string_lossy().into_owned(),
            platform: InstallPlatform::Windows,
            executable: "Game/game.exe".into(),
            ..Default::default()
        };
        let wine = WineLaunch {
            bin: "/opt/wine/bin/wine".into(),
            build_bin: "/opt/wine/bin/wine".into(),
            kind: WineKind::Wine,
            prefix: "/pfx".into(),
            uses_umu: false,
            proton_verb: "waitforexitandrun",
        };
        let plan = build_plan(&record, &request(Some(wine))).unwrap();
        assert_eq!(plan.command.program, PathBuf::from("gamemoderun"));
        let a = args(&plan);
        if cfg!(windows) {
            assert!(!plan.track_wineserver);
            assert!(a[0].ends_with("game.exe"));
        } else {
            assert!(plan.track_wineserver);
            assert_eq!(a[0], "/opt/wine/bin/wine");
            assert!(a[1].ends_with("game.exe"));
            assert_eq!(plan.command.env["WINEPREFIX"], "/pfx");
        }
        assert_eq!(&a[a.len() - 2..], ["-windowed", "--extra"]);
        assert_eq!(plan.command.env["STORE"], "itchio");
        assert_eq!(plan.command.env["MY_VAR"], "1");
        assert_eq!(
            plan.command.cwd.as_deref(),
            Some(dir.path().join("Game").as_path())
        );
        if !cfg!(windows) {
            assert!(build_plan(&record, &request(None)).is_err());
        }
    }

    #[test]
    fn reports_missing_files() {
        let record = InstalledRecord {
            title: "Gone".into(),
            install_path: "/nonexistent/itch/game".into(),
            executable: "game".into(),
            ..Default::default()
        };
        let e = build_plan(&record, &request(None)).unwrap_err();
        assert!(e.to_string().contains("appear to be deleted"), "{e:#}");
    }
}
