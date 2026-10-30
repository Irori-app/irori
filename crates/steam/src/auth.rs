//! Sign-in through Steam's `IAuthenticationService`, as the Steam client
//! does: a QR code approved in the Steam mobile app, or account name and
//! password (RSA-encrypted) followed by Steam Guard. Both end by polling the
//! session until Steam hands out a refresh token, which later buys access
//! tokens.
//!
//! Tokens, passwords, guard data and Steam Guard codes never reach a log or
//! a `Debug` string.

use std::fmt;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rsa::{BigUint, Pkcs1v15Encrypt, RsaPublicKey};
use serde::Deserialize;

use crate::api::{Client, Verb};
use crate::error::SteamError;
use crate::proto;

/// The `website_id` the Steam client signs in with.
const WEBSITE_ID: &str = "Client";

/// Poll interval when Steam doesn't name a usable one.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

/// `EOSType` reported for this computer in the account's device list.
///
/// * Windows: `k_EOSTypeWindows10` (16). Telling Windows 11 (20) apart
///   needs an OS version query; Steam only uses this for display.
/// * macOS: `k_EOSTypeMacOSUnknown` (-102).
/// * Linux: `k_EOSTypeLinuxUnknown` (-203).
/// * Anything else: `k_EOSTypeUnknown` (-1).
pub const OS_TYPE: i32 = if cfg!(target_os = "windows") {
    16
} else if cfg!(target_os = "macos") {
    -102
} else if cfg!(target_os = "linux") {
    -203
} else {
    -1
};

/// The computer signing in, as the account's device list will show it.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Name shown in the device list and the mobile app's approval prompt.
    pub friendly_name: String,
    /// Stable bytes identifying this installation. Steam ties guard data to
    /// it, so keep it the same across sign-ins.
    pub machine_id: Vec<u8>,
}

impl fmt::Debug for DeviceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceInfo")
            .field("friendly_name", &self.friendly_name)
            .finish_non_exhaustive()
    }
}

impl DeviceInfo {
    fn details(&self) -> proto::DeviceDetails {
        proto::DeviceDetails {
            device_friendly_name: self.friendly_name.clone(),
            platform_type: proto::PLATFORM_STEAM_CLIENT,
            os_type: OS_TYPE,
            gaming_device_type: proto::GAMING_DEVICE_STANDARD_PC,
            client_count: 0,
            machine_id: self.machine_id.clone(),
        }
    }
}

/// A Steam Guard confirmation kind (`EAuthSessionGuardType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GuardKind {
    /// No confirmation needed (1).
    None,
    /// A code sent by email (2).
    EmailCode,
    /// A code from the Steam mobile app's authenticator (3).
    DeviceCode,
    /// An approval tapped in the Steam mobile app (4).
    DeviceConfirmation,
    /// A link clicked in an email (5).
    EmailConfirmation,
    /// The guard data from an earlier sign-in on this machine (6).
    MachineToken,
    /// A kind this crate doesn't know.
    Unknown(i32),
}

impl GuardKind {
    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Self::None,
            2 => Self::EmailCode,
            3 => Self::DeviceCode,
            4 => Self::DeviceConfirmation,
            5 => Self::EmailConfirmation,
            6 => Self::MachineToken,
            other => Self::Unknown(other),
        }
    }

    pub fn code(self) -> i32 {
        match self {
            Self::None => 1,
            Self::EmailCode => 2,
            Self::DeviceCode => 3,
            Self::DeviceConfirmation => 4,
            Self::EmailConfirmation => 5,
            Self::MachineToken => 6,
            Self::Unknown(code) => code,
        }
    }

    /// Whether the user types a code for this kind.
    pub fn takes_code(self) -> bool {
        matches!(self, Self::EmailCode | Self::DeviceCode)
    }
}

/// One way Steam accepts to confirm a sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    pub kind: GuardKind,
    /// Steam's hint for it, such as the email domain the code went to.
    pub message: Option<String>,
}

