//! [`SteamError`]: the refusals callers act on, from Steam's `EResult` codes
//! and HTTP statuses.

/// Steam refused a request. Callers match on the variant; the message is
/// worded for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SteamError {
    /// `EResult::InvalidPassword` (5): the account name or password is wrong.
    #[error("The Steam account name or password is wrong.")]
    InvalidPassword,
    /// `EResult::RateLimitExceeded` (84) or HTTP 429.
    #[error(
        "Steam is refusing requests for now because there were too many. Wait a while and try again."
    )]
    RateLimited,
    /// `EResult::InvalidLoginAuthCode` (65) or `TwoFactorCodeMismatch` (88).
    #[error("The Steam Guard code is wrong or has expired. Check it and try again.")]
    InvalidCode,
    /// The saved sign-in no longer works: `EResult::AccessDenied` (15),
    /// `Expired` (27), or HTTP 401/403. Signing in again fixes it; retrying
    /// does not.
    #[error("Steam no longer accepts the saved sign-in. Sign in to Steam again.")]
    SessionRejected,
    /// The sign-in attempt being polled ran out or was declined
    /// (`EResult::FileNotFound` (9) or `Expired` (27) while polling).
    #[error("The Steam sign-in attempt has expired or was declined. Start signing in again.")]
    LoginExpired,
    /// Any other `EResult` code.
    #[error("Steam refused the request (error code {0}).")]
    Other(i32),
}

impl SteamError {
    /// The error for an `EResult` other than OK (1).
    pub fn from_eresult(code: i32) -> Self {
        match code {
            5 => Self::InvalidPassword,
            84 => Self::RateLimited,
            65 | 88 => Self::InvalidCode,
            15 | 27 => Self::SessionRejected,
            other => Self::Other(other),
        }
    }

    /// The error for an HTTP status that carries a meaning of its own.
    pub fn from_http_status(status: u16) -> Option<Self> {
        match status {
            401 | 403 => Some(Self::SessionRejected),
            429 => Some(Self::RateLimited),
            _ => None,
        }
    }

    /// The `EResult` code this error came from, where there is one code.
    pub fn eresult(&self) -> Option<i32> {
        match self {
            Self::InvalidPassword => Some(5),
            Self::RateLimited => Some(84),
            Self::Other(code) => Some(*code),
            Self::InvalidCode | Self::SessionRejected | Self::LoginExpired => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_eresult_codes() {
        assert_eq!(SteamError::from_eresult(5), SteamError::InvalidPassword);
        assert_eq!(SteamError::from_eresult(84), SteamError::RateLimited);
        assert_eq!(SteamError::from_eresult(65), SteamError::InvalidCode);
        assert_eq!(SteamError::from_eresult(88), SteamError::InvalidCode);
        assert_eq!(SteamError::from_eresult(15), SteamError::SessionRejected);
        assert_eq!(SteamError::from_eresult(27), SteamError::SessionRejected);
        assert_eq!(SteamError::from_eresult(2), SteamError::Other(2));
        assert!(SteamError::from_eresult(42).to_string().contains("42"));
        assert_eq!(SteamError::Other(42).eresult(), Some(42));

        assert_eq!(
            SteamError::from_http_status(401),
            Some(SteamError::SessionRejected)
        );
        assert_eq!(
            SteamError::from_http_status(403),
            Some(SteamError::SessionRejected)
        );
        assert_eq!(
            SteamError::from_http_status(429),
            Some(SteamError::RateLimited)
        );
        assert_eq!(SteamError::from_http_status(500), None);
    }
}
