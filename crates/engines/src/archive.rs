//! Unpacking a downloaded engine build into its install folder: zip, 7-Zip,
//! tar archives (gzip, xz, zstd, bzip2), macOS disk images and single
//! programs (AppImages). Entries that would land outside the folder are
//! refused. Blocking except [`unpack_dmg`]; run on the blocking pool.

use std::fs::File;
use std::io::{self, BufReader, Read as _};
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use irori_core::archive_guard;
use irori_core::install::ignore_progress;
use irori_core::process::CancellationToken;

/// Longest target of a symbolic link: more is not a path.
const MAX_LINK_TARGET: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Zip,
    SevenZip,
    Tar,
    Dmg,
    /// A program on its own (an AppImage, a portable `.exe`).
    Plain,
}

pub fn classify(name: &str) -> Kind {
    let name = name.to_ascii_lowercase();
    let ends = |exts: &[&str]| exts.iter().any(|e| name.ends_with(e));
    if ends(&[".zip"]) {
        Kind::Zip
    } else if ends(&[".7z"]) {
        Kind::SevenZip
    } else if ends(&[
        ".tar", ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.zst", ".tar.bz2", ".tbz2",
    ]) {
        Kind::Tar
    } else if ends(&[".dmg"]) {
        Kind::Dmg
    } else {
        Kind::Plain
    }
}

/// Unpack `archive`, named `name`, into `dest` (which must not exist yet).
/// Disk images need [`unpack_dmg`] instead.
pub fn unpack(archive: &Path, name: &str, dest: &Path, cancel: &CancellationToken) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    match classify(name) {
        Kind::Zip => unzip(archive, dest, cancel),
        Kind::SevenZip => un7z(archive, dest, cancel),
        Kind::Tar => {
            irori_runtime::download::extract_tar(archive, dest, 0, ignore_progress(), cancel)
        }
        Kind::Dmg => bail!("disk images are unpacked with unpack_dmg"),
        Kind::Plain => {
            let file = Path::new(name)
                .file_name()
                .filter(|f| !f.is_empty())
                .context("the download has no file name")?;
            let target = dest.join(file);
            std::fs::copy(archive, &target)
                .with_context(|| format!("copying to {}", target.display()))?;
            make_executable(&target)
        }
    }
}

/// Set the executable bit (no-op on Windows).
pub fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn unzip(archive: &Path, dest: &Path, cancel: &CancellationToken) -> Result<()> {
    let file = File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).context("the zip file is damaged")?;
    for i in 0..zip.len() {
        if cancel.is_cancelled() {
            return Err(irori_runtime::download::Cancelled.into());
        }
        let mut entry = zip.by_index(i).context("the zip file is damaged")?;
        let Some(rel) = entry.enclosed_name() else {
            bail!(
                "the zip file holds an unsafe path: {}",
                entry.name().unwrap_or_default()
            );
        };
        let out = dest.join(&rel);
        if entry.is_dir() {
            archive_guard::ensure_no_symlinks(dest, &rel)?;
            std::fs::create_dir_all(&out)?;
            continue;
        }
        let parent = rel.parent().unwrap_or(Path::new(""));
        archive_guard::ensure_no_symlinks(dest, parent)?;
        std::fs::create_dir_all(dest.join(parent))?;
        if entry.is_symlink() {
            // Bundles (`.app`) inside zips link their frameworks.
            let mut target = String::new();
            io::Read::read_to_string(&mut (&mut entry).take(MAX_LINK_TARGET), &mut target)?;
            if archive_guard::check_symlink(dest, &rel, Path::new(&target))? {
                #[cfg(unix)]
                std::os::unix::fs::symlink(&target, &out)
                    .with_context(|| format!("linking {}", out.display()))?;
            }
            continue;
        }
        // `File::create` would write through a link left at `out`.
        if std::fs::symlink_metadata(&out).is_ok_and(|m| m.file_type().is_symlink()) {
            std::fs::remove_file(&out)?;
        }
        let mut f = File::create(&out).with_context(|| format!("writing {}", out.display()))?;
        io::copy(&mut entry, &mut f).with_context(|| format!("unpacking {}", out.display()))?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                &out,
                std::fs::Permissions::from_mode((mode & 0o777) | 0o600),
            )?;
        }
    }
    Ok(())
}

fn un7z(archive: &Path, dest: &Path, cancel: &CancellationToken) -> Result<()> {
    let root = dest.to_path_buf();
    sevenz_rust2::decompress_file_with_extract_fn(archive, dest, |entry, reader, path| {
        if cancel.is_cancelled() {
            return Err(sevenz_rust2::Error::Other("cancelled".into()));
        }
        // The default extractor rejects `..`; also refuse absolute names.
        if !path.starts_with(&root) {
            return Err(sevenz_rust2::Error::Other(
                format!("unsafe path {}", entry.name()).into(),
            ));
        }
        sevenz_rust2::default_entry_extract_fn(entry, reader, path)
    })
    .map_err(|e| {
        if cancel.is_cancelled() {
            anyhow::Error::new(irori_runtime::download::Cancelled)
        } else {
            anyhow::anyhow!("the 7-Zip archive could not be unpacked: {e}")
        }
    })?;
    // 7-Zip keeps no Unix modes Irori can rely on; programs need theirs.
    mark_programs_executable(dest);
    Ok(())
}

