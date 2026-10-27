//! Command line and launch-URL handling.

use ember_core::{GameId, Runner};
use ember_ui::Route;

/// What the user asked for on the command line (or a forwarded second
/// instance invocation).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cli {
    pub launch: Option<GameId>,
    /// Open a game's page (`ember://game/...`).
    pub open: Option<GameId>,
    /// Open a screen (`ember://library`, `ember://settings/<section>`…).
    pub route: Option<Route>,
    pub big_picture: bool,
    /// Start hidden in the tray.
    pub minimized: bool,
    pub version: bool,
    pub help: bool,
}

impl Cli {
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Self {
        let mut cli = Cli::default();
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--launch" | "-l" => cli.launch = it.next().as_deref().and_then(parse_game_ref),
                "--big-picture" | "--console" => cli.big_picture = true,
                "--minimized" | "--tray" => cli.minimized = true,
                "--version" | "-V" => cli.version = true,
                "--help" | "-h" => cli.help = true,
                s if s.contains("://") => cli.apply_url(s),
                _ => {}
            }
        }
        cli
    }

    /// Apply `ember://launch/<runner>/<app>`, `ember://game/<runner>/<app>`,
    /// and Heroic's `heroic://launch/<runner>/<app>` or
    /// `heroic://launch?appName=..&runner=..` forms.
    fn apply_url(&mut self, raw: &str) {
        let Ok(url) = url::Url::parse(raw) else {
            return;
        };
        if !matches!(url.scheme(), "ember" | "heroic") {
            return;
        }
        let verb = url.host_str().unwrap_or_default();
        let mut path: Vec<String> = url
            .path_segments()
            .map(|s| s.filter(|p| !p.is_empty()).map(decode).collect())
            .unwrap_or_default();
        let query = |key: &str| {
            url.query_pairs()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.into_owned())
        };
        let game = match (path.len(), query("appName")) {
            (_, Some(app)) => {
                let runner = query("runner")
                    .as_deref()
                    .and_then(Runner::from_id)
                    .unwrap_or(Runner::Legendary);
                Some(GameId::new(runner, app))
            }
            (2.., None) => {
                let app = path.remove(1);
                Runner::from_id(&path[0]).map(|r| GameId::new(r, app))
            }
            // Old Heroic links carry only the app name.
            (1, None) => Some(GameId::new(Runner::Legendary, path.remove(0))),
            _ => None,
        };
        match verb {
            "launch" => self.launch = game,
            "game" | "open" => self.open = game,
            "bigpicture" | "console" => self.big_picture = true,
            "library" => self.route = Some(Route::Library),
            "downloads" => self.route = Some(Route::Downloads),
            "accounts" => self.route = Some(Route::Accounts),
            "stores" => self.route = Some(Route::Stores),
            "stats" => self.route = Some(Route::Stats),
            "wine" => self.route = Some(Route::WineManager),
            "emulators" => self.route = Some(Route::Emulators),
            "settings" => {
                let section = url
                    .path_segments()
                    .and_then(|mut s| s.find(|p| !p.is_empty()).map(decode))
                    .unwrap_or_else(|| "general".to_string());
                self.route = Some(Route::Settings(section.into()));
            }
            _ => {}
        }
    }

    /// Whether this invocation asks the running instance to do something
    /// beyond showing its window.
    pub fn has_request(&self) -> bool {
        self.launch.is_some() || self.open.is_some() || self.route.is_some() || self.big_picture
    }
}

fn parse_game_ref(s: &str) -> Option<GameId> {
    let (runner, app) = s.split_once('/')?;
    Some(GameId::new(Runner::from_id(runner)?, app))
}

fn decode(s: &str) -> String {
    url::form_urlencoded::parse(s.as_bytes())
        .map(|(k, v)| {
            if v.is_empty() {
                k.into_owned()
            } else {
                format!("{k}={v}")
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

pub const HELP: &str = "Ember — a fast game launcher for Epic, GOG, Amazon and ZOOM games

Usage: ember [options] [url]

Options:
  -l, --launch <runner>/<app>   Launch a game (runners: legendary, gog, nile, zoom, sideload)
      --big-picture             Start in Big Picture mode
      --minimized               Start in the tray
  -V, --version                 Print the version
  -h, --help                    Print this help

URLs: ember://launch/<runner>/<app>, ember://game/<runner>/<app>, heroic://launch/<runner>/<app>,
      ember://library, ember://downloads, ember://accounts, ember://stores, ember://wine,
      ember://settings/<section>";

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_flags_and_urls() {
        assert_eq!(
            parse(&["--launch", "gog/1207658924"]).launch,
            Some(GameId::new(Runner::Gog, "1207658924"))
        );
        assert_eq!(
            parse(&["ember://launch/legendary/Fortnite"]).launch,
            Some(GameId::new(Runner::Legendary, "Fortnite"))
        );
        assert_eq!(
            parse(&["heroic://launch?appName=abc&runner=nile"]).launch,
            Some(GameId::new(Runner::Nile, "abc"))
        );
        assert_eq!(
            parse(&["heroic://launch/Sugar"]).launch,
            Some(GameId::new(Runner::Legendary, "Sugar"))
        );
        assert_eq!(
            parse(&["ember://game/sideload/x1"]).open,
            Some(GameId::new(Runner::Sideload, "x1"))
        );
        assert!(parse(&["--big-picture"]).big_picture);
        assert_eq!(parse(&["ember://library"]).route, Some(Route::Library));
        assert_eq!(
            parse(&["ember://settings/wine"]).route,
            Some(Route::Settings("wine".into()))
        );
        assert_eq!(
            parse(&["ember://settings"]).route,
            Some(Route::Settings("general".into()))
        );
        assert!(!parse(&["https://example.com"]).has_request());
    }
}
