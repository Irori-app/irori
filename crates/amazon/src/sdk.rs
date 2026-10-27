//! The Amazon Games SDK (Fuel), which games load at runtime from the folders
//! named by `FUEL_DIR` and `AMAZON_GAMES_SDK_PATH`. Its files ship inside
//! the Amazon Games app build.

use std::path::{Path, PathBuf};

use anyhow::Result;
use irori_depot::FileEntry;

use crate::api::{Client, GameDownload};
use crate::manifest::{Manifest, ManifestFile};

/// Whether a file of the Amazon Games app belongs to the SDK.
pub fn is_sdk_file(path: &str) -> bool {
    path.contains("FuelSDK_x64.dll") || path.contains("AmazonGamesSDK_")
}

/// The SDK files of the current app build.
pub async fn sdk_files(client: &Client) -> Result<(GameDownload, Vec<FileEntry>)> {
    let download = client.launcher_download().await?;
    let fetched = client.manifest(&download).await?;
    let files: Vec<&ManifestFile> = fetched
        .manifest
        .files
        .iter()
        .filter(|f| is_sdk_file(&f.path))
        .collect();
    let entries = Manifest::entries(files, &download.download_url)?;
    Ok((download, entries))
}

/// `FUEL_DIR` inside an SDK folder.
pub fn fuel_dir(sdk_dir: &Path) -> PathBuf {
    sdk_dir.join("Amazon Games Services").join("Legacy")
}

/// `AMAZON_GAMES_SDK_PATH` inside an SDK folder.
pub fn games_sdk_dir(sdk_dir: &Path) -> PathBuf {
    sdk_dir.join("Amazon Games Services").join("AmazonGamesSDK")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_sdk_files() {
        assert!(is_sdk_file("Amazon Games Services/Legacy/FuelSDK_x64.dll"));
        assert!(is_sdk_file(
            "Amazon Games Services/AmazonGamesSDK/AmazonGamesSDK_Windows_x64.dll"
        ));
        assert!(!is_sdk_file("App/Amazon Games.exe"));
        assert_eq!(
            fuel_dir(Path::new("/sdk")),
            Path::new("/sdk/Amazon Games Services/Legacy")
        );
    }
}
