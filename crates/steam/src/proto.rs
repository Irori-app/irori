//! The `IAuthenticationService` messages Irori sends and reads, with the
//! field numbers of Valve's `steammessages_auth.steamclient.proto`.
//!
//! Valve's file is proto2; these use implicit presence, which encodes the
//! same bytes because every field Irori leaves at zero or empty has that as
//! its proto2 default too (`qos_level`, whose default is 2, is never sent, so
//! Steam applies its own default). Fields Irori never reads are left out:
//! prost skips unknown fields while decoding.
//!
//! Messages carrying a password, a token or a Steam Guard code skip prost's
//! derived `Debug` and print them redacted.

use std::fmt;

/// `EAuthTokenPlatformType::k_EAuthTokenPlatformType_SteamClient`.
pub(crate) const PLATFORM_STEAM_CLIENT: i32 = 1;

/// `ESessionPersistence::k_ESessionPersistence_Persistent`.
pub(crate) const PERSISTENCE_PERSISTENT: i32 = 1;

/// `EGamingDeviceType::k_EGamingDeviceType_StandardPC`.
pub(crate) const GAMING_DEVICE_STANDARD_PC: u32 = 1;

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DeviceDetails {
    #[prost(string, tag = "1")]
    pub device_friendly_name: String,
    /// `EAuthTokenPlatformType`.
    #[prost(int32, tag = "2")]
    pub platform_type: i32,
    /// `EOSType`.
    #[prost(int32, tag = "3")]
    pub os_type: i32,
    /// `EGamingDeviceType`.
    #[prost(uint32, tag = "4")]
    pub gaming_device_type: u32,
    #[prost(uint32, tag = "5")]
    pub client_count: u32,
    #[prost(bytes = "vec", tag = "6")]
    pub machine_id: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AllowedConfirmation {
    /// `EAuthSessionGuardType`.
    #[prost(int32, tag = "1")]
    pub confirmation_type: i32,
    #[prost(string, tag = "2")]
    pub associated_message: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct BeginAuthSessionViaQrRequest {
    #[prost(string, tag = "1")]
    pub device_friendly_name: String,
    #[prost(int32, tag = "2")]
    pub platform_type: i32,
    #[prost(message, optional, tag = "3")]
    pub device_details: Option<DeviceDetails>,
    #[prost(string, tag = "4")]
    pub website_id: String,
}

#[derive(Clone, PartialEq, prost::Message)]
#[prost(skip_debug)]
pub(crate) struct BeginAuthSessionViaQrResponse {
    #[prost(uint64, tag = "1")]
    pub client_id: u64,
    #[prost(string, tag = "2")]
    pub challenge_url: String,
    #[prost(bytes = "vec", tag = "3")]
    pub request_id: Vec<u8>,
    #[prost(float, tag = "4")]
    pub interval: f32,
    #[prost(message, repeated, tag = "5")]
    pub allowed_confirmations: Vec<AllowedConfirmation>,
    #[prost(int32, tag = "6")]
    pub version: i32,
}

impl fmt::Debug for BeginAuthSessionViaQrResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BeginAuthSessionViaQrResponse")
            .field("client_id", &self.client_id)
            .field("interval", &self.interval)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetPasswordRsaPublicKeyRequest {
    #[prost(string, tag = "1")]
    pub account_name: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GetPasswordRsaPublicKeyResponse {
    /// Modulus, in hexadecimal.
    #[prost(string, tag = "1")]
    pub publickey_mod: String,
    /// Exponent, in hexadecimal.
    #[prost(string, tag = "2")]
    pub publickey_exp: String,
    #[prost(uint64, tag = "3")]
    pub timestamp: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
#[prost(skip_debug)]
pub(crate) struct BeginAuthSessionViaCredentialsRequest {
    #[prost(string, tag = "1")]
    pub device_friendly_name: String,
    #[prost(string, tag = "2")]
    pub account_name: String,
    /// Base64 of the password encrypted with RSA PKCS#1 v1.5.
    #[prost(string, tag = "3")]
    pub encrypted_password: String,
    #[prost(uint64, tag = "4")]
    pub encryption_timestamp: u64,
    #[prost(bool, tag = "5")]
    pub remember_login: bool,
    #[prost(int32, tag = "6")]
    pub platform_type: i32,
    /// `ESessionPersistence`.
    #[prost(int32, tag = "7")]
    pub persistence: i32,
    #[prost(string, tag = "8")]
    pub website_id: String,
    #[prost(message, optional, tag = "9")]
    pub device_details: Option<DeviceDetails>,
    #[prost(string, tag = "10")]
    pub guard_data: String,
    #[prost(uint32, tag = "11")]
    pub language: u32,
    #[prost(int32, tag = "12")]
    pub qos_level: i32,
}

impl fmt::Debug for BeginAuthSessionViaCredentialsRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BeginAuthSessionViaCredentialsRequest")
            .field("platform_type", &self.platform_type)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, prost::Message)]
#[prost(skip_debug)]
pub(crate) struct BeginAuthSessionViaCredentialsResponse {
    #[prost(uint64, tag = "1")]
    pub client_id: u64,
    #[prost(bytes = "vec", tag = "2")]
    pub request_id: Vec<u8>,
    #[prost(float, tag = "3")]
    pub interval: f32,
    #[prost(message, repeated, tag = "4")]
    pub allowed_confirmations: Vec<AllowedConfirmation>,
    #[prost(uint64, tag = "5")]
    pub steamid: u64,
    /// A string in Valve's file (a short-lived JWT), not an integer.
    #[prost(string, tag = "6")]
    pub weak_token: String,
    #[prost(string, tag = "7")]
    pub agreement_session_url: String,
    #[prost(string, tag = "8")]
    pub extended_error_message: String,
}

impl fmt::Debug for BeginAuthSessionViaCredentialsResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BeginAuthSessionViaCredentialsResponse")
            .field("client_id", &self.client_id)
            .field("interval", &self.interval)
            .field("allowed_confirmations", &self.allowed_confirmations)
            .field("steamid", &self.steamid)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, prost::Message)]
