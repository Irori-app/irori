//! Translations of the management screens (`manage.` keys).

use std::sync::Once;

use ember_ui::i18n::Catalogs;

/// This crate's catalogs, one per language in [`ember_ui::i18n::LANGUAGES`].
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

/// Register [`CATALOGS`] with the translation registry, once per process.
/// Called from [`crate::init`] and, defensively, by every view constructor.
pub(crate) fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| ember_ui::i18n::register(CATALOGS));
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::path::Path;

    use ember_ui::i18n::{LANGUAGES, missing_keys};
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
        parse("en", CATALOGS[0].1)
    }

    #[test]
    fn every_language_has_a_catalog() {
        let codes: Vec<&str> = CATALOGS.iter().map(|(c, _)| *c).collect();
        let expected: Vec<&str> = LANGUAGES.iter().map(|l| l.code).collect();
        assert_eq!(codes, expected);
    }

    #[test]
    fn catalogs_parse_and_match_english() {
        let en = english();
        assert!(en.keys().all(|k| k.starts_with("manage.")));
        for (code, json) in CATALOGS.iter() {
            let catalog = parse(code, json);
            assert_eq!(missing_keys(CATALOGS, code), Vec::<String>::new(), "{code}");
            let extra: Vec<&String> = catalog.keys().filter(|k| !en.contains_key(*k)).collect();
            assert!(extra.is_empty(), "{code} has extra keys: {extra:?}");
            for (key, text) in &catalog {
                assert!(!text.trim().is_empty(), "{code}: {key} is empty");
                assert_eq!(
                    placeholders(text),
                    placeholders(&en[key]),
                    "{code}: placeholders of {key}"
                );
            }
        }
    }

    fn sources(dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                out.push((path.display().to_string(), text));
            }
        }
    }

    #[test]
    fn keys_used_in_sources_exist() {
        let en = english();
        let re = Regex::new(r#"\bt(?:_args)?\(\s*"([^"]+)""#).unwrap();
        let mut files = Vec::new();
        sources(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut used = BTreeSet::new();
        for (file, text) in &files {
            for c in re.captures_iter(text) {
                let key = c[1].to_string();
                assert!(en.contains_key(&key), "{file}: {key} is not in en.json");
                used.insert(key);
            }
        }
        // Emulator option texts are looked up by option id.
        used.extend(crate::emulators::option_keys());
        let unused: Vec<&String> = en.keys().filter(|k| !used.contains(*k)).collect();
        assert!(
            unused.is_empty(),
            "keys in en.json not used in sources: {unused:?}"
        );
    }
}