impl From<proto::AllowedConfirmation> for Confirmation {
    fn from(c: proto::AllowedConfirmation) -> Self {
        Self {
            kind: GuardKind::from_code(c.confirmation_type),
            message: non_empty(c.associated_message),
        }
    }
}

/// A QR sign-in in progress: show `challenge_url` as a QR code and
/// [`Client::poll`] every `interval`.
#[derive(Clone)]
pub struct QrSession {
    pub client_id: u64,
    pub request_id: Vec<u8>,
    pub challenge_url: String,
    pub interval: Duration,
}

impl fmt::Debug for QrSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QrSession")
            .field("client_id", &self.client_id)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

/// A password sign-in waiting for its Steam Guard confirmation. Submit a
/// code with [`Client::submit_guard_code`] if `confirmations` asks for one,
/// then [`Client::poll`] every `interval`.
#[derive(Clone)]
pub struct CredentialsSession {
    pub client_id: u64,
    pub request_id: Vec<u8>,
    pub steamid: u64,
    pub interval: Duration,
    /// The confirmations Steam accepts, in its order of preference.
    pub confirmations: Vec<Confirmation>,
}

impl fmt::Debug for CredentialsSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialsSession")
            .field("client_id", &self.client_id)
            .field("steamid", &self.steamid)
            .field("interval", &self.interval)
            .field("confirmations", &self.confirmations)
            .finish_non_exhaustive()
    }
}

/// A signed-in account's tokens.
#[derive(Clone, PartialEq, Eq)]
pub struct Tokens {
    pub account_name: String,
    pub steamid: u64,
    /// Long-lived (about 200 days); buys access tokens.
    pub refresh_token: String,
    /// Short-lived (about a day); authenticates Web API calls.
    pub access_token: String,
    /// Steam Guard data for this machine: passing it to the next
    /// [`Client::begin_credentials`] skips the email code.
    pub guard_data: Option<String>,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("account_name", &self.account_name)
            .field("steamid", &self.steamid)
            .finish_non_exhaustive()
    }
}

/// Where a sign-in stands.
#[derive(Clone, PartialEq, Eq)]
pub enum Poll {
    /// Not confirmed yet. A QR code Steam renewed has a new challenge URL
    /// (show it instead) and sometimes a new client id (poll with it).
    Pending {
        new_challenge_url: Option<String>,
        new_client_id: Option<u64>,
    },
    /// Signed in.
    Done(Tokens),
}

impl fmt::Debug for Poll {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending {
                new_challenge_url,
                new_client_id,
            } => f
                .debug_struct("Pending")
                .field("new_challenge_url", &new_challenge_url.is_some())
                .field("new_client_id", new_client_id)
                .finish(),
            Self::Done(tokens) => f.debug_tuple("Done").field(tokens).finish(),
        }
    }
}

/// What a Steam token (a JWT) says about itself. Read without checking the
/// signature: only Steam can judge the token, this only tells when to renew
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenClaims {
    pub steamid: u64,
    /// Unix time the token expires.
    pub expires_at: u64,
    /// Where the token is valid (`web`, `client`, `renew`, `derive`…).
    pub audience: Vec<String>,
}

impl TokenClaims {
    /// Read the claims of a JWT.
    pub fn parse(jwt: &str) -> Result<Self> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }
        #[derive(Deserialize)]
        struct Raw {
            sub: serde_json::Value,
            #[serde(default)]
            exp: u64,
            #[serde(default)]
            aud: Option<OneOrMany>,
        }
        let unreadable = || anyhow!("Steam sent a sign-in token Irori could not read.");
        let payload = jwt.trim().split('.').nth(1).ok_or_else(unreadable)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .map_err(|_| unreadable())?;
        let raw: Raw = serde_json::from_slice(&bytes).map_err(|_| unreadable())?;
        let steamid = match &raw.sub {
            serde_json::Value::String(s) => s.parse().ok(),
            v => v.as_u64(),
        }
        .ok_or_else(unreadable)?;
        Ok(Self {
            steamid,
            expires_at: raw.exp,
            audience: match raw.aud {
                Some(OneOrMany::One(a)) => vec![a],
                Some(OneOrMany::Many(a)) => a,
                None => Vec::new(),
            },
        })
    }

    /// Whether the token expires within `margin_secs` of `now` (Unix
    /// seconds).
    pub fn is_expiring(&self, now: u64, margin_secs: u64) -> bool {
        now.saturating_add(margin_secs) >= self.expires_at
    }
}

