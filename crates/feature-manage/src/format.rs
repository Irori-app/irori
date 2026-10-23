//! Text formatting shared by the management screens.

use ember_core::util::{format_bytes, format_duration};
use ember_ui::i18n::{t, t_args};
use gpui_kit::SharedString;

/// `then` relative to `now` (both unix seconds), phrased to also fit inside
/// a sentence: "just now", "5 min ago", "3 h ago", "yesterday", "4 days ago".
/// Future times read as "just now".
pub(crate) fn relative_time(then: u64, now: u64) -> SharedString {
    let secs = now.saturating_sub(then);
    match secs {
        0..60 => t("manage.time.just_now"),
        60..3_600 => t_args(
            "manage.time.minutes_ago",
            &[("count", &(secs / 60).to_string())],
        ),
        3_600..86_400 => t_args(
            "manage.time.hours_ago",
            &[("count", &(secs / 3_600).to_string())],
        ),
        86_400..172_800 => t("manage.time.yesterday"),
        _ => t_args(
            "manage.time.days_ago",
            &[("count", &(secs / 86_400).to_string())],
        ),
    }
}

/// A transfer rate, or an em dash when unknown or zero.
pub(crate) fn rate(bytes_per_sec: Option<u64>) -> String {
    match bytes_per_sec.filter(|b| *b > 0) {
        Some(b) => format!("{}/s", format_bytes(b)),
        None => "—".to_string(),
    }
}

/// Remaining time, or an em dash when unknown.
pub(crate) fn eta(secs: Option<u64>) -> String {
    match secs.filter(|s| *s > 0) {
        Some(s) => format_duration(s),
        None => "—".to_string(),
    }
}

/// A size in bytes, or `None` when unknown (zero).
pub(crate) fn size(bytes: u64) -> Option<String> {
    (bytes > 0).then(|| format_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times() {
        crate::i18n::register();
        let now = 1_000_000;
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(relative_time(now + 30, now), "just now");
        assert_eq!(relative_time(now - 125, now), "2 min ago");
        assert_eq!(relative_time(now - 7_200, now), "2 h ago");
        assert_eq!(relative_time(now - 90_000, now), "yesterday");
        assert_eq!(relative_time(now - 5 * 86_400, now), "5 days ago");
    }

    #[test]
    fn rates_and_sizes() {
        assert_eq!(rate(None), "—");
        assert_eq!(rate(Some(0)), "—");
        assert_eq!(rate(Some(1536)), "1.5 KiB/s");
        assert_eq!(eta(Some(95)), "1m 35s");
        assert_eq!(size(0), None);
        assert_eq!(size(2048).as_deref(), Some("2.0 KiB"));
    }
}
