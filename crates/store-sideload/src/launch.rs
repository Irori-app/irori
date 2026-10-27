//! Launch commands for sideloaded apps.
//!
//! * Linux apps run directly (wrappers and environment from the runtime, cwd =
//!   the executable's folder).
//! * Windows apps run through the runtime's Wine layer: `exec_prefix + exe`.
//! * Browser apps open in the user's default browser through `xdg-open`. When
//!   a custom user agent or full screen is requested they open instead in a
//!   dedicated app window of an installed browser: Chromium-family browsers
//!   (`--app=<url>`, `--user-agent`, `--start-fullscreen`) are preferred
//!   because they give a chrome-less window; Firefox-family browsers are the
//!   fallback (`--kiosk` for full screen, user agent through the profile's
//!   `user.js`). Each web app host gets its own browser profile under the
//!   store's config directory, which keeps its logins and saves separate and
//!   makes the browser process live as long as the window, so playtime is
//!   tracked. If no supported browser is installed, `xdg-open` is used and
//!   the user agent/full-screen options are ignored.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use irori_core::paths::{expand_home, sanitize};
use irori_core::process::{CommandSpec, split_args};
use irori_core::{LaunchEnvironment, LaunchOption, LaunchPlan, LaunchRequest};

use crate::app::SideloadApp;

/// Chromium-family browsers, in order of preference.
const CHROMIUM_BROWSERS: &[&str] = &[
    "chromium",
    "chromium-browser",
    "google-chrome-stable",
    "google-chrome",
    "brave-browser",
    "brave",
    "microsoft-edge-stable",
    "vivaldi-stable",
];

/// Firefox-family browsers, used when no Chromium-family browser exists.
const FIREFOX_BROWSERS: &[&str] = &["firefox", "firefox-esr", "librewolf"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserKind {
    Chromium,
    Firefox,
}

/// A browser program able to open a dedicated app window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Browser {
    pub kind: BrowserKind,
    pub program: PathBuf,
}

/// First supported browser that `lookup` (a `PATH` search) finds.
pub(crate) fn find_browser(lookup: &dyn Fn(&str) -> Option<PathBuf>) -> Option<Browser> {
    let chromium = CHROMIUM_BROWSERS.iter().find_map(|name| {
        lookup(name).map(|program| Browser {
            kind: BrowserKind::Chromium,
            program,
        })
    });
    chromium.or_else(|| {
        FIREFOX_BROWSERS.iter().find_map(|name| {
            lookup(name).map(|program| Browser {
                kind: BrowserKind::Firefox,
                program,
            })
        })
    })
}

