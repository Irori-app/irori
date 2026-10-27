//! Native extraction of downloaded archives (blocking; run on the blocking pool).
//!
//! Tar archives (`.tar`, gzip, bzip2, xz, lzma, zstd), zip archives, MojoSetup
//! `.sh` installers (a shell stub followed by a zip; only `data/noarch/` is
//! the game) and makeself `.sh`/`.run` installers (a shell stub followed by
//! compressed tarballs) are unpacked in-process. Nothing in an archive can
//! write or link outside the destination folder: entries with absolute or
//! `..` paths are skipped, and links pointing outside it or entries written
//! through a link fail the extraction. Other formats are left to the caller.

use std::fs::File;
use std::io::{self, BufRead as _, BufReader, Read, Seek as _, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result, bail};
use irori_core::archive_guard;
use irori_core::process::CancellationToken;
use once_cell::sync::Lazy;
use regex::bytes::Regex;

/// What a downloaded file is, judged by name and content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Tar,
    TarGz,
    TarXz,
    TarZst,
    TarBz2,
    /// `.tar.lzma` (LZMA-alone stream).
    TarLzma,
    /// Compression we cannot decode natively (lzip); use system `tar`.
    TarOther,
    Zip,
    /// MojoSetup installer: shell stub + zip with the game under `data/noarch/`.
    MojoSetup,
    /// Makeself self-extracting shell archive (see [`makeself_layout`]).
    Makeself,
    /// Windows executable (installer).
    WindowsExe,
    /// Anything else (a standalone binary, AppImage, part of a multi-file set).
    Other,
}

/// Classify a file (blocking: may open it to look inside `.sh` files).
pub fn classify(path: &Path) -> ArchiveKind {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let ends = |exts: &[&str]| exts.iter().any(|e| name.ends_with(e));
    if ends(&[".tar.gz", ".tgz"]) {
        ArchiveKind::TarGz
    } else if ends(&[".tar.xz", ".txz"]) {
        ArchiveKind::TarXz
    } else if ends(&[".tar.zst", ".tzst"]) {
        ArchiveKind::TarZst
    } else if ends(&[".tar"]) {
        ArchiveKind::Tar
    } else if ends(&[".tar.bz2", ".tbz2", ".tbz"]) {
        ArchiveKind::TarBz2
    } else if ends(&[".tar.lzma", ".tlz"]) {
        ArchiveKind::TarLzma
    } else if ends(&[".tar.lz"]) {
        ArchiveKind::TarOther
    } else if ends(&[".zip"]) {
        ArchiveKind::Zip
    } else if ends(&[".exe"]) {
        ArchiveKind::WindowsExe
    } else if ends(&[".sh", ".run"]) {
        if is_mojosetup(path) {
            ArchiveKind::MojoSetup
        } else if read_makeself_layout(path).is_some() {
            ArchiveKind::Makeself
        } else {
            // A plain script or a self-extractor we do not understand.
            ArchiveKind::Other
        }
    } else {
        ArchiveKind::Other
    }
}

fn is_mojosetup(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(mut zip) = zip::ZipArchive::new(BufReader::new(file)) else {
        return false;
    };
    (0..zip.len()).any(|i| {
        zip.by_index_raw(i)
            .map(|f| f.name().is_ok_and(|n| n.starts_with("data/noarch/")))
            .unwrap_or(false)
    })
}

/// Where a makeself archive's payload is: it starts after the first `lines`
/// lines of the file (the script) and holds one tarball per size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MakeselfLayout {
    /// Byte offset of the first tarball.
    pub offset: u64,
    /// Sizes of the tarballs, in order; empty means "the rest of the file".
    pub sizes: Vec<u64>,
}

/// How much of a makeself file to read looking for its script header
/// (licenses can be embedded in it).
const MAKESELF_HEADER_MAX: u64 = 8 << 20;