#[prost(skip_debug)]
pub(crate) struct UpdateAuthSessionWithSteamGuardCodeRequest {
    #[prost(uint64, tag = "1")]
    pub client_id: u64,
    #[prost(fixed64, tag = "2")]
    pub steamid: u64,
    #[prost(string, tag = "3")]
    pub code: String,
    /// `EAuthSessionGuardType`.
    #[prost(int32, tag = "4")]
    pub code_type: i32,
}

impl fmt::Debug for UpdateAuthSessionWithSteamGuardCodeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpdateAuthSessionWithSteamGuardCodeRequest")
            .field("client_id", &self.client_id)
            .field("steamid", &self.steamid)
            .field("code_type", &self.code_type)
            .finish_non_exhaustive()
    }
}

/// Steam answers a guard code with an empty message; the result is in
/// `x-eresult`.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct UpdateAuthSessionWithSteamGuardCodeResponse {}

#[derive(Clone, PartialEq, prost::Message)]
#[prost(skip_debug)]
pub(crate) struct PollAuthSessionStatusRequest {
    #[prost(uint64, tag = "1")]
    pub client_id: u64,
    #[prost(bytes = "vec", tag = "2")]
    pub request_id: Vec<u8>,
    #[prost(fixed64, tag = "3")]
    pub token_to_revoke: u64,
}

impl fmt::Debug for PollAuthSessionStatusRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PollAuthSessionStatusRequest")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, prost::Message)]
#[prost(skip_debug)]
pub(crate) struct PollAuthSessionStatusResponse {
    #[prost(uint64, tag = "1")]
    pub new_client_id: u64,
    #[prost(string, tag = "2")]
    pub new_challenge_url: String,
    #[prost(string, tag = "3")]
    pub refresh_token: String,
    #[prost(string, tag = "4")]
    pub access_token: String,
    #[prost(bool, tag = "5")]
    pub had_remote_interaction: bool,
    #[prost(string, tag = "6")]
    pub account_name: String,
    #[prost(string, tag = "7")]
    pub new_guard_data: String,
    #[prost(string, tag = "8")]
    pub agreement_session_url: String,
}

impl fmt::Debug for PollAuthSessionStatusResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PollAuthSessionStatusResponse")
            .field("new_client_id", &self.new_client_id)
            .field("had_remote_interaction", &self.had_remote_interaction)
            .field("account_name", &self.account_name)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, prost::Message)]
#[prost(skip_debug)]
pub(crate) struct GenerateAccessTokenForAppRequest {
    #[prost(string, tag = "1")]
    pub refresh_token: String,
    #[prost(fixed64, tag = "2")]
    pub steamid: u64,
    /// `ETokenRenewalType`: 0 keeps the refresh token, 1 may rotate it.
    #[prost(int32, tag = "3")]
    pub renewal_type: i32,
}

impl fmt::Debug for GenerateAccessTokenForAppRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GenerateAccessTokenForAppRequest")
            .field("steamid", &self.steamid)
            .field("renewal_type", &self.renewal_type)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, prost::Message)]
