//! The wrapper chain placed in front of a game: gamescope, the user's
//! wrappers, MangoHud, GameMode and (for native games on the Steam runtime)
//! umu-run.

use ember_core::GameSettings;
use ember_core::GamescopeSettings;
use ember_core::process::split_args;

/// Whether gamescope is configured at all: any of its options asks for it,
/// including a cursor grab or additional options set on their own.
pub fn gamescope_enabled(gs: &GamescopeSettings) -> bool {
    gs.enable_limiter
        || gs.enable_upscaling
        || gs.enable_force_grab_cursor
        || !gs.additional_options.trim().is_empty()
}

/// gamescope arguments after the binary, ending with `--`.
///
/// `new_cli` selects the 3.12+ flags (`-F`/`-S`), detected by looking for
/// `-F, --filter` in `gamescope --help`. With `show_mangohud`, MangoHud runs
/// inside gamescope through `--mangoapp`.
pub fn gamescope_args(gs: &GamescopeSettings, show_mangohud: bool, new_cli: bool) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let set = |s: &str| !s.trim().is_empty();
    if gs.enable_upscaling {
        for (flag, value) in [
            ("-w", &gs.game_width),
            ("-h", &gs.game_height),
            ("-W", &gs.upscale_width),
            ("-H", &gs.upscale_height),
        ] {
            if set(value) {
                args.push(flag.into());
                args.push(value.trim().into());
            }
        }
        match (gs.upscale_method.as_str(), new_cli) {
            ("fsr", true) => args.extend(["-F".into(), "fsr".into()]),
            ("fsr", false) => args.push("-U".into()),
            ("nis", true) => args.extend(["-F".into(), "nis".into()]),
            ("nis", false) => args.push("-Y".into()),
            ("integer", true) => args.extend(["-S".into(), "integer".into()]),
            ("integer", false) => args.push("-i".into()),
            ("stretch", true) => args.extend(["-S".into(), "stretch".into()]),
            _ => {}
        }
        match gs.window_type.as_str() {
            "fullscreen" => args.push("-f".into()),
            "borderless" => args.push("-b".into()),
            _ => {}
        }
    }
    if gs.enable_limiter {
        if set(&gs.fps_limiter) {
            args.push("-r".into());
            args.push(gs.fps_limiter.trim().into());
        }
        if set(&gs.fps_limiter_no_focus) {
            args.push("-o".into());
            args.push(gs.fps_limiter_no_focus.trim().into());
        }
    }
    if gs.enable_force_grab_cursor {
        args.push("--force-grab-cursor".into());
    }
    if show_mangohud {
        args.push("--mangoapp".into());
    }
    args.extend(split_args(&gs.additional_options));
    args.push("--".into());
    args
}

/// Resolved wrapper programs for one launch.
#[derive(Debug, Clone, Default)]
pub struct WrapperParts {
    /// `[gamescope, args…, --]`; empty when gamescope is off or missing.
    pub gamescope: Vec<String>,
    /// `[mangohud, --dlsym]` when MangoHud is enabled.
    pub mangohud: Vec<String>,
    /// `gamemoderun` when GameMode is enabled.
    pub gamemode: Option<String>,
    /// `[umu-run]` for native games on the Steam runtime.
    pub steam_runtime: Vec<String>,
}

/// The wrapper chain, outermost first, in Heroic's order: gamescope, user
/// wrappers, MangoHud (only without gamescope, which uses `--mangoapp`
/// instead), gamemoderun, then umu-run. Empty arguments are dropped.
pub fn setup_wrappers(gs: &GameSettings, parts: &WrapperParts) -> Vec<Vec<String>> {
    let mut chain: Vec<Vec<String>> = Vec::new();
    if !parts.gamescope.is_empty() {
        chain.push(parts.gamescope.clone());
    }
    for w in &gs.wrapper_options {
        let exe = w.exe.trim();
        if exe.is_empty() {
            continue;
        }
        let mut cmd = vec![exe.to_string()];
        cmd.extend(split_args(&w.args));
        chain.push(cmd);
    }
    if parts.gamescope.is_empty() && !parts.mangohud.is_empty() {
        chain.push(parts.mangohud.clone());
    }
    if let Some(gm) = &parts.gamemode {
        chain.push(vec![gm.clone()]);
    }
    if !parts.steam_runtime.is_empty() {
        chain.push(parts.steam_runtime.clone());
    }
    chain
        .into_iter()
        .map(|c| c.into_iter().filter(|a| !a.is_empty()).collect::<Vec<_>>())
        .filter(|c| !c.is_empty())
        .collect()
}

