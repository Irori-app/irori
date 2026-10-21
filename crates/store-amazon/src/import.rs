//! Converting nile's state (as Heroic keeps it) into Ember's files.
//!
//! nile stores, in its config dir: `current_user.json` (`user_id`, `name`),
//! the device registration in `<md5(user_id)>.enc` (AES-256-CBC, key
//! `sha256(user_id)`, IV encrypted with AES-ECB in the first block) or in a
//! plain `user.json` (older versions), `library.json` (raw entitlements),
//! `installed.json` (`[{id, version, path, size}]`) and the installed builds'
//! manifests in `manifests/<id>.raw`.

use std::path::Path;

use aes::cipher::{BlockDecrypt as _, BlockDecryptMut as _, KeyInit as _, KeyIvInit as _};
use anyhow::{Context as _, Result, bail};
use ember_amazon_api::{Credentials, Entitlement};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::state::{InstalledGame, Layout};

#[derive(Debug, Default, Deserialize)]
struct NileUser {
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct NileInstalled {
    id: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    size: Option<u64>,
}

/// What was carried over.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub login: bool,
    pub installs: usize,
}

/// Decrypt one of nile's `.enc` files.
fn decrypt(bytes: &[u8], user_id: &str) -> Result<Vec<u8>> {
    if bytes.len() < 32 || !bytes.len().is_multiple_of(16) {
        bail!("the encrypted login is truncated");
    }
    let key = Sha256::digest(user_id.as_bytes());
    let mut iv = aes::Block::clone_from_slice(&bytes[..16]);
    aes::Aes256::new(&key).decrypt_block(&mut iv);
    cbc::Decryptor::<aes::Aes256>::new(&key, &iv)
        .decrypt_padded_vec_mut::<aes::cipher::block_padding::Pkcs7>(&bytes[16..])
        .map_err(|_| anyhow::anyhow!("the encrypted login does not belong to this account"))
}

/// Ember credentials from nile's device registration record.
fn credentials(record: &Value, fallback_name: &str) -> Option<Credentials> {
    let s = |path: &str| {
        record
            .pointer(path)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let num = |path: &str| {
        let v = record.pointer(path)?;
        v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())
    };
    let obtained = num("/NILE/token_obtain_time").unwrap_or(0.0);
    let expires_in = num("/tokens/bearer/expires_in").unwrap_or(0.0);
    Some(Credentials {
        access_token: s("/tokens/bearer/access_token").unwrap_or_default(),
        refresh_token: s("/tokens/bearer/refresh_token")?,
        expires_at: (obtained + expires_in).max(0.0) as u64,
        device_serial: s("/extensions/device_info/device_serial_number")?,
        user_id: s("/extensions/customer_info/user_id")?,
        display_name: s("/extensions/customer_info/given_name")
            .unwrap_or_else(|| fallback_name.to_string()),
    })
}

fn read_login(nile_dir: &Path) -> Result<Option<Credentials>> {
    let user: NileUser =
        ember_core::json_store::read(&nile_dir.join("current_user.json"))?.unwrap_or_default();
    let record: Option<Value> = if !user.user_id.is_empty() {
        let name = format!("{:x}.enc", md5::compute(user.user_id.as_bytes()));
        match std::fs::read(nile_dir.join(name)) {
            Ok(bytes) => Some(serde_json::from_slice(&decrypt(&bytes, &user.user_id)?)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).context("reading nile's login"),
        }
    } else {
        None
    };
    let record = match record {
        Some(r) => Some(r),
        None => ember_core::json_store::read::<Value>(&nile_dir.join("user.json"))?,
    };
    Ok(record.and_then(|r| credentials(&r, &user.name)))
}

