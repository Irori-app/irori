//! The library's translation catalogs (`library.` keys) and the labels it
//! gives to values owned by other crates (statuses, sort orders, playtime).

use std::sync::Once;

use ember_core::StatusKind;
use ember_services::LibrarySort;
use ember_ui::i18n::{Catalogs, t, t_args};

/// One flat JSON catalog per interface language, English first.
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

/// Hand [`CATALOGS`] to the translation registry, once per process.
pub(crate) fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| ember_ui::i18n::register(CATALOGS));
}

/// Label of a game's live status ("Installing", "Verifying"…).
pub(crate) fn status_label(kind: StatusKind) -> String {
    let key = match kind {
        StatusKind::Installing => "library.status.installing",
        StatusKind::Importing => "library.status.importing",
        StatusKind::Updating => "library.status.updating",
        StatusKind::Launching => "library.status.launching",
        StatusKind::Playing => "library.status.playing",
        StatusKind::Uninstalling => "library.status.uninstalling",
        StatusKind::Repairing => "library.status.verifying",
        StatusKind::Moving => "library.status.moving",
        StatusKind::Queued => "library.status.queued",
        StatusKind::SyncingSaves => "library.status.syncing_saves",
        StatusKind::Redist => "library.status.redist",
        StatusKind::Extracting => "library.status.extracting",
        StatusKind::Winetricks => "library.status.winetricks",
        StatusKind::Done => "library.status.done",
        StatusKind::Canceled => "library.status.canceled",
        StatusKind::Error => "library.status.error",
    };
    t(key).to_string()
}

/// Name of a sort order.
pub(crate) fn sort_label(sort: LibrarySort) -> String {
    let key = match sort {
        LibrarySort::Title => "library.sort.title",
        LibrarySort::LastPlayed => "library.sort.last_played",
        LibrarySort::Playtime => "library.sort.playtime",
        LibrarySort::InstallSize => "library.sort.install_size",
        LibrarySort::InstalledFirst => "library.sort.installed_first",
        LibrarySort::ReleaseDate => "library.sort.release_date",
        LibrarySort::DateAdded => "library.sort.date_added",
        LibrarySort::Score => "library.sort.score",
    };
    t(key).to_string()
}

/// "45 min", "2 h", "2 h 5 min".
pub(crate) fn playtime(minutes: u64) -> String {
    let (h, m) = (minutes / 60, minutes % 60);
    let (hours, mins) = (h.to_string(), m.to_string());
    match (h, m) {
        (0, _) => t_args("library.playtime.minutes", &[("minutes", &mins)]),
        (_, 0) => t_args("library.playtime.hours", &[("hours", &hours)]),
        _ => t_args(
            "library.playtime.hours_minutes",
            &[("hours", &hours), ("minutes", &mins)],
        ),
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::path::Path;

    use ember_ui::i18n::{LANGUAGES, missing_keys};

    use super::CATALOGS;

    fn parse(code: &str, json: &str) -> HashMap<String, String> {
        serde_json::from_str(json).unwrap_or_else(|e| panic!("{code}.json is invalid: {e}"))
    }

    fn placeholders(text: &str) -> BTreeSet<String> {
        let re = regex::Regex::new(r"\{([a-z_]+)\}").unwrap();
        re.captures_iter(text).map(|c| c[1].to_string()).collect()
    }

    #[test]
    fn every_catalog_parses() {
        for (code, json) in CATALOGS {
            let catalog = parse(code, json);
            assert!(!catalog.is_empty(), "{code}.json is empty");
            for (key, value) in &catalog {
                assert!(
                    key.starts_with("library."),
                    "{code}: {key} is outside library."
                );
                assert!(!value.trim().is_empty(), "{code}: {key} is blank");
            }
        }
    }

    #[test]
    fn every_language_matches_english() {
        let en = parse("en", CATALOGS[0].1);
        for lang in LANGUAGES {
            let Some((_, json)) = CATALOGS.iter().find(|(c, _)| *c == lang.code) else {
                panic!("no library catalog for {}", lang.code);
            };
            let missing = missing_keys(CATALOGS, lang.code);
            assert!(missing.is_empty(), "{}: missing {missing:?}", lang.code);
            let catalog = parse(lang.code, json);
            let extra: Vec<_> = catalog.keys().filter(|k| !en.contains_key(*k)).collect();
            assert!(extra.is_empty(), "{}: extra keys {extra:?}", lang.code);
            for (key, english) in &en {
                assert_eq!(
                    placeholders(&catalog[key]),
                    placeholders(english),
                    "{}: placeholders of {key}",
                    lang.code
                );
            }
        }
        assert_eq!(CATALOGS.len(), LANGUAGES.len());
    }

    fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn every_used_key_exists() {
        let en = parse("en", CATALOGS[0].1);
        // Every `library.` literal: arguments of `t`/`t_args` and keys chosen
        // in a `match` before being translated.
        let re = regex::Regex::new(r#""(library\.[a-z0-9_.]+)""#).unwrap();
        let mut files = Vec::new();
        sources(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut used = 0;
        for file in files {
            let src = std::fs::read_to_string(&file).unwrap();
            for cap in re.captures_iter(&src) {
                used += 1;
                assert!(
                    en.contains_key(&cap[1]),
                    "{} uses unknown key {}",
                    file.display(),
                    &cap[1]
                );
            }
        }
        assert!(used > 100, "only {used} keys found; is the pattern stale?");
    }
}
