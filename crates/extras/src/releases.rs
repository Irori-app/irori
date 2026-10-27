//! Heroic's release-info feed, which publishes MD5 hashes of the current
//! AreWeAntiCheatYet and known-fixes data so unchanged files are not
//! downloaded again.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;

use crate::util;

/// URL of Heroic's release-info JSON.
pub const RELEASE_INFO_URL: &str = "https://raw.githubusercontent.com/Heroic-Games-Launcher/releases-info/refs/heads/main/release-data.json";

/// The parts of the release info Irori uses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReleasesInfo {
    pub anticheat_files: AnticheatHashes,
    /// MD5 (hex) of `known_fixes.json`.
    pub known_fixes_sha: String,
}

/// MD5 (hex) hashes of the anticheat data files.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AnticheatHashes {
    pub sha_linux: String,
    pub sha_mac: String,
}

/// Fetch the release info.
pub async fn fetch() -> Result<ReleasesInfo> {
    util::get_json(RELEASE_INFO_URL, Duration::from_secs(10)).await
}

/// Hex MD5 of a file, or `None` when it cannot be read.
pub(crate) async fn file_md5(path: &Path) -> Option<String> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let digest = tokio::task::spawn_blocking(move || md5::compute(&bytes))
        .await
        .ok()?;
    Some(format!("{digest:x}"))
}

/// Whether a cached file must be downloaded: it is missing, or `latest_md5`
/// is known and differs from the file's hash.
pub(crate) async fn needs_download(path: &Path, latest_md5: Option<&str>) -> bool {
    let Some(local) = file_md5(path).await else {
        return true;
    };
    match latest_md5.filter(|h| !h.is_empty()) {
        Some(latest) => !latest.eq_ignore_ascii_case(&local),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_info() {
        let json = r#"{"ge-proton":{"tag":"x","published_at":"y"},
            "anticheatFiles":{"shaMac":"aa","shaLinux":"bb"},"knownFixesSha":"cc"}"#;
        let info: ReleasesInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.anticheat_files.sha_linux, "bb");
        assert_eq!(info.known_fixes_sha, "cc");
    }

    #[tokio::test]
    async fn md5_checks() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.json");
        assert!(needs_download(&p, None).await);
        std::fs::write(&p, b"hello").unwrap();
        assert_eq!(
            file_md5(&p).await.as_deref(),
            Some("5d41402abc4b2a76b9719d911017c592")
        );
        assert!(!needs_download(&p, None).await);
        assert!(!needs_download(&p, Some("5D41402ABC4B2A76B9719D911017C592")).await);
        assert!(needs_download(&p, Some("00")).await);
    }
}
