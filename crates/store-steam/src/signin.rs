//! Signing in to Steam in Irori's own dialog: with a QR code the Steam
//! mobile app scans, or with the account name, password and Steam Guard.
//! Irori never keeps the password: Steam answers with tokens, which the
//! dialog passes to [`irori_core::Store::login`] through [`login_input`].

use anyhow::Result;
use irori_core::{Paths, Runner};
use irori_steam_api::{Client, DeviceInfo};

pub use crate::account::login_input;
pub use irori_steam_api::{
    Confirmation, CredentialsSession, GuardKind, Poll, QrSession, SteamError, Tokens,
};

use crate::account::{self, AccountFiles};

/// One sign-in attempt's connection to Steam, as this computer.
#[derive(Clone)]
pub struct SignIn {
    client: Client,
    device: DeviceInfo,
    /// Steam Guard's remembered approval of this device, from an earlier
    /// sign-in.
    guard_data: Option<String>,
    remembered_account: Option<String>,
}

impl SignIn {
    /// Blocking: reads (and on first use writes) this device's identity.
    pub fn new() -> Result<Self> {
        let paths = Paths::get();
        let files = AccountFiles::new(
            &paths.store_config_dir(Runner::Steam),
            &paths.store_cache_dir(Runner::Steam),
        );
        let saved = files.read_session();
        Ok(Self {
            client: Client::new()?,
            device: account::device(&files),
            guard_data: saved.as_ref().and_then(|s| s.guard_data.clone()),
            remembered_account: saved.map(|s| s.account_name),
        })
    }

    /// Start a QR sign-in; show [`QrSession::challenge_url`] as a QR code.
    pub async fn begin_qr(&self) -> Result<QrSession> {
        self.client.begin_qr(&self.device).await
    }

    /// Start a sign-in with the account name and password. The answer says
    /// which Steam Guard confirmations Steam accepts.
    pub async fn begin_password(
        &self,
        account_name: &str,
        password: &str,
    ) -> Result<CredentialsSession> {
        let same_account = self
            .remembered_account
            .as_deref()
            .is_some_and(|a| a.eq_ignore_ascii_case(account_name));
        let guard = self.guard_data.as_deref().filter(|_| same_account);
        self.client
            .begin_credentials(account_name, password, &self.device, guard)
            .await
    }

    /// Send the Steam Guard code from the email or the mobile app.
    pub async fn submit_code(
        &self,
        session: &CredentialsSession,
        code: &str,
        kind: GuardKind,
    ) -> Result<()> {
        self.client
            .submit_guard_code(session.client_id, session.steamid, code, kind)
            .await
    }

    /// Ask whether the sign-in finished. Call it every
    /// [`QrSession::interval`] (or the credentials session's).
    pub async fn poll(&self, client_id: u64, request_id: &[u8]) -> Result<Poll> {
        self.client.poll(client_id, request_id).await
    }
}