impl Client {
    /// Start a QR sign-in.
    pub async fn begin_qr(&self, device: &DeviceInfo) -> Result<QrSession> {
        let req = proto::BeginAuthSessionViaQrRequest {
            device_friendly_name: device.friendly_name.clone(),
            platform_type: proto::PLATFORM_STEAM_CLIENT,
            device_details: Some(device.details()),
            website_id: WEBSITE_ID.into(),
        };
        let resp: proto::BeginAuthSessionViaQrResponse = self
            .auth_call(Verb::Post, "BeginAuthSessionViaQR", &req)
            .await?;
        if resp.challenge_url.is_empty() || resp.request_id.is_empty() {
            return Err(anyhow!("Steam did not start the QR sign-in. Try again."));
        }
        Ok(QrSession {
            client_id: resp.client_id,
            request_id: resp.request_id,
            challenge_url: resp.challenge_url,
            interval: interval(resp.interval),
        })
    }

    /// Start a sign-in with account name and password. `guard_data` is
    /// [`Tokens::guard_data`] from an earlier sign-in on this machine.
    pub async fn begin_credentials(
        &self,
        account_name: &str,
        password: &str,
        device: &DeviceInfo,
        guard_data: Option<&str>,
    ) -> Result<CredentialsSession> {
        let account_name = account_name.trim();
        let key_req = proto::GetPasswordRsaPublicKeyRequest {
            account_name: account_name.into(),
        };
        let key: proto::GetPasswordRsaPublicKeyResponse = self
            .auth_call(Verb::Get, "GetPasswordRSAPublicKey", &key_req)
            .await?;
        let encrypted_password =
            encrypt_password(&key.publickey_mod, &key.publickey_exp, password)?;
        let req = proto::BeginAuthSessionViaCredentialsRequest {
            device_friendly_name: device.friendly_name.clone(),
            account_name: account_name.into(),
            encrypted_password,
            encryption_timestamp: key.timestamp,
            remember_login: true,
            platform_type: proto::PLATFORM_STEAM_CLIENT,
            persistence: proto::PERSISTENCE_PERSISTENT,
            website_id: WEBSITE_ID.into(),
            device_details: Some(device.details()),
            guard_data: guard_data.unwrap_or_default().into(),
            language: 0,
            qos_level: 0,
        };
        let resp: proto::BeginAuthSessionViaCredentialsResponse = self
            .auth_call(Verb::Post, "BeginAuthSessionViaCredentials", &req)
            .await?;
        if resp.request_id.is_empty() {
            return Err(anyhow!("Steam did not start the sign-in. Try again."));
        }
        Ok(CredentialsSession {
            client_id: resp.client_id,
            request_id: resp.request_id,
            steamid: resp.steamid,
            interval: interval(resp.interval),
            confirmations: resp
                .allowed_confirmations
                .into_iter()
                .map(Confirmation::from)
                .collect(),
        })
    }

    /// Send the Steam Guard code the user typed (`kind` is
    /// [`GuardKind::EmailCode`] or [`GuardKind::DeviceCode`]).
    pub async fn submit_guard_code(
        &self,
        client_id: u64,
        steamid: u64,
        code: &str,
        kind: GuardKind,
    ) -> Result<()> {
        let req = proto::UpdateAuthSessionWithSteamGuardCodeRequest {
            client_id,
            steamid,
            code: code.trim().to_ascii_uppercase(),
            code_type: kind.code(),
        };
        let _: proto::UpdateAuthSessionWithSteamGuardCodeResponse = self
            .auth_call(Verb::Post, "UpdateAuthSessionWithSteamGuardCode", &req)
            .await?;
        Ok(())
    }

