use async_zip::base::read::mem::ZipFileReader;
use derive_getters::Getters;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component as PathComponent, Path};
use std::time::Duration;
use std::{fmt::Display, path::PathBuf, time::Instant};
use tokio::fs;

use reqwest::Client;

#[allow(dead_code)]
pub enum Platform {
    Windows,
    Mac,
}

impl Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Windows => f.write_str("windows"),
            Self::Mac => f.write_str("osx"),
        }
    }
}

#[derive(Serialize, Deserialize, Getters, Debug)]
#[serde(rename_all = "camelCase")]
struct ComponentManifest {
    application_type: String,
    #[serde(rename = "baseURI")]
    base_uri: String,
    files: Vec<ComponentFile>,
    force_update: bool,
    project_name: String,
    symlinks: Vec<ComponentSymlink>,
    timestamp: String,
    version: String,
}

#[derive(Serialize, Deserialize, Getters, Debug)]
struct ComponentFile {
    hash: String,
    path: String,
    resource: String,
    sha256: String,
    size: u32,
}

#[derive(Serialize, Deserialize, Getters, Debug)]
struct ComponentSymlink {
    path: String,
    target: String,
}

#[derive(Debug)]
pub enum Component {
    Peer,
    Overlay,
    Web,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct ComponentManifestLocal {
    pub time: i64,
    pub version: String,
    /// Files written before hashes were checked are downloaded once more:
    /// games load them as code.
    #[serde(default)]
    pub verified: bool,
}

/// Largest file a component may unpack to. Peer libraries are around 11 MiB.
const MAX_FILE_SIZE: u64 = 256 * 1024 * 1024;

type ComponentError = Box<dyn std::error::Error + Send + Sync>;

/// `path` below `root`, refused if it could land anywhere else. The manifest
/// comes from the network, and games load what is written here as code.
fn path_inside(root: &Path, path: &str) -> Result<PathBuf, ComponentError> {
    let relative = Path::new(path);
    let inside = !path.is_empty()
        && !path.contains('\\')
        && relative
            .components()
            .all(|part| matches!(part, PathComponent::Normal(_)));
    if !inside {
        return Err(format!("Unsafe path in component manifest: {path:?}").into());
    }
    Ok(root.join(relative))
}

/// Whether a symlink at `link` pointing to `target` stays inside the component.
/// Both are relative to its root.
fn symlink_stays_inside(link: &str, target: &str) -> bool {
    let target = Path::new(target);
    let mut depth = Path::new(link).components().count().saturating_sub(1);
    target.components().all(|part| match part {
        PathComponent::Normal(_) => {
            depth += 1;
            true
        }
        PathComponent::CurDir => true,
        PathComponent::ParentDir if depth > 0 => {
            depth -= 1;
            true
        }
        _ => false,
    })
}

/// The file inside a downloaded zip. `sha256` in the manifest is the hash of
/// the unpacked file, not of the zip.
async fn unpack_verified(
    zip: Vec<u8>,
    sha256: &str,
) -> Result<(Vec<u8>, Option<u16>), ComponentError> {
    use futures::AsyncReadExt as _;

    let zip = ZipFileReader::new(zip).await?;
    let reader = zip.reader_with_entry(0).await?;
    let mut data = Vec::new();
    let mut limited = reader.take(MAX_FILE_SIZE + 1);
    limited.read_to_end(&mut data).await?;
    if data.len() as u64 > MAX_FILE_SIZE {
        return Err("Component file is too large".into());
    }
    let actual = hex::encode(Sha256::digest(&data));
    if !actual.eq_ignore_ascii_case(sha256) {
        return Err(format!("Component file failed verification: {actual} != {sha256}").into());
    }
    let permissions = zip
        .file()
        .entries()
        .first()
        .and_then(|entry| entry.unix_permissions());
    Ok((data, permissions))
}

/// Replace `path` in one step, so a failed write never leaves a game loading
/// half a library.
async fn write_atomically(path: &Path, data: &[u8]) -> Result<(), ComponentError> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".part");
    fs::write(&partial, data).await?;
    fs::rename(&partial, path).await?;
    Ok(())
}

impl Display for Component {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Peer => f.write_str("desktop-galaxy-peer"),
            Self::Overlay => f.write_str("desktop-galaxy-overlay"),
            Self::Web => f.write_str("desktop-galaxy-client"),
        }
    }
}