/// `PATH` lookup used outside tests.
pub(crate) fn which_lookup(name: &str) -> Option<PathBuf> {
    if let Ok(path) = which::which(name) {
        return Some(path);
    }
    let mut candidates = Vec::new();
    if cfg!(target_os = "macos") {
        let app = match name {
            "google-chrome" => Some(("Google Chrome.app", "Google Chrome")),
            "brave" => Some(("Brave Browser.app", "Brave Browser")),
            "microsoft-edge-stable" => Some(("Microsoft Edge.app", "Microsoft Edge")),
            "firefox" => Some(("Firefox.app", "firefox")),
            _ => None,
        };
        if let Some((bundle, exe)) = app {
            for base in [
                Some(PathBuf::from("/Applications")),
                dirs::home_dir().map(|h| h.join("Applications")),
            ]
            .into_iter()
            .flatten()
            {
                candidates.push(base.join(bundle).join("Contents/MacOS").join(exe));
            }
        }
    }
    if cfg!(windows) {
        let rel = match name {
            "google-chrome" => Some("Google/Chrome/Application/chrome.exe"),
            "brave" => Some("BraveSoftware/Brave-Browser/Application/brave.exe"),
            "microsoft-edge-stable" => Some("Microsoft/Edge/Application/msedge.exe"),
            "firefox" => Some("Mozilla Firefox/firefox.exe"),
            _ => None,
        };
        if let Some(rel) = rel {
            for base in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"]
                .into_iter()
                .filter_map(std::env::var_os)
            {
                candidates.push(PathBuf::from(base).join(rel));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// What launching a browser app needs: the command, and for Firefox the
/// profile's `user.js` to write first.
#[derive(Debug, Clone)]
pub(crate) struct BrowserLaunch {
    pub command: CommandSpec,
    pub user_js: Option<(PathBuf, String)>,
}

/// Build the command for a browser app. `profiles_dir` holds the per-host
/// browser profiles.
pub(crate) fn browser_launch(
    app: &SideloadApp,
    profiles_dir: &Path,
    lookup: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<BrowserLaunch> {
    let url = app
        .browser_url
        .as_deref()
        .context("This app has no web address")?;
    let xdg_open = || BrowserLaunch {
        command: if cfg!(target_os = "macos") {
            CommandSpec::new("/usr/bin/open").arg(url)
        } else if cfg!(windows) {
            CommandSpec::new("rundll32.exe")
                .arg("url.dll,FileProtocolHandler")
                .arg(url)
        } else {
            CommandSpec::new("xdg-open").arg(url)
        },
        user_js: None,
    };
    if app.custom_user_agent.is_none() && !app.launch_fullscreen {
        return Ok(xdg_open());
    }
    let Some(browser) = find_browser(lookup) else {
        tracing::warn!(
            "no Chromium or Firefox browser found; opening {url} in the default browser without the custom user agent or full screen"
        );
        return Ok(xdg_open());
    };
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(sanitize))
        .unwrap_or_else(|| "default".into());
    Ok(match browser.kind {
        BrowserKind::Chromium => {
            let profile = profiles_dir.join("chromium").join(host);
            let mut cmd = CommandSpec::new(browser.program)
                .arg(format!("--user-data-dir={}", profile.display()))
                .arg("--no-first-run")
                .arg("--no-default-browser-check")
                .arg(format!("--app={url}"));
            if let Some(ua) = &app.custom_user_agent {
                cmd = cmd.arg(format!("--user-agent={ua}"));
            }
            if app.launch_fullscreen {
                cmd = cmd.arg("--start-fullscreen");
            }
            BrowserLaunch {
                command: cmd,
                user_js: None,
            }
        }
        BrowserKind::Firefox => {
            let profile = profiles_dir.join("firefox").join(host);
            let mut cmd = CommandSpec::new(browser.program)
                .arg("--profile")
                .arg(profile.as_os_str())
                .arg("--no-remote");
            if app.launch_fullscreen {
                cmd = cmd.arg("--kiosk");
            }
            BrowserLaunch {
                command: cmd.arg(url),
                user_js: Some((
                    profile.join("user.js"),
                    firefox_user_js(app.custom_user_agent.as_deref()),
                )),
            }
        }
    })
}

/// Preferences of a dedicated Firefox profile.
fn firefox_user_js(user_agent: Option<&str>) -> String {
    let mut prefs = String::from(
        "user_pref(\"browser.shell.checkDefaultBrowser\", false);\n\
         user_pref(\"browser.aboutwelcome.enabled\", false);\n\
         user_pref(\"datareporting.policy.dataSubmissionPolicyBypassNotification\", true);\n",
    );
    if let Some(ua) = user_agent {
        let escaped = ua.replace('\\', "\\\\").replace('"', "\\\"");
        prefs.push_str(&format!(
            "user_pref(\"general.useragent.override\", \"{escaped}\");\n"
        ));
    }
    prefs
}

/// Executable to run: the per-game "alternative executable" setting when set
/// (relative paths resolve against the install folder), else the app's own.
pub(crate) fn resolve_executable(app: &SideloadApp, target_exe: &str) -> Option<PathBuf> {
    let target = target_exe.trim();
    if !target.is_empty() {
        let path = expand_home(target);
        if path.is_absolute() {
            return Some(path);
        }
        if let Some(dir) = app.install_dir() {
            return Some(dir.join(path));
        }
    }
    app.executable.clone()
}

/// Arguments for a launch: the app's own arguments, the per-game launcher
/// arguments setting, the chosen launch option, then extra arguments.
pub(crate) fn launch_args(app: &SideloadApp, request: &LaunchRequest) -> Vec<String> {
    let mut args = split_args(&app.launcher_args);
    args.extend(split_args(&request.settings.launcher_args));
    if let Some(LaunchOption::Basic { parameters, .. }) = &request.option {
        args.extend(split_args(parameters));
    }
    args.extend(request.extra_args.iter().cloned());
    args
}

fn working_dir(exe: &Path) -> Option<PathBuf> {
    exe.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

/// Native launch: wrappers + exe + args, environment from the runtime.
pub(crate) fn native_plan(exe: &Path, args: Vec<String>, env: &LaunchEnvironment) -> LaunchPlan {
    let mut cmd = CommandSpec::new(exe).args(args).envs(env.env.clone());
    if let Some(dir) = working_dir(exe) {
        cmd = cmd.cwd(dir);
    }
    LaunchPlan {
        command: cmd.wrapped(&env.wrappers),
        track_wineserver: false,
    }
}

/// Windows launch: wrappers + Wine exec prefix + exe + args.
pub(crate) fn wine_plan(
    exe: &Path,
    args: Vec<String>,
    env: &LaunchEnvironment,
) -> Result<LaunchPlan> {
    let wine = env
        .wine
        .as_ref()
        .context("Choose a Wine or Proton version in the app's settings to run Windows apps")?;
    let mut prefix = wine.exec_prefix().into_iter();
    let program = prefix.next().context("The Wine command is empty")?;
    let mut cmd = CommandSpec::new(program)
        .args(prefix)
        .arg(exe.as_os_str())
        .args(args)
        .envs(env.env.clone());
    if let Some(dir) = working_dir(exe) {
        cmd = cmd.cwd(dir);
    }
    Ok(LaunchPlan {
        command: cmd.wrapped(&env.wrappers),
        track_wineserver: true,
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use irori_core::{GameSettings, WineKind, WineLaunch};

    use super::*;
    use crate::app::SideloadPlatform;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    fn request(env: LaunchEnvironment) -> LaunchRequest {
        LaunchRequest {
            settings: GameSettings {
                launcher_args: "--setting \"two words\"".into(),
                ..Default::default()
            },
            environment: env,
            option: Some(LaunchOption::Basic {
                name: "Safe".into(),
                parameters: "-safe".into(),
            }),
            extra_args: vec!["--extra".into()],
            offline: false,
            skip_version_check: false,
            log_file: "/tmp/log".into(),
        }
    }

    fn app() -> SideloadApp {
        SideloadApp {
            title: "Game".into(),
            executable: Some("/games/g/run.sh".into()),
            platform: SideloadPlatform::Linux,
            launcher_args: r#"-a "b c""#.into(),
            ..Default::default()
        }
    }

    fn web(ua: Option<&str>, fullscreen: bool) -> SideloadApp {
        SideloadApp {
            title: "Web".into(),
            platform: SideloadPlatform::Browser,
            browser_url: Some("https://play.example.com/g".into()),
            custom_user_agent: ua.map(Into::into),
            launch_fullscreen: fullscreen,
            ..Default::default()
        }
    }

    #[test]
    fn composes_arguments() {
        let req = request(LaunchEnvironment::default());
        assert_eq!(
            launch_args(&app(), &req),
            vec!["-a", "b c", "--setting", "two words", "-safe", "--extra"]
        );
    }

    #[test]
    fn resolves_alternative_executable() {
        let a = app();
        assert_eq!(resolve_executable(&a, ""), Some("/games/g/run.sh".into()));
        assert_eq!(
            resolve_executable(&a, "bin/x"),
            Some("/games/g/bin/x".into())
        );
        assert_eq!(resolve_executable(&a, "/opt/y"), Some("/opt/y".into()));
    }

    #[test]
    fn native_plan_uses_wrappers_env_and_cwd() {
        let mut env = LaunchEnvironment::default();
        env.env.insert("FOO".into(), "1".into());
        env.wrappers = vec![vec!["gamemoderun".into()], vec!["mangohud".into()]];
        let plan = native_plan(Path::new("/games/g/run.sh"), vec!["-x".into()], &env);
        assert_eq!(plan.command.program, PathBuf::from("gamemoderun"));
        assert_eq!(
            plan.command.args,
            os(&["mangohud", "/games/g/run.sh", "-x"])
        );
        assert_eq!(plan.command.cwd.as_deref(), Some(Path::new("/games/g")));
        assert_eq!(plan.command.env.get("FOO").map(String::as_str), Some("1"));
        assert!(!plan.track_wineserver);
    }

    #[test]
    fn wine_plan_uses_exec_prefix() {
        let mut env = LaunchEnvironment::default();
        assert!(wine_plan(Path::new("/g/game.exe"), vec![], &env).is_err());
        env.wine = Some(WineLaunch {
            bin: "/proton/proton".into(),
            build_bin: "/proton/proton".into(),
            kind: WineKind::Proton,
            prefix: "/pfx".into(),
            uses_umu: false,
            proton_verb: "waitforexitandrun",
        });
        env.wrappers = vec![vec!["gamescope".into(), "--".into()]];
        let plan = wine_plan(Path::new("/g/game.exe"), vec!["-y".into()], &env).unwrap();
        assert_eq!(plan.command.program, PathBuf::from("gamescope"));
        assert_eq!(
            plan.command.args,
            os(&[
                "--",
                "/proton/proton",
                "waitforexitandrun",
                "/g/game.exe",
                "-y"
            ])
        );
        assert_eq!(plan.command.cwd.as_deref(), Some(Path::new("/g")));
        assert!(plan.track_wineserver);
    }

    #[test]
    fn plain_browser_apps_use_xdg_open() {
        let launch = browser_launch(&web(None, false), Path::new("/p"), &|_| {
            Some("/usr/bin/chromium".into())
        })
        .unwrap();
        assert_eq!(
            launch.command.program,
            PathBuf::from(if cfg!(target_os = "macos") {
                "/usr/bin/open"
            } else if cfg!(windows) {
                "rundll32.exe"
            } else {
                "xdg-open"
            })
        );
        assert_eq!(
            launch.command.args.last().unwrap(),
            "https://play.example.com/g"
        );
    }

    #[test]
    fn chromium_app_mode() {
        let lookup = |name: &str| (name == "brave").then(|| PathBuf::from("/usr/bin/brave"));
        assert_eq!(
            find_browser(&lookup).map(|b| b.kind),
            Some(BrowserKind::Chromium)
        );
        let launch = browser_launch(&web(Some("UA/1"), true), Path::new("/p"), &lookup).unwrap();
        assert_eq!(launch.command.program, PathBuf::from("/usr/bin/brave"));
        assert_eq!(
            launch.command.args,
            os(&[
                &format!(
                    "--user-data-dir={}",
                    Path::new("/p")
                        .join("chromium")
                        .join("play.example.com")
                        .display()
                ),
                "--no-first-run",
                "--no-default-browser-check",
                "--app=https://play.example.com/g",
                "--user-agent=UA/1",
                "--start-fullscreen",
            ])
        );
        assert!(launch.user_js.is_none());
    }

    #[test]
    fn firefox_fallback_writes_user_agent_pref() {
        let lookup = |name: &str| (name == "firefox").then(|| PathBuf::from("/usr/bin/firefox"));
        let launch =
            browser_launch(&web(Some("Agent \"x\""), false), Path::new("/p"), &lookup).unwrap();
        assert_eq!(
            launch.command.args,
            os(&[
                "--profile",
                &Path::new("/p")
                    .join("firefox")
                    .join("play.example.com")
                    .to_string_lossy(),
                "--no-remote",
                "https://play.example.com/g"
            ])
        );
        let (path, js) = launch.user_js.unwrap();
        assert_eq!(path, PathBuf::from("/p/firefox/play.example.com/user.js"));
        assert!(js.contains(r#"user_pref("general.useragent.override", "Agent \"x\"");"#));
    }

    #[test]
    fn falls_back_to_xdg_open_without_browser() {
        let launch = browser_launch(&web(None, true), Path::new("/p"), &|_| None).unwrap();
        assert_eq!(
            launch.command.program,
            PathBuf::from(if cfg!(target_os = "macos") {
                "/usr/bin/open"
            } else if cfg!(windows) {
                "rundll32.exe"
            } else {
                "xdg-open"
            })
        );
    }
}
