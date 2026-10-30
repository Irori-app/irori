//! Unpacking tar (any compression), zip and single gzip files into an empty
//! staging folder. Entries with absolute paths or `..` are refused, and
//! nothing is written through a link that leads outside the staging folder.
//! Blocking: call from `spawn_blocking`.

use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use irori_core::process::CancellationToken;
use irori_runtime::Cancelled;
use irori_runtime::download::{Compression, detect_compression};

use crate::fsops::resolve;
use crate::model::ArchiveFormat;

/// The archive kind of `path`, from its name and first bytes.
pub(crate) fn detect(path: &Path) -> io::Result<ArchiveFormat> {
    let mut header = [0u8; 512];
    let n = File::open(path)?.read(&mut header)?;
    let header = &header[..n];
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if header.starts_with(b"PK\x03\x04") || header.starts_with(b"PK\x05\x06") {
        return Ok(ArchiveFormat::Zip);
    }
    let tar_name = [
        ".tar",
        ".tar.gz",
        ".tgz",
        ".tar.xz",
        ".txz",
        ".tar.lzma",
        ".tar.bz2",
        ".tbz2",
        ".tbz",
        ".tar.zst",
        ".tzst",
    ]
    .iter()
    .any(|ext| name.ends_with(ext));
    if tar_name || header.get(257..262) == Some(b"ustar") {
        return Ok(ArchiveFormat::Tar);
    }
    match detect_compression(header) {
        Compression::Gzip if name.ends_with(".gz") => Ok(ArchiveFormat::Gzip),
        Compression::None => {
            let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
            let format = match ext {
                "exe" | "msi" | "7z" | "rar" | "deb" | "cab" => ext,
                "appimage" => "AppImage",
                _ => "auto",
            };
            Ok(ArchiveFormat::External(format.into()))
        }
        _ => Ok(ArchiveFormat::Tar),
    }
}

/// Fail unless `target` (to be created) stays inside `root` once links are
/// followed.
fn ensure_inside(root: &Path, target: &Path) -> Result<()> {
    let real = resolve(target).with_context(|| format!("checking {}", target.display()))?;
    if !real.starts_with(root) {
        bail!(
            "the archive tries to write outside its folder ({})",
            target.display()
        );
    }
    Ok(())
}

/// A relative entry path with no `..`, absolute or drive parts.
fn safe_relative(path: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => bail!("unsafe path in archive: {}", path.display()),
        }
    }
    Ok(out)
}

/// Unpack a tarball (gzip, bzip2, xz, zstd or plain) into `dest`.
pub(crate) fn extract_tar(archive: &Path, dest: &Path, cancel: &CancellationToken) -> Result<()> {
    let mut header = [0u8; 6];
    let n = File::open(archive)
        .and_then(|mut f| f.read(&mut header))
        .with_context(|| format!("opening {}", archive.display()))?;
    let file = BufReader::with_capacity(1 << 16, File::open(archive)?);
    let reader: Box<dyn Read> = match detect_compression(&header[..n]) {
        Compression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(file)),
        Compression::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(file)),
        Compression::Xz => Box::new(xz2::read::XzDecoder::new_multi_decoder(file)),
        Compression::Zstd => {
            Box::new(zstd::stream::read::Decoder::new(file).context("invalid zstd archive")?)
        }
        Compression::None => Box::new(file),
    };
    fs::create_dir_all(dest)?;
    let mut tar = tar::Archive::new(reader);
    tar.set_preserve_mtime(true);
    tar.set_overwrite(true);
    let mut wrote = false;
    for entry in tar
        .entries()
        .with_context(|| format!("reading {}", archive.display()))?
    {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let mut entry = entry.with_context(|| format!("{} is damaged", archive.display()))?;
        let path = entry.path()?.into_owned();
        safe_relative(&path)?;
        use tar::EntryType as T;
        if !matches!(
            entry.header().entry_type(),
            T::Regular | T::Continuous | T::Directory | T::Symlink | T::Link | T::GNUSparse
        ) {
            continue;
        }
        // `unpack_in` only guards what is written through a link. Its target
        // needs checking too: later steps write to paths in this tree.
        if entry.header().entry_type() == T::Symlink {
            let link = entry
                .link_name()?
                .context("symbolic link without a target")?
                .into_owned();
            let rel = safe_relative(&path)?;
            if !irori_core::archive_guard::check_symlink(dest, &rel, &link)? {
                continue;
            }
        }
        // `unpack_in` resolves the parent folder and refuses entries that
        // would land outside `dest` through a link.
        if !entry
            .unpack_in(dest)
            .with_context(|| format!("extracting {}", path.display()))?
        {
            bail!("unsafe path in archive: {}", path.display());
        }
        wrote = true;
    }
    if !wrote {
        bail!("{} contains no files", archive.display());
    }
    Ok(())
}

