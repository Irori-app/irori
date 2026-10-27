//! itch.io protocol client, built on itch.io's public API (the one its own
//! app and butler use).
//!
//! * [`Client`]: the account behind an API key, the games it owns (download
//!   keys), their uploads, and download links.
//! * [`model`]: the API's answers and [`best_upload`], which picks the file
//!   to install for an operating system.
//! * Login is an API key the user creates on itch.io ([`LOGIN_URL`]);
//!   [`extract_api_key`] finds it in pasted text.
//!
//! Nothing here knows about Irori: no paths, settings or events.

#![forbid(unsafe_code)]

mod api;
pub mod model;

use once_cell::sync::Lazy;
use regex::Regex;

pub use api::{API_BASE, ApiError, Client};
pub use model::{Build, Game, Os, OwnedKey, Platforms, Upload, User, best_upload, is_installable};

/// Where the user creates and views API keys.
pub const LOGIN_URL: &str = "https://itch.io/user/settings/api-keys";

/// Capture pattern for a key shown on [`LOGIN_URL`]: itch.io's keys are 40
/// letters and digits.
pub const LOGIN_CAPTURE: &str = r"\b([A-Za-z0-9]{40})\b";

static KEY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b([A-Za-z0-9]{32,64})\b").expect("valid regex"));

/// The API key in pasted text: the key alone, or the text of the API keys
/// page around it.
pub fn extract_api_key(input: &str) -> Option<String> {
    KEY.captures_iter(input)
        .map(|c| c[1].to_string())
        // Words of letters only are page text, not keys.
        .find(|k| k.chars().any(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_key_in_pasted_text() {
        let key = "Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4zAb7cDe0f";
        assert_eq!(extract_api_key(key).as_deref(), Some(key));
        assert_eq!(extract_api_key(&format!("  {key}\n")).as_deref(), Some(key));
        assert_eq!(
            extract_api_key(&format!("API keys\nsource: desktop\nkey: {key} View")).as_deref(),
            Some(key)
        );
        assert_eq!(extract_api_key("no key here"), None);
        assert_eq!(
            extract_api_key("Supercalifragilisticexpialidociousword"),
            None
        );
        let page_capture = Regex::new(LOGIN_CAPTURE).unwrap();
        assert_eq!(&page_capture.captures(key).unwrap()[1], key);
    }
}
