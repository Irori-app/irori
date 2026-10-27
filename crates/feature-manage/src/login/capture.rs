//! Recognising a store's login secret in pasted or copied text.
//!
//! Secrets never leave this module except as the text handed to
//! `Store::login`; the watcher remembers clipboard contents by hash only.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use regex::Regex;

/// Compile a store's capture pattern. A broken pattern disables automatic
/// detection; manual paste still works.
pub(crate) fn compile_capture(pattern: &str) -> Option<Regex> {
    if pattern.is_empty() {
        return None;
    }
    match Regex::new(pattern) {
        Ok(re) => Some(re),
        Err(e) => {
            tracing::warn!("invalid login capture pattern: {e}");
            None
        }
    }
}

/// Whether `text` contains a login secret: the pattern's first capture group
/// matches something non-empty.
pub(crate) fn contains_secret(re: &Regex, text: &str) -> bool {
    re.captures(text)
        .and_then(|c| c.get(1))
        .is_some_and(|m| !m.as_str().trim().is_empty())
}

fn fingerprint(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Watches clipboard snapshots for a newly copied login secret.
///
/// Whatever was on the clipboard when the dialog opened is ignored, and each
/// distinct clipboard content is offered at most once, so a rejected code is
/// not resubmitted in a loop.
#[derive(Debug, Default)]
pub(crate) struct ClipboardWatcher {
    last_seen: Option<u64>,
}

impl ClipboardWatcher {
    /// Start watching; `initial` is the clipboard content at that moment.
    pub fn new(initial: Option<&str>) -> Self {
        Self {
            last_seen: initial.map(fingerprint),
        }
    }

    /// Mark content as seen without offering it (text Irori itself copied).
    pub fn ignore(&mut self, text: &str) {
        self.last_seen = Some(fingerprint(text));
    }

    /// Feed the current clipboard text. Returns the trimmed text to submit
    /// when it changed since the last observation and contains a secret.
    pub fn observe(&mut self, re: &Regex, text: Option<&str>) -> Option<String> {
        let text = text?;
        let fp = fingerprint(text);
        if self.last_seen == Some(fp) {
            return None;
        }
        self.last_seen = Some(fp);
        contains_secret(re, text).then(|| text.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use irori_amazon_api::auth::CODE_CAPTURE as AMAZON;
    use irori_epic_api::auth::CAPTURE as EPIC;
    use irori_gog_api::auth::LOGIN_CAPTURE as GOG;

    /// ZOOM's pattern is private to its store crate, which this crate does
    /// not depend on; keep it in step with `LI_TOKEN_CAPTURE` there.
    const ZOOM: &str = r"(?:^|[?&#])li_token=([^&#\s]+)";

    #[test]
    fn matches_store_secrets() {
        let gog = compile_capture(GOG).unwrap();
        assert!(contains_secret(
            &gog,
            "https://embed.gog.com/on_login_success?origin=client&code=AbC-123_x.y"
        ));
        assert!(!contains_secret(&gog, "https://www.gog.com/"));
        let zoom = compile_capture(ZOOM).unwrap();
        assert!(contains_secret(
            &zoom,
            "https://www.zoom-platform.com/?li_token=tok123&x=1"
        ));
        let epic = compile_capture(EPIC).unwrap();
        assert!(contains_secret(
            &epic,
            r#"{"redirectUrl":"x","authorizationCode":"0123456789abcdef0123456789abcdef","sid":null}"#
        ));
        assert!(!contains_secret(&epic, r#"{"authorizationCode":null}"#));
        let amazon = compile_capture(AMAZON).unwrap();
        assert!(contains_secret(
            &amazon,
            "https://www.amazon.com/?openid.oa2.authorization_code=ANcode123&openid.mode=id_res"
        ));
        assert!(!contains_secret(
            &amazon,
            "https://www.amazon.com/ap/signin"
        ));
    }

    #[test]
    fn broken_or_empty_patterns_disable_detection() {
        assert!(compile_capture("").is_none());
        assert!(compile_capture("(unclosed").is_none());
    }

    #[test]
    fn pattern_without_group_never_matches() {
        let re = compile_capture("code=").unwrap();
        assert!(!contains_secret(&re, "code=abc"));
    }

    #[test]
    fn watcher_offers_each_new_secret_once() {
        let re = compile_capture(ZOOM).unwrap();
        let stale = "https://x/?li_token=old";
        let mut w = ClipboardWatcher::new(Some(stale));
        // Content present when the dialog opened is not submitted.
        assert_eq!(w.observe(&re, Some(stale)), None);
        assert_eq!(w.observe(&re, None), None);
        assert_eq!(w.observe(&re, Some("unrelated text")), None);
        let fresh = "  https://x/?li_token=new  ";
        assert_eq!(
            w.observe(&re, Some(fresh)).as_deref(),
            Some("https://x/?li_token=new")
        );
        // Still on the clipboard after a failed attempt: not offered again.
        assert_eq!(w.observe(&re, Some(fresh)), None);
        // Copied again after something else: offered again.
        assert_eq!(w.observe(&re, Some("other")), None);
        assert!(w.observe(&re, Some(fresh)).is_some());
    }

    #[test]
    fn watcher_ignores_text_irori_copied() {
        let re = compile_capture(ZOOM).unwrap();
        let mut w = ClipboardWatcher::new(None);
        let link = "https://www.zoom-platform.com/login?li=irori&return_li_token=true";
        w.ignore(link);
        assert_eq!(w.observe(&re, Some(link)), None);
    }
}