pub async fn get_component(
    reqwest_client: &Client,
    dest_path: PathBuf,
    platform: Platform,
    component: Component,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let manifest_path = dest_path.join(format!(".{}-{}.toml", component, platform));
    let local_manifest: ComponentManifestLocal = match fs::read_to_string(&manifest_path).await {
        Ok(manifest_str) => toml::from_str(&manifest_str).unwrap_or_default(),
        Err(err) => {
            log::debug!("Failed to read component manifest {err:?}");
            ComponentManifestLocal::default()
        }
    };
    if local_manifest.verified && local_manifest.time + (24 * 3600) > chrono::Utc::now().timestamp()
    {
        return Ok(());
    }
    log::debug!("Checking for peer updates");
    let url = format!(
        "https://cfg.gog.com/{}/7/master/files-{}.json",
        component, platform
    );

    let manifest_res = reqwest_client.get(url).send().await?;
    let manifest: ComponentManifest = manifest_res.json().await?;

    if dest_path.exists() {
        if local_manifest.verified
            && local_manifest.version == manifest.version
            && !manifest.force_update
        {
            return Ok(());
        }
    } else {
        fs::create_dir_all(&dest_path).await?;
    }

    let files_to_dl: Vec<&ComponentFile> = manifest
        .files()
        .iter()
        .filter(|file| !matches!(component, Component::Web) || file.path().starts_with("web"))
        .collect();

    // Refuse the whole manifest before anything reaches the disk.
    for file in &files_to_dl {
        path_inside(&dest_path, file.path())?;
    }
    for symlink in manifest.symlinks() {
        path_inside(&dest_path, symlink.path())?;
        if !symlink_stays_inside(symlink.path(), symlink.target()) {
            return Err(format!("Unsafe symlink in manifest: {:?}", symlink.path()).into());
        }
    }

    let total_size: u32 = files_to_dl.iter().map(|file| file.size).sum();
    let mut tasks = Vec::with_capacity(files_to_dl.len());
    log::info!(
        "Downloading component {} - total download size {:.2} MiB",
        component,
        (total_size as f32) / 1024.0 / 1024.0
    );
    // Download
    for file in files_to_dl.into_iter() {
        let url = format!("{}/{}", manifest.base_uri(), file.resource());
        let reqwest_client = reqwest_client.clone();
        let file_path = path_inside(&dest_path, file.path())?;
        let parent = file_path.parent();
        if let Some(parent) = parent {
            fs::create_dir_all(parent).await?;
        }
        tasks.push(async move {
            let response = reqwest_client.get(url).send().await?;
            let response = response.error_for_status()?;
            let data = response.bytes().await?;

            let (data, _permissions) = unpack_verified(data.to_vec(), file.sha256()).await?;
            write_atomically(&file_path, &data).await?;
            #[cfg(unix)]
            if let Some(permissions) = _permissions {
                use std::{fs::Permissions, os::unix::fs::PermissionsExt};
                let permissions = Permissions::from_mode(permissions as u32 & 0o777);
                fs::set_permissions(file_path, permissions).await?;
            }
            Ok::<_, ComponentError>(file.size)
        });
    }

    let mut pending_tasks = futures::stream::iter(tasks).buffer_unordered(4);

    let mut total_dl = 0;
    let mut instant = Instant::now();
    while let Some(res) = pending_tasks.next().await {
        total_dl += res?;
        if instant.elapsed() > Duration::from_secs(1) {
            log::info!(
                "[{:?}] {:.2} / {:.2}",
                component,
                total_dl as f32 / 1024.0 / 1024.0,
                total_size as f32 / 1024.0 / 1024.0
            );
            instant = Instant::now();
        }
    }
    log::info!(
        "[{:?}] {:.2} / {:.2}",
        component,
        total_dl as f32 / 1024.0 / 1024.0,
        total_size as f32 / 1024.0 / 1024.0
    );

    #[cfg(unix)]
    for symlink in manifest.symlinks() {
        let link = path_inside(&dest_path, symlink.path())?;
        // Left over from the previous version of the component.
        let _ = fs::remove_file(&link).await;
        fs::symlink(symlink.target(), link).await?;
    }

    let new_manifest = ComponentManifestLocal {
        version: manifest.version().clone(),
        time: chrono::Utc::now().timestamp(),
        verified: true,
    };
    let data = toml::to_string(&new_manifest)?;
    fs::write(manifest_path, data).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_zip::base::write::ZipFileWriter;
    use async_zip::{Compression, ZipEntryBuilder};

    async fn zip_of(data: &[u8]) -> Vec<u8> {
        let mut writer = ZipFileWriter::new(futures::io::Cursor::new(Vec::new()));
        let entry = ZipEntryBuilder::new("file".to_string().into(), Compression::Deflate);
        writer.write_entry_whole(entry, data).await.unwrap();
        writer.close().await.unwrap().into_inner()
    }

    #[tokio::test]
    async fn a_file_matching_its_hash_unpacks() {
        let sha256 = hex::encode(Sha256::digest(b"peer library"));
        let (data, _) = unpack_verified(zip_of(b"peer library").await, &sha256)
            .await
            .unwrap();
        assert_eq!(data, b"peer library");
    }

    #[tokio::test]
    async fn a_tampered_file_is_refused() {
        let sha256 = hex::encode(Sha256::digest(b"peer library"));
        let err = unpack_verified(zip_of(b"something else").await, &sha256)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("verification"), "{err}");
    }

    #[tokio::test]
    async fn the_hash_is_of_the_unpacked_file_not_the_zip() {
        let zip = zip_of(b"peer library").await;
        let sha256 = hex::encode(Sha256::digest(&zip));
        assert!(unpack_verified(zip, &sha256).await.is_err());
    }

    #[test]
    fn manifest_paths_stay_inside_the_component() {
        let root = Path::new("root");
        assert_eq!(
            path_inside(root, "peer/msvc-18/GalaxyPeer.dll").unwrap(),
            root.join("peer/msvc-18/GalaxyPeer.dll")
        );
        for path in [
            "",
            "..",
            "../evil.dll",
            "peer/../../evil.dll",
            "./peer.dll",
            "/etc/evil",
            r"..\\evil.dll",
            r"C:\\evil.dll",
        ] {
            assert!(path_inside(root, path).is_err(), "{path:?}");
        }
    }

    #[test]
    fn symlinks_stay_inside_the_component() {
        assert!(symlink_stays_inside("lib/current", "v1/library.dylib"));
        assert!(symlink_stays_inside(
            "lib/current",
            "../other/library.dylib"
        ));
        assert!(!symlink_stays_inside("current", "../library.dylib"));
        assert!(!symlink_stays_inside("lib/current", "../../library.dylib"));
        assert!(!symlink_stays_inside(
            "lib/current",
            "/usr/lib/library.dylib"
        ));
    }
}