/// Unpack a zip archive into `dest`.
pub(crate) fn extract_zip(archive: &Path, dest: &Path, cancel: &CancellationToken) -> Result<()> {
    fs::create_dir_all(dest)?;
    let root = fs::canonicalize(dest)?;
    let file = File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))
        .with_context(|| format!("{} is not a valid zip archive", archive.display()))?;
    for i in 0..zip.len() {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let mut entry = zip
            .by_index(i)
            .with_context(|| format!("{} is damaged", archive.display()))?;
        let Some(rel) = entry.enclosed_name() else {
            bail!(
                "unsafe path in archive: {}",
                entry.name().unwrap_or_default()
            );
        };
        let rel = safe_relative(&rel)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = root.join(&rel);
        ensure_inside(&root, &target)?;
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            let rel_parent = rel.parent().unwrap_or(Path::new(""));
            irori_core::archive_guard::ensure_no_symlinks(&root, rel_parent)?;
            fs::create_dir_all(parent)?;
            ensure_inside(&root, parent)?;
        }
        if fs::symlink_metadata(&target).is_ok() {
            fs::remove_file(&target)?;
        }
        if entry.is_symlink() {
            let mut link = String::new();
            entry.read_to_string(&mut link)?;
            if irori_core::archive_guard::check_symlink(&root, &rel, Path::new(&link))? {
                write_link(&root, &target, &link)?;
            }
            continue;
        }
        let mut out = File::create(&target)?;
        io::copy(&mut entry, &mut out).with_context(|| format!("extracting {}", rel.display()))?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