/// Parse the script header of a makeself archive: `offset=`head -n <lines>
/// "$1" | wc -c`` (makeself 2.x) or `skip="<lines>"` (older), and
/// `filesizes="<n> …"`.
pub fn makeself_layout(head: &[u8]) -> Option<MakeselfLayout> {
    static LINES: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"head -n\s*"?(\d+)"?\s+"?\$[01]"?|\bskip="?(\d+)"?"#).expect("valid regex")
    });
    static SIZES: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\bfilesizes="([0-9 ]*)""#).expect("valid regex"));
    if !head.starts_with(b"#!") {
        return None;
    }
    let caps = LINES.captures(head)?;
    let lines: usize = std::str::from_utf8(caps.get(1).or(caps.get(2))?.as_bytes())
        .ok()?
        .parse()
        .ok()?;
    let offset = head
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(lines.checked_sub(1)?)
        .map(|(i, _)| i as u64 + 1)?;
    let sizes = SIZES
        .captures(head)
        .and_then(|c| std::str::from_utf8(&c[1]).ok().map(str::to_string))
        .map(|s| {
            s.split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    Some(MakeselfLayout { offset, sizes })
}

fn read_makeself_layout(path: &Path) -> Option<MakeselfLayout> {
    let mut head = Vec::new();
    File::open(path)
        .ok()?
        .take(MAKESELF_HEADER_MAX)
        .read_to_end(&mut head)
        .ok()?;
    makeself_layout(&head)
}

/// Reports `(bytes_read, total_bytes)` of the archive while extracting.
pub type ExtractProgress<'a> = &'a mut dyn FnMut(u64, u64);

/// Extract `archive` of `kind` into `dest`. Returns `false` when cancelled.
pub fn extract(
    archive: &Path,
    kind: ArchiveKind,
    dest: &Path,
    cancel: &CancellationToken,
    progress: ExtractProgress<'_>,
) -> Result<bool> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    let res = match kind {
        ArchiveKind::Tar
        | ArchiveKind::TarGz
        | ArchiveKind::TarXz
        | ArchiveKind::TarZst
        | ArchiveKind::TarBz2
        | ArchiveKind::TarLzma => extract_tar(archive, kind, dest, cancel, progress),
        ArchiveKind::Makeself => extract_makeself(archive, dest, cancel, progress),
        ArchiveKind::Zip => extract_zip(archive, None, dest, cancel, progress),
        ArchiveKind::MojoSetup => {
            extract_zip(archive, Some("data/noarch/"), dest, cancel, progress)
        }
        other => bail!("{other:?} files cannot be extracted natively"),
    };
    res.with_context(|| {
        format!(
            "extracting {}",
            archive.file_name().unwrap_or_default().to_string_lossy()
        )
    })
}

struct Counting<R> {
    inner: R,
    count: Arc<AtomicU64>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// A decoder for `kind`'s compression over `reader`.
fn decoder<'a>(kind: ArchiveKind, reader: impl Read + 'a) -> Result<Box<dyn Read + 'a>> {
    Ok(match kind {
        ArchiveKind::TarGz => Box::new(flate2::read::MultiGzDecoder::new(reader)),
        ArchiveKind::TarXz => Box::new(xz2::read::XzDecoder::new_multi_decoder(reader)),
        ArchiveKind::TarLzma => Box::new(xz2::read::XzDecoder::new_stream(
            reader,
            xz2::stream::Stream::new_lzma_decoder(u64::MAX)?,
        )),
        ArchiveKind::TarZst => Box::new(zstd::stream::read::Decoder::new(reader)?),
        ArchiveKind::TarBz2 => Box::new(bzip2::read::MultiBzDecoder::new(reader)),
        _ => Box::new(reader),
    })
}

/// The compression of a tarball, from its first bytes.
fn sniff(head: &[u8]) -> Option<ArchiveKind> {
    Some(if head.starts_with(&[0x1f, 0x8b]) {
        ArchiveKind::TarGz
    } else if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        ArchiveKind::TarXz
    } else if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        ArchiveKind::TarZst
    } else if head.starts_with(b"BZh") {
        ArchiveKind::TarBz2
    } else if head.get(257..262) == Some(b"ustar") {
        ArchiveKind::Tar
    } else {
        return None;
    })
}

/// Unpack a tar stream into `dest`. Returns `false` when cancelled.
///
/// Entries with absolute or `..` paths are skipped. Links whose target
/// resolves outside `dest` and entries that would be written through a
/// symbolic link fail the extraction (see [`archive_guard`]).
fn unpack_tar(
    decoded: impl Read,
    dest: &Path,
    cancel: &CancellationToken,
    mut on_entry: impl FnMut(),
) -> Result<bool> {
    let mut tar = tar::Archive::new(decoded);
    tar.set_preserve_permissions(true);
    tar.set_preserve_mtime(true);
    tar.set_overwrite(true);
    for entry in tar.entries()? {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        let mut entry = entry?;
        unpack_tar_entry(&mut entry, dest)?;
        on_entry();
    }
    Ok(true)
}

