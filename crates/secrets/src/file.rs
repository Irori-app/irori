//! Reading and writing one secret file: an encrypted envelope, or plain JSON
//! when there is no keyring.

use std::io::Write as _;
use std::path::Path;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use aes_gcm::aead::{Aead as _, AeadCore as _, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::vault::Vault;

const VERSION: u32 = 1;

/// What an encrypted file holds on disk. The name of the first field is what
/// tells it from a plain login.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sealed {
    irori_sealed: u32,
    nonce: String,
    data: String,
}

impl Sealed {
    fn seal(cipher: &Aes256Gcm, plain: &[u8]) -> Result<Self> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let data = cipher
            .encrypt(&nonce, plain)
            .map_err(|_| anyhow!("Could not encrypt the saved login"))?;
        Ok(Self {
            irori_sealed: VERSION,
            nonce: STANDARD.encode(nonce),
            data: STANDARD.encode(data),
        })
    }

    fn open(&self, cipher: &Aes256Gcm) -> Result<Vec<u8>> {
        if self.irori_sealed != VERSION {
            bail!("The saved login was written by a newer version of Irori");
        }
        let nonce = STANDARD.decode(&self.nonce).ok();
        let data = STANDARD.decode(&self.data).ok();
        let (Some(nonce), Some(data)) = (nonce, data) else {
            bail!("The saved login is damaged");
        };
        if nonce.len() != 12 {
            bail!("The saved login is damaged");
        }
        cipher
            .decrypt(Nonce::from_slice(&nonce), data.as_slice())
            .map_err(|_| anyhow!("The saved login does not match the key in the system keyring"))
    }
}

pub(crate) fn read<T: DeserializeOwned>(
    vault: Option<&Vault>,
    path: &Path,
    unreadable: &AtomicBool,
) -> Result<Option<T>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let cipher = vault.and_then(Vault::cipher);
    if let Ok(sealed) = serde_json::from_slice::<Sealed>(&bytes) {
        let plain = cipher
            .as_ref()
            .context("The system keyring is locked or unavailable")
            .and_then(|cipher| sealed.open(cipher))
            .inspect_err(|_| unreadable.store(true, Ordering::Relaxed))
            .with_context(|| format!("opening {}", path.display()))?;
        return serde_json::from_slice(&plain)
            .map(Some)
            .with_context(|| format!("invalid JSON in {}", path.display()));
    }
    let value = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid JSON in {}", path.display()))?;
    match cipher {
        Some(cipher) => {
            let moved = serde_json::from_slice::<serde_json::Value>(&bytes)
                .context("reading the saved login")
                .and_then(|plain| write_with(Some(&cipher), path, &plain));
            if let Err(e) = moved {
                tracing::warn!("encrypting {}: {e:#}", path.display());
            }
        }
        // Earlier versions could leave these files readable by everyone.
        None => restrict_to_user(path),
    }
    Ok(Some(value))
}

pub(crate) fn write<T: Serialize + ?Sized>(
    vault: Option<&Vault>,
    path: &Path,
    value: &T,
) -> Result<()> {
    let cipher = vault.and_then(Vault::cipher);
    if vault.is_some() && cipher.is_none() {
        static WARNED: Once = Once::new();
        WARNED.call_once(|| {
            tracing::warn!("no system keyring: saving logins in files only this user can read");
        });
    }
    write_with(cipher.as_ref(), path, value)
}

fn write_with<T: Serialize + ?Sized>(
    cipher: Option<&Aes256Gcm>,
    path: &Path,
    value: &T,
) -> Result<()> {
    let plain = serde_json::to_vec_pretty(value)?;
    let bytes = match cipher {
        Some(cipher) => serde_json::to_vec_pretty(&Sealed::seal(cipher, &plain)?)?,
        None => plain,
    };
    write_private(path, &bytes)
}

/// Write `bytes` atomically, readable only by the user where the platform
/// supports it. The file has those permissions from its first byte, and a
/// fresh name each time so a stale temp file can't pass on looser ones.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().context("invalid credentials path")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path
        .file_name()
        .context("invalid credentials path")?
        .to_string_lossy();
    let tmp = dir.join(format!(
        "{name}.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Only the user may read the file (Unix; Windows profiles are already
/// private to the user).
fn restrict_to_user(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let open = std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o077 != 0);
        if open
            && let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        {
            tracing::warn!("restricting {}: {e}", path.display());
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}
