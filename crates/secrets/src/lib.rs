//! Files that hold secrets: store logins and private branch passwords.
//!
//! Each file is encrypted with one key kept in the system keyring (Secret
//! Service on Linux, Keychain on macOS, Credential Manager on Windows). The
//! keyring holds a key rather than the logins because Windows limits an
//! entry to about 2.5 KB, which Epic's session and GOG's game tokens exceed.
//!
//! Where there is no keyring the logins stay usable: files are then plain
//! JSON that only the user can read. A plain file is encrypted the next time
//! it is read with a keyring available, which is also how logins saved by
//! earlier versions move over.
//!
//! The keyring is off until [`use_os_keyring`] turns it on. Only the Irori
//! program does, so tests never reach the user's real keyring.

#![forbid(unsafe_code)]

mod file;
mod os_keyring;
mod vault;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::vault::Vault;

/// How long a read or write waits for the keyring. It can be showing the
/// user an unlock prompt, and logins read at start hold the first render.
const KEYRING_WAIT: Duration = Duration::from_secs(30);

static VAULT: OnceLock<Vault> = OnceLock::new();
static UNREADABLE: AtomicBool = AtomicBool::new(false);

/// Encrypt secrets with a key from the system keyring from now on. Nothing
/// touches the keyring until the first secret is read or written.
pub fn use_os_keyring() {
    let _ = VAULT.set(Vault::new(Box::new(os_keyring::OsKeyring), KEYRING_WAIT));
}

/// Read and decode a secret file; `Ok(None)` when it does not exist.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    file::read(VAULT.get(), path, &UNREADABLE)
}

/// [`read_json`] for callers that treat an unreadable file as a missing one.
/// The failure is logged.
pub fn load<T: DeserializeOwned>(path: &Path) -> Option<T> {
    read_json(path)
        .map_err(|e| tracing::warn!("{e:#}"))
        .ok()
        .flatten()
}

/// Encode and atomically replace a secret file, readable by the user only.
pub fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    file::write(VAULT.get(), path, value)
}

/// Whether a saved secret could not be opened in this run, so the user looks
/// logged out: the keyring is locked or gone, or its key has changed.
pub fn unreadable_logins() -> bool {
    UNREADABLE.load(Ordering::Relaxed)
}
