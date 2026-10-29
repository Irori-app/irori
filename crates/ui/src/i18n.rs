//! Interface translations.
//!
//! Each crate with user-facing text owns its catalogs: flat JSON objects
//! (`"library.search.placeholder": "Search library"`) in
//! `crates/<crate>/locales/<code>.json`, embedded with `include_str!` and
//! handed to [`register`] from the crate's `init`. Keys are namespaced by
//! crate (`library.`, `game.`, `settings.`, `manage.`, `bigpicture.`, `shell.`,
//! `ui.`), so catalogs never collide.
//!
//! English is the source and the fallback for keys a locale lacks.
//! Placeholders use `{name}`.
//!
//! ```ignore
//! use irori_ui::i18n::{t, t_args};
//! Button::new("play").label(t("game.play"));
//! t_args("downloads.installed", &[("title", &game.title)]);
//! ```
//!
//! Keys describe intent (`game.uninstall.title`), never the English text;
//! sentences are never assembled from fragments.

use std::collections::HashMap;

use gpui_kit::SharedString;
use once_cell::sync::Lazy;
use parking_lot::RwLock;

/// A selectable interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    /// Code stored in settings (`ui_language`), e.g. `pt_BR`.
    pub code: &'static str,
    /// Name in the language itself.
    pub native_name: &'static str,
}

/// Languages Irori ships translations for, English first.
pub const LANGUAGES: &[Language] = &[
    Language {
        code: "en",
        native_name: "English",
    },
    Language {
        code: "cs",
        native_name: "Čeština",
    },
    Language {
        code: "de",
        native_name: "Deutsch",
    },
    Language {
        code: "es",
        native_name: "Español",
    },
    Language {
        code: "fr",
        native_name: "Français",
    },
    Language {
        code: "it",
        native_name: "Italiano",
    },
    Language {
        code: "nl",
        native_name: "Nederlands",
    },
    Language {
        code: "pl",
        native_name: "Polski",
    },
    Language {
        code: "pt",
        native_name: "Português",
    },
    Language {
        code: "pt_BR",
        native_name: "Português (Brasil)",
    },
    Language {
        code: "ro",
        native_name: "Română",
    },
    Language {
        code: "sv",
        native_name: "Svenska",
    },
    Language {
        code: "tr",
        native_name: "Türkçe",
    },
    Language {
        code: "ru",
        native_name: "Русский",
    },
    Language {
        code: "uk",
        native_name: "Українська",
    },
    Language {
        code: "ja",
        native_name: "日本語",
    },
    Language {
        code: "ko",
        native_name: "한국어",
    },
    Language {
        code: "zh_Hans",
        native_name: "简体中文",
    },
    Language {
        code: "zh_Hant",
        native_name: "繁體中文",
    },
];

/// Catalogs of this crate (`ui.` keys: statuses, progress, shared components).
pub static CATALOGS: Catalogs = &[
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

/// Translated label of a game status.
pub fn status_label(kind: irori_core::StatusKind) -> SharedString {
    use irori_core::StatusKind as K;
    t(match kind {
        K::Installing => "ui.status.installing",
        K::Importing => "ui.status.importing",
        K::Updating => "ui.status.updating",
        K::Launching => "ui.status.launching",
        K::Playing => "ui.status.playing",
        K::Uninstalling => "ui.status.uninstalling",
        K::Repairing => "ui.status.repairing",
        K::Moving => "ui.status.moving",
        K::Queued => "ui.status.queued",
        K::SyncingSaves => "ui.status.syncing_saves",
        K::Redist => "ui.status.redist",
        K::Extracting => "ui.status.extracting",
        K::Winetricks => "ui.status.winetricks",
        K::Done => "ui.status.done",
        K::Canceled => "ui.status.canceled",
        K::Error => "ui.status.error",
    })
}

/// `(language code, JSON catalog)` pairs of one crate.
pub type Catalogs = &'static [(&'static str, &'static str)];

type Catalog = HashMap<String, String>;

struct State {
    code: String,
    sources: Vec<Catalogs>,
    current: Catalog,
    english: Catalog,
}

impl State {
    fn new() -> Self {
        Self {
            code: "en".into(),
            sources: Vec::new(),
            current: Catalog::new(),
            english: Catalog::new(),
        }
    }

    fn register(&mut self, catalogs: Catalogs) {
        self.sources.push(catalogs);
        self.english = load(&self.sources, "en");
        self.current = if self.code == "en" {
            Catalog::new()
        } else {
            load(&self.sources, &self.code)
        };
    }

    fn set_language(&mut self, code: &str) {
        let base = code.split(['_', '-']).next().unwrap_or(code);
        let resolved = LANGUAGES
            .iter()
            .find(|l| l.code == code)
            .or_else(|| LANGUAGES.iter().find(|l| l.code == base))
            .map(|l| l.code)
            .unwrap_or("en");
        self.code = resolved.to_string();
        self.current = if resolved == "en" {
            Catalog::new()
        } else {
            load(&self.sources, resolved)
        };
    }

    fn t(&self, key: &str) -> SharedString {
        self.current
            .get(key)
            .or_else(|| self.english.get(key))
            .map(|s| SharedString::from(s.clone()))
            .unwrap_or_else(|| SharedString::from(key.to_string()))
    }
}

static STATE: Lazy<RwLock<State>> = Lazy::new(|| RwLock::new(State::new()));

fn parse(json: &str) -> Catalog {
    match serde_json::from_str::<Catalog>(json) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("invalid translation catalog: {e}");
            Catalog::new()
        }
    }
}

