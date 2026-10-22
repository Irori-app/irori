//! Log helpers: reading the tail of a log file, and sharing logs through
//! dpaste.com (the service Heroic uses), with a record of uploads.
//!
//! dpaste has no delete tokens, so an upload's "delete token" is its paste
//! URL. Deleting forgets the upload locally and asks dpaste to remove it
//! (dpaste may ignore that; pastes expire after 2 days anyway).

use std::collections::BTreeMap;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use ember_core::Paths;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio::sync::Mutex;

use crate::util;

/// Bytes shown by log viewers.
pub const DEFAULT_TAIL_BYTES: u64 = 1024 * 1024;
/// Upload size cap.
pub const MAX_UPLOAD_BYTES: u64 = 10 * 1024 * 1024;
/// Days dpaste keeps a paste.
pub const EXPIRY_DAYS: u64 = 2;
const DPASTE_URL: &str = "https://dpaste.com/api/v2/";

/// Decode bytes that may start in the middle of a UTF-8 sequence: up to 4
/// leading continuation bytes are skipped, invalid sequences replaced.
pub fn decode_tail(bytes: &[u8]) -> String {
    let skip = bytes
        .iter()
        .take(4)
        .take_while(|&&b| b & 0xC0 == 0x80)
        .count();
    String::from_utf8_lossy(&bytes[skip..]).into_owned()
}

/// The last `max_bytes` of a file as text. A missing file gives `""`.
pub async fn read_tail(path: &Path, max_bytes: u64) -> Result<String> {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    };
    let len = file.metadata().await?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start)).await?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.take(max_bytes).read_to_end(&mut buf).await?;
    Ok(decode_tail(&buf))
}

/// A log shared through dpaste.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadedLog {
    /// Public link to the paste.
    pub url: String,
    /// Pass to [`delete_uploaded`]. (dpaste has no tokens; this is the URL.)
    pub delete_token: String,
    /// Display name chosen by the caller ("Ember log", "Game log: X").
    pub name: String,
    /// Unix seconds.
    pub uploaded_at: u64,
}

impl UploadedLog {
    pub fn is_expired(&self, now: u64) -> bool {
        now.saturating_sub(self.uploaded_at) >= EXPIRY_DAYS * 24 * 3600
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct UploadStore {
    uploads: BTreeMap<String, UploadedLog>,
}

static STORE_LOCK: Mutex<()> = Mutex::const_new(());

fn store_file() -> PathBuf {
    Paths::get().state_dir().join("uploaded_logs.json")
}

async fn read_store() -> UploadStore {
    match tokio::fs::read(store_file()).await {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_default(),
        Err(_) => UploadStore::default(),
    }
}

async fn write_store(s: &UploadStore) -> Result<()> {
    util::write_atomic(&store_file(), &serde_json::to_vec_pretty(s)?).await
}

/// `application/x-www-form-urlencoded` body for dpaste.
fn form_body(content: &str) -> String {
    format!(
        "content={}&expiry_days={EXPIRY_DAYS}",
        util::encode_component(content)
    )
}

/// Upload the first 10 MiB of a log file to dpaste and record it.
pub async fn upload(path: &Path, name: &str) -> Result<UploadedLog> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("The log file {} does not exist.", path.display()))?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_UPLOAD_BYTES)
        .read_to_end(&mut bytes)
        .await?;
    let content = String::from_utf8_lossy(&bytes);
    let resp = ember_core::http::client()
        .post(DPASTE_URL)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(form_body(&content))
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .context("Failed to send the log to dpaste.com")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let url = text.trim();
    if !status.is_success() || url::Url::parse(url).is_err() {
        bail!(
            "dpaste.com rejected the upload (HTTP {status}): {}",
            url.chars().take(200).collect::<String>()
        );
    }
    let entry = UploadedLog {
        url: url.to_string(),
        delete_token: url.to_string(),
        name: name.to_string(),
        uploaded_at: ember_core::util::unix_now(),
    };
    let _guard = STORE_LOCK.lock().await;
    let mut store = read_store().await;
    store.uploads.insert(entry.url.clone(), entry.clone());
    write_store(&store).await?;
    tracing::info!("Uploaded log file {name} to {}", entry.url);
    Ok(entry)
}

/// Forget an upload and ask dpaste to delete it. Returns `false` for an
/// unknown token.
pub async fn delete_uploaded(token: &str) -> Result<bool> {
    let _guard = STORE_LOCK.lock().await;
    let mut store = read_store().await;
    let Some(entry) = store
        .uploads
        .values()
        .find(|e| e.delete_token == token || e.url == token)
        .cloned()
    else {
        return Ok(false);
    };
    let remote = ember_core::http::client()
        .delete(&entry.url)
        .timeout(Duration::from_secs(15))
        .send()
        .await;
    if let Err(e) = remote.and_then(|r| r.error_for_status()) {
        tracing::debug!("dpaste did not delete {}: {e}", entry.url);
    }
    store.uploads.remove(&entry.url);
    write_store(&store).await?;
    Ok(true)
}

/// Uploads younger than 2 days, newest first. Expired records are dropped.
pub async fn list_uploaded() -> Result<Vec<UploadedLog>> {
    let _guard = STORE_LOCK.lock().await;
    let mut store = read_store().await;
    let now = ember_core::util::unix_now();
    let before = store.uploads.len();
    store.uploads.retain(|_, e| !e.is_expired(now));
    if store.uploads.len() != before {
        write_store(&store).await?;
    }
    let mut list: Vec<_> = store.uploads.into_values().collect();
    list.sort_by_key(|e| std::cmp::Reverse(e.uploaded_at));
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_mid_sequence() {
        let s = "aé€";
        let bytes = s.as_bytes();
        assert_eq!(decode_tail(&bytes[2..]), "€");
        assert_eq!(decode_tail(&bytes[4..]), "");
        assert_eq!(decode_tail(b"plain"), "plain");
    }

    #[tokio::test]
    async fn tail_of_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.log");
        assert_eq!(read_tail(&p, 10).await.unwrap(), "");
        std::fs::write(&p, "line1\nline2\nline3\n").unwrap();
        assert_eq!(read_tail(&p, 6).await.unwrap(), "line3\n");
        assert_eq!(read_tail(&p, 1000).await.unwrap(), "line1\nline2\nline3\n");
    }

    #[test]
    fn upload_body_and_expiry() {
        assert_eq!(form_body("a b&c"), "content=a%20b%26c&expiry_days=2");
        let e = UploadedLog {
            url: "u".into(),
            delete_token: "u".into(),
            name: "n".into(),
            uploaded_at: 1000,
        };
        assert!(!e.is_expired(1000 + 3600));
        assert!(e.is_expired(1000 + 2 * 24 * 3600));
    }
}