#[prost(skip_debug)]
pub(crate) struct GenerateAccessTokenForAppResponse {
    #[prost(string, tag = "1")]
    pub access_token: String,
    #[prost(string, tag = "2")]
    pub refresh_token: String,
}

impl fmt::Debug for GenerateAccessTokenForAppResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GenerateAccessTokenForAppResponse { <redacted> }")
    }
}

#[cfg(test)]
mod tests {
    //! Hand-encoded bytes pin the field numbers and wire types: a field with
    //! the wrong tag or type would decode to a default and fail here.

    use super::*;
    use prost::Message as _;

    /// Protobuf key byte for a field number below 16.
    fn key(field: u8, wire: u8) -> u8 {
        (field << 3) | wire
    }

    const VARINT: u8 = 0;
    const I64: u8 = 1;
    const LEN: u8 = 2;
    const I32: u8 = 5;

    fn len_field(field: u8, data: &[u8]) -> Vec<u8> {
        let mut out = vec![key(field, LEN), data.len() as u8];
        out.extend_from_slice(data);
        out
    }

    fn device_bytes() -> Vec<u8> {
        let mut d = len_field(1, b"pc");
        d.extend([key(2, VARINT), 1]);
        // os_type -203 as a ten-byte two's complement varint.
        d.push(key(3, VARINT));
        d.extend([0xb5, 0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]);
        d.extend([key(4, VARINT), 1]);
        d.extend(len_field(6, &[9, 8]));
        d
    }

    fn device() -> DeviceDetails {
        DeviceDetails {
            device_friendly_name: "pc".into(),
            platform_type: 1,
            os_type: -203,
            gaming_device_type: 1,
            client_count: 0,
            machine_id: vec![9, 8],
        }
    }

    #[test]
    fn device_details_and_qr_request() {
        assert_eq!(
            DeviceDetails::decode(&device_bytes()[..]).unwrap(),
            device()
        );
        assert_eq!(device().encode_to_vec(), device_bytes());

        let mut qr = len_field(1, b"pc");
        qr.extend([key(2, VARINT), 1]);
        qr.extend(len_field(3, &device_bytes()));
        qr.extend(len_field(4, b"Client"));
        let msg = BeginAuthSessionViaQrRequest {
            device_friendly_name: "pc".into(),
            platform_type: 1,
            device_details: Some(device()),
            website_id: "Client".into(),
        };
        assert_eq!(BeginAuthSessionViaQrRequest::decode(&qr[..]).unwrap(), msg);
        assert_eq!(msg.encode_to_vec(), qr);
    }

    #[test]
    fn qr_response() {
        let mut b = vec![key(1, VARINT), 42];
        b.extend(len_field(2, b"https://s.team/q/1/2"));
        b.extend(len_field(3, &[1, 2, 3]));
        b.push(key(4, I32));
        b.extend(5.0f32.to_le_bytes());
        let mut conf = vec![key(1, VARINT), 4];
        conf.extend(len_field(2, b"hi"));
        b.extend(len_field(5, &conf));
        b.extend([key(6, VARINT), 1]);
        let r = BeginAuthSessionViaQrResponse::decode(&b[..]).unwrap();
        assert_eq!(r.client_id, 42);
        assert_eq!(r.challenge_url, "https://s.team/q/1/2");
        assert_eq!(r.request_id, [1, 2, 3]);
        assert_eq!(r.interval, 5.0);
        assert_eq!(r.allowed_confirmations[0].confirmation_type, 4);
        assert_eq!(r.allowed_confirmations[0].associated_message, "hi");
        assert_eq!(r.version, 1);
        assert!(!format!("{r:?}").contains("s.team"));
    }

    #[test]
    fn rsa_key_messages() {
        let req = GetPasswordRsaPublicKeyRequest {
            account_name: "player".into(),
        };
        assert_eq!(req.encode_to_vec(), len_field(1, b"player"));
        let mut b = len_field(1, b"c0ffee");
        b.extend(len_field(2, b"010001"));
        b.extend([key(3, VARINT), 0x80, 0x01]);
        let r = GetPasswordRsaPublicKeyResponse::decode(&b[..]).unwrap();
        assert_eq!(
            (
                r.publickey_mod.as_str(),
                r.publickey_exp.as_str(),
                r.timestamp
            ),
            ("c0ffee", "010001", 128)
        );
    }