fn load(sources: &[Catalogs], code: &str) -> Catalog {
    let mut out = Catalog::new();
    for catalogs in sources {
        for (c, json) in catalogs.iter() {
            if *c == code {
                out.extend(parse(json));
            }
        }
    }
    out
}

fn fill(text: &str, args: &[(&str, &str)]) -> SharedString {
    let mut text = text.to_string();
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    SharedString::from(text)
}

/// Add a crate's catalogs. Call once from the crate's `init`.
pub fn register(catalogs: Catalogs) {
    STATE.write().register(catalogs);
}

/// Switch the interface language. Unknown codes fall back to English (a
/// region-specific code like `pt_PT` falls back to `pt` first).
pub fn set_language(code: &str) {
    STATE.write().set_language(code);
}

/// Code of the active language.
pub fn language() -> String {
    STATE.read().code.clone()
}

/// Translate `key`. Missing keys fall back to English, then to the key itself
/// (so a missing string is visible rather than blank).
pub fn t(key: &str) -> SharedString {
    STATE.read().t(key)
}

/// Translate `key` and fill `{name}` placeholders.
pub fn t_args(key: &str, args: &[(&str, &str)]) -> SharedString {
    fill(&t(key), args)
}

/// Keys present in English but missing in `code` (for catalog tests).
pub fn missing_keys(catalogs: Catalogs, code: &str) -> Vec<String> {
    let en = load(&[catalogs], "en");
    let other = load(&[catalogs], code);
    let mut missing: Vec<String> = en
        .keys()
        .filter(|k| !other.contains_key(*k))
        .cloned()
        .collect();
    missing.sort();
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST: Catalogs = &[
        (
            "en",
            r#"{"i18n.test.hello": "Hello {name}", "i18n.test.only_en": "Only"}"#,
        ),
        ("de", r#"{"i18n.test.hello": "Hallo {name}"}"#),
    ];

    #[test]
    fn own_catalogs_are_complete() {
        for lang in LANGUAGES {
            assert!(
                missing_keys(CATALOGS, lang.code).is_empty(),
                "{}",
                lang.code
            );
        }
    }

    // Uses its own state: switching the global language would race with
    // every test that compares translated text.
    #[test]
    fn registers_falls_back_and_fills_placeholders() {
        let mut state = State::new();
        state.register(TEST);
        state.set_language("de_AT");
        assert_eq!(state.code, "de");
        assert_eq!(
            fill(&state.t("i18n.test.hello"), &[("name", "Ana")]),
            "Hallo Ana"
        );
        assert_eq!(state.t("i18n.test.only_en"), "Only");
        assert_eq!(state.t("i18n.test.missing"), "i18n.test.missing");
        assert_eq!(
            missing_keys(TEST, "de"),
            vec!["i18n.test.only_en".to_string()]
        );
        state.set_language("xx");
        assert_eq!(state.code, "en");
    }
}
