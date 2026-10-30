//! The keyring of the operating system. Its store is opened per call, so a
//! Secret Service that starts after Irori is found, and only ever on the
//! vault's own thread.

use anyhow::{Context as _, Result};
use keyring_core::Error;
use keyring_core::api::CredentialStoreApi as _;

use crate::vault::Keyring;

const SERVICE: &str = "irori";
const USER: &str = "credentials-key";

pub(crate) struct OsKeyring;

impl Keyring for OsKeyring {
    fn get(&self) -> Result<Option<String>> {
        match entry()?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(Error::NoEntry | Error::BadEncoding(_) | Error::BadDataFormat(..)) => Ok(None),
            Err(e) => Err(e).context("reading the key"),
        }
    }

    fn set(&self, key: &str) -> Result<()> {
        entry()?.set_password(key).context("saving the key")
    }
}

#[cfg(target_os = "linux")]
fn entry() -> Result<keyring_core::Entry> {
    let store = zbus_secret_service_keyring_store::Store::new().context("no Secret Service")?;
    Ok(store.build(SERVICE, USER, None)?)
}

#[cfg(target_os = "macos")]
fn entry() -> Result<keyring_core::Entry> {
    let store = apple_native_keyring_store::keychain::Store::new().context("no Keychain")?;
    Ok(store.build(SERVICE, USER, None)?)
}

#[cfg(windows)]
fn entry() -> Result<keyring_core::Entry> {
    let store = windows_native_keyring_store::Store::new().context("no Credential Manager")?;
    Ok(store.build(SERVICE, USER, None)?)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn entry() -> Result<keyring_core::Entry> {
    anyhow::bail!("no keyring on this platform")
}