    #[test]
    fn credentials_request_and_response() {
        let req = BeginAuthSessionViaCredentialsRequest {
            device_friendly_name: "pc".into(),
            account_name: "player".into(),
            encrypted_password: "ZW5j".into(),
            encryption_timestamp: 7,
            remember_login: true,
            platform_type: 1,
            persistence: 1,
            website_id: "Client".into(),
            device_details: Some(device()),
            guard_data: "gd".into(),
            language: 0,
            qos_level: 0,
        };
        let mut b = len_field(1, b"pc");
        b.extend(len_field(2, b"player"));
        b.extend(len_field(3, b"ZW5j"));
        b.extend([key(4, VARINT), 7]);
        b.extend([key(5, VARINT), 1]);
        b.extend([key(6, VARINT), 1]);
        b.extend([key(7, VARINT), 1]);
        b.extend(len_field(8, b"Client"));
        b.extend(len_field(9, &device_bytes()));
        b.extend(len_field(10, b"gd"));
        assert_eq!(req.encode_to_vec(), b);
        assert_eq!(
            BeginAuthSessionViaCredentialsRequest::decode(&b[..]).unwrap(),
            req
        );
        let dbg = format!("{req:?}");
        assert!(!dbg.contains("ZW5j") && !dbg.contains("player") && !dbg.contains("gd"));

        let mut r = vec![key(1, VARINT), 5];
        r.extend(len_field(2, &[7, 7]));
        r.push(key(3, I32));
        r.extend(0.1f32.to_le_bytes());
        r.extend(len_field(4, &[key(1, VARINT), 2]));
        r.extend(len_field(4, &[key(1, VARINT), 3]));
        r.extend([key(5, VARINT), 0x01]);
        r.extend(len_field(6, b"eyJ.weak"));
        r.extend(len_field(8, b"more"));
        let r = BeginAuthSessionViaCredentialsResponse::decode(&r[..]).unwrap();
        assert_eq!((r.client_id, r.steamid), (5, 1));
        assert_eq!(r.request_id, [7, 7]);
        let kinds: Vec<_> = r
            .allowed_confirmations
            .iter()
            .map(|c| c.confirmation_type)
            .collect();
        assert_eq!(kinds, [2, 3]);
        assert_eq!(r.weak_token, "eyJ.weak");
        assert_eq!(r.extended_error_message, "more");
    }

    #[test]
    fn guard_code_poll_and_token_requests() {
        let req = UpdateAuthSessionWithSteamGuardCodeRequest {
            client_id: 3,
            steamid: 76561197960265729,
            code: "AB12C".into(),
            code_type: 3,
        };
        let mut b = vec![key(1, VARINT), 3, key(2, I64)];
        b.extend(76561197960265729u64.to_le_bytes());
        b.extend(len_field(3, b"AB12C"));
        b.extend([key(4, VARINT), 3]);
        assert_eq!(req.encode_to_vec(), b);
        assert!(!format!("{req:?}").contains("AB12C"));

        let poll = PollAuthSessionStatusRequest {
            client_id: 3,
            request_id: vec![1],
            token_to_revoke: 0,
        };
        let mut b = vec![key(1, VARINT), 3];
        b.extend(len_field(2, &[1]));
        assert_eq!(poll.encode_to_vec(), b);

        let mut r = vec![key(1, VARINT), 9];
        r.extend(len_field(2, b"https://s.team/q/new"));
        r.extend(len_field(3, b"refresh"));
        r.extend(len_field(4, b"access"));
        r.extend([key(5, VARINT), 1]);
        r.extend(len_field(6, b"player"));
        r.extend(len_field(7, b"guard"));
        let r = PollAuthSessionStatusResponse::decode(&r[..]).unwrap();
        assert_eq!(r.new_client_id, 9);
        assert_eq!(r.new_challenge_url, "https://s.team/q/new");
        assert_eq!(
            (r.refresh_token.as_str(), r.access_token.as_str()),
            ("refresh", "access")
        );
        assert!(r.had_remote_interaction);
        assert_eq!(
            (r.account_name.as_str(), r.new_guard_data.as_str()),
            ("player", "guard")
        );
        let dbg = format!("{r:?}");
        assert!(
            !dbg.contains("refresh\"") && !dbg.contains("access\"") && !dbg.contains("guard\"")
        );

        let request = GenerateAccessTokenForAppRequest {
            refresh_token: "rt".into(),
            steamid: 1,
            renewal_type: 0,
        };
        let mut b = len_field(1, b"rt");
        b.push(key(2, I64));
        b.extend(1u64.to_le_bytes());
        assert_eq!(request.encode_to_vec(), b);
        assert!(!format!("{request:?}").contains("rt\""));

        let mut r = len_field(1, b"at");
        r.extend(len_field(2, b"rt2"));
        let r = GenerateAccessTokenForAppResponse::decode(&r[..]).unwrap();
        assert_eq!(
            (r.access_token.as_str(), r.refresh_token.as_str()),
            ("at", "rt2")
        );
    }
}
