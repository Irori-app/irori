//! `irori://` and `heroic://` URLs, and the `--launch <runner>/<app>` command
//! line form, parsed into a typed [`ProtocolAction`].
//!
//! Accepted forms (both schemes):
//! - `launch/<app>` and `launch/<runner>/<app>` (path segments are
//!   percent-decoded)
//! - `launch?appName=<app>&runner=<runner>[&arg=a&arg=b][&altExe=/abs][&gui=false]`
//!   (also `launch/?…`)
//! - `install/<runner>/<app>` and `install?appName=…&runner=…`
//! - `ping[?…]`
//!
//! Runner names accept Heroic's ids plus `epic` / `amazon`. An unknown runner
//! is dropped (the caller then searches every store), like Heroic.

use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use irori_core::{GameId, Runner};
use url::Url;

use crate::util;

/// URL scheme a request arrived with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Irori,
    Heroic,
}

impl Scheme {
    pub fn prefix(self) -> &'static str {
        match self {
            Scheme::Irori => "irori://",
            Scheme::Heroic => "heroic://",
        }
    }
}

/// A game launch requested from outside the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchUrl {
    /// `None` when the URL named no (known) runner: search all stores.
    pub runner: Option<Runner>,
    pub app_name: String,
    /// Extra game arguments (`arg=` parameters, in order).
    pub args: Vec<String>,
    /// Alternative executable; only absolute paths are kept.
    pub alt_exe: Option<PathBuf>,
    /// `gui=false|0|no`: keep the main window hidden for this launch.
    pub no_gui: bool,
}

impl LaunchUrl {
    /// The game ID when the runner is known.
    pub fn game_id(&self) -> Option<GameId> {
        self.runner.map(|r| GameId::new(r, self.app_name.as_str()))
    }
}

/// What a protocol URL asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolAction {
    /// Launch (or offer to install, when not installed) a game.
    Launch(LaunchUrl),
    /// Open the install dialog of a game.
    Install {
        runner: Option<Runner>,
        app_name: String,
    },
    /// Liveness check; only logged. Carries the query parameters.
    Ping { params: Vec<(String, String)> },
    /// Anything else (ignored by the caller).
    Unknown { action: String },
}

/// A parsed protocol request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolRequest {
    pub scheme: Scheme,
    pub action: ProtocolAction,
}

impl ProtocolRequest {
    /// Whether the main window should stay hidden: launches with `gui=false`,
    /// or any launch when `hide_window_on_protocol_launch` is set.
    pub fn hides_window(&self, hide_window_on_protocol_launch: bool) -> bool {
        match &self.action {
            ProtocolAction::Launch(l) => l.no_gui || hide_window_on_protocol_launch,
            _ => false,
        }
    }
}

/// `irori://launch/<runner>/<app>` for a game (app name percent-encoded).
pub fn launch_url(id: &GameId) -> String {
    format!(
        "irori://launch/{}/{}",
        id.runner.id(),
        util::encode_component(&id.app_name)
    )
}

/// Parse an `irori://` or `heroic://` URL.
pub fn parse(input: &str) -> Result<ProtocolRequest> {
    let trimmed = input.trim();
    let scheme = if starts_with_ci(trimmed, "irori://") {
        Scheme::Irori
    } else if starts_with_ci(trimmed, "heroic://") {
        Scheme::Heroic
    } else {
        bail!("not an irori:// or heroic:// link: {trimmed}");
    };
    let url = Url::parse(trimmed).map_err(|e| anyhow!("invalid link {trimmed}: {e}"))?;
    let action_name = url.host_str().unwrap_or("").to_ascii_lowercase();
    let segments: Vec<String> = url
        .path()
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| util::decode_component(s).unwrap_or_else(|| s.to_string()))
        .collect();
    let query: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let param = |name: &str| {
        query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };

    let target = || -> (Option<Runner>, String) {
        if let Some((app, runner)) = segments.split_last() {
            return (runner.last().and_then(|r| Runner::from_id(r)), app.clone());
        }
        (
            param("runner").and_then(|r| Runner::from_id(&r)),
            param("appName").unwrap_or_default(),
        )
    };

    let action = match action_name.as_str() {
        "launch" => {
            let (runner, app_name) = target();
            if app_name.is_empty() {
                bail!("No appName in protocol URL");
            }
            let args = query
                .iter()
                .filter(|(k, _)| k == "arg")
                .map(|(_, v)| v.clone())
                .collect();
            let alt_exe = param("altExe")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute());
            let no_gui = param("gui").is_some_and(|g| matches!(g.as_str(), "false" | "0" | "no"));
            ProtocolAction::Launch(LaunchUrl {
                runner,
                app_name,
                args,
                alt_exe,
                no_gui,
            })
        }
        "install" => {
            let (runner, app_name) = target();
            if app_name.is_empty() {
                bail!("No appName in protocol URL");
            }
            ProtocolAction::Install { runner, app_name }
        }
        "ping" => ProtocolAction::Ping { params: query },
        other => ProtocolAction::Unknown {
            action: other.to_string(),
        },
    };
    Ok(ProtocolRequest { scheme, action })
}

/// Parse the value of `--launch`: `<runner>/<app>` or a bare `<app>`.
pub fn parse_launch_target(value: &str) -> Option<LaunchUrl> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if starts_with_ci(value, "irori://") || starts_with_ci(value, "heroic://") {
        return match parse(value).ok()?.action {
            ProtocolAction::Launch(l) => Some(l),
            _ => None,
        };
    }
    let (runner, app) = match value.split_once('/') {
        Some((r, app)) if Runner::from_id(r).is_some() => (Runner::from_id(r), app),
        _ => (None, value),
    };
    (!app.is_empty()).then(|| LaunchUrl {
        runner,
        app_name: app.to_string(),
        args: Vec::new(),
        alt_exe: None,
        no_gui: false,
    })
}

