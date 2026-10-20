//! The sign-in page and the authorization code it shows.

use anyhow::{Result, bail};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

/// Epic's sign-in page, redirecting to a page that shows a JSON blob with
/// `authorizationCode` for `client_id`.
pub fn login_url(client_id: &str) -> String {
    let redirect =
        format!("https://www.epicgames.com/id/api/redirect?clientId={client_id}&responseType=code");
    let mut url = url::Url::parse("https://www.epicgames.com/id/login").expect("valid url");
    url.query_pairs_mut().append_pair("redirectUrl", &redirect);
    url.to_string()
}

/// Regex (one capture group) matching an authorization code inside the JSON
/// blob or a `?code=` redirect URL.
pub const CAPTURE: &str = r#"(?i)(?:"authorizationCode"\s*:\s*"|[?&]code=)([0-9a-f]{32})"#;

static CAPTURE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(CAPTURE).expect("regex"));
static URL_CODE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"[?&]code=([^&#\s"']+)"#).expect("regex"));
static BARE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[A-Za-z0-9]{16,128}$").expect("regex"));

/// The authorization code in a pasted JSON blob, redirect URL or bare code.
pub fn extract_code(input: &str) -> Result<String> {
    let text = input.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    if text.is_empty() {
        bail!("Paste the authorization code shown after signing in to Epic Games.");
    }
    if text.starts_with('{')
        && let Ok(json) = serde_json::from_str::<Value>(text)
    {
        if let Some(code) = json
            .get("authorizationCode")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
        {
            return Ok(code.to_string());
        }
        if let Some(url) = json.get("redirectUrl").and_then(Value::as_str)
            && let Some(c) = URL_CODE_RE.captures(url)
        {
            return Ok(c[1].to_string());
        }
        bail!(
            "The pasted text has no authorization code. Make sure you are signed in on the \
             Epic page, reload it and copy the text again."
        );
    }
    if let Some(c) = CAPTURE_RE.captures(text) {
        return Ok(c[1].to_string());
    }
    if let Some(c) = URL_CODE_RE.captures(text) {
        return Ok(c[1].to_string());
    }
    if BARE_RE.is_match(text) {
        return Ok(text.to_string());
    }
    bail!("That does not look like an Epic authorization code. Copy the text the login page shows.")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn builds_login_url() {
        let url = url::Url::parse(&login_url("abc")).unwrap();
        let (_, redirect) = url.query_pairs().find(|(k, _)| k == "redirectUrl").unwrap();
        assert_eq!(
            redirect,
            "https://www.epicgames.com/id/api/redirect?clientId=abc&responseType=code"
        );
    }

    #[test]
    fn extracts_codes() {
        let blob = format!(
            r#"{{"redirectUrl":"https://localhost/launcher/authorized?code={CODE}","authorizationCode":"{CODE}","exchangeCode":null,"sid":null}}"#
        );
        assert_eq!(extract_code(&blob).unwrap(), CODE);
        let no_code =
            r#"{"redirectUrl":"https://localhost/launcher/authorized","authorizationCode":null}"#;
        assert!(extract_code(no_code).is_err());
        let url = format!("https://localhost/launcher/authorized?code={CODE}&x=1");
        assert_eq!(extract_code(&url).unwrap(), CODE);
        assert_eq!(extract_code(&format!("  \"{CODE}\"\n")).unwrap(), CODE);
        assert!(extract_code("hello world").is_err());
        assert!(extract_code("").is_err());
        let re = Regex::new(CAPTURE).unwrap();
        let text = format!(r#"blah "authorizationCode": "{CODE}", more"#);
        assert_eq!(&re.captures(&text).unwrap()[1], CODE);
    }
}
