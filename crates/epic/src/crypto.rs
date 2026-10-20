//! AES-GCM used by encrypted manifests and chunks. Keys come from the
//! manifest API's `secrets` (hex, 128 or 256 bit) keyed by the upper-case
//! hex of the secret GUID.

use aes_gcm::aead::{Aead as _, KeyInit as _, Payload};
use aes_gcm::{Aes128Gcm, Aes256Gcm, Nonce};
use anyhow::{Result, anyhow, bail};

/// Decrypt `ciphertext` with a detached 16-byte `tag`.
pub(crate) fn decrypt(key: &[u8], nonce: &[u8], ciphertext: &[u8], tag: &[u8]) -> Result<Vec<u8>> {
    if nonce.len() != 12 {
        bail!("unsupported nonce length {}", nonce.len());
    }
    let mut sealed = Vec::with_capacity(ciphertext.len() + tag.len());
    sealed.extend_from_slice(ciphertext);
    sealed.extend_from_slice(tag);
    let payload = Payload {
        msg: &sealed,
        aad: &[],
    };
    let nonce = Nonce::from_slice(nonce);
    let out = match key.len() {
        16 => Aes128Gcm::new_from_slice(key)
            .map_err(|_| anyhow!("invalid key"))?
            .decrypt(nonce, payload),
        32 => Aes256Gcm::new_from_slice(key)
            .map_err(|_| anyhow!("invalid key"))?
            .decrypt(nonce, payload),
        n => bail!("unsupported key length {n}"),
    };
    out.map_err(|_| anyhow!("decryption failed (wrong key or damaged data)"))
}

/// Encrypt, returning the ciphertext and the detached tag (tests and
/// fixtures).
#[cfg(test)]
pub(crate) fn encrypt(key: &[u8], nonce: &[u8], plain: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let cipher = Aes256Gcm::new_from_slice(key).unwrap();
    let mut sealed = cipher.encrypt(Nonce::from_slice(nonce), plain).unwrap();
    let tag = sealed.split_off(sealed.len() - 16);
    (sealed, tag)
}

/// Secret key for a secret GUID from the manifest API's `secrets` map.
pub(crate) fn find_key(
    secrets: &std::collections::HashMap<String, String>,
    guid_hex: &str,
) -> Result<Vec<u8>> {
    let hex_key = secrets
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(guid_hex))
        .map(|(_, v)| v)
        .ok_or_else(|| anyhow!("the decryption key for {guid_hex} is missing"))?;
    hex::decode(hex_key.trim()).map_err(|_| anyhow!("invalid decryption key for {guid_hex}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let key = [3u8; 32];
        let nonce = [1u8; 12];
        let (ct, tag) = encrypt(&key, &nonce, b"secret data");
        assert_eq!(decrypt(&key, &nonce, &ct, &tag).unwrap(), b"secret data");
        assert!(decrypt(&[4u8; 32], &nonce, &ct, &tag).is_err());
        assert!(decrypt(&key, &[0u8; 8], &ct, &tag).is_err());
        let secrets = [("ABCD".to_string(), hex::encode(key))].into();
        assert_eq!(find_key(&secrets, "abcd").unwrap(), key);
        assert!(find_key(&secrets, "ffff").is_err());
    }
}