    /// Ask whether the sign-in has been confirmed. When it has, the account
    /// id comes from the refresh token (the answer has no field for it), and
    /// a missing access token is fetched with the refresh token.
    pub async fn poll(&self, client_id: u64, request_id: &[u8]) -> Result<Poll> {
        let req = proto::PollAuthSessionStatusRequest {
            client_id,
            request_id: request_id.to_vec(),
            token_to_revoke: 0,
        };
        let resp: proto::PollAuthSessionStatusResponse = self
            .auth_call(Verb::Post, "PollAuthSessionStatus", &req)
            .await
            .map_err(poll_error)?;
        if resp.refresh_token.is_empty() {
            return Ok(Poll::Pending {
                new_challenge_url: non_empty(resp.new_challenge_url),
                new_client_id: (resp.new_client_id != 0).then_some(resp.new_client_id),
            });
        }
        let steamid = TokenClaims::parse(&resp.refresh_token)?.steamid;
        let access_token = if resp.access_token.is_empty() {
            self.refresh_access_token(&resp.refresh_token, steamid)
                .await?
        } else {
            resp.access_token
        };
        Ok(Poll::Done(Tokens {
            account_name: resp.account_name,
            steamid,
            refresh_token: resp.refresh_token,
            access_token,
            guard_data: non_empty(resp.new_guard_data),
        }))
    }

    /// A new access token for a refresh token. [`SteamError::SessionRejected`]
    /// means the refresh token no longer works and the user must sign in
    /// again.
    pub async fn refresh_access_token(&self, refresh_token: &str, steamid: u64) -> Result<String> {
        let req = proto::GenerateAccessTokenForAppRequest {
            refresh_token: refresh_token.into(),
            steamid,
            renewal_type: 0,
        };
        let resp: proto::GenerateAccessTokenForAppResponse = self
            .auth_call(Verb::Post, "GenerateAccessTokenForApp", &req)
            .await?;
        if resp.access_token.is_empty() {
            return Err(SteamError::SessionRejected.into());
        }
        Ok(resp.access_token)
    }

    /// Ask Steam to end a saved sign-in, so a copy of its refresh token stops
    /// working. The token goes in the form body, not the address.
    pub async fn revoke_refresh_token(
        &self,
        refresh_token: &str,
        access_token: &str,
    ) -> Result<()> {
        let req = proto::RevokeTokenRequest {
            token: refresh_token.into(),
            revoke_action: proto::REVOKE_PERMANENT,
        };
        let _: () = self
            .auth_call_as(Verb::Post, "RevokeToken", &req, Some(access_token))
            .await?;
        Ok(())
    }
}

/// While polling, "expired", "access denied" and "not found" mean the
/// sign-in attempt itself is over, not a saved sign-in.
fn poll_error(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<SteamError>() {
        Some(SteamError::SessionRejected | SteamError::Other(9)) => SteamError::LoginExpired.into(),
        _ => e,
    }
}

/// Encrypt a password with the account's RSA key (hexadecimal modulus and
/// exponent), PKCS#1 v1.5, as base64.
fn encrypt_password(modulus_hex: &str, exponent_hex: &str, password: &str) -> Result<String> {
    let invalid = || anyhow!("Steam sent an invalid sign-in key. Try again.");
    let n = BigUint::parse_bytes(modulus_hex.trim().as_bytes(), 16).ok_or_else(invalid)?;
    let e = BigUint::parse_bytes(exponent_hex.trim().as_bytes(), 16).ok_or_else(invalid)?;
    let key = RsaPublicKey::new(n, e).map_err(|_| invalid())?;
    let encrypted = key
        .encrypt(
            &mut rsa::rand_core::OsRng,
            Pkcs1v15Encrypt,
            password.as_bytes(),
        )
        .context("Could not encrypt the password for Steam")?;
    Ok(STANDARD.encode(encrypted))
}

