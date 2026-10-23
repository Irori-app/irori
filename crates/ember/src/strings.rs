//! The shell's translation catalogs (`shell.` keys).

use ember_ui::i18n::Catalogs;

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

#[cfg(test)]
mod tests {
    use super::CATALOGS;
    use ember_ui::i18n::{LANGUAGES, missing_keys};

    #[test]
    fn every_language_is_complete() {
        for lang in LANGUAGES {
            assert!(
                CATALOGS.iter().any(|(c, _)| *c == lang.code),
                "no shell catalog for {}",
                lang.code
            );
            let missing = missing_keys(CATALOGS, lang.code);
            assert!(missing.is_empty(), "{}: missing {missing:?}", lang.code);
        }
    }

    #[test]
    fn every_used_key_exists() {
        let en: std::collections::HashMap<String, String> =
            serde_json::from_str(CATALOGS[0].1).unwrap();
        let re = regex::Regex::new(r#""(shell\.[a-z0-9_.]+)""#).unwrap();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let services = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../services/src");
        for d in [dir, services] {
            for entry in std::fs::read_dir(&d).unwrap().flatten() {
                let src = std::fs::read_to_string(entry.path()).unwrap();
                for cap in re.captures_iter(&src) {
                    // Keys passed to `item(...)` and `keys(...)` are literals too.
                    assert!(
                        en.contains_key(&cap[1]),
                        "{} uses unknown key {}",
                        entry.path().display(),
                        &cap[1]
                    );
                }
            }
        }
    }
}
