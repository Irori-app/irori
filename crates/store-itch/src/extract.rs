//! Unpacking a downloaded upload into its install folder (blocking; run on
//! the blocking pool). Zip and tar archives (plain, gzip, xz, bzip2) are
//! unpacked in-process, refusing entries that would land outside the
//! folder. A file that is not an archive (a portable `.exe`, an AppImage, a
//! `.jar`) is copied in as it is. Disk images and 7-Zip or RAR archives are
//! refused with an explanation.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use irori_core::process::CancellationToken;

/// How a downloaded file is unpacked, judged by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Zip,
    Tar,
    TarGz,
    TarXz,
    TarBz2,
    /// Copied as it is.
    Plain,
    /// Disk images and archives Irori cannot read.
    Unsupported,
}

pub(crate) fn classify(name: &str) -> Kind {
    let name = name.to_ascii_lowercase();
    let ends = |exts: &[&str]| exts.iter().any(|e| name.ends_with(e));
    if ends(&[".zip"]) {
        Kind::Zip
    } else if ends(&[".tar.gz", ".tgz"]) {
        Kind::TarGz
    } else if ends(&[".tar.xz", ".txz"]) {
        Kind::TarXz
    } else if ends(&[".tar.bz2", ".tbz2", ".tbz"]) {
        Kind::TarBz2
    } else if ends(&[".tar"]) {
        Kind::Tar
    } else if ends(&[".dmg", ".7z", ".rar", ".pkg", ".msi"]) {
        Kind::Unsupported
    } else {
        Kind::Plain
    }
}

/// Unpack `archive` (named `name` on itch.io) into `dest`.
pub(crate) fn unpack(
    archive: &Path,
    name: &str,
    dest: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    let open = || {
        File::open(archive)
            .map(BufReader::new)
            .with_context(|| format!("opening {}", archive.display()))
    };
    match classify(name) {
        Kind::Zip => unzip(archive, dest, cancel),
        Kind::Tar => untar(open()?, dest, cancel),
        Kind::TarGz => untar(flate2::read::GzDecoder::new(open()?), dest, cancel),
        Kind::TarXz => untar(xz2::read::XzDecoder::new(open()?), dest, cancel),
        Kind::TarBz2 => untar(bzip2::read::BzDecoder::new(open()?), dest, cancel),
        Kind::Plain => {
            let file = Path::new(name)
                .file_name()
                .filter(|f| !f.is_empty())
                .context("the download has no file name")?;
            let target = dest.join(file);
            std::fs::copy(archive, &target)
                .with_context(|| format!("copying to {}", target.display()))?;
            Ok(())
        }
        Kind::Unsupported => bail!(
            "{name} is a kind of file Irori cannot unpack yet. Download it from the game's itch.io page instead."
        ),
    }
}

fn unzip(archive: &Path, dest: &Path, cancel: &CancellationToken) -> Result<()> {
    let file = File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).context("the zip file is damaged")?;
    for i in 0..zip.len() {
        if cancel.is_cancelled() {
            bail!("cancelled");
        }
        let mut entry = zip.by_index(i).context("the zip file is damaged")?;
        let Some(rel) = entry.enclosed_name() else {
            bail!(
                "the zip file holds an unsafe path: {}",
                entry.name().unwrap_or_default()
            );
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
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

fn untar(reader: impl io::Read, dest: &Path, cancel: &CancellationToken) -> Result<()> {
    let mut tar = tar::Archive::new(reader);
    tar.set_preserve_permissions(true);
    for entry in tar.entries().context("the archive is damaged")? {
        if cancel.is_cancelled() {
            bail!("cancelled");
        }
        let mut entry = entry.context("the archive is damaged")?;
        let path = entry
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        // `unpack_in` refuses entries that would escape `dest`.
        if !entry
            .unpack_in(dest)
            .with_context(|| format!("unpacking {path}"))?
        {
            bail!("the archive holds an unsafe path: {path}");
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write as _;

    use super::*;

    /// A zip with the given `(path, content)` entries.
    pub(crate) fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut out);
            for (name, data) in entries {
                let options = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(0o755);
                zip.start_file(*name, options).unwrap();
                zip.write_all(data).unwrap();
            }
            zip.finish().unwrap();
        }
        out.into_inner()
    }

    #[test]
    fn unpacks_zip_and_tar_and_copies_plain_files() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let zip = dir.path().join("dl.zip");
        std::fs::write(
            &zip,
            zip_bytes(&[("Game/game.exe", b"MZ"), ("Game/data/a.pak", b"1")]),
        )
        .unwrap();
        let dest = dir.path().join("zip");
        unpack(&zip, "game.zip", &dest, &cancel).unwrap();
        assert_eq!(std::fs::read(dest.join("Game/game.exe")).unwrap(), b"MZ");
        assert_eq!(std::fs::read(dest.join("Game/data/a.pak")).unwrap(), b"1");

        let tgz = dir.path().join("dl.tgz");
        {
            let enc =
                flate2::write::GzEncoder::new(File::create(&tgz).unwrap(), Default::default());
            let mut tar = tar::Builder::new(enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, "game/run.x86_64", &b"\x7fELF"[..])
                .unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        let dest = dir.path().join("tar");
        unpack(&tgz, "game-linux.tar.gz", &dest, &cancel).unwrap();
        assert_eq!(
            std::fs::read(dest.join("game/run.x86_64")).unwrap(),
            b"\x7fELF"
        );

        let plain = dir.path().join("dl.bin");
        std::fs::write(&plain, b"AppImage").unwrap();
        let dest = dir.path().join("plain");
        unpack(&plain, "Game-x86_64.AppImage", &dest, &cancel).unwrap();
        assert!(dest.join("Game-x86_64.AppImage").is_file());

        assert!(unpack(&plain, "game.dmg", &dir.path().join("dmg"), &cancel).is_err());
    }

    #[test]
    fn refuses_entries_outside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("evil.zip");
        std::fs::write(&zip, zip_bytes(&[("../outside.txt", b"x")])).unwrap();
        let dest = dir.path().join("dest");
        assert!(unpack(&zip, "evil.zip", &dest, &CancellationToken::new()).is_err());
        assert!(!dir.path().join("outside.txt").exists());
    }
}