/// Convert nile's state in `nile_dir` into Ember's files in `layout`.
///
/// * The login and owned-games list are taken only when Ember has no login,
///   so an Ember login is never replaced.
/// * Install records are merged (Ember's own entries win), with the
///   installed builds' manifests.
pub fn import_nile_dir(nile_dir: &Path, layout: &Layout) -> Result<Imported> {
    let mut imported = Imported::default();
    if !nile_dir.is_dir() {
        return Ok(imported);
    }
    if layout.read_credentials().is_none()
        && let Some(creds) = read_login(nile_dir)?
    {
        layout.write_credentials(&creds)?;
        imported.login = true;
        if let Some(list) =
            ember_core::json_store::read::<Vec<Entitlement>>(&nile_dir.join("library.json"))
                .ok()
                .flatten()
        {
            layout.write_entitlements(&ember_amazon_api::library::dedupe(list))?;
        }
    }

    let nile_installed: Vec<NileInstalled> =
        ember_core::json_store::read(&nile_dir.join("installed.json"))?.unwrap_or_default();
    if !nile_installed.is_empty() {
        let mut installed = layout.read_installed();
        for game in nile_installed {
            if game.id.is_empty() || game.path.is_empty() || installed.contains_key(&game.id) {
                continue;
            }
            let manifest = nile_dir.join("manifests").join(format!("{}.raw", game.id));
            if let Ok(bytes) = std::fs::read(&manifest) {
                layout.write_manifest(&game.id, &bytes)?;
            }
            installed.insert(
                game.id,
                InstalledGame {
                    version: game.version,
                    path: game.path.into(),
                    size: game.size.unwrap_or(0),
                },
            );
            imported.installs += 1;
        }
        if imported.installs > 0 {
            layout.write_installed(&installed)?;
        }
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use aes::cipher::{BlockEncrypt as _, BlockEncryptMut as _};

    use super::*;

    /// Encrypt like nile's `Config.write(..., JSONENC)`.
    fn encrypt(plain: &[u8], user_id: &str) -> Vec<u8> {
        let key = Sha256::digest(user_id.as_bytes());
        let iv = aes::Block::from([9u8; 16]);
        let body = cbc::Encryptor::<aes::Aes256>::new(&key, &iv)
            .encrypt_padded_vec_mut::<aes::cipher::block_padding::Pkcs7>(plain);
        let mut enc_iv = iv;
        aes::Aes256::new(&key).encrypt_block(&mut enc_iv);
        [enc_iv.as_slice(), &body].concat()
    }

    fn record() -> Value {
        serde_json::json!({
            "tokens": {"bearer": {"access_token": "Atna|a", "refresh_token": "Atnr|r", "expires_in": "3600"}},
            "extensions": {
                "device_info": {"device_serial_number": "SERIAL"},
                "customer_info": {"given_name": "Radu", "user_id": "amzn1.account.X"}},
            "NILE": {"token_obtain_time": 1000.5}
        })
    }

    #[test]
    fn imports_encrypted_login_installs_and_manifests() {
        let tmp = tempfile::tempdir().unwrap();
        let nile = tmp.path().join("nile");
        std::fs::create_dir_all(nile.join("manifests")).unwrap();
        let uid = "amzn1.account.X";
        std::fs::write(
            nile.join("current_user.json"),
            br#"{"name": "Radu", "user_id": "amzn1.account.X"}"#,
        )
        .unwrap();
        std::fs::write(
            nile.join(format!("{:x}.enc", md5::compute(uid))),
            encrypt(record().to_string().as_bytes(), uid),
        )
        .unwrap();
        std::fs::write(
            nile.join("library.json"),
            br#"[{"id":"e1","product":{"id":"a","title":"A","sku":"S"}}]"#,
        )
        .unwrap();
        std::fs::write(
            nile.join("installed.json"),
            br#"[{"id":"a","version":"1","path":"/h/A"},{"id":"b","version":"2","path":"/h/B","size":10}]"#,
        )
        .unwrap();
        std::fs::write(nile.join("manifests/b.raw"), b"manifest").unwrap();

        let layout = Layout::new(tmp.path().join("ember"));
        let mut existing = crate::state::Installed::new();
        existing.insert(
            "a".into(),
            InstalledGame {
                version: "9".into(),
                path: "/e/A".into(),
                size: 0,
            },
        );
        layout.write_installed(&existing).unwrap();

        let imported = import_nile_dir(&nile, &layout).unwrap();
        assert_eq!(
            imported,
            Imported {
                login: true,
                installs: 1
            }
        );
        let creds = layout.read_credentials().unwrap();
        assert_eq!(creds.refresh_token, "Atnr|r");
        assert_eq!(creds.device_serial, "SERIAL");
        assert_eq!(creds.display_name, "Radu");
        assert_eq!(creds.expires_at, 4600);
        assert_eq!(
            layout.read_entitlements()[0].product.sku.as_deref(),
            Some("S")
        );
        let installed = layout.read_installed();
        assert_eq!(installed["a"].path, Path::new("/e/A"));
        assert_eq!(installed["b"].size, 10);
        assert_eq!(layout.read_manifest("b").unwrap(), b"manifest");

        // A second import keeps Ember's login.
        let again = import_nile_dir(&nile, &layout).unwrap();
        assert_eq!(again, Imported::default());
        assert!(
            import_nile_dir(&tmp.path().join("missing"), &layout).unwrap() == Imported::default()
        );
    }

    #[test]
    fn imports_legacy_plain_login_and_rejects_wrong_key() {
        let tmp = tempfile::tempdir().unwrap();
        let nile = tmp.path().join("nile");
        std::fs::create_dir_all(&nile).unwrap();
        std::fs::write(nile.join("user.json"), record().to_string()).unwrap();
        let layout = Layout::new(tmp.path().join("ember"));
        assert!(import_nile_dir(&nile, &layout).unwrap().login);
        assert_eq!(
            layout.read_credentials().unwrap().user_id,
            "amzn1.account.X"
        );

        let enc = encrypt(b"{}", "someone");
        assert!(decrypt(&enc, "other").is_err());
        assert_eq!(decrypt(&enc, "someone").unwrap(), b"{}");
        assert!(decrypt(&[0; 8], "x").is_err());
    }
}
