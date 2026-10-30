//! Command line and launch-URL handling.

use irori_core::{GameId, Runner};
use irori_ui::Route;

/// What the user asked for on the command line (or a forwarded second
/// instance invocation).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cli {
    pub launch: Option<GameId>,
    /// The launch came from a link, which anything on the computer or the
    /// web can open, not from Irori's own shortcuts.
    pub launch_from_link: bool,
    /// Open a game's page (`irori://game/...`).
    pub open: Option<GameId>,
    /// Open a screen (`irori://library`, `irori://settings/<section>`…).
    pub route: Option<Route>,
    /// Open the Lutris script installer at a lutris.net game
    /// (`irori://lutris/<slug>`, `lutris:<slug>`).
    pub lutris: Option<String>,
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
                s if s.starts_with("lutris:") => cli.lutris = lutris_slug(&s["lutris:".len()..]),
                _ => {}
            }
        }
        cli
    }

    /// Apply `irori://launch/<runner>/<app>`, `irori://game/<runner>/<app>`,
    /// and Heroic's `heroic://launch/<runner>/<app>` or
    /// `heroic://launch?appName=..&runner=..` forms.
    fn apply_url(&mut self, raw: &str) {
        let Ok(url) = url::Url::parse(raw) else {
            return;
        };
        if !matches!(url.scheme(), "irori" | "heroic") {
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
            "launch" => {
                self.launch = game;
                self.launch_from_link = true;
            }
            "game" | "open" => self.open = game,
            "bigpicture" | "console" => self.big_picture = true,
            "library" => self.route = Some(Route::Library),
            "downloads" => self.route = Some(Route::Downloads),
            "updates" => self.route = Some(Route::Updates),
            "accounts" => self.route = Some(Route::Accounts),
            "stores" => self.route = Some(Route::Stores),
            "stats" => self.route = Some(Route::Stats),
            "wine" => self.route = Some(Route::WineManager),
            "emulators" => self.route = Some(Route::Emulators),
            "lutris" => {
                self.lutris = url
                    .path_segments()
                    .and_then(|mut s| s.find(|p| !p.is_empty()).map(decode))
                    .filter(|s| is_slug(s))
            }
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
        self.launch.is_some()
            || self.open.is_some()
            || self.route.is_some()
            || self.lutris.is_some()
            || self.big_picture
    }
}

/// The game slug of a Lutris link body: `<slug>`, `install/<slug>` or
/// `<slug>?revision=…` (Lutris's own `lutris:` forms that name a game).
fn lutris_slug(body: &str) -> Option<String> {
    let body = body.trim_start_matches('/');
    let path = body.split(['?', '#']).next().unwrap_or_default();
    let slug = match path.split('/').collect::<Vec<_>>().as_slice() {
        [slug] => *slug,
        ["install", slug, ..] => *slug,
        _ => return None,
    };
    is_slug(slug).then(|| slug.to_string())
}

/// A lutris.net game slug: letters, digits, `-`, `_` and `.`.
fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
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

pub const HELP: &str = "Irori — a fast game launcher for Epic, GOG, Amazon and ZOOM games

Usage: irori [options] [url]

Options:
  -l, --launch <runner>/<app>   Launch a game (runners: legendary, gog, nile, zoom, sideload)
      --big-picture             Start in Big Picture mode
      --minimized               Start in the tray
  -V, --version                 Print the version
  -h, --help                    Print this help

URLs: irori://launch/<runner>/<app>, irori://game/<runner>/<app>, heroic://launch/<runner>/<app>,
      irori://library, irori://downloads, irori://accounts, irori://stores, irori://wine,
      irori://settings/<section>, irori://lutris/<slug>, lutris:<slug>";

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
            parse(&["irori://launch/legendary/Fortnite"]).launch,
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
            parse(&["irori://game/sideload/x1"]).open,
            Some(GameId::new(Runner::Sideload, "x1"))
        );
        assert!(parse(&["--big-picture"]).big_picture);
        assert_eq!(parse(&["irori://library"]).route, Some(Route::Library));
        assert_eq!(
            parse(&["irori://settings/wine"]).route,
            Some(Route::Settings("wine".into()))
        );
        assert_eq!(
            parse(&["irori://settings"]).route,
            Some(Route::Settings("general".into()))
        );
        assert!(!parse(&["https://example.com"]).has_request());
    }

    #[test]
    fn lutris_links_open_the_script_installer() {
        let slug = |args: &[&str]| parse(args).lutris;
        assert_eq!(slug(&["irori://lutris/quake"]), Some("quake".into()));
        assert_eq!(slug(&["lutris:quake"]), Some("quake".into()));
        assert_eq!(
            slug(&["lutris:install/the-witcher-3"]),
            Some("the-witcher-3".into())
        );
        assert_eq!(slug(&["lutris:quake?revision=12"]), Some("quake".into()));
        assert!(parse(&["lutris:quake"]).has_request());
        // Not a game: Lutris's own launch links and anything path-like.
        assert_eq!(slug(&["lutris:rungameid/12"]), None);
        assert_eq!(slug(&["irori://lutris/..%2Fetc"]), None);
        assert_eq!(slug(&["lutris:"]), None);
        assert_eq!(slug(&["irori://lutris"]), None);
    }
}