/// Find a request in a command line: the first `irori://`/`heroic://`
/// argument, or `--launch <target>` / `--launch=<target>`.
pub fn find_in_args<I, S>(args: I) -> Option<ProtocolRequest>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        let arg = arg.as_ref();
        if starts_with_ci(arg, "irori://") || starts_with_ci(arg, "heroic://") {
            match parse(arg) {
                Ok(req) => return Some(req),
                Err(e) => {
                    tracing::warn!("ignoring link argument: {e:#}");
                    continue;
                }
            }
        }
        let target = if arg == "--launch" {
            iter.next().map(|v| v.as_ref().to_string())
        } else {
            arg.strip_prefix("--launch=").map(str::to_string)
        };
        if let Some(target) = target {
            return parse_launch_target(&target).map(|l| ProtocolRequest {
                scheme: Scheme::Irori,
                action: ProtocolAction::Launch(l),
            });
        }
    }
    None
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(url: &str) -> LaunchUrl {
        match parse(url).unwrap().action {
            ProtocolAction::Launch(l) => l,
            other => panic!("not a launch: {other:?}"),
        }
    }

    #[test]
    fn heroic_query_form() {
        let l = launch("heroic://launch?appName=Fortnite&runner=legendary");
        assert_eq!(l.runner, Some(Runner::Legendary));
        assert_eq!(l.app_name, "Fortnite");
        assert!(!l.no_gui);
        let l = launch("heroic://launch/?appName=a%20b&runner=gog&arg=-x&arg=y+z&gui=0");
        assert_eq!(l.app_name, "a b");
        assert_eq!(l.runner, Some(Runner::Gog));
        assert_eq!(l.args, ["-x", "y z"]);
        assert!(l.no_gui);
    }

    #[test]
    fn path_forms() {
        let l = launch("irori://launch/gog/1423049311");
        assert_eq!(l.game_id(), Some(GameId::new(Runner::Gog, "1423049311")));
        let l = launch("heroic://launch/Fortnite");
        assert_eq!((l.runner, l.app_name.as_str()), (None, "Fortnite"));
        let l = launch("irori://launch/sideload/my%20app");
        assert_eq!(l.app_name, "my app");
        let l = launch("irori://Launch/zoom/42");
        assert_eq!(l.runner, Some(Runner::Zoom));
        let l = launch("irori://launch/steam/620");
        assert_eq!(l.game_id(), Some(GameId::new(Runner::Steam, "620")));
        let l = launch("heroic://launch/uplay/42");
        assert_eq!(l.runner, None);
    }

    #[test]
    fn alt_exe_and_errors() {
        let (url, path) = if cfg!(windows) {
            (
                "heroic://launch?appName=x&altExe=C%3A%5CGames%5Cgame.exe",
                r"C:\Games\game.exe",
            )
        } else {
            (
                "heroic://launch?appName=x&altExe=%2Fopt%2Fgame.exe",
                "/opt/game.exe",
            )
        };
        assert_eq!(launch(url).alt_exe, Some(PathBuf::from(path)));
        let l = launch("heroic://launch?appName=x&altExe=relative.exe");
        assert_eq!(l.alt_exe, None);
        assert!(parse("heroic://launch").is_err());
        assert!(parse("heroic://launch?runner=gog").is_err());
        assert!(parse("https://launch/x").is_err());
    }

    #[test]
    fn other_actions() {
        assert_eq!(
            parse("heroic://ping?a=1").unwrap().action,
            ProtocolAction::Ping {
                params: vec![("a".into(), "1".into())]
            }
        );
        assert_eq!(
            parse("irori://install/nile/amzn1.adg.product.x")
                .unwrap()
                .action,
            ProtocolAction::Install {
                runner: Some(Runner::Nile),
                app_name: "amzn1.adg.product.x".into()
            }
        );
        assert_eq!(
            parse("irori://foo").unwrap().action,
            ProtocolAction::Unknown {
                action: "foo".into()
            }
        );
        let r = parse("heroic://ping").unwrap();
        assert_eq!(r.scheme, Scheme::Heroic);
        assert!(!r.hides_window(true));
    }

    #[test]
    fn launch_url_roundtrip() {
        let id = GameId::new(Runner::Sideload, "a b/c");
        let url = launch_url(&id);
        assert_eq!(url, "irori://launch/sideload/a%20b%2Fc");
        assert_eq!(launch(&url).game_id(), Some(id));
    }

    #[test]
    fn command_line() {
        let r = find_in_args(["irori", "--launch", "gog/123"]).unwrap();
        assert_eq!(
            r.action,
            ProtocolAction::Launch(LaunchUrl {
                runner: Some(Runner::Gog),
                app_name: "123".into(),
                args: vec![],
                alt_exe: None,
                no_gui: false,
            })
        );
        let r = find_in_args(["irori", "--launch=Fortnite"]).unwrap();
        assert!(matches!(r.action, ProtocolAction::Launch(ref l) if l.runner.is_none()));
        let r = find_in_args(["irori", "--no-gui", "heroic://launch?appName=x&gui=no"]).unwrap();
        assert_eq!(r.scheme, Scheme::Heroic);
        assert!(r.hides_window(false));
        assert!(find_in_args(["irori", "--foo"]).is_none());
    }
}
