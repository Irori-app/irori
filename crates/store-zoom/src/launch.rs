//! Building the launch command of an installed ZOOM game.
//!
//! * Native Linux builds run directly (`start.sh` or the game binary).
//! * DOSBox games run with the system DOSBox and the game's `.conf` files,
//!   from the directory holding those files; without a system DOSBox the
//!   bundled `dosbox.exe` runs through Wine.
//! * Windows builds run through the Wine layer prepared by the runtime.
//!
//! The wrapper chain and environment from [`LaunchRequest::environment`] are
//! applied in every case.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ember_core::process::{CommandSpec, split_args};
use ember_core::{LaunchOption, LaunchPlan, LaunchRequest, WineKind};

use crate::records::InstalledRecord;

/// Build the command for `record`. `dosbox` is the system DOSBox, if any.
pub fn build_plan(
    record: &InstalledRecord,
    title: &str,
    request: &LaunchRequest,
    dosbox: Option<&Path>,
) -> Result<LaunchPlan> {
    let install = PathBuf::from(&record.install_path);
    if !install.is_dir() {
        bail!(
            "The files of {title} appear to be deleted (expected at {})",
            install.display()
        );
    }

    let mut args: Vec<String> = split_args(&request.settings.launcher_args);
    let mut exe_override = None;
    match &request.option {
        Some(LaunchOption::Basic { parameters, .. }) => args.extend(split_args(parameters)),
        Some(LaunchOption::AltExe { executable }) if !executable.is_empty() => {
            exe_override = Some(executable.clone());
        }
        _ => {}
    }
    if exe_override.is_none() && !request.settings.target_exe.trim().is_empty() {
        exe_override = Some(request.settings.target_exe.trim().to_string());
    }
    args.extend(request.extra_args.iter().cloned());

    let env = &request.environment;
    let uses_wine = command_uses_wine(record, request, dosbox, exe_override.is_some());
    let mut command = if record.is_dosbox && exe_override.is_none() {
        dosbox_command(record, title, request, dosbox, args)?
    } else {
        let exe = match &exe_override {
            Some(e) => record.resolve(e),
            None => record.executable_path().with_context(|| {
                format!(
                    "No executable is known for {title}. Choose one in the game settings (target executable)."
                )
            })?,
        };
        if !exe.is_file() {
            bail!(
                "The executable of {title} was not found at {}",
                exe.display()
            );
        }
        let cwd = exe.parent().unwrap_or(&install).to_path_buf();
        let windows_exe = exe
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe") || e.eq_ignore_ascii_case("bat"));
        if record.platform.needs_wine() || windows_exe {
            wine_command(request, title, &exe, &cwd, args)?
        } else {
            ensure_executable(&exe);
            CommandSpec::new(&exe).args(args).cwd(cwd)
        }
    };

    if let Some(wine) = env.wine.as_ref().filter(|_| uses_wine) {
        // Defaults for a bare environment; the runtime's values win.
        let prefix = wine.prefix.to_string_lossy().into_owned();
        command = if wine.kind == WineKind::Proton {
            command.env("STEAM_COMPAT_DATA_PATH", prefix)
        } else {
            command.env("WINEPREFIX", prefix)
        };
    }
    command = command
        .envs(env.env.iter().map(|(k, v)| (k.clone(), v.clone())))
        .env("HEROIC_APP_SOURCE", "zoom")
        .env("STORE", "zoomplatform")
        .wrapped(&env.wrappers);
    Ok(LaunchPlan {
        command,
        track_wineserver: uses_wine,
    })
}

fn command_uses_wine(
    record: &InstalledRecord,
    request: &LaunchRequest,
    dosbox: Option<&Path>,
    overridden: bool,
) -> bool {
    if record.is_dosbox && !overridden {
        dosbox.is_none() && request.environment.wine.is_some()
    } else {
        record.platform.needs_wine()
    }
}

fn wine_command(
    request: &LaunchRequest,
    title: &str,
    exe: &Path,
    cwd: &Path,
    args: Vec<String>,
) -> Result<CommandSpec> {
    let wine = request.environment.wine.as_ref().with_context(|| {
        format!("{title} is a Windows game, but no Wine or Proton version is configured")
    })?;
    let mut prefix = wine.exec_prefix();
    let program = prefix.remove(0);
    Ok(CommandSpec::new(program)
        .args(prefix)
        .arg(exe.as_os_str())
        .args(args)
        .cwd(cwd))
}

fn dosbox_command(
    record: &InstalledRecord,
    title: &str,
    request: &LaunchRequest,
    dosbox: Option<&Path>,
    args: Vec<String>,
) -> Result<CommandSpec> {
    let cwd = record.resolve(&record.dosbox_dir);
    let mut conf_args: Vec<String> = Vec::new();
    for conf in &record.dosbox_conf {
        conf_args.push("-conf".into());
        conf_args.push(record.resolve(conf).to_string_lossy().into_owned());
    }
    if let Some(bin) = dosbox {
        return Ok(CommandSpec::new(bin).args(conf_args).args(args).cwd(cwd));
    }
    let bundled = record
        .executable_path()
        .filter(|p| p.is_file() && record.executable.to_ascii_lowercase().ends_with(".exe"));
    match (bundled, &request.environment.wine) {
        (Some(exe), Some(_)) => {
            // Windows DOSBox understands host paths through Wine's Z: drive.
            wine_command(request, title, &exe, &cwd, [conf_args, args].concat())
        }
        _ => bail!(
            "{title} needs DOSBox. Install DOSBox (dosbox, dosbox-staging or dosbox-x) from your distribution."
        ),
    }
}