fn interval(seconds: f32) -> Duration {
    if seconds.is_finite() && seconds > 0.0 {
        Duration::from_secs_f32(seconds.clamp(1.0, 60.0))
    } else {
        DEFAULT_INTERVAL
    }
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_depot::test_server::{Req, Resp, TestServer};
    use rsa::RsaPrivateKey;
    use rsa::pkcs1::DecodeRsaPrivateKey as _;
    use rsa::traits::PublicKeyParts as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::api::Endpoints;

    /// A throwaway 1024-bit key made for these tests.
    const TEST_KEY: &str = "-----BEGIN RSA PRIVATE KEY-----
MIICXQIBAAKBgQDFwhjYWJHKE5DoyU8v8+9gYdyMBOsG76I8XPDs3SQxbhZC5MIU
gK0BAKfC/CzHnGLSYj9rXR1I3luWNu4fsGP/I+W48MuOLW0hj88jJA/VqEcZQnrY
2VJVBelQ8l6wGLnROajjqdow6drCLAsOWcra/VShPP3vvqSdzaTikOphzQIDAQAB
AoGBAIunMzZs7Z+fy6CosRQzFqOg2EfFHVhLl+060STkUi/aBzx8IZaq/AGOPeqT
fOgFxz+9A/5Exm50IIdnCSg1v5bq0n0lMgr1TnbuwSf4GapZApff6BGfj0rVWXTD
1gb0DGKInGicoLkaMADMBOvLtnWhNCIz6PVA9sYn2IHtxkpdAkEA7AcCWxm5k+SS
3yu+pKWB73JpPzSgHHsZJ6TINeg2K0yNUwgEi356HKxf/6tRowzw8D6eJIIyyIxl
Vz6jlnglGwJBANZ+EuaGPIcd8X27Zoa9sE8PJlxGeSAiyROVYukhVnQ4Ze4lW+yK
VZ1jmff+L5kbzBsXxj0W5jh8+Y+rN/fqyzcCQDLSz48OWwedkIK9yK9rdC87U65o
hpGbcC85LGNvPTrVoRjgARqIgYLrG0DSgyG5B6iz0xnJb2X/1VKUCmW8OUECQQCW
aq1IeiSiQI5ktPHOVeH5X09wcH9q2NoX4LAPezCMqixZGFvhgpVNnVaRQfERNxwo
d8CugIuMW6ALb2/9bOt7AkAv9Ar5zc5ASqAuHTWIe20lFwpqIpe4XmXUZvuZYkht
oJ5AuFG/mkhc6SJBeyPKP/c87cxXhRNAlivwKivIUvqY
-----END RSA PRIVATE KEY-----";

    const STEAMID: u64 = 76561197960265729;

    /// An unsigned JWT with the given claims (enough for [`TokenClaims`]).
    fn jwt(claims: serde_json::Value) -> String {
        format!(
            "{}.{}.sig",
            URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"EdDSA"}"#),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        )
    }

    fn device() -> DeviceInfo {
        DeviceInfo {
            friendly_name: "Irori test".into(),
            machine_id: vec![1, 2, 3],
        }
    }

    fn proto_resp(msg: &impl prost::Message) -> Resp {
        Resp {
            status: 200,
            headers: vec![("x-eresult".into(), "1".into())],
            body: msg.encode_to_vec(),
            ..Default::default()
        }
    }

    fn eresult(code: i32) -> Resp {
        Resp {
            status: 200,
            headers: vec![("x-eresult".into(), code.to_string())],
            ..Default::default()
        }
    }

    fn client(server: &TestServer) -> Client {
        Client::with_endpoints(Endpoints {
            api: server.base(),
            store: server.base(),
            assets: server.base(),
        })
        .unwrap()
    }

    /// The request message a GET carried in its query.
    fn query_message<M: prost::Message + Default>(req: &Req) -> M {
        let query = req.path.split_once('?').unwrap().1;
        let (_, value) = url::form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == "input_protobuf_encoded")
            .unwrap();
        M::decode(&STANDARD.decode(value.as_bytes()).unwrap()[..]).unwrap()
    }

    #[test]
    fn encrypts_passwords_for_the_account_key() {
        let private = RsaPrivateKey::from_pkcs1_pem(TEST_KEY).unwrap();
        let modulus = private.n().to_str_radix(16);
        let exponent = private.e().to_str_radix(16);
        let encrypted = encrypt_password(&modulus, &exponent, "hunter2").unwrap();
        let bytes = STANDARD.decode(encrypted).unwrap();
        assert_eq!(bytes.len(), 128);
        assert_eq!(
            private.decrypt(Pkcs1v15Encrypt, &bytes).unwrap(),
            b"hunter2"
        );
        let e = encrypt_password("not hex", "10001", "x").unwrap_err();
        assert!(e.to_string().contains("invalid sign-in key"), "{e}");
    }

    #[test]
    fn reads_token_claims() {
        let token = jwt(serde_json::json!({
            "iss": "steam",
            "sub": STEAMID.to_string(),
            "aud": ["client", "web", "renew", "derive"],
            "exp": 1_800_000_000u64,
        }));
        let claims = TokenClaims::parse(&token).unwrap();
        assert_eq!(claims.steamid, STEAMID);
        assert_eq!(claims.expires_at, 1_800_000_000);
        assert_eq!(claims.audience, ["client", "web", "renew", "derive"]);
        assert!(!claims.is_expiring(1_799_000_000, 300));
        assert!(claims.is_expiring(1_799_999_800, 300));
        assert!(claims.is_expiring(1_800_000_001, 0));

        let one = jwt(serde_json::json!({"sub": STEAMID, "aud": "web", "exp": 5}));
        assert_eq!(TokenClaims::parse(&one).unwrap().audience, ["web"]);
        assert!(TokenClaims::parse("garbage").is_err());
        assert!(TokenClaims::parse("a.b.c").is_err());
        assert!(TokenClaims::parse(&jwt(serde_json::json!({"sub": "x"}))).is_err());
    }

    #[test]
    fn debug_output_hides_secrets() {
        let tokens = Tokens {
            account_name: "player".into(),
            steamid: STEAMID,
            refresh_token: "secret-refresh".into(),
            access_token: "secret-access".into(),
            guard_data: Some("secret-guard".into()),
        };
        let dbg = format!("{:?}", Poll::Done(tokens));
        assert!(dbg.contains("player") && !dbg.contains("secret"), "{dbg}");
        let qr = QrSession {
            client_id: 1,
            request_id: vec![1],
            challenge_url: "https://s.team/q/secret".into(),
            interval: DEFAULT_INTERVAL,
        };
        assert!(!format!("{qr:?}").contains("secret"));
        let pending = Poll::Pending {
            new_challenge_url: Some("https://s.team/q/secret".into()),
            new_client_id: None,
        };
        assert!(!format!("{pending:?}").contains("secret"));
    }

    #[test]
    fn maps_guard_kinds_and_intervals() {
        for code in 1..=6 {
            assert_eq!(GuardKind::from_code(code).code(), code);
        }
        assert_eq!(GuardKind::from_code(9), GuardKind::Unknown(9));
        assert!(GuardKind::EmailCode.takes_code() && !GuardKind::DeviceConfirmation.takes_code());
        assert_eq!(interval(5.0), Duration::from_secs(5));
        assert_eq!(interval(0.1), Duration::from_secs(1));
        assert_eq!(interval(0.0), DEFAULT_INTERVAL);
        assert_eq!(interval(f32::NAN), DEFAULT_INTERVAL);
    }

    #[tokio::test]
    async fn begins_a_qr_sign_in() {
        let server = TestServer::start(|req| {
            if req.path == "/IAuthenticationService/BeginAuthSessionViaQR/v1" {
                proto_resp(&proto::BeginAuthSessionViaQrResponse {
                    client_id: 77,
                    challenge_url: "https://s.team/q/1/77".into(),
                    request_id: vec![4, 5],
                    interval: 5.0,
                    allowed_confirmations: vec![proto::AllowedConfirmation {
                        confirmation_type: 4,
                        associated_message: String::new(),
                    }],
                    version: 1,
                })
            } else {
                Resp::status(404)
            }
        });
        let qr = client(&server).begin_qr(&device()).await.unwrap();
        assert_eq!(qr.client_id, 77);
        assert_eq!(qr.challenge_url, "https://s.team/q/1/77");
        assert_eq!(qr.request_id, [4, 5]);
        assert_eq!(qr.interval, Duration::from_secs(5));
        let req = &server.requests()[0];
        assert_eq!(
            req.header("content-type"),
            Some("application/x-www-form-urlencoded")
        );
    }

    #[tokio::test]
    async fn begins_a_password_sign_in() {
        let private = RsaPrivateKey::from_pkcs1_pem(TEST_KEY).unwrap();
        let key = proto::GetPasswordRsaPublicKeyResponse {
            publickey_mod: private.n().to_str_radix(16),
            publickey_exp: private.e().to_str_radix(16),
            timestamp: 123,
        };
        let server = TestServer::start(move |req| {
            if req
                .path
                .starts_with("/IAuthenticationService/GetPasswordRSAPublicKey/v1?")
            {
                proto_resp(&key)
            } else if req.path == "/IAuthenticationService/BeginAuthSessionViaCredentials/v1" {
                proto_resp(&proto::BeginAuthSessionViaCredentialsResponse {
                    client_id: 8,
                    request_id: vec![9],
                    interval: 0.1,
                    allowed_confirmations: vec![proto::AllowedConfirmation {
                        confirmation_type: 2,
                        associated_message: "example.com".into(),
                    }],
                    steamid: STEAMID,
                    ..Default::default()
                })
            } else {
                Resp::status(404)
            }
        });
        let session = client(&server)
            .begin_credentials(" player ", "hunter2", &device(), None)
            .await
            .unwrap();
        assert_eq!((session.client_id, session.steamid), (8, STEAMID));
        assert_eq!(session.interval, Duration::from_secs(1));
        assert_eq!(
            session.confirmations,
            [Confirmation {
                kind: GuardKind::EmailCode,
                message: Some("example.com".into()),
            }]
        );
        let sent: proto::GetPasswordRsaPublicKeyRequest = query_message(&server.requests()[0]);
        assert_eq!(sent.account_name, "player");
    }

    #[tokio::test]
    async fn reports_a_wrong_password_and_code() {
        let server = TestServer::start(|req| {
            if req.path.contains("GetPasswordRSAPublicKey") {
                eresult(5)
            } else if req.path.contains("UpdateAuthSessionWithSteamGuardCode") {
                eresult(88)
            } else {
                eresult(84)
            }
        });
        let api = client(&server);
        let e = api
            .begin_credentials("player", "hunter2", &device(), Some("gd"))
            .await
            .unwrap_err();
        assert_eq!(e.downcast_ref(), Some(&SteamError::InvalidPassword));
        assert!(!format!("{e:#}").contains("hunter2"));
        let e = api
            .submit_guard_code(1, STEAMID, "abcde", GuardKind::DeviceCode)
            .await
            .unwrap_err();
        assert_eq!(e.downcast_ref(), Some(&SteamError::InvalidCode));
        let e = api.begin_qr(&device()).await.unwrap_err();
        assert_eq!(e.downcast_ref(), Some(&SteamError::RateLimited));
    }

    #[tokio::test]
    async fn polls_until_done() {
        let refresh = jwt(serde_json::json!({
            "sub": STEAMID.to_string(),
            "aud": ["client", "web", "renew", "derive"],
            "exp": 1_900_000_000u64,
        }));
        let calls = AtomicUsize::new(0);
        let token = refresh.clone();
        let server = TestServer::start(move |req| {
            match req.path.as_str() {
                "/IAuthenticationService/PollAuthSessionStatus/v1" => {
                    match calls.fetch_add(1, Ordering::SeqCst) {
                        0 => proto_resp(&proto::PollAuthSessionStatusResponse {
                            new_challenge_url: "https://s.team/q/1/78".into(),
                            new_client_id: 78,
                            ..Default::default()
                        }),
                        1 => proto_resp(&proto::PollAuthSessionStatusResponse::default()),
                        2 => proto_resp(&proto::PollAuthSessionStatusResponse {
                            refresh_token: token.clone(),
                            access_token: "access-1".into(),
                            account_name: "player".into(),
                            new_guard_data: "guard".into(),
                            ..Default::default()
                        }),
                        // Steam leaves the access token out for some platforms.
                        3 => proto_resp(&proto::PollAuthSessionStatusResponse {
                            refresh_token: token.clone(),
                            account_name: "player".into(),
                            ..Default::default()
                        }),
                        _ => eresult(9),
                    }
                }
                "/IAuthenticationService/GenerateAccessTokenForApp/v1" => {
                    proto_resp(&proto::GenerateAccessTokenForAppResponse {
                        access_token: "access-2".into(),
                        refresh_token: String::new(),
                    })
                }
                _ => Resp::status(404),
            }
        });
        let api = client(&server);
        assert_eq!(
            api.poll(77, &[4, 5]).await.unwrap(),
            Poll::Pending {
                new_challenge_url: Some("https://s.team/q/1/78".into()),
                new_client_id: Some(78),
            }
        );
        assert_eq!(
            api.poll(78, &[4, 5]).await.unwrap(),
            Poll::Pending {
                new_challenge_url: None,
                new_client_id: None,
            }
        );
        let Poll::Done(tokens) = api.poll(78, &[4, 5]).await.unwrap() else {
            panic!("expected tokens");
        };
        assert_eq!(
            tokens,
            Tokens {
                account_name: "player".into(),
                steamid: STEAMID,
                refresh_token: refresh.clone(),
                access_token: "access-1".into(),
                guard_data: Some("guard".into()),
            }
        );
        let Poll::Done(tokens) = api.poll(78, &[4, 5]).await.unwrap() else {
            panic!("expected tokens");
        };
        assert_eq!(tokens.access_token, "access-2");
        assert_eq!(tokens.guard_data, None);
        let e = api.poll(78, &[4, 5]).await.unwrap_err();
        assert_eq!(e.downcast_ref(), Some(&SteamError::LoginExpired));
    }

    #[tokio::test]
    async fn revokes_a_sign_in_without_putting_it_in_the_address() {
        let server = TestServer::start(|req| {
            assert_eq!(req.path, "/IAuthenticationService/RevokeToken/v1");
            eresult(1)
        });
        client(&server)
            .revoke_refresh_token("REFRESH", "ACCESS")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn refresh_rejections_ask_to_sign_in_again() {
        let calls = AtomicUsize::new(0);
        let server = TestServer::start(move |req| {
            if !req.path.contains("GenerateAccessTokenForApp") {
                return Resp::status(404);
            }
            match calls.fetch_add(1, Ordering::SeqCst) {
                0 => proto_resp(&proto::GenerateAccessTokenForAppResponse {
                    access_token: "fresh".into(),
                    refresh_token: String::new(),
                }),
                1 => eresult(27),
                2 => eresult(15),
                3 => Resp::status(401),
                4 => Resp::status(403),
                _ => eresult(42),
            }
        });
        let api = client(&server);
        assert_eq!(
            api.refresh_access_token("rt", STEAMID).await.unwrap(),
            "fresh"
        );
        for expected in [
            SteamError::SessionRejected,
            SteamError::SessionRejected,
            SteamError::SessionRejected,
            SteamError::SessionRejected,
            SteamError::Other(42),
        ] {
            let e = api.refresh_access_token("rt", STEAMID).await.unwrap_err();
            assert_eq!(e.downcast_ref(), Some(&expected));
        }
    }
}
