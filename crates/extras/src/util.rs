//! Crate-private helpers: atomic async writes, a TTL'd on-disk JSON cache,
//! HTTP shortcuts, percent-encoding and file-name sanitising.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Write `bytes` to `<path>.tmp`, then rename it over `path`. The parent
/// directory is created. The temporary file is removed on failure.
pub(crate) async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = tmp_path(path);
    if let Err(e) = tokio::fs::write(&tmp, bytes).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e).with_context(|| format!("writing {}", tmp.display()));
    }
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
}

pub(crate) fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[derive(Serialize, Deserialize)]
struct CacheEntry<T> {
    /// Unix seconds when the value was stored.
    saved_at: u64,
    value: T,
}

/// Read a cached value and its age. `None` when missing or unreadable.
pub(crate) async fn cache_read_entry<T: DeserializeOwned>(path: &Path) -> Option<(T, Duration)> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let entry: CacheEntry<T> = match serde_json::from_slice(&bytes) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!("discarding unreadable cache {}: {e}", path.display());
            return None;
        }
    };
    let age = irori_core::util::unix_now().saturating_sub(entry.saved_at);
    Some((entry.value, Duration::from_secs(age)))
}

/// Read a cached value that is younger than `ttl`.
pub(crate) async fn cache_read<T: DeserializeOwned>(path: &Path, ttl: Duration) -> Option<T> {
    let (value, age) = cache_read_entry(path).await?;
    (age <= ttl).then_some(value)
}

/// Store a value in the cache file at `path`, stamped with the current time.
pub(crate) async fn cache_write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let entry = CacheEntry {
        saved_at: irori_core::util::unix_now(),
        value,
    };
    write_atomic(path, &serde_json::to_vec(&entry)?).await
}

/// Directory for this crate's caches.
pub(crate) fn cache_dir(sub: &str) -> PathBuf {
    irori_core::Paths::get().cache.join(sub)
}

/// GET `url` and decode JSON, failing on non-2xx statuses.
pub(crate) async fn get_json<T: DeserializeOwned>(url: &str, timeout: Duration) -> Result<T> {
    let resp = irori_core::http::client()
        .get(url)
        .timeout(timeout)
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.json().await?)
}

/// GET `url` as text, failing on non-2xx statuses.
pub(crate) async fn get_text(url: &str, timeout: Duration) -> Result<String> {
    let resp = irori_core::http::client()
        .get(url)
        .timeout(timeout)
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.text().await?)
}

/// GET `url` as bytes, failing on non-2xx statuses.
pub(crate) async fn get_bytes(url: &str, timeout: Duration) -> Result<Vec<u8>> {
    let resp = irori_core::http::client()
        .get(url)
        .timeout(timeout)
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.bytes().await?.to_vec())
}

/// Load image bytes from an `http(s)` URL, a `file://` URL or a local path.
pub(crate) async fn load_image_source(source: &str) -> Result<Vec<u8>> {
    let source = source.trim();
    if source.starts_with("http://") || source.starts_with("https://") {
        let url = source.replace(' ', "%20").replacen("{ext}", "jpg", 1);
        return get_bytes(&url, Duration::from_secs(30))
            .await
            .with_context(|| format!("downloading {url}"));
    }
    if source.starts_with("data:") {
        bail!("inline images are not supported");
    }
    let path = match source.strip_prefix("file://") {
        Some(p) => PathBuf::from(decode_component(p).unwrap_or_else(|| p.to_string())),
        None => irori_core::paths::expand_home(source),
    };
    tokio::fs::read(&path)
        .await
        .with_context(|| format!("reading {}", path.display()))
}

/// `encodeURIComponent`: everything except `A-Z a-z 0-9 - _ . ! ~ * ' ( )`
/// is percent-encoded as UTF-8.
pub(crate) fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Strict percent-decoding (no `+` handling). `None` on a malformed escape or
/// a result that is not UTF-8.
pub(crate) fn decode_component(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Make a title usable as a file name: drops `/ ? < > \ : * | "` and control
/// characters, Windows-reserved names and trailing dots/spaces, and truncates
/// to 255 UTF-8 bytes. Returns an empty string when nothing is left.
pub(crate) fn sanitize_filename(name: &str) -> String {
    let mut s: String = name
        .chars()
        .filter(|c| !matches!(c, '/' | '?' | '<' | '>' | '\\' | ':' | '*' | '|' | '"'))
        .filter(|c| !c.is_control())
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if reserved || s == "." || s == ".." {
        return String::new();
    }
    if s.len() > 255 {
        let mut end = 255;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// Value of a non-empty environment variable.
pub(crate) fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_roundtrip() {
        assert_eq!(encode_component("a b/c&d"), "a%20b%2Fc%26d");
        assert_eq!(decode_component("a%20b%2Fc").as_deref(), Some("a b/c"));
        assert_eq!(decode_component("%E2%84%A2").as_deref(), Some("™"));
        assert_eq!(decode_component("bad%2"), None);
        assert_eq!(decode_component("bad%zz"), None);
    }

    #[test]
    fn filenames() {
        assert_eq!(sanitize_filename("Half-Life: Alyx?"), "Half-Life Alyx");
        assert_eq!(sanitize_filename("a/b\\c"), "abc");
        assert_eq!(sanitize_filename("Game. "), "Game");
        assert_eq!(sanitize_filename("CON"), "");
        assert_eq!(sanitize_filename("com1.txt"), "");
        assert_eq!(sanitize_filename("COM10"), "COM10");
        let long = "é".repeat(200);
        assert!(sanitize_filename(&long).len() <= 255);
    }

    #[tokio::test]
    async fn cache_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c/x.json");
        cache_write(&p, &vec![1u8, 2]).await.unwrap();
        let v: Vec<u8> = cache_read(&p, Duration::from_secs(60)).await.unwrap();
        assert_eq!(v, vec![1, 2]);
        let (_, age): (Vec<u8>, _) = cache_read_entry(&p).await.unwrap();
        assert!(age < Duration::from_secs(5));
    }
}