/// Join an argv into one shell-quoted string (Python `shlex.join` style), as
/// store helpers take wrappers through a single `--wrapper` argument.
pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| shlex_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn any_gamescope_option_enables_it() {
        let mut gs = GamescopeSettings::default();
        assert!(!gamescope_enabled(&gs));
        gs.additional_options = "  ".into();
        assert!(!gamescope_enabled(&gs));
        gs.additional_options = "--hdr-enabled".into();
        assert!(gamescope_enabled(&gs));
        assert_eq!(gamescope_args(&gs, false, true), ["--hdr-enabled", "--"]);
        let grab = GamescopeSettings {
            enable_force_grab_cursor: true,
            ..Default::default()
        };
        assert!(gamescope_enabled(&grab));
    }

    use super::*;
    use ember_core::Wrapper;

    fn scope() -> GamescopeSettings {
        GamescopeSettings {
            enable_upscaling: true,
            enable_limiter: true,
            enable_force_grab_cursor: true,
            window_type: "borderless".into(),
            game_width: "1280".into(),
            game_height: "720".into(),
            upscale_width: "2560".into(),
            upscale_height: "".into(),
            upscale_method: "fsr".into(),
            fps_limiter: "60".into(),
            fps_limiter_no_focus: "15".into(),
            additional_options: "--hdr-enabled -e".into(),
        }
    }

    #[test]
    fn gamescope_new_cli() {
        assert_eq!(
            gamescope_args(&scope(), true, true),
            vec![
                "-w",
                "1280",
                "-h",
                "720",
                "-W",
                "2560",
                "-F",
                "fsr",
                "-b",
                "-r",
                "60",
                "-o",
                "15",
                "--force-grab-cursor",
                "--mangoapp",
                "--hdr-enabled",
                "-e",
                "--"
            ]
        );
    }

    #[test]
    fn gamescope_old_cli_and_methods() {
        let mut gs = scope();
        gs.enable_limiter = false;
        gs.enable_force_grab_cursor = false;
        gs.additional_options.clear();
        gs.window_type = "fullscreen".into();
        assert_eq!(
            gamescope_args(&gs, false, false),
            vec!["-w", "1280", "-h", "720", "-W", "2560", "-U", "-f", "--"]
        );
        for (method, new, old) in [
            ("nis", vec!["-F", "nis"], vec!["-Y"]),
            ("integer", vec!["-S", "integer"], vec!["-i"]),
            ("stretch", vec!["-S", "stretch"], vec![]),
        ] {
            let g = GamescopeSettings {
                enable_upscaling: true,
                upscale_method: method.into(),
                window_type: "windowed".into(),
                ..Default::default()
            };
            let mut n: Vec<&str> = new.clone();
            n.push("--");
            let mut o: Vec<&str> = old.clone();
            o.push("--");
            assert_eq!(gamescope_args(&g, false, true), n);
            assert_eq!(gamescope_args(&g, false, false), o);
        }
    }

    #[test]
    fn wrapper_order() {
        let gs = GameSettings {
            wrapper_options: vec![
                Wrapper {
                    exe: "strace".into(),
                    args: "-f -o '/tmp/a b'".into(),
                },
                Wrapper {
                    exe: " ".into(),
                    args: "x".into(),
                },
            ],
            ..Default::default()
        };
        let mut parts = WrapperParts {
            gamescope: vec!["/usr/bin/gamescope".into(), "-f".into(), "--".into()],
            mangohud: vec!["/usr/bin/mangohud".into(), "--dlsym".into()],
            gamemode: Some("/usr/bin/gamemoderun".into()),
            steam_runtime: vec!["/usr/bin/umu-run".into()],
        };
        assert_eq!(
            setup_wrappers(&gs, &parts),
            vec![
                vec!["/usr/bin/gamescope", "-f", "--"],
                vec!["strace", "-f", "-o", "/tmp/a b"],
                vec!["/usr/bin/gamemoderun"],
                vec!["/usr/bin/umu-run"],
            ]
        );
        parts.gamescope.clear();
        assert_eq!(
            setup_wrappers(&gs, &parts),
            vec![
                vec!["strace", "-f", "-o", "/tmp/a b"],
                vec!["/usr/bin/mangohud", "--dlsym"],
                vec!["/usr/bin/gamemoderun"],
                vec!["/usr/bin/umu-run"],
            ]
        );
        assert!(setup_wrappers(&GameSettings::default(), &WrapperParts::default()).is_empty());
    }

    #[test]
    fn joins_like_shlex() {
        let argv = vec![
            "strace".to_string(),
            "-o".into(),
            "/tmp/a b".into(),
            "it's".into(),
            "".into(),
        ];
        assert_eq!(shell_join(&argv), r#"strace -o '/tmp/a b' 'it'"'"'s' ''"#);
    }
}
