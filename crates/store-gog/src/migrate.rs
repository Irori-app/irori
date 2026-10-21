//! Converts gogdl state (from earlier Ember versions or from Heroic) into
//! Ember's own files: build records, support files and the
//! redistributables record. The old gogdl folder is kept as a backup.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde_json::Value;

use ember_core::{InstallPlatform, json_store};

use crate::build::{self, BuildRecord};
use crate::installed::{InstalledFile, InstalledRecord};
use crate::paths::GogPaths;
use crate::redist::RedistRecord;

/// gogdl's record of the files unpacked from Linux installers.
const LINUX_MANIFEST: &str = ".gogdl-linux-manifest";

pub(crate) fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let target = dst.join(e.file_name());
        let ft = e.file_type()?;
        if ft.is_dir() {
            copy_tree(&e.path(), &target)?;
        } else if ft.is_file() && !target.exists() {
            std::fs::copy(e.path(), &target)?;
        }
    }
    Ok(())
}

fn read_value(path: &Path) -> Option<Value> {
    json_store::read::<Value>(path).ok().flatten()
}

/// Convert gogdl's manifests and support files in `gogdl_dir`
/// (`heroic_gogdl`) for `apps` (every app when `None`). Existing Ember
/// records win. Returns how many builds were converted.
pub(crate) fn convert_gogdl_dir(
    gogdl_dir: &Path,
    dst: &GogPaths,
    apps: Option<&[String]>,
) -> Result<usize> {
    let wanted = |app: &str| apps.is_none_or(|a| a.iter().any(|x| x == app));
    let mut converted = 0;
    for e in std::fs::read_dir(gogdl_dir.join("manifests"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let app = e.file_name().to_string_lossy().into_owned();
        let target = dst.build_file(&app);
        if !wanted(&app) || target.exists() || !e.path().is_file() {
            continue;
        }
        let Some(record) = read_value(&e.path()).and_then(build::from_gogdl_manifest) else {
            tracing::warn!("skipping the unreadable gogdl manifest of {app}");
            continue;
        };
        json_store::write(&target, &record)
            .with_context(|| format!("saving the build record of {app}"))?;
        converted += 1;
    }
    for e in std::fs::read_dir(gogdl_dir.join("gog-support"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let app = e.file_name().to_string_lossy().into_owned();
        if wanted(&app) && e.path().is_dir() {
            copy_tree(&e.path(), &dst.support_dir(&app))
                .with_context(|| format!("copying support files of {app}"))?;
        }
    }
    Ok(converted)
}

/// Build records for Linux installs that only have gogdl's file list.
pub(crate) fn convert_linux_installs(dst: &GogPaths, installed: &[InstalledRecord]) -> Result<()> {
    for rec in installed
        .iter()
        .filter(|r| r.platform == InstallPlatform::Linux)
    {
        let target = dst.build_file(&rec.app_name);
        if target.exists() {
            continue;
        }
        let Some(files) = read_value(&rec.path().join(LINUX_MANIFEST))
            .and_then(|v| build::from_gogdl_linux_manifest(&v))
        else {
            continue;
        };
        let record = BuildRecord {
            platform: "linux".into(),
            language: rec.language_or_default().to_string(),
            dlcs: rec.installed_dlcs.clone(),
            linux: Some(files),
            ..Default::default()
        };
        json_store::write(&target, &record)?;
    }
    Ok(())
}

/// Convert gogdl's redistributables file next to the downloads.
fn convert_redist(dst: &GogPaths) -> Result<()> {
    let old = dst.redist.join(".gogdl-redist-manifest");
    if !old.is_file() {
        return Ok(());
    }
    if !dst.redist_record().exists()
        && let Some(record) =
            read_value(&old).and_then(|v| serde_json::from_value::<RedistRecord>(v).ok())
    {
        json_store::write(&dst.redist_record(), &record)?;
    }
    std::fs::rename(&old, old.with_extension("backup"))?;
    Ok(())
}

/// Move a folder aside as `<name>-backup` (numbered when taken).
fn back_up(dir: &Path) -> Result<()> {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut target = dir.with_file_name(format!("{name}-backup"));
    let mut n = 2;
    while target.exists() {
        target = dir.with_file_name(format!("{name}-backup-{n}"));
        n += 1;
    }
    std::fs::rename(dir, &target)
        .with_context(|| format!("moving {} to {}", dir.display(), target.display()))
}

/// Convert what earlier Ember versions kept for gogdl. Runs at startup and
/// does nothing once converted.
pub fn migrate_gogdl_state(paths: &GogPaths) -> Result<()> {
    let installed: InstalledFile = json_store::read_or_default(&paths.installed_file());
    let legacy = paths.legacy_gogdl_dir();
    if legacy.is_dir() {
        let n = convert_gogdl_dir(&legacy, paths, None)?;
        tracing::info!("converted {n} gogdl manifests into build records");
        back_up(&paths.config.join("gogdl"))?;
    }
    convert_linux_installs(paths, &installed.installed)?;
    convert_redist(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_gogdl_state() {
        let d = tempfile::tempdir().unwrap();
        let paths = GogPaths::under(d.path());
        let gogdl = paths.legacy_gogdl_dir();
        std::fs::create_dir_all(gogdl.join("manifests")).unwrap();
        std::fs::write(
            gogdl.join("manifests/1"),
            r#"{"version": 2, "baseProductId": "1", "buildId": "5", "HGLInstallLanguage": "en-US",
                "HGLdlcs": [], "depots": [], "products": [], "dependencies": ["MSVC2015"]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(gogdl.join("gog-support/1/1")).unwrap();
        std::fs::write(gogdl.join("gog-support/1/1/setup.exe"), b"MZ").unwrap();
        let linux_dir = d.path().join("games/L");
        std::fs::create_dir_all(&linux_dir).unwrap();
        std::fs::write(
            linux_dir.join(LINUX_MANIFEST),
            r#"{"files": [{"file_name": "data/noarch/start.sh", "crc32": 9}]}"#,
        )
        .unwrap();
        json_store::write(
            &paths.installed_file(),
            &InstalledFile {
                installed: vec![InstalledRecord {
                    app_name: "2".into(),
                    platform: InstallPlatform::Linux,
                    install_path: linux_dir.to_string_lossy().into_owned(),
                    ..Default::default()
                }],
            },
        )
        .unwrap();
        std::fs::create_dir_all(&paths.redist).unwrap();
        std::fs::write(
            paths.redist.join(".gogdl-redist-manifest"),
            r#"{"build_id": "7", "depots": [], "HGLInstalled": ["ISI"]}"#,
        )
        .unwrap();

        migrate_gogdl_state(&paths).unwrap();
        let r = build::read(&paths.build_file("1")).unwrap();
        assert_eq!((r.build_id.as_str(), r.language.as_str()), ("5", "en-US"));
        assert!(paths.support_dir("1").join("1/setup.exe").is_file());
        assert!(!paths.config.join("gogdl").exists());
        assert!(
            paths
                .config
                .join("gogdl-backup/heroic_gogdl/manifests/1")
                .is_file()
        );
        let l = build::read(&paths.build_file("2")).unwrap();
        assert_eq!(l.linux.unwrap().files["start.sh"], 9);
        let redist: RedistRecord = json_store::read(&paths.redist_record()).unwrap().unwrap();
        assert_eq!(redist.installed, ["ISI"]);
        // Running again changes nothing.
        migrate_gogdl_state(&paths).unwrap();
    }
}