/// Recreate a zip symlink whose target stays inside the archive.
#[cfg(unix)]
fn write_link(root: &Path, at: &Path, target: &str) -> Result<()> {
    let target_path = Path::new(target);
    let base = at.parent().unwrap_or(root);
    if target_path.is_absolute()
        || !crate::fsops::lexical(&base.join(target_path)).starts_with(root)
    {
        bail!("the archive has a link leading outside it ({target})");
    }
    std::os::unix::fs::symlink(target_path, at)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_link(_root: &Path, _at: &Path, _target: &str) -> Result<()> {
    // Creating symlinks needs privileges on Windows; Windows games do not
    // rely on them.
    Ok(())
}

/// Decompress one gzip file into `dest`, named without `.gz`.
pub(crate) fn gunzip(archive: &Path, dest: &Path) -> Result<()> {
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let out_name = name
        .strip_suffix(".gz")
        .filter(|n| !n.is_empty())
        .unwrap_or("file");
    fs::create_dir_all(dest)?;
    let mut input = flate2::read::MultiGzDecoder::new(BufReader::new(File::open(archive)?));
    let mut out = File::create(dest.join(out_name))?;
    io::copy(&mut input, &mut out).with_context(|| format!("{} is damaged", archive.display()))?;
    Ok(())
}

/// `path` itself, or the files matching its last component's `*`/`?`
/// pattern (sorted).
pub(crate) fn glob(path: &Path) -> Vec<PathBuf> {
    if path.exists() {
        return vec![path.to_path_buf()];
    }
    let (Some(dir), Some(pattern)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
    else {
        return Vec::new();
    };
    if !pattern.contains(['*', '?']) {
        return Vec::new();
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| wildcard(pattern, &e.file_name().to_string_lossy()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// `*` and `?` matching.
fn wildcard(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni, mut star, mut mark) = (0, 0, None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// The only entry of `dir`, when it is a folder (Lutris moves its contents
/// up).
pub(crate) fn single_folder(dir: &Path) -> Option<PathBuf> {
    let mut entries = std::fs::read_dir(dir).ok()?.filter_map(Result::ok);
    let first = entries.next()?;
    if entries.next().is_some() {
        return None;
    }
    let meta = std::fs::symlink_metadata(first.path()).ok()?;
    meta.is_dir().then(|| first.path())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write as _;

    pub(crate) fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    pub(crate) fn make_tar_gz(path: &Path, files: &[(&str, &[u8])]) {
        let enc = flate2::write::GzEncoder::new(File::create(path).unwrap(), Default::default());
        let mut tar = tar::Builder::new(enc);
        for (name, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            tar.append_data(&mut h, name, *data).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn detects_formats() {
        let tmp = tempfile::tempdir().unwrap();
        let z = tmp.path().join("a.zip");
        make_zip(&z, &[("x", b"1")]);
        assert_eq!(detect(&z).unwrap(), ArchiveFormat::Zip);
        let t = tmp.path().join("a.bin");
        make_tar_gz(&t, &[("x", b"1")]);
        assert_eq!(detect(&t).unwrap(), ArchiveFormat::Tar);
        let e = tmp.path().join("setup.exe");
        fs::write(&e, b"MZ....").unwrap();
        assert_eq!(detect(&e).unwrap(), ArchiveFormat::External("exe".into()));
    }

    #[test]
    fn unpacks_zip_and_tar() {
        let tmp = tempfile::tempdir().unwrap();
        let z = tmp.path().join("a.zip");
        make_zip(&z, &[("dir/a.txt", b"zip"), ("b.txt", b"b")]);
        let out = tmp.path().join("z");
        extract_zip(&z, &out, &CancellationToken::new()).unwrap();
        assert_eq!(fs::read(out.join("dir/a.txt")).unwrap(), b"zip");
        let t = tmp.path().join("a.tar.gz");
        make_tar_gz(&t, &[("top/run.sh", b"#!/bin/sh")]);
        let out = tmp.path().join("t");
        extract_tar(&t, &out, &CancellationToken::new()).unwrap();
        assert_eq!(fs::read(out.join("top/run.sh")).unwrap(), b"#!/bin/sh");
    }

    #[test]
    fn refuses_escaping_tar_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path().join("evil.tar");
        // Write the header by hand: `tar::Builder` refuses `..` itself.
        let mut h = tar::Header::new_old();
        h.as_old_mut().name[..9].copy_from_slice(b"../escape");
        h.set_size(1);
        h.set_mode(0o644);
        h.set_cksum();
        let mut data = h.as_bytes().to_vec();
        data.extend_from_slice(b"x");
        data.resize(512 * 2, 0);
        data.extend_from_slice(&[0; 1024]);
        fs::write(&t, data).unwrap();
        let out = tmp.path().join("out");
        assert!(extract_tar(&t, &out, &CancellationToken::new()).is_err());
        assert!(!tmp.path().join("escape").exists());
    }

    #[test]
    fn wildcards_match() {
        assert!(wildcard("*.zip", "a.zip"));
        assert!(wildcard("data?.cab", "data1.cab"));
        assert!(!wildcard("*.zip", "a.rar"));
        assert!(wildcard("a*b*c", "aXXbYYc"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_tar_link_leading_out_of_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        let enc = flate2::write::GzEncoder::new(
            File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(enc);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        tar.append_link(&mut header, "a.txt.tmp", "/tmp/outside")
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let dest = dir.path().join("out");
        let e = extract_tar(&archive, &dest, &CancellationToken::new()).unwrap_err();
        assert!(format!("{e:#}").contains("unsafe link"), "{e:#}");
    }

    #[cfg(unix)]
    #[test]
    fn a_tar_keeps_no_setuid_bit() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        let enc = flate2::write::GzEncoder::new(
            File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(enc);
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o4755);
        header.set_cksum();
        tar.append_data(&mut header, "prog", &b"x"[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let dest = dir.path().join("out");
        extract_tar(&archive, &dest, &CancellationToken::new()).unwrap();
        let mode = fs::metadata(dest.join("prog"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7000, 0);
    }
}
