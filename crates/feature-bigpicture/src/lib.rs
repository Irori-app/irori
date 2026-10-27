//! Big Picture: controller support for the whole application and a
//! fullscreen, 10-foot library for playing from the couch.
//!
//! The shell calls [`init`] once after `irori_ui::init`, renders
//! [`BigPictureView`] for [`irori_ui::Route::BigPicture`] (hiding its own
//! chrome), and may show [`gamepad::connected_controllers`] on the settings
//! page.

pub mod gamepad;
mod grid;
mod model;
mod text;
mod view;

use gpui_kit::{App, Global, KeyBinding};
use irori_core::GameId;
use irori_ui::Route;
use irori_ui::nav::{self, navigator};

pub use grid::{FocusGrid, GridMove};
pub use view::BigPictureView;

/// Key context of the Big Picture root element.
pub const CONTEXT: &str = "BigPicture";

/// Big Picture's interface translations (`bigpicture.*` keys), one catalog
/// per language in [`irori_ui::i18n::LANGUAGES`].
pub static CATALOGS: irori_ui::i18n::Catalogs = &[
    ("en", include_str!("../locales/en.json")),
    ("cs", include_str!("../locales/cs.json")),
    ("de", include_str!("../locales/de.json")),
    ("es", include_str!("../locales/es.json")),
    ("fr", include_str!("../locales/fr.json")),
    ("it", include_str!("../locales/it.json")),
    ("nl", include_str!("../locales/nl.json")),
    ("pl", include_str!("../locales/pl.json")),
    ("pt", include_str!("../locales/pt.json")),
    ("pt_BR", include_str!("../locales/pt_BR.json")),
    ("ro", include_str!("../locales/ro.json")),
    ("sv", include_str!("../locales/sv.json")),
    ("tr", include_str!("../locales/tr.json")),
    ("ru", include_str!("../locales/ru.json")),
    ("uk", include_str!("../locales/uk.json")),
    ("ja", include_str!("../locales/ja.json")),
    ("ko", include_str!("../locales/ko.json")),
    ("zh_Hans", include_str!("../locales/zh_Hans.json")),
    ("zh_Hant", include_str!("../locales/zh_Hant.json")),
];

/// Register [`CATALOGS`] with `irori_ui::i18n`. Runs once; later calls do
/// nothing. Public helpers that return text (such as
/// [`gamepad::Battery::label`]) call it too, so they work before [`init`].
pub(crate) fn register_catalogs() {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| irori_ui::i18n::register(CATALOGS));
}

/// Register translations, bind Big Picture's keys and start controller
/// support.
pub fn init(cx: &mut App) {
    register_catalogs();
    bind_keys(cx);
    gamepad::install(cx);
}

/// Keyboard equivalents of the controller inputs inside Big Picture: arrows
/// move, Enter opens, Space or Ctrl+Enter plays, Escape or Backspace goes back,
/// M or Shift+F10 opens the menu, Page Up/Down page through a shelf.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", gamepad::Up, ctx),
        KeyBinding::new("down", gamepad::Down, ctx),
        KeyBinding::new("left", gamepad::Left, ctx),
        KeyBinding::new("right", gamepad::Right, ctx),
        KeyBinding::new("enter", gamepad::Confirm, ctx),
        KeyBinding::new("space", gamepad::Alt, ctx),
        KeyBinding::new("ctrl-enter", gamepad::Alt, ctx),
        KeyBinding::new("escape", gamepad::Back, ctx),
        KeyBinding::new("backspace", gamepad::Back, ctx),
        KeyBinding::new("m", gamepad::Menu, ctx),
        KeyBinding::new("shift-f10", gamepad::Menu, ctx),
        KeyBinding::new("pageup", gamepad::PrevPage, ctx),
        KeyBinding::new("pagedown", gamepad::NextPage, ctx),
    ]);
}

/// Enter Big Picture, or leave it when it is showing.
pub fn toggle_big_picture(cx: &mut App) {
    if navigator(cx).read(cx).current() == &Route::BigPicture {
        exit_big_picture(cx);
    } else {
        nav::navigate(Route::BigPicture, cx);
    }
}

/// A game Big Picture should show next (see [`show_game`]).
#[derive(Default)]
pub(crate) struct ShowGame(pub Option<GameId>);

impl Global for ShowGame {}

