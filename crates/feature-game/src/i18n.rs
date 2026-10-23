//! Interface text of the game page and its workflows (`game.` keys), plus
//! translated labels for core enums whose own labels are English-only.

use std::sync::Once;

use ember_core::{InstallPlatform, StatusKind};
use ember_ui::i18n::{Catalogs, t, t_args};
use gpui_kit::SharedString;

/// Catalogs of this crate, one per language in `ember_ui::i18n::LANGUAGES`.
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

static REGISTER: Once = Once::new();

/// Register [`CATALOGS`] with `ember_ui::i18n`. Runs once however often it
/// is called.
pub(crate) fn register() {
    REGISTER.call_once(|| ember_ui::i18n::register(CATALOGS));
}

/// Present-progressive label of a live status ("Installing", "Verifying"…).
pub(crate) fn status_label(kind: StatusKind) -> SharedString {
    t(match kind {
        StatusKind::Installing => "game.status.installing",
        StatusKind::Importing => "game.status.importing",
        StatusKind::Updating => "game.status.updating",
        StatusKind::Launching => "game.status.launching",
        StatusKind::Playing => "game.status.playing",
        StatusKind::Uninstalling => "game.status.uninstalling",
        StatusKind::Repairing => "game.status.repairing",
        StatusKind::Moving => "game.status.moving",
        StatusKind::Queued => "game.status.queued",
        StatusKind::SyncingSaves => "game.status.syncing_saves",
        StatusKind::Redist => "game.status.redist",
        StatusKind::Extracting => "game.status.extracting",
        StatusKind::Winetricks => "game.status.winetricks",
        StatusKind::Done => "game.status.done",
        StatusKind::Canceled => "game.status.canceled",
        StatusKind::Error => "game.status.error",
    })
}

/// Name of an install platform (operating systems keep their names).
pub(crate) fn platform_label(platform: InstallPlatform) -> SharedString {
    match platform {
        InstallPlatform::Browser => t("game.platform.browser"),
        p => SharedString::from(p.label()),
    }
}

/// Total play time for the stats: "Not played yet" before the first session
/// and "< 1 min" once played for less than a whole minute.
pub(crate) fn playtime_label(minutes: u64, played: bool) -> SharedString {
    match (minutes, played) {
        (0, false) => t("game.info.not_played"),
        (0, true) => t("game.duration.under_minute"),
        _ => format_playtime(minutes),
    }
}

/// Playtime such as "2 h 5 min".
pub(crate) fn format_playtime(minutes: u64) -> SharedString {
    let (h, m) = ((minutes / 60).to_string(), (minutes % 60).to_string());
    match (minutes / 60, minutes % 60) {
        (0, _) => t_args("game.duration.minutes", &[("minutes", &m)]),
        (_, 0) => t_args("game.duration.hours", &[("hours", &h)]),
        _ => t_args(
            "game.duration.hours_minutes",
            &[("hours", &h), ("minutes", &m)],
        ),
    }
}

/// "{store} is not available" for a store without a backend.
pub(crate) fn store_unavailable(runner: ember_core::Runner) -> anyhow::Error {
    anyhow::anyhow!(t_args(
        "game.error.store_unavailable",
        &[("store", runner.store_name())]
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::path::Path;

    use ember_ui::i18n::{LANGUAGES, missing_keys};
    use regex::Regex;

    use super::*;

    fn parse(code: &str, json: &str) -> HashMap<String, String> {
        serde_json::from_str(json).unwrap_or_else(|e| panic!("{code}.json does not parse: {e}"))
    }

    fn placeholders(text: &str) -> BTreeSet<String> {
        let re = Regex::new(r"\{([a-z_]+)\}").expect("regex");
        re.captures_iter(text).map(|c| c[1].to_string()).collect()
    }

    #[test]
    fn every_language_has_a_catalog() {
        let codes: Vec<&str> = CATALOGS.iter().map(|(c, _)| *c).collect();
        let expected: Vec<&str> = LANGUAGES.iter().map(|l| l.code).collect();
        assert_eq!(codes, expected);
    }

    #[test]
    fn catalogs_parse_and_match_english() {
        let en = parse("en", CATALOGS[0].1);
        assert!(!en.is_empty());
        for key in en.keys() {
            assert!(
                key.starts_with("game."),
                "{key} is outside the game namespace"
            );
        }
        for (code, json) in CATALOGS.iter() {
            let catalog = parse(code, json);
            assert_eq!(missing_keys(CATALOGS, code), Vec::<String>::new(), "{code}");
            let extra: Vec<&String> = catalog.keys().filter(|k| !en.contains_key(*k)).collect();
            assert!(extra.is_empty(), "{code} has unknown keys: {extra:?}");
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

    fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read src") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn keys_used_in_sources_exist() {
        let en = parse("en", CATALOGS[0].1);
        // Translation calls with a literal key, and keys kept in tables (two
        // or more segments after `game.`, so file names like "game.log" don't
        // match).
        let call = Regex::new(r#"\bt(?:_args)?\(\s*"([^"]+)""#).expect("regex");
        let table = Regex::new(r#""(game\.[a-z0-9_]+\.[a-z0-9_.]+)""#).expect("regex");
        let mut files = Vec::new();
        sources(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut used = BTreeSet::new();
        for file in &files {
            let text = std::fs::read_to_string(file).expect("read source");
            for c in call.captures_iter(&text).chain(table.captures_iter(&text)) {
                used.insert((c[1].to_string(), file.display().to_string()));
            }
        }
        assert!(used.len() > 100, "the scan found too few keys");
        let missing: Vec<_> = used.iter().filter(|(k, _)| !en.contains_key(k)).collect();
        assert!(missing.is_empty(), "keys missing from en.json: {missing:?}");
    }

    #[test]
    fn enum_labels_and_durations() {
        register();
        assert_eq!(status_label(StatusKind::Repairing), "Verifying");
        assert_eq!(platform_label(InstallPlatform::Linux), "Linux");
        assert_eq!(platform_label(InstallPlatform::Browser), "Browser");
        assert_eq!(format_playtime(5), "5 min");
        assert_eq!(format_playtime(120), "2 h");
        assert_eq!(format_playtime(125), "2 h 5 min");
    }
}
