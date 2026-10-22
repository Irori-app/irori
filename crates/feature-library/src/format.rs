//! Display strings for library metadata.

use ember_core::util::{format_bytes, parse_rfc3339};
use ember_core::{GameInfo, Runner};
use ember_ui::i18n::{t, t_args};

/// "Just now", "3 hours ago", "Yesterday", "5 days ago", "2 weeks ago",
/// "4 months ago", "2 years ago".
pub fn relative_time(then: i64, now: i64) -> String {
    const HOUR: i64 = 3600;
    const DAY: i64 = 24 * HOUR;
    let diff = now - then;
    let plural = |n: i64, one: &str, other: &str| {
        if n == 1 {
            t(one).to_string()
        } else {
            t_args(other, &[("count", &n.to_string())]).to_string()
        }
    };
    match diff {
        d if d < HOUR => t("library.time.just_now").to_string(),
        d if d < DAY => plural(
            d / HOUR,
            "library.time.hours_one",
            "library.time.hours_other",
        ),
        d if d < 2 * DAY => t("library.time.yesterday").to_string(),
        // Two days or more: never singular.
        d if d < 7 * DAY => t_args(
            "library.time.days_other",
            &[("count", &(d / DAY).to_string())],
        )
        .to_string(),
        d if d < 30 * DAY => plural(
            d / (7 * DAY),
            "library.time.weeks_one",
            "library.time.weeks_other",
        ),
        d if d < 365 * DAY => plural(
            d / (30 * DAY),
            "library.time.months_one",
            "library.time.months_other",
        ),
        d => plural(
            d / (365 * DAY),
            "library.time.years_one",
            "library.time.years_other",
        ),
    }
}

/// Relative "last played" text from an RFC 3339 timestamp.
pub fn last_played(rfc3339: Option<&str>, now: i64) -> Option<String> {
    rfc3339
        .and_then(parse_rfc3339)
        .map(|t| relative_time(t, now))
}

/// Playtime, or `None` for games never played.
pub fn playtime(minutes: u64) -> Option<String> {
    (minutes > 0).then(|| crate::i18n::playtime(minutes))
}

/// Size on disk of an installed game, when known. Sideloaded apps have none.
pub fn install_size(game: &GameInfo) -> Option<String> {
    if game.runner() == Runner::Sideload {
        return None;
    }
    let install = game.install.as_ref()?;
    if install.install_size_bytes > 0 {
        Some(format_bytes(install.install_size_bytes))
    } else {
        Some(install.install_size.trim().to_string()).filter(|s| !s.is_empty())
    }
}

/// Current unix time in seconds.
pub fn now() -> i64 {
    ember_core::util::unix_now() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_core::InstalledInfo;

    #[test]
    fn relative_times() {
        crate::i18n::register();
        let now = 1_000_000_000;
        let h = 3600;
        assert_eq!(relative_time(now - 60, now), "Just now");
        assert_eq!(relative_time(now + 60, now), "Just now");
        assert_eq!(relative_time(now - h, now), "1 hour ago");
        assert_eq!(relative_time(now - 5 * h, now), "5 hours ago");
        assert_eq!(relative_time(now - 30 * h, now), "Yesterday");
        assert_eq!(relative_time(now - 3 * 24 * h, now), "3 days ago");
        assert_eq!(relative_time(now - 15 * 24 * h, now), "2 weeks ago");
        assert_eq!(relative_time(now - 65 * 24 * h, now), "2 months ago");
        assert_eq!(relative_time(now - 800 * 24 * h, now), "2 years ago");
        assert_eq!(last_played(Some("not a date"), now), None);
        assert_eq!(last_played(None, now), None);
        assert!(last_played(Some("2001-09-08T00:00:00Z"), now).is_some());
    }

    #[test]
    fn sizes_and_playtime() {
        crate::i18n::register();
        assert_eq!(playtime(0), None);
        assert_eq!(playtime(125).as_deref(), Some("2 h 5 min"));
        let mut g = GameInfo {
            runner: Some(Runner::Gog),
            install: Some(InstalledInfo {
                install_size_bytes: 1536,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(install_size(&g).as_deref(), Some("1.5 KiB"));
        g.install = Some(InstalledInfo {
            install_size: " 12.3 GiB ".into(),
            ..Default::default()
        });
        assert_eq!(install_size(&g).as_deref(), Some("12.3 GiB"));
        g.runner = Some(Runner::Sideload);
        assert_eq!(install_size(&g), None);
    }
}
