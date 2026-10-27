//! Translation catalogs of the settings feature (`settings.` keys) and the
//! helpers its views use to follow language changes.

use std::collections::HashMap;
use std::sync::Once;

use irori_ui::i18n::{Catalogs, language};
use once_cell::sync::Lazy;

/// `(language code, JSON catalog)` of every language Irori ships.
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

/// Hand the catalogs to `irori_ui::i18n` (once per process).
pub(crate) fn register() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| irori_ui::i18n::register(CATALOGS));
}

static ENGLISH: Lazy<HashMap<String, String>> = Lazy::new(|| {
    serde_json::from_str(include_str!("../locales/en.json")).unwrap_or_else(|e| {
        tracing::error!("invalid English settings catalog: {e}");
        HashMap::new()
    })
});

/// English text of `key` whatever the interface language (the settings
/// search also matches it); empty for an unknown key.
pub(crate) fn english(key: &str) -> &'static str {
    ENGLISH.get(key).map(String::as_str).unwrap_or("")
}

/// The language retained text (input placeholders, select items) was last
/// built in. Views check it while rendering and rebuild that text after a
/// language change.
pub(crate) struct LanguageStamp(String);

impl LanguageStamp {
    pub(crate) fn new() -> Self {
        Self(language())
    }

    /// Whether the interface language changed since the last call.
    pub(crate) fn changed(&mut self) -> bool {
        let current = language();
        if current == self.0 {
            return false;
        }
        self.0 = current;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use irori_ui::i18n::{LANGUAGES, missing_keys};
    use regex::Regex;

    use super::*;

    fn catalog(code: &str) -> HashMap<String, String> {
        let json = CATALOGS
            .iter()
            .find(|(c, _)| *c == code)
            .unwrap_or_else(|| panic!("no catalog for {code}"))
            .1;
        serde_json::from_str(json).unwrap_or_else(|e| panic!("{code}.json: {e}"))
    }

    fn placeholders(text: &str) -> BTreeSet<String> {
        static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\{([a-z_]+)\}").unwrap());
        RE.captures_iter(text).map(|c| c[1].to_string()).collect()
    }

    #[test]
    fn every_language_has_a_catalog_and_all_parse() {
        for lang in LANGUAGES {
            let c = catalog(lang.code);
            assert!(!c.is_empty(), "{} is empty", lang.code);
        }
        assert_eq!(CATALOGS.len(), LANGUAGES.len());
    }

    #[test]
    fn catalogs_match_english_keys_and_placeholders() {
        let en = catalog("en");
        for key in en.keys() {
            assert!(key.starts_with("settings."), "{key} is outside settings.");
        }
        for lang in LANGUAGES.iter().filter(|l| l.code != "en") {
            let missing = missing_keys(CATALOGS, lang.code);
            assert!(missing.is_empty(), "{} lacks {missing:?}", lang.code);
            let other = catalog(lang.code);
            let extra: Vec<_> = other.keys().filter(|k| !en.contains_key(*k)).collect();
            assert!(extra.is_empty(), "{} has extra keys {extra:?}", lang.code);
            for (key, text) in &other {
                assert_eq!(
                    placeholders(text),
                    placeholders(&en[key]),
                    "{}: placeholders of {key}",
                    lang.code
                );
                assert!(!text.trim().is_empty(), "{}: {key} is empty", lang.code);
            }
        }
    }

    #[test]
    fn every_key_used_in_sources_exists() {
        let en = catalog("en");
        let call = Regex::new(r#"\bt(?:_args)?\(\s*"([^"]+)""#).unwrap();
        let literal = Regex::new(r#""(settings\.[A-Za-z0-9_.]+)""#).unwrap();
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut stack = vec![std::path::PathBuf::from(dir)];
        let mut checked = 0;
        while let Some(path) = stack.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).unwrap() {
                    stack.push(entry.unwrap().path());
                }
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for c in call
                .captures_iter(&text)
                .chain(literal.captures_iter(&text))
            {
                let key = &c[1];
                assert!(
                    en.contains_key(key),
                    "{} uses unknown key {key}",
                    path.display()
                );
                checked += 1;
            }
        }
        assert!(checked > 100, "only {checked} keys found");
    }

    #[test]
    fn registry_keys_exist() {
        let en = catalog("en");
        for field in crate::registry::FIELDS {
            for key in field.keys() {
                assert!(en.contains_key(key), "{}: unknown key {key}", field.path);
            }
        }
    }
}
