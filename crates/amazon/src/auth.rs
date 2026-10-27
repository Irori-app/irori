//! Device sign-in: the PKCE sign-in page, the authorization code it yields,
//! and the tokens obtained by registering Irori as an Amazon Games device.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Device type of the Amazon Games app for Windows.
pub const DEVICE_TYPE: &str = "A2UMVHOX7UP4V7";
pub(crate) const APP_NAME: &str = "AGSLauncher for Windows";
pub(crate) const APP_VERSION: &str = "1.0.0";
const MARKETPLACE_ID: &str = "ATVPDKIKX0DER";

/// Query parameter Amazon appends to the redirect after a successful sign-in.
pub const CODE_PARAM: &str = "openid.oa2.authorization_code";

/// Regex (one capture group) that finds the code in the redirect address.
pub const CODE_CAPTURE: &str = r"openid\.oa2\.authorization_code=([^&#\s]+)";

static CODE_IN_TEXT: Lazy<Regex> = Lazy::new(|| Regex::new(CODE_CAPTURE).expect("valid regex"));
static BARE_CODE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Za-z0-9._~\-]{8,}$").expect("valid regex"));

/// A pending sign-in: the page to open and the secrets needed to register
/// the device with the code it returns. Kept in memory only.
#[derive(Clone)]
pub struct LoginRequest {
    pub url: String,
    pub(crate) code_verifier: String,
    pub(crate) serial: String,
    pub(crate) client_id: String,
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LoginRequest { <redacted> }")
    }
}

impl LoginRequest {
    /// A fresh device identity and PKCE pair.
    pub fn new() -> Self {
        let mut secret = Vec::with_capacity(32);
        secret.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        secret.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        let serial = uuid::Uuid::new_v4().simple().to_string().to_uppercase();
        Self::from_parts(&secret, serial)
    }

    fn from_parts(secret: &[u8], serial: String) -> Self {
        let code_verifier = URL_SAFE_NO_PAD.encode(secret);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let client_id = hex::encode(format!("{serial}#{DEVICE_TYPE}"));
        let url = url::Url::parse_with_params(
            "https://amazon.com/ap/signin",
            [
                ("openid.ns", "http://specs.openid.net/auth/2.0"),
                (
                    "openid.claimed_id",
                    "http://specs.openid.net/auth/2.0/identifier_select",
                ),
                (
                    "openid.identity",
                    "http://specs.openid.net/auth/2.0/identifier_select",
                ),
                ("openid.mode", "checkid_setup"),
                ("openid.oa2.scope", "device_auth_access"),
                ("openid.ns.oa2", "http://www.amazon.com/ap/ext/oauth/2"),
                ("openid.oa2.response_type", "code"),
                ("openid.oa2.code_challenge_method", "S256"),
                ("openid.oa2.client_id", &format!("device:{client_id}")),
                ("language", "en_US"),
                ("marketPlaceId", MARKETPLACE_ID),
                ("openid.return_to", "https://www.amazon.com"),
                ("openid.pape.max_auth_age", "0"),
                ("openid.assoc_handle", "amzn_sonic_games_launcher"),
                ("pageId", "amzn_sonic_games_launcher"),
                ("openid.oa2.code_challenge", &challenge),
            ],
        )
        .expect("valid sign-in url")
        .to_string();
        Self {
            url,
            code_verifier,
            serial,
            client_id,
        }
    }
}