fn unpack_tar_entry(entry: &mut tar::Entry<'_, impl Read>, dest: &Path) -> Result<()> {
    let path = entry.path()?.into_owned();
    let Some(rel) = archive_guard::relative_entry_path(&path) else {
        tracing::warn!("skipped unsafe archive entry {}", path.display());
        return Ok(());
    };
    let target = dest.join(&rel);
    let kind = entry.header().entry_type();
    // Old tar headers mark folders with a trailing slash only.
    let is_dir = kind.is_dir() || entry.path_bytes().ends_with(b"/");
    let parent = rel.parent().unwrap_or(Path::new(""));
    archive_guard::ensure_no_symlinks(dest, if is_dir { &rel } else { parent })?;
    std::fs::create_dir_all(dest.join(parent))
        .with_context(|| format!("creating {}", dest.join(parent).display()))?;
    if is_dir {
        entry
            .unpack(&target)
            .with_context(|| format!("creating {}", target.display()))?;
        return Ok(());
    }
    if kind.is_hard_link() {
        let link = entry
            .link_name()?
            .context("hard link without a target")?
            .into_owned();
        let unsafe_link = || {
            anyhow::anyhow!(
                "unsafe link in archive: {} -> {}",
                rel.display(),
                link.display()
            )
        };
        let source_rel = archive_guard::relative_entry_path(&link).ok_or_else(unsafe_link)?;
        archive_guard::ensure_no_symlinks(dest, source_rel.parent().unwrap_or(Path::new("")))?;
        let source = dest.join(&source_rel);
        // Only regular files: some systems follow a symbolic link when hard
        // linking it, and the copy fallback always does.
        if !std::fs::symlink_metadata(&source).is_ok_and(|m| m.is_file()) {
            return Err(unsafe_link());
        }
        let _ = std::fs::remove_file(&target);
        if std::fs::hard_link(&source, &target).is_err() {
            std::fs::copy(&source, &target)
                .with_context(|| format!("linking {}", target.display()))?;
        }
        return Ok(());
    }
    if kind.is_symlink() {
        let link = entry
            .link_name()?
            .context("symbolic link without a target")?
            .into_owned();
        if !archive_guard::check_symlink(dest, &rel, &link)? {
            return Ok(());
        }
    }
    // A regular file replaces a link at `target` instead of following it.
    entry
        .unpack(&target)
        .with_context(|| format!("writing {}", target.display()))?;
    Ok(())
}

fn extract_tar(
    archive: &Path,
    kind: ArchiveKind,
    dest: &Path,
    cancel: &CancellationToken,
    progress: ExtractProgress<'_>,
) -> Result<bool> {
    let file = File::open(archive)?;
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);
    let count = Arc::new(AtomicU64::new(0));
    let reader = Counting {
        inner: BufReader::with_capacity(1 << 20, file),
        count: count.clone(),
    };
    let ok = unpack_tar(decoder(kind, reader)?, dest, cancel, || {
        progress(count.load(Ordering::Relaxed), total)
    })?;
    if ok {
        progress(total, total);
    }
    Ok(ok)
}

/// Unpack the tarballs of a makeself archive into `dest`, as its own
/// `--noexec --target dest` would, without running the script.
fn extract_makeself(
    archive: &Path,
    dest: &Path,
    cancel: &CancellationToken,
    progress: ExtractProgress<'_>,
) -> Result<bool> {
    let layout = read_makeself_layout(archive).context("not a makeself archive")?;
    let mut file = File::open(archive)?;
    let len = file.metadata()?.len();
    let sizes = if layout.sizes.is_empty() {
        vec![len.saturating_sub(layout.offset)]
    } else {
        layout.sizes.clone()
    };
    let total: u64 = sizes.iter().sum();
    if layout.offset + total > len {
        bail!("the installer is incomplete");
    }
    file.seek(SeekFrom::Start(layout.offset))?;
    let count = Arc::new(AtomicU64::new(0));
    let mut done_before = 0u64;
    for size in sizes {
        let mut reader = BufReader::with_capacity(
            1 << 20,
            Counting {
                inner: (&mut file).take(size),
                count: count.clone(),
            },
        );
        let kind = sniff(reader.fill_buf()?)
            .context("the installer's payload is encrypted or uses an unsupported compression")?;
        let ok = unpack_tar(decoder(kind, &mut reader)?, dest, cancel, || {
            progress(count.load(Ordering::Relaxed), total)
        })?;
        if !ok {
            return Ok(false);
        }
        // Skip whatever the tar reader left of this tarball (padding).
        io::copy(&mut reader, &mut io::sink())?;
        drop(reader);
        done_before += size;
        file.seek(SeekFrom::Start(layout.offset + done_before))?;
    }
    progress(total, total);
    Ok(true)
}

