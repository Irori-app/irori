//! Async load state and small text helpers shared by the views.

use gpui_kit::SharedString;
use irori_ui::i18n::{t, t_args};

/// State of data fetched from the backend.
#[derive(Debug, Clone, Default)]
pub(crate) enum Load<T> {
    /// Not requested (e.g. not applicable offline).
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(SharedString),
}

impl<T> Load<T> {
    pub(crate) fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(v) => Some(v),
            _ => None,
        }
    }

    pub(crate) fn is_loading(&self) -> bool {
        matches!(self, Load::Loading)
    }

    pub(crate) fn from_result(r: anyhow::Result<T>) -> Self {
        match r {
            Ok(v) => Load::Ready(v),
            Err(e) => Load::Failed(format!("{e:#}").into()),
        }
    }
}

/// Display name of a store language code in the interface language
/// ("de-DE" → "German (de-DE)").
pub(crate) fn language_name(code: &str) -> String {
    let base = code
        .split(['-', '_'])
        .next()
        .unwrap_or(code)
        .to_ascii_lowercase();
    // English names and aliases some stores use instead of ISO codes.
    let english = match base.as_str() {
        "english" => "en",
        "german" => "de",
        "french" => "fr",
        "spanish" => "es",
        "italian" => "it",
        "portuguese" => "pt",
        "polish" => "pl",
        "russian" => "ru",
        "jp" => "ja",
        "nb" => "no",
        other => other,
    };
    let key = match english {
        "en" => "game.language_name.en",
        "de" => "game.language_name.de",
        "fr" => "game.language_name.fr",
        "es" => "game.language_name.es",
        "it" => "game.language_name.it",
        "pt" => "game.language_name.pt",
        "pl" => "game.language_name.pl",
        "ru" => "game.language_name.ru",
        "uk" => "game.language_name.uk",
        "cs" => "game.language_name.cs",
        "hu" => "game.language_name.hu",
        "nl" => "game.language_name.nl",
        "sv" => "game.language_name.sv",
        "da" => "game.language_name.da",
        "fi" => "game.language_name.fi",
        "no" => "game.language_name.no",
        "tr" => "game.language_name.tr",
        "ja" => "game.language_name.ja",
        "ko" => "game.language_name.ko",
        "zh" => "game.language_name.zh",
        "ar" => "game.language_name.ar",
        "he" => "game.language_name.he",
        "th" => "game.language_name.th",
        "el" => "game.language_name.el",
        "ro" => "game.language_name.ro",
        _ => return code.to_string(),
    };
    let name = t(key);
    if base.len() > 3 {
        // A full English name ("english") stands for itself.
        name.to_string()
    } else {
        t_args(
            "game.language_name.with_code",
            &[("name", &name), ("code", code)],
        )
        .to_string()
    }
}

/// The language to preselect: `current` when available, else one matching the
/// UI language, else English, else the first.
pub(crate) fn preferred_language(
    languages: &[String],
    current: Option<&str>,
    ui_language: &str,
) -> Option<String> {
    if let Some(c) = current
        && let Some(l) = languages.iter().find(|l| l.eq_ignore_ascii_case(c))
    {
        return Some(l.clone());
    }
    let starts = |prefix: &str| {
        let prefix = prefix.to_ascii_lowercase();
        languages
            .iter()
            .find(|l| l.to_ascii_lowercase().starts_with(&prefix))
            .cloned()
    };
    (!ui_language.is_empty())
        .then(|| starts(ui_language))
        .flatten()
        .or_else(|| starts("en"))
        .or_else(|| languages.first().cloned())
}

/// Date part of an RFC 3339 timestamp, relative when recent ("Today").
pub(crate) fn format_day(rfc3339: &str, now_unix: i64) -> String {
    let Some(ts) = irori_core::util::parse_rfc3339(rfc3339) else {
        return rfc3339.chars().take(10).collect();
    };
    let days = (now_unix - ts).div_euclid(86_400);
    match days {
        d if d < 0 => rfc3339.chars().take(10).collect(),
        0 => t("game.date.today").to_string(),
        1 => t("game.date.yesterday").to_string(),
        d if d < 7 => t_args("game.date.days_ago", &[("count", &d.to_string())]).to_string(),
        _ => rfc3339.chars().take(10).collect(),
    }
}

/// Whether store text is HTML rather than Markdown.
pub(crate) fn looks_like_html(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('<')
        || [
            "<p>", "<br", "<ul>", "<li>", "<b>", "<div", "<h2", "<h3", "<strong>",
        ]
        .iter()
        .any(|tag| text.contains(tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_names() {
        crate::i18n::register();
        assert_eq!(language_name("de-DE"), "German (de-DE)");
        assert_eq!(language_name("english"), "English");
        assert_eq!(language_name("xx"), "xx");
    }

    #[test]
    fn preferred() {
        let langs: Vec<String> = ["de-DE", "en-US", "fr-FR"].map(String::from).to_vec();
        assert_eq!(
            preferred_language(&langs, Some("FR-fr"), "de").as_deref(),
            Some("fr-FR")
        );
        assert_eq!(
            preferred_language(&langs, None, "de").as_deref(),
            Some("de-DE")
        );
        assert_eq!(
            preferred_language(&langs, None, "ja").as_deref(),
            Some("en-US")
        );
        assert_eq!(preferred_language(&[], None, "ja"), None);
    }

    #[test]
    fn days() {
        crate::i18n::register();
        let now = 1_700_000_000;
        let iso = |t: i64| {
            time::OffsetDateTime::from_unix_timestamp(t)
                .unwrap()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap()
        };
        assert_eq!(format_day(&iso(now - 60), now), "Today");
        assert_eq!(format_day(&iso(now - 86_400 - 5), now), "Yesterday");
        assert_eq!(format_day(&iso(now - 3 * 86_400), now), "3 days ago");
        assert_eq!(format_day("2020-01-02T00:00:00Z", now), "2020-01-02");
    }

    #[test]
    fn html_detection() {
        assert!(looks_like_html("<p>Hi</p>"));
        assert!(looks_like_html("Text<br/>more"));
        assert!(!looks_like_html("# Title\n\n- item"));
    }
}