impl Default for LoginRequest {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract the authorization code from what the sign-in produced: the
/// redirect address (any URL carrying `openid.oa2.authorization_code`) or
/// the bare code.
pub fn extract_auth_code(input: &str) -> Option<String> {
    let input = input.trim().trim_matches(|c| c == '"' || c == '\'');
    if input.is_empty() {
        return None;
    }
    if let Ok(url) = url::Url::parse(input)
        && let Some((_, v)) = url.query_pairs().find(|(k, _)| k == CODE_PARAM)
    {
        return (!v.is_empty()).then(|| v.into_owned());
    }
    if let Some(c) = CODE_IN_TEXT.captures(input) {
        let raw = format!("c={}", &c[1]);
        return url::form_urlencoded::parse(raw.as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .filter(|v| !v.is_empty());
    }
    BARE_CODE.is_match(input).then(|| input.to_string())
}

/// A registered device's tokens and account. Irori's own format.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix time the access token expires.
    pub expires_at: u64,
    /// Serial of the registered device (identifies it to the library API).
    pub device_serial: String,
    pub user_id: String,
    /// First name of the account holder.
    pub display_name: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("user_id", &self.user_id)
            .field("display_name", &self.display_name)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    /// Whether the access token expires within a minute of `now`.
    pub fn needs_refresh(&self, now: u64) -> bool {
        now + 60 >= self.expires_at
    }

    /// `hardwareHash` the entitlement service expects for this device.
    pub(crate) fn hardware_hash(&self) -> String {
        hex::encode_upper(Sha256::digest(self.device_serial.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_pkce_sign_in() {
        let req = LoginRequest::from_parts(&[7u8; 32], "ABC123".into());
        assert_eq!(req.code_verifier, URL_SAFE_NO_PAD.encode([7u8; 32]));
        assert_eq!(req.client_id, hex::encode("ABC123#A2UMVHOX7UP4V7"));
        let url = url::Url::parse(&req.url).unwrap();
        assert_eq!(url.host_str(), Some("amazon.com"));
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            q["openid.oa2.client_id"],
            format!("device:{}", req.client_id)
        );
        assert_eq!(q["openid.oa2.code_challenge_method"], "S256");
        assert_eq!(
            q["openid.oa2.code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(req.code_verifier.as_bytes()))
        );
        assert_eq!(q["openid.pape.max_auth_age"], "0");
        assert_eq!(format!("{req:?}"), "LoginRequest { <redacted> }");

        let a = LoginRequest::new();
        let b = LoginRequest::new();
        assert_ne!(a.code_verifier, b.code_verifier);
        assert_eq!(a.serial.len(), 32);
        assert_eq!(a.code_verifier.len(), 43);
    }

    #[test]
    fn extracts_auth_code() {
        let url = "https://www.amazon.com/?openid.assoc_handle=amzn_sonic_games_launcher&openid.oa2.authorization_code=ANabcDEF123456%2Bx&openid.mode=id_res";
        assert_eq!(extract_auth_code(url).as_deref(), Some("ANabcDEF123456+x"));
        assert_eq!(
            extract_auth_code("  openid.oa2.authorization_code=ANxyz12345678&foo=1 ").as_deref(),
            Some("ANxyz12345678")
        );
        assert_eq!(
            extract_auth_code("ANbareCode1234").as_deref(),
            Some("ANbareCode1234")
        );
        assert_eq!(
            extract_auth_code("\"ANbareCode1234\"").as_deref(),
            Some("ANbareCode1234")
        );
        assert_eq!(extract_auth_code("https://www.amazon.com/?foo=bar"), None);
        assert_eq!(extract_auth_code("not a code"), None);
        assert_eq!(extract_auth_code(""), None);
        let re = Regex::new(CODE_CAPTURE).unwrap();
        assert_eq!(&re.captures(url).unwrap()[1], "ANabcDEF123456%2Bx");
    }

    #[test]
    fn credentials_hide_secrets() {
        let c = Credentials {
            access_token: "Atna|secret".into(),
            refresh_token: "Atnr|secret".into(),
            expires_at: 1000,
            device_serial: "SERIAL".into(),
            user_id: "amzn1.account.X".into(),
            display_name: "Radu".into(),
        };
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("secret") && dbg.contains("Radu"));
        assert!(c.needs_refresh(950) && !c.needs_refresh(900));
        assert_eq!(
            c.hardware_hash(),
            hex::encode_upper(Sha256::digest(b"SERIAL"))
        );
    }
}