/// Focus `id` in Big Picture and open its commands (Install for a game that
/// isn't installed yet). Big Picture picks it up when it is open, or when it
/// opens next.
pub fn show_game(id: GameId, cx: &mut App) {
    cx.set_global(ShowGame(Some(id)));
}

/// The game requested through [`show_game`], once.
pub(crate) fn take_shown_game(cx: &mut App) -> Option<GameId> {
    let id = cx.try_global::<ShowGame>()?.0.clone()?;
    cx.set_global(ShowGame(None));
    Some(id)
}

/// Leave Big Picture: back to where the user came from, or the library when
/// Irori started in Big Picture. The view restores the window's previous
/// fullscreen state when it is released.
pub fn exit_big_picture(cx: &mut App) {
    let nav = navigator(cx);
    if nav.read(cx).can_go_back() {
        nav::go_back(cx);
    } else {
        nav::navigate(Route::Library, cx);
    }
}

#[cfg(test)]
mod catalog_tests {
    use std::collections::{BTreeSet, HashMap};
    use std::path::Path;

    use irori_ui::i18n::{LANGUAGES, missing_keys};
    use regex::Regex;

    use super::CATALOGS;

    fn parse(code: &str, json: &str) -> HashMap<String, String> {
        serde_json::from_str(json).unwrap_or_else(|e| panic!("{code}.json does not parse: {e}"))
    }

    fn placeholders(text: &str) -> BTreeSet<String> {
        let re = Regex::new(r"\{([a-z_]+)\}").unwrap();
        re.captures_iter(text).map(|c| c[1].to_string()).collect()
    }

    fn english() -> HashMap<String, String> {
        let (_, json) = CATALOGS.iter().find(|(c, _)| *c == "en").unwrap();
        parse("en", json)
    }

    #[test]
    fn every_language_has_a_catalog_that_parses() {
        let codes: Vec<&str> = CATALOGS.iter().map(|(c, _)| *c).collect();
        let expected: Vec<&str> = LANGUAGES.iter().map(|l| l.code).collect();
        assert_eq!(codes, expected);
        for (code, json) in CATALOGS {
            let catalog = parse(code, json);
            assert!(!catalog.is_empty(), "{code}.json is empty");
            for (key, value) in &catalog {
                assert!(
                    key.starts_with("bigpicture."),
                    "{code}: {key} is outside the namespace"
                );
                assert!(!value.trim().is_empty(), "{code}: {key} is empty");
            }
        }
    }

    #[test]
    fn catalogs_match_english_keys_and_placeholders() {
        let en = english();
        for (code, json) in CATALOGS {
            assert_eq!(
                missing_keys(CATALOGS, code),
                Vec::<String>::new(),
                "{code} misses keys"
            );
            let catalog = parse(code, json);
            let mut extra: Vec<&String> = catalog.keys().filter(|k| !en.contains_key(*k)).collect();
            extra.sort();
            assert!(extra.is_empty(), "{code} has keys English lacks: {extra:?}");
            for (key, text) in &en {
                assert_eq!(
                    placeholders(&catalog[key]),
                    placeholders(text),
                    "{code}: placeholders of {key} differ from English"
                );
            }
        }
    }

    fn rust_sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn every_key_used_in_code_exists_and_every_key_is_used() {
        let en = english();
        let re = Regex::new(r#"\b(?:t|t_args)\(\s*"([^"]+)""#).unwrap();
        let literal = Regex::new(r#""(bigpicture\.[a-z0-9_.]+)""#).unwrap();
        let mut files = Vec::new();
        rust_sources(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut used = BTreeSet::new();
        let mut mentioned = BTreeSet::new();
        for file in files {
            let source = std::fs::read_to_string(&file).unwrap();
            for c in re.captures_iter(&source) {
                used.insert(c[1].to_string());
            }
            for c in literal.captures_iter(&source) {
                mentioned.insert(c[1].to_string());
            }
        }
        let unknown: Vec<&String> = used.iter().filter(|k| !en.contains_key(*k)).collect();
        assert!(unknown.is_empty(), "keys missing from en.json: {unknown:?}");
        let unknown: Vec<&String> = mentioned.iter().filter(|k| !en.contains_key(*k)).collect();
        assert!(
            unknown.is_empty(),
            "key literals missing from en.json: {unknown:?}"
        );
        let mut unused: Vec<&String> = en.keys().filter(|k| !mentioned.contains(*k)).collect();
        unused.sort();
        assert!(
            unused.is_empty(),
            "keys never referenced in code: {unused:?}"
        );
    }
}