fn extract_zip(
    archive: &Path,
    prefix: Option<&str>,
    dest: &Path,
    cancel: &CancellationToken,
    progress: ExtractProgress<'_>,
) -> Result<bool> {
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(BufReader::with_capacity(1 << 20, file))
        .context("not a valid zip archive")?;
    let total: u64 = (0..zip.len())
        .filter_map(|i| zip.by_index_raw(i).ok().map(|f| f.compressed_size()))
        .sum();
    let mut done = 0u64;
    for i in 0..zip.len() {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        let mut entry = zip.by_index(i)?;
        done += entry.compressed_size();
        let Some(name) = entry.enclosed_name() else {
            tracing::warn!("skipped unsafe archive entry {:?}", entry.name().ok());
            continue;
        };
        let rel: PathBuf = match prefix {
            Some(p) => match name.strip_prefix(p.trim_end_matches('/')) {
                Ok(r) if !r.as_os_str().is_empty() => r.to_path_buf(),
                _ => continue,
            },
            None => name,
        };
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            continue;
        }
        let out = dest.join(&rel);
        let mode = entry.unix_mode();
        if entry.is_dir() {
            archive_guard::ensure_no_symlinks(dest, &rel)?;
            std::fs::create_dir_all(&out)?;
            progress(done, total);
            continue;
        }
        let parent = rel.parent().unwrap_or(Path::new(""));
        archive_guard::ensure_no_symlinks(dest, parent)?;
        std::fs::create_dir_all(dest.join(parent))?;
        if mode.is_some_and(|m| m & 0o170000 == 0o120000) {
            let mut target = String::new();
            entry.read_to_string(&mut target)?;
            #[cfg(not(unix))]
            anyhow::bail!("This archive requires Unix symbolic links");
            #[cfg(unix)]
            if archive_guard::check_symlink(dest, &rel, Path::new(&target))? {
                std::os::unix::fs::symlink(target, &out)
                    .with_context(|| format!("creating link {}", out.display()))?;
            }
        } else {
            // `File::create` would write through a link left at `out`.
            if std::fs::symlink_metadata(&out).is_ok_and(|m| m.file_type().is_symlink()) {
                std::fs::remove_file(&out)
                    .with_context(|| format!("replacing {}", out.display()))?;
            }
            let mut f = File::create(&out).with_context(|| format!("writing {}", out.display()))?;
            io::copy(&mut entry, &mut f).with_context(|| format!("writing {}", out.display()))?;
            #[cfg(unix)]
            if let Some(m) = mode {
                use std::os::unix::fs::PermissionsExt as _;
                let perm = (m & 0o777) | 0o600;
                std::fs::set_permissions(&out, std::fs::Permissions::from_mode(perm))?;
            }
        }
        progress(done, total);
    }
    progress(total, total);
    Ok(true)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    pub fn make_tar_gz(path: &Path, files: &[(&str, &[u8], u32)]) {
        let f = File::create(path).unwrap();
        let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        let mut b = tar::Builder::new(gz);
        for (name, data, mode) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(*mode);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
    }

    pub fn make_zip(prefix_bytes: &[u8], files: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut buf = io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            for (name, data, mode) in files {
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(*mode);
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
        }
        let mut out = prefix_bytes.to_vec();
        out.extend_from_slice(&buf.into_inner());
        out
    }

    enum Compress {
        Gzip,
        Bzip2,
        None,
    }

    fn tar_bytes(files: &[(&str, &[u8], u32)], c: Compress) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data, mode) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(*mode);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        let raw = b.into_inner().unwrap();
        match c {
            Compress::Gzip => {
                let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                e.write_all(&raw).unwrap();
                e.finish().unwrap()
            }
            Compress::Bzip2 => {
                let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
                e.write_all(&raw).unwrap();
                e.finish().unwrap()
            }
            Compress::None => raw,
        }
    }

    /// A makeself 2.x archive: its script header, then the tarballs.
    fn makeself(tarballs: &[Vec<u8>]) -> Vec<u8> {
        let sizes: Vec<String> = tarballs.iter().map(|t| t.len().to_string()).collect();
        let script = format!(
            "#!/bin/sh\n# This script was generated using Makeself 2.4.5\n\
             filesizes=\"{}\"\noffset=`head -n 5 \"$0\" | wc -c | tr -d \" \"`\nexit 0\n",
            sizes.join(" ")
        );
        let mut out = script.into_bytes();
        for t in tarballs {
            out.extend_from_slice(t);
        }
        out
    }

    #[test]
    fn parses_makeself_headers() {
        let l = makeself_layout(b"#!/bin/sh\nskip=\"3\"\nfilesizes=\"10 20\"\nPAYLOAD").unwrap();
        assert_eq!(
            l,
            MakeselfLayout {
                offset: 37,
                sizes: vec![10, 20]
            }
        );
        assert!(makeself_layout(b"no shebang\nhead -n 1 \"$1\"").is_none());
        assert!(makeself_layout(b"#!/bin/sh\necho hi\n").is_none());
    }

    #[test]
    fn extracts_makeself_payloads_natively() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("game.run");
        std::fs::write(
            &a,
            makeself(&[
                tar_bytes(&[("bin/game", b"ELF", 0o755)], Compress::Gzip),
                tar_bytes(&[("data/a.pak", b"pak", 0o644)], Compress::Bzip2),
                tar_bytes(&[("readme.txt", b"hi", 0o644)], Compress::None),
            ]),
        )
        .unwrap();
        let dest = dir.path().join("out");
        let mut last = (0, 0);
        assert!(
            extract(
                &a,
                ArchiveKind::Makeself,
                &dest,
                &CancellationToken::new(),
                &mut |d, t| last = (d, t)
            )
            .unwrap()
        );
        assert_eq!(std::fs::read(dest.join("bin/game")).unwrap(), b"ELF");
        assert_eq!(std::fs::read(dest.join("data/a.pak")).unwrap(), b"pak");
        assert_eq!(std::fs::read(dest.join("readme.txt")).unwrap(), b"hi");
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(dest.join("bin/game"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }
        assert_eq!(last.0, last.1);

        // A truncated download is reported, not half-extracted silently.
        let data = std::fs::read(&a).unwrap();
        std::fs::write(&a, &data[..data.len() - 10]).unwrap();
        assert!(
            extract(
                &a,
                ArchiveKind::Makeself,
                &dest,
                &CancellationToken::new(),
                &mut noop()
            )
            .is_err()
        );
    }

    fn noop() -> impl FnMut(u64, u64) {
        |_, _| {}
    }

    #[test]
    fn classifies_files() {
        let dir = tempfile::tempdir().unwrap();
        for (n, k) in [
            ("a.tar.xz", ArchiveKind::TarXz),
            ("a.TGZ", ArchiveKind::TarGz),
            ("a.tar.bz2", ArchiveKind::TarBz2),
            ("a.tar.lzma", ArchiveKind::TarLzma),
            ("a.tar.lz", ArchiveKind::TarOther),
            ("setup_x.exe", ArchiveKind::WindowsExe),
            ("a.zip", ArchiveKind::Zip),
            ("game.AppImage", ArchiveKind::Other),
        ] {
            assert_eq!(classify(&dir.path().join(n)), k, "{n}");
        }
        let mojo = dir.path().join("game.sh");
        std::fs::write(
            &mojo,
            make_zip(
                b"#!/bin/sh\nexit 0\n",
                &[("data/noarch/start.sh", b"x", 0o755)],
            ),
        )
        .unwrap();
        assert_eq!(classify(&mojo), ArchiveKind::MojoSetup);
        let ms = dir.path().join("installer.run");
        std::fs::write(
            &ms,
            makeself(&[tar_bytes(&[("a", b"x", 0o644)], Compress::Gzip)]),
        )
        .unwrap();
        assert_eq!(classify(&ms), ArchiveKind::Makeself);
        let plain = dir.path().join("other.sh");
        std::fs::write(&plain, b"#!/bin/sh\necho hello\n").unwrap();
        assert_eq!(classify(&plain), ArchiveKind::Other);
    }

    #[test]
    fn extracts_tar_gz_with_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("g.tar.gz");
        make_tar_gz(
            &a,
            &[
                ("Game/start.sh", b"#!/bin/sh\n", 0o755),
                ("Game/data/x.dat", b"data", 0o644),
            ],
        );
        let dest = dir.path().join("out");
        let mut last = (0, 0);
        let ok = extract(
            &a,
            ArchiveKind::TarGz,
            &dest,
            &CancellationToken::new(),
            &mut |d, t| last = (d, t),
        )
        .unwrap();
        assert!(ok);
        assert_eq!(
            std::fs::read(dest.join("Game/data/x.dat")).unwrap(),
            b"data"
        );
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(dest.join("Game/start.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }
        assert_eq!(last.0, last.1);
    }

    #[test]
    fn extracts_mojosetup_payload_only() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("game.sh");
        std::fs::write(
            &a,
            make_zip(
                b"#!/bin/sh\n# mojosetup stub\nexit 0\n",
                &[
                    ("scripts/mojosetup_init.lua", b"--", 0o644),
                    ("data/noarch/start.sh", b"#!/bin/sh\n", 0o755),
                    ("data/noarch/game/bin", b"ELF", 0o755),
                ],
            ),
        )
        .unwrap();
        let dest = dir.path().join("out");
        assert!(
            extract(
                &a,
                ArchiveKind::MojoSetup,
                &dest,
                &CancellationToken::new(),
                &mut noop()
            )
            .unwrap()
        );
        assert!(dest.join("start.sh").is_file());
        assert!(dest.join("game/bin").is_file());
        assert!(!dest.join("scripts").exists());
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(dest.join("start.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o755, 0o755);
        }
    }

    #[test]
    fn zip_rejects_traversal_and_honours_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("evil.zip");
        std::fs::write(
            &a,
            make_zip(
                b"",
                &[("../escape.txt", b"x", 0o644), ("ok.txt", b"y", 0o644)],
            ),
        )
        .unwrap();
        let dest = dir.path().join("out");
        assert!(
            extract(
                &a,
                ArchiveKind::Zip,
                &dest,
                &CancellationToken::new(),
                &mut noop()
            )
            .unwrap()
        );
        assert!(!dir.path().join("escape.txt").exists());
        assert!(dest.join("ok.txt").exists());

        let cancel = CancellationToken::new();
        cancel.cancel();
        let dest2 = dir.path().join("out2");
        assert!(!extract(&a, ArchiveKind::Zip, &dest2, &cancel, &mut noop()).unwrap());
    }

    /// One entry of a hand-built archive.
    enum Item<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        Symlink(&'a str, &'a str),
        /// Tar only: zip has no hard links.
        HardLink(&'a str, &'a str),
    }

    fn build_tar(items: &[Item]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for item in items {
            let mut h = tar::Header::new_gnu();
            h.set_mode(0o644);
            match *item {
                Item::File(path, data) => {
                    h.set_size(data.len() as u64);
                    b.append_data(&mut h, path, data).unwrap();
                }
                Item::Dir(path) => {
                    h.set_entry_type(tar::EntryType::Directory);
                    h.set_mode(0o755);
                    h.set_size(0);
                    b.append_data(&mut h, path, io::empty()).unwrap();
                }
                Item::Symlink(path, target) | Item::HardLink(path, target) => {
                    h.set_entry_type(match item {
                        Item::Symlink(..) => tar::EntryType::Symlink,
                        _ => tar::EntryType::Link,
                    });
                    h.set_size(0);
                    b.append_link(&mut h, path, target).unwrap();
                }
            }
        }
        b.into_inner().unwrap()
    }

    fn build_zip(items: &[Item]) -> Vec<u8> {
        let mut buf = io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
        for item in items {
            match *item {
                Item::File(path, data) => {
                    w.start_file(path, opts).unwrap();
                    w.write_all(data).unwrap();
                }
                Item::Dir(path) => w.add_directory(path, opts).unwrap(),
                Item::Symlink(path, target) => w.add_symlink(path, target, opts).unwrap(),
                Item::HardLink(..) => unreachable!("zip has no hard links"),
            }
        }
        w.finish().unwrap();
        buf.into_inner()
    }

    /// A scratch folder with an `out` destination and an `outside` folder
    /// holding `secret`, which no archive may reach.
    struct Sandbox {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    impl Sandbox {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            // Canonical, so links to it are what a real attack would write.
            let root = dir.path().canonicalize().unwrap();
            std::fs::create_dir_all(root.join("outside")).unwrap();
            std::fs::write(root.join("outside/secret"), b"keep").unwrap();
            std::fs::create_dir_all(root.join("out")).unwrap();
            Self { _dir: dir, root }
        }

        fn dest(&self) -> PathBuf {
            self.root.join("out")
        }

        fn outside(&self) -> String {
            self.root.join("outside").to_string_lossy().into_owned()
        }

        fn extract(&self, kind: ArchiveKind, items: &[Item]) -> Result<bool> {
            let (name, bytes) = match kind {
                ArchiveKind::Zip => ("a.zip", build_zip(items)),
                _ => ("a.tar", build_tar(items)),
            };
            let archive = self.root.join(name);
            std::fs::write(&archive, bytes).unwrap();
            extract(
                &archive,
                kind,
                &self.dest(),
                &CancellationToken::new(),
                &mut noop(),
            )
        }

        /// The outside folder still holds exactly the untouched `secret`.
        fn assert_outside_untouched(&self) {
            let names: Vec<_> = std::fs::read_dir(self.root.join("outside"))
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(names, ["secret"]);
            assert_eq!(
                std::fs::read(self.root.join("outside/secret")).unwrap(),
                b"keep"
            );
            assert!(!self.root.join("x").exists());
        }

        fn has(&self, rel: &str) -> bool {
            self.dest().join(rel).symlink_metadata().is_ok()
        }
    }

    const FORMATS: [ArchiveKind; 2] = [ArchiveKind::Tar, ArchiveKind::Zip];

    #[test]
    fn refuses_writing_through_an_archive_symlink() {
        for kind in FORMATS {
            let sb = Sandbox::new();
            let outside = sb.outside();
            let items = [
                Item::Symlink("dir", &outside),
                Item::File("dir/x", b"pwned"),
                Item::Dir("dir/sub/"),
            ];
            assert!(sb.extract(kind, &items).is_err(), "{kind:?}");
            sb.assert_outside_untouched();
        }
    }

    #[test]
    fn refuses_absolute_and_climbing_link_targets() {
        for kind in FORMATS {
            let sb = Sandbox::new();
            let outside = sb.outside();
            for (path, target) in [
                ("abs", outside.as_str()),
                ("root", "/"),
                ("up", "../outside"),
                ("a/b/up", "../../../outside/secret"),
            ] {
                let res = sb.extract(kind, &[Item::Symlink(path, target)]);
                assert!(res.is_err(), "{kind:?} {path} -> {target}");
                assert!(!sb.has(path), "{kind:?} {path}");
            }
            sb.assert_outside_untouched();
        }
    }

    #[test]
    fn refuses_hard_links_outside() {
        let sb = Sandbox::new();
        let items = [Item::HardLink("h", "../outside/secret")];
        assert!(sb.extract(ArchiveKind::Tar, &items).is_err());
        assert!(!sb.has("h"));
        sb.assert_outside_untouched();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_links_resolving_out_through_earlier_links() {
        for kind in FORMATS {
            let sb = Sandbox::new();
            // Read lexically, `self/..` is `dest`; on disk it is `dest/..`.
            let items = [
                Item::Symlink("self", "."),
                Item::Symlink("up", "self/.."),
                Item::File("up/x", b"pwned"),
            ];
            assert!(sb.extract(kind, &items).is_err(), "{kind:?}");
            assert!(!sb.has("up"));
            sb.assert_outside_untouched();
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_links_replacing_entries() {
        // Tar only: zip readers keep one entry per name.
        let sb = Sandbox::new();
        // `up -> a/..` is `dest` while `a` is a folder link, `dest/..` after.
        let items = [
            Item::Dir("d/e/"),
            Item::Symlink("a", "d/e"),
            Item::Symlink("up", "a/.."),
            Item::Symlink("a", "."),
            Item::File("up/x", b"pwned"),
        ];
        assert!(sb.extract(ArchiveKind::Tar, &items).is_err());
        assert_eq!(
            std::fs::read_link(sb.dest().join("a")).unwrap(),
            Path::new("d/e")
        );
        sb.assert_outside_untouched();

        // A link left by an earlier extraction is not replaced either.
        let sb = Sandbox::new();
        std::fs::create_dir_all(sb.dest().join("d/e")).unwrap();
        std::os::unix::fs::symlink("d/e", sb.dest().join("a")).unwrap();
        std::fs::write(sb.dest().join("f"), b"file").unwrap();
        for kind in FORMATS {
            assert!(sb.extract(kind, &[Item::Symlink("a", ".")]).is_err());
            assert!(sb.extract(kind, &[Item::Symlink("f", ".")]).is_err());
        }
        assert_eq!(
            std::fs::read_link(sb.dest().join("a")).unwrap(),
            Path::new("d/e")
        );
        assert_eq!(std::fs::read(sb.dest().join("f")).unwrap(), b"file");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_writing_through_an_existing_symlink() {
        for kind in FORMATS {
            let sb = Sandbox::new();
            std::os::unix::fs::symlink(sb.outside(), sb.dest().join("dir")).unwrap();
            assert!(sb.extract(kind, &[Item::File("dir/x", b"pwned")]).is_err());
            assert!(sb.extract(kind, &[Item::Dir("dir/sub/")]).is_err());
            assert!(sb.extract(kind, &[Item::Symlink("dir/l", "../l")]).is_err());
            sb.assert_outside_untouched();
        }
        // Linking to `dir/secret` would share the outside file's inode.
        let sb = Sandbox::new();
        std::os::unix::fs::symlink(sb.outside(), sb.dest().join("dir")).unwrap();
        let items = [Item::HardLink("h", "dir/secret")];
        assert!(sb.extract(ArchiveKind::Tar, &items).is_err());
        assert!(!sb.has("h"));
        sb.assert_outside_untouched();
    }

    #[cfg(unix)]
    #[test]
    fn replaces_a_final_symlink_instead_of_following_it() {
        for kind in FORMATS {
            let sb = Sandbox::new();
            let secret = format!("{}/secret", sb.outside());
            std::os::unix::fs::symlink(&secret, sb.dest().join("f")).unwrap();
            sb.extract(kind, &[Item::File("f", b"mine")]).unwrap();
            assert!(!sb.dest().join("f").symlink_metadata().unwrap().is_symlink());
            assert_eq!(std::fs::read(sb.dest().join("f")).unwrap(), b"mine");
            sb.assert_outside_untouched();
        }
    }

    #[cfg(unix)]
    #[test]
    fn keeps_links_inside_the_tree() {
        for kind in FORMATS {
            let sb = Sandbox::new();
            let mut items = vec![
                Item::File("Game/lib64/libfoo.so", b"so"),
                Item::Symlink("Game/lib", "lib64"),
                Item::File("Game/bin/game", b"elf"),
                Item::Symlink("Game/bin/game.x86_64", "game"),
                Item::Symlink("Game/bin/libfoo.so", "../lib/libfoo.so"),
                // A link may name a folder that comes later in the archive.
                Item::Symlink("Game/pfx/dosdevices/c:", "../drive_c"),
                Item::Dir("Game/pfx/drive_c/"),
                Item::File("Game/pfx/drive_c/x.txt", b"c"),
            ];
            if kind == ArchiveKind::Tar {
                items.push(Item::HardLink("Game/bin/game-copy", "Game/bin/game"));
            }
            assert!(sb.extract(kind, &items).unwrap());
            // Extracting again (a resumed install) keeps the same links.
            assert!(sb.extract(kind, &items).unwrap(), "{kind:?} again");
            let game = sb.dest().join("Game");
            assert_eq!(std::fs::read(game.join("lib/libfoo.so")).unwrap(), b"so");
            assert_eq!(std::fs::read(game.join("bin/game.x86_64")).unwrap(), b"elf");
            assert_eq!(std::fs::read(game.join("bin/libfoo.so")).unwrap(), b"so");
            assert_eq!(
                std::fs::read(game.join("pfx/dosdevices/c:/x.txt")).unwrap(),
                b"c"
            );
            if kind == ArchiveKind::Tar {
                assert_eq!(std::fs::read(game.join("bin/game-copy")).unwrap(), b"elf");
            }
            sb.assert_outside_untouched();
        }
    }
}