fn ensure_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Ok(meta) = std::fs::metadata(path) {
        let mode = meta.permissions().mode();
        if mode & 0o111 == 0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o755));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ember_core::{GameSettings, InstallPlatform, LaunchEnvironment, WineLaunch};

    use super::*;

    fn request(wine: Option<WineLaunch>) -> LaunchRequest {
        let mut env = BTreeMap::new();
        env.insert("WINEPREFIX".to_string(), "/pfx".to_string());
        LaunchRequest {
            settings: GameSettings {
                launcher_args: "-fullscreen \"-name x\"".into(),
                ..Default::default()
            },
            environment: LaunchEnvironment {
                env,
                wrappers: vec![vec!["gamemoderun".into()]],
                wine,
            },
            option: None,
            extra_args: vec!["--extra".into()],
            offline: false,
            skip_version_check: false,
            log_file: PathBuf::from("/tmp/x.log"),
        }
    }

    fn proton() -> WineLaunch {
        WineLaunch {
            bin: "/p/proton".into(),
            build_bin: "/p/proton".into(),
            kind: WineKind::Proton,
            prefix: "/pfx".into(),
            uses_umu: false,
            proton_verb: "waitforexitandrun",
        }
    }

    fn args(spec: &CommandSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn install(files: &[&str]) -> (tempfile::TempDir, InstalledRecord) {
        let d = tempfile::tempdir().unwrap();
        for f in files {
            let p = d.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
        let rec = InstalledRecord {
            app_name: "1".into(),
            install_path: d.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        (d, rec)
    }

    #[test]
    fn native_launch_uses_wrappers_and_env() {
        let (d, mut rec) = install(&["Game/start.sh"]);
        rec.executable = "Game/start.sh".into();
        rec.platform = InstallPlatform::Linux;
        let plan = build_plan(&rec, "G", &request(None), None).unwrap();
        assert_eq!(plan.command.program, PathBuf::from("gamemoderun"));
        let a = args(&plan.command);
        assert_eq!(a[0], d.path().join("Game/start.sh").to_string_lossy());
        assert_eq!(&a[1..], ["-fullscreen", "-name x", "--extra"]);
        assert_eq!(
            plan.command.cwd.as_deref(),
            Some(d.path().join("Game").as_path())
        );
        assert_eq!(plan.command.env["STORE"], "zoomplatform");
        assert_eq!(plan.command.env["WINEPREFIX"], "/pfx");
        assert!(!plan.track_wineserver);
    }

    #[test]
    fn windows_launch_goes_through_wine() {
        let (d, mut rec) = install(&["bin/Game.exe", "Other.exe"]);
        rec.executable = "bin/Game.exe".into();
        rec.platform = InstallPlatform::Windows;
        let plan = build_plan(&rec, "G", &request(Some(proton())), None).unwrap();
        let a = args(&plan.command);
        assert_eq!(a[0], "/p/proton");
        assert_eq!(a[1], "waitforexitandrun");
        assert_eq!(a[2], d.path().join("bin/Game.exe").to_string_lossy());
        assert!(plan.track_wineserver);

        let mut req = request(Some(proton()));
        req.option = Some(LaunchOption::AltExe {
            executable: "Other.exe".into(),
        });
        let plan = build_plan(&rec, "G", &req, None).unwrap();
        assert_eq!(
            args(&plan.command)[2],
            d.path().join("Other.exe").to_string_lossy()
        );

        let err = build_plan(&rec, "G", &request(None), None).unwrap_err();
        assert!(err.to_string().contains("Wine"));
    }

    #[test]
    fn dosbox_launch_prefers_system_dosbox() {
        let (d, mut rec) = install(&["DOSBOX/dosbox.exe", "game/a.conf", "game/b.conf"]);
        rec.is_dosbox = true;
        rec.executable = "DOSBOX/dosbox.exe".into();
        rec.dosbox_dir = "game".into();
        rec.dosbox_conf = vec!["game/a.conf".into(), "game/b.conf".into()];
        rec.platform = InstallPlatform::Linux;
        let mut req = request(None);
        req.environment.wrappers.clear();
        req.settings.launcher_args.clear();
        req.extra_args.clear();
        let plan = build_plan(&rec, "G", &req, Some(Path::new("/usr/bin/dosbox"))).unwrap();
        assert_eq!(plan.command.program, PathBuf::from("/usr/bin/dosbox"));
        let a = args(&plan.command);
        assert_eq!(a[0], "-conf");
        assert_eq!(a[1], d.path().join("game/a.conf").to_string_lossy());
        assert_eq!(a.len(), 4);
        assert_eq!(
            plan.command.cwd.as_deref(),
            Some(d.path().join("game").as_path())
        );

        let err = build_plan(&rec, "G", &req, None).unwrap_err();
        assert!(err.to_string().contains("DOSBox"));

        req.environment.wine = Some(proton());
        let plan = build_plan(&rec, "G", &req, None).unwrap();
        assert_eq!(plan.command.program, PathBuf::from("/p/proton"));
        assert!(args(&plan.command).contains(&"-conf".to_string()));
        assert!(plan.track_wineserver);
    }

    #[test]
    fn missing_install_is_reported() {
        let rec = InstalledRecord {
            install_path: "/nonexistent/zoom/game".into(),
            executable: "x".into(),
            ..Default::default()
        };
        let err = build_plan(&rec, "G", &request(None), None).unwrap_err();
        assert!(err.to_string().contains("deleted"));
    }
}