/// Make AppImages and extensionless files in the top two levels runnable.
fn mark_programs_executable(dir: &Path) {
    fn walk(dir: &Path, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth > 0 {
                    walk(&path, depth - 1);
                }
            } else if path
                .extension()
                .is_none_or(|e| e.eq_ignore_ascii_case("appimage"))
            {
                let _ = make_executable(&path);
            }
        }
    }
    walk(dir, 1);
}

/// Copy the app bundles of a macOS disk image into `dest`.
pub async fn unpack_dmg(image: &Path, dest: &Path, scratch: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use irori_core::process::{self, CommandSpec, RunOptions};
        let mount = scratch.join("mount");
        tokio::fs::create_dir_all(&mount).await?;
        tokio::fs::create_dir_all(dest).await?;
        let attach = CommandSpec::new("/usr/bin/hdiutil").args([
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-noautoopen".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            image.as_os_str(),
        ]);
        // Images with a licence agreement wait for an answer on stdin.
        let result = process::run(
            &attach,
            RunOptions {
                stdin: Some(b"Y\n".to_vec()),
                ..Default::default()
            },
        )
        .await?;
        if result.code != Some(0) {
            bail!(
                "the disk image could not be opened: {}",
                result.stderr.trim()
            );
        }
        let copied = copy_bundles(&mount, dest).await;
        let detach = CommandSpec::new("/usr/bin/hdiutil").args([
            "detach".as_ref(),
            "-force".as_ref(),
            mount.as_os_str(),
        ]);
        if let Err(e) = process::run(&detach, RunOptions::default()).await {
            tracing::warn!("detaching {}: {e:#}", mount.display());
        }
        copied
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (image, dest, scratch);
        bail!("disk images only open on macOS")
    }
}

#[cfg(target_os = "macos")]
async fn copy_bundles(mount: &Path, dest: &Path) -> Result<()> {
    use irori_core::process::{self, CommandSpec, RunOptions};
    let mut found = false;
    let mut entries = tokio::fs::read_dir(mount).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "app") {
            found = true;
            let target = dest.join(entry.file_name());
            // `ditto` keeps signatures, links and extended attributes.
            let copy =
                CommandSpec::new("/usr/bin/ditto").args([path.as_os_str(), target.as_os_str()]);
            let r = process::run(&copy, RunOptions::default()).await?;
            if r.code != Some(0) {
                bail!("copying {} failed: {}", path.display(), r.stderr.trim());
            }
        }
    }
    if !found {
        bail!("the disk image holds no app");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn classifies_by_name() {
        assert_eq!(classify("RetroArch.7z"), Kind::SevenZip);
        assert_eq!(classify("x.tar.xz"), Kind::Tar);
        assert_eq!(classify("DuckStation-x64.AppImage"), Kind::Plain);
        assert_eq!(classify("PPSSPP_macOS.dmg"), Kind::Dmg);
        assert_eq!(classify("a.ZIP"), Kind::Zip);
    }

    #[test]
    fn unzips_and_copies_programs() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("a.zip");
        {
            let mut zip = zip::ZipWriter::new(File::create(&zip_path).unwrap());
            let opts = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
            zip.start_file("emu/emu.exe", opts).unwrap();
            zip.write_all(b"MZ").unwrap();
            zip.finish().unwrap();
        }
        let cancel = CancellationToken::new();
        let dest = dir.path().join("out");
        unpack(&zip_path, "a.zip", &dest, &cancel).unwrap();
        assert_eq!(std::fs::read(dest.join("emu/emu.exe")).unwrap(), b"MZ");

        let app = dir.path().join("download");
        std::fs::write(&app, b"\x7fELF").unwrap();
        let dest = dir.path().join("plain");
        unpack(&app, "Emu-x64.AppImage", &dest, &cancel).unwrap();
        assert!(irori_core::util::is_executable(
            &dest.join("Emu-x64.AppImage")
        ));
    }

    #[cfg(unix)]
    fn zip_of_links(path: &Path, links: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (name, target) in links {
            zip.add_symlink(*name, *target, opts).unwrap();
        }
        zip.start_file("x/evil", opts).unwrap();
        io::Write::write_all(&mut zip, b"evil").unwrap();
        zip.finish().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bundle_links_stay_and_links_cannot_lead_out() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let archive = dir.path().join("a.zip");

        zip_of_links(&archive, &[("A.app/Frameworks/y", "../z")]);
        let dest = dir.path().join("fine");
        unpack(&archive, "a.zip", &dest, &cancel).unwrap();
        assert!(dest.join("A.app/Frameworks/y").is_symlink());

        for links in [
            &[("x", "../../outside")][..],
            &[("x", "/etc")][..],
            // Each link looks inside on its own; together they climb out.
            &[("a/b/l", "../.."), ("x", "a/b/l/..")][..],
        ] {
            zip_of_links(&archive, links);
            let dest = dir.path().join("deep/er/out");
            assert!(
                unpack(&archive, "a.zip", &dest, &cancel).is_err(),
                "{links:?}"
            );
            assert!(!dir.path().join("deep/er/evil").exists());
            std::fs::remove_dir_all(dir.path().join("deep")).unwrap();
        }
    }
}
