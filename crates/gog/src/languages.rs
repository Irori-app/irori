//! GOG's language names and codes. Depots name languages by code, English
//! name or a deprecated code (`en`, `English`, `en-US` are the same).

/// `(code, English name, deprecated codes)` of the languages GOG uses.
pub(crate) const LANGUAGES: &[(&str, &str, &[&str])] = &[
    ("af-ZA", "Afrikaans", &[]),
    ("ar", "Arabic", &[]),
    ("az-AZ", "Azeri", &[]),
    ("be-BY", "Belarusian", &["be"]),
    ("bn-BD", "Bengali", &["bn_BD"]),
    ("bg-BG", "Bulgarian", &["bg", "bl"]),
    ("bs-BA", "Bosnian", &[]),
    ("ca-ES", "Catalan", &["ca"]),
    ("cs-CZ", "Czech", &["cz"]),
    ("cy-GB", "Welsh", &[]),
    ("da-DK", "Danish", &["da"]),
    ("de-DE", "German", &["de"]),
    ("dv-MV", "Divehi", &[]),
    ("el-GR", "Greek", &["gk", "el-GK"]),
    ("en-GB", "British English", &["en_GB"]),
    ("en-US", "English", &["en"]),
    ("es-ES", "Spanish", &["es"]),
    ("es-MX", "Latin American Spanish", &["es_mx"]),
    ("et-EE", "Estonian", &["et"]),
    ("eu-ES", "Basque", &[]),
    ("fa-IR", "Persian", &["fa"]),
    ("fi-FI", "Finnish", &["fi"]),
    ("fo-FO", "Faroese", &[]),
    ("fr-FR", "French", &["fr"]),
    ("gl-ES", "Galician", &[]),
    ("gu-IN", "Gujarati", &["gu"]),
    ("he-IL", "Hebrew", &["he"]),
    ("hi-IN", "Hindi", &["hi"]),
    ("hr-HR", "Croatian", &[]),
    ("hu-HU", "Hungarian", &["hu"]),
    ("hy-AM", "Armenian", &[]),
    ("id-ID", "Indonesian", &[]),
    ("is-IS", "Icelandic", &["is"]),
    ("it-IT", "Italian", &["it"]),
    ("ja-JP", "Japanese", &["jp"]),
    ("jv-ID", "Javanese", &["jv"]),
    ("ka-GE", "Georgian", &[]),
    ("kk-KZ", "Kazakh", &[]),
    ("kn-IN", "Kannada", &[]),
    ("ko-KR", "Korean", &["ko"]),
    ("kok-IN", "Konkani", &[]),
    ("ky-KG", "Kyrgyz", &[]),
    ("la", "Latin", &[]),
    ("lt-LT", "Lithuanian", &[]),
    ("lv-LV", "Latvian", &[]),
    ("ml-IN", "Malayalam", &["ml"]),
    ("mi-NZ", "Maori", &[]),
    ("mk-MK", "Macedonian", &[]),
    ("mn-MN", "Mongolian", &[]),
    ("mr-IN", "Marathi", &["mr"]),
    ("ms-MY", "Malay", &[]),
    ("mt-MT", "Maltese", &[]),
    ("nb-NO", "Norwegian", &["no"]),
    ("nl-NL", "Dutch", &["nl"]),
    ("ns-ZA", "Northern Sotho", &[]),
    ("pa-IN", "Punjabi", &[]),
    ("pl-PL", "Polish", &["pl"]),
    ("ps-AR", "Pashto", &[]),
    ("pt-BR", "Portuguese (Brazilian)", &["br"]),
    ("pt-PT", "Portuguese", &["pt"]),
    ("ro-RO", "Romanian", &["ro"]),
    ("ru-RU", "Russian", &["ru"]),
    ("sa-IN", "Sanskrit", &[]),
    ("sk-SK", "Slovak", &["sk"]),
    ("sl-SI", "Slovenian", &[]),
    ("sq-AL", "Albanian", &[]),
    ("sr-SP", "Serbian", &["sb"]),
    ("sv-SE", "Swedish", &["sv"]),
    ("sw-KE", "Kiswahili", &[]),
    ("ta-IN", "Tamil", &["ta_IN"]),
    ("te-IN", "Telugu", &["te"]),
    ("th-TH", "Thai", &["th"]),
    ("tl-PH", "Tagalog", &[]),
    ("tn-ZA", "Setswana", &[]),
    ("tr-TR", "Turkish", &["tr"]),
    ("tt-RU", "Tatar", &[]),
    ("uk-UA", "Ukrainian", &["uk"]),
    ("ur-PK", "Urdu", &["ur_PK"]),
    ("uz-UZ", "Uzbek", &[]),
    ("vi-VN", "Vietnamese", &["vi"]),
    ("xh-ZA", "isiXhosa", &[]),
    ("zh-Hans", "Chinese (Simplified)", &["zh_Hans", "zh", "cn"]),
    ("zh-Hant", "Chinese (Traditional)", &["zh_Hant"]),
    ("zu-ZA", "isiZulu", &[]),
];

/// The canonical code (`en-US`) of a language given in any of GOG's forms;
/// `None` for `*`/`Neutral` and unknown names.
pub fn code(value: &str) -> Option<&'static str> {
    if value == "*" || value.eq_ignore_ascii_case("neutral") {
        return None;
    }
    LANGUAGES
        .iter()
        .find(|(code, name, deprecated)| {
            code.eq_ignore_ascii_case(value)
                || name.eq_ignore_ascii_case(value)
                || deprecated.iter().any(|d| d.eq_ignore_ascii_case(value))
        })
        .map(|(code, _, _)| *code)
}

/// English name of a language code (`en-US` → `English`).
pub fn name(code: &str) -> Option<&'static str> {
    LANGUAGES
        .iter()
        .find(|(c, _, _)| c.eq_ignore_ascii_case(code))
        .map(|(_, name, _)| *name)
}

/// Whether a depot language entry applies to `target` (a canonical code).
pub fn matches(entry: &str, target: &str) -> bool {
    entry == "*" || entry.eq_ignore_ascii_case("neutral") || code(entry) == Some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_language_forms() {
        assert_eq!(code("English"), Some("en-US"));
        assert_eq!(code("en"), Some("en-US"));
        assert_eq!(code("EN-us"), Some("en-US"));
        assert_eq!(code("Deutsch"), None);
        assert_eq!(code("*"), None);
        assert_eq!(name("de-DE"), Some("German"));
        assert!(matches("*", "de-DE") && matches("German", "de-DE") && !matches("French", "de-DE"));
    }
}
