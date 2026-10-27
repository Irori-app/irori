//! Streaming downloads with progress, cancellation and SHA-512 hashing, and
//! archive extraction (tar.gz / tar.xz / tar.zst / plain tar) in Rust.

use std::fmt;
use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use ember_core::InstallProgress;
use ember_core::install::ProgressSink;
use ember_core::process::CancellationToken;
use once_cell::sync::Lazy;
use sha2::{Digest, Sha512};
use tokio::io::AsyncWriteExt;

/// Error returned when an operation was cancelled by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Whether `err` (or its cause chain) is a cancellation.
pub fn is_cancelled(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.is::<Cancelled>())
}

/// Client for large transfers: no overall timeout (Proton builds are hundreds
/// of MiB), but stalled connections fail after 60 s without data.
static DOWNLOAD_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent(ember_core::http::USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

/// Client for small API requests, with a short overall timeout.
static API_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent(ember_core::http::USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

/// GET a JSON document with a short timeout.
pub async fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    API_CLIENT
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .with_context(|| format!("could not reach {}", host_of(url)))?
        .error_for_status()
        .with_context(|| format!("request to {} failed", host_of(url)))?
        .json()
        .await
        .with_context(|| format!("unexpected response from {}", host_of(url)))
}

/// GET a small text document with a short timeout.
pub async fn get_text(url: &str) -> Result<String> {
    API_CLIENT
        .get(url)
        .send()
        .await
        .with_context(|| format!("could not reach {}", host_of(url)))?
        .error_for_status()
        .with_context(|| format!("request to {} failed", host_of(url)))?
        .text()
        .await
        .with_context(|| format!("unexpected response from {}", host_of(url)))
}

fn host_of(url: &str) -> &str {
    url.split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or(url)
}

/// Result of a completed download.
#[derive(Debug, Clone)]
pub struct Downloaded {
    pub path: PathBuf,
    pub bytes: u64,
    /// Lower-case hex SHA-512 of the file.
    pub sha512: String,
}

/// Download `url` to `dest`, reporting progress and hashing on the fly. Data
/// goes to `<dest>.part` first and is renamed on success, so an interrupted
/// download never leaves a truncated file at `dest`.
pub async fn download(
    url: &str,
    dest: &Path,
    progress: &ProgressSink,
    cancel: &CancellationToken,
) -> Result<Downloaded> {
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let result = download_inner(url, &part, progress, cancel).await;
    match result {
        Ok((bytes, sha512)) => {
            tokio::fs::rename(&part, dest)
                .await
                .with_context(|| format!("saving {}", dest.display()))?;
            Ok(Downloaded {
                path: dest.to_path_buf(),
                bytes,
                sha512,
            })
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(e)
        }
    }
}

async fn download_inner(
    url: &str,
    part: &Path,
    progress: &ProgressSink,
    cancel: &CancellationToken,
) -> Result<(u64, String)> {
    let request = DOWNLOAD_CLIENT.get(url).send();
    let response = tokio::select! {
        r = request => r.with_context(|| format!("could not reach {}", host_of(url)))?,
        _ = cancel.cancelled() => return Err(Cancelled.into()),
    };
    let mut response = response
        .error_for_status()
        .with_context(|| format!("download from {} failed", host_of(url)))?;
    let total = response.content_length();
    let mut file = tokio::fs::File::create(part)
        .await
        .with_context(|| format!("creating {}", part.display()))?;
    let mut hasher = Sha512::new();
    let mut done = 0u64;
    let started = Instant::now();
    let mut last_report = Instant::now() - Duration::from_secs(1);
    loop {
        let chunk = tokio::select! {
            c = response.chunk() => c.with_context(|| format!("download from {} was interrupted", host_of(url)))?,
            _ = cancel.cancelled() => return Err(Cancelled.into()),
        };
        let Some(chunk) = chunk else { break };
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .with_context(|| format!("writing {}", part.display()))?;
        done += chunk.len() as u64;
        if last_report.elapsed() >= Duration::from_millis(500) {
            last_report = Instant::now();
            progress(transfer_progress(done, total, started.elapsed()));
        }
    }
    file.flush().await?;
    drop(file);
    if let Some(t) = total
        && done < t
    {
        bail!(
            "download from {} ended early ({done} of {t} bytes)",
            host_of(url)
        );
    }
    progress(transfer_progress(
        done,
        total.or(Some(done)),
        started.elapsed(),
    ));
    Ok((done, hex::encode(hasher.finalize())))
}

/// Progress values for a transfer of `done` of `total` bytes after `elapsed`.
pub fn transfer_progress(done: u64, total: Option<u64>, elapsed: Duration) -> InstallProgress {
    let secs = elapsed.as_secs_f64().max(0.001);
    let speed = (done as f64 / secs) as u64;
    InstallProgress {
        percent: total
            .filter(|t| *t > 0)
            .map(|t| (done as f64 * 100.0 / t as f64).min(100.0) as f32),
        bytes_done: Some(done),
        bytes_total: total,
        eta_secs: total
            .filter(|_| speed > 0)
            .map(|t| t.saturating_sub(done) / speed),
        download_speed: Some(speed),
        ..Default::default()
    }
}

/// SHA-512 of a file, hex encoded (blocking; run in `spawn_blocking`).
pub fn sha512_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha512::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Extract the expected SHA-512 for `file_name` from a `.sha512sum` document
/// (`<hex>  <name>`, `<hex> *<name>`, or a lone hex digest).
pub fn parse_sha512sum(text: &str, file_name: &str) -> Option<String> {
    let is_digest = |s: &str| s.len() == 128 && s.chars().all(|c| c.is_ascii_hexdigit());
    let mut lone = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(digest) = parts.next().filter(|d| is_digest(d)) else {
            continue;
        };
        match parts.next() {
            Some(name) => {
                let name = name.trim_start_matches('*');
                let base = name.rsplit('/').next().unwrap_or(name);
                if base == file_name {
                    return Some(digest.to_ascii_lowercase());
                }
                lone.push(digest);
            }
            None => lone.push(digest),
        }
    }
    (lone.len() == 1).then(|| lone[0].to_ascii_lowercase())
}

/// Compare a computed digest with an expected one: exact, case-insensitive.
pub fn checksum_matches(expected: &str, actual: &str) -> bool {
    expected.trim().eq_ignore_ascii_case(actual.trim())
}

/// Compression of an archive, detected from its magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Gzip,
    Bzip2,
    Xz,
    Zstd,
    None,
}

pub fn detect_compression(header: &[u8]) -> Compression {
    if header.starts_with(b"BZh") {
        return Compression::Bzip2;
    }
    if header.starts_with(&[0x1f, 0x8b]) {
        Compression::Gzip
    } else if header.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        Compression::Xz
    } else if header.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Compression::Zstd
    } else {
        Compression::None
    }
}

/// Reader that counts consumed (compressed) bytes for progress reporting.
struct Counting<R> {
    inner: R,
    count: u64,
    total: u64,
    progress: ProgressSink,
    last: Instant,
    started: Instant,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count += n as u64;
        if self.last.elapsed() >= Duration::from_millis(500) {
            self.last = Instant::now();
            let mut p = transfer_progress(self.count, Some(self.total), self.started.elapsed());
            p.disk_speed = p.download_speed.take();
            (self.progress)(p);
        }
        Ok(n)
    }
}

/// Extract a (compressed) tarball into `dest`, dropping the first `strip`
/// path components of every entry (like `tar --strip-components`). Blocking:
/// call from `spawn_blocking`.
///
/// Nothing in the archive can create or change anything outside `dest`:
/// entries with absolute or `..` paths, links whose target resolves outside
/// `dest`, and entries that would be written through a symbolic link fail
/// the extraction.
pub fn extract_tar(
    archive: &Path,
    dest: &Path,
    strip: usize,
    progress: ProgressSink,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut file =
        std::fs::File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut header = [0u8; 6];
    let n = file.read(&mut header)?;
    let compression = detect_compression(&header[..n]);
    drop(file);
    let file = std::fs::File::open(archive)?;
    let counting = Counting {
        inner: BufReader::with_capacity(1 << 16, file),
        count: 0,
        total,
        progress,
        last: Instant::now(),
        started: Instant::now(),
    };
    let reader: Box<dyn Read> = match compression {
        Compression::Bzip2 => Box::new(bzip2::read::BzDecoder::new(counting)),
        Compression::Gzip => Box::new(flate2::read::GzDecoder::new(counting)),
        Compression::Xz => Box::new(xz2::read::XzDecoder::new_multi_decoder(counting)),
        Compression::Zstd => {
            Box::new(zstd::stream::read::Decoder::new(counting).context("invalid zstd archive")?)
        }
        Compression::None => Box::new(counting),
    };
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    let mut tar = tar::Archive::new(reader);
    tar.set_preserve_mtime(true);
    tar.set_overwrite(true);
    let mut wrote_any = false;
    for entry in tar
        .entries()
        .with_context(|| format!("reading {}", archive.display()))?
    {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let mut entry = entry.with_context(|| format!("corrupt archive {}", archive.display()))?;
        let path = entry.path()?.into_owned();
        let Some(rel) = strip_components(&path, strip)? else {
            continue;
        };
        let target = dest.join(&rel);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            ensure_no_symlinks(dest, &rel)?;
            std::fs::create_dir_all(&target)
                .with_context(|| format!("creating {}", target.display()))?;
            continue;
        }
        let parent = rel.parent().unwrap_or(Path::new(""));
        ensure_no_symlinks(dest, parent)?;
        std::fs::create_dir_all(dest.join(parent))
            .with_context(|| format!("creating {}", dest.join(parent).display()))?;
        if kind.is_hard_link() {
            let link = entry
                .link_name()?
                .context("hard link without a target")?
                .into_owned();
            let Some(link_rel) = strip_components(&link, strip)? else {
                continue;
            };
            ensure_no_symlinks(dest, link_rel.parent().unwrap_or(Path::new("")))?;
            let source = dest.join(&link_rel);
            // Only regular files: some systems follow a symbolic link when
            // hard linking it, and the copy fallback always does.
            if !std::fs::symlink_metadata(&source).is_ok_and(|m| m.is_file()) {
                bail!(
                    "unsafe link in archive: {} -> {}",
                    rel.display(),
                    link.display()
                );
            }
            let _ = std::fs::remove_file(&target);
            if std::fs::hard_link(&source, &target).is_err() {
                std::fs::copy(&source, &target)
                    .with_context(|| format!("linking {}", target.display()))?;
            }
            wrote_any = true;
            continue;
        }
        if kind.is_symlink() {
            let link = entry
                .link_name()?
                .context("symbolic link without a target")?
                .into_owned();
            // A link replacing an existing entry could move the target of a
            // link checked earlier (`b -> a/..`, then `a -> .`) outside.
            if std::fs::symlink_metadata(&target).is_ok()
                || resolve_inside(dest, parent, &link, &mut 0).is_none()
            {
                bail!(
                    "unsafe link in archive: {} -> {}",
                    rel.display(),
                    link.display()
                );
            }
        }
        entry
            .unpack(&target)
            .with_context(|| format!("extracting {}", rel.display()))?;
        wrote_any = true;
    }
    if !wrote_any {
        bail!("{} contained no files", archive.display());
    }
    Ok(())
}

/// Drop `strip` leading components; `None` when nothing remains. Fails on
/// absolute paths or `..` so entries cannot escape the destination.
fn strip_components(path: &Path, strip: usize) -> Result<Option<PathBuf>> {
    let mut out = PathBuf::new();
    let mut skipped = 0;
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(part) => {
                if skipped < strip {
                    skipped += 1;
                } else {
                    out.push(part);
                }
            }
            _ => bail!("unsafe path in archive: {}", path.display()),
        }
    }
    Ok((!out.as_os_str().is_empty()).then_some(out))
}

/// Fail when any existing component of `rel` under `dest` is a symbolic
/// link, so nothing is created or written through one. Components that do
/// not exist yet are created by the caller as real folders.
fn ensure_no_symlinks(dest: &Path, rel: &Path) -> Result<()> {
    let mut path = dest.to_path_buf();
    for part in rel.components() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                bail!("unsafe path in archive: {} is a link", rel.display())
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

/// Most links followed while resolving one target, as in Linux's ELOOP.
const MAX_LINK_HOPS: u32 = 40;

/// Resolve `link`, the target of a symbolic link in the folder `base`
/// (relative to `dest`), and return where it points relative to `dest`, or
/// `None` when it leaves `dest`. Links already extracted are followed, so
/// `self -> .` cannot make `self/..` look like `dest`. `..` after a
/// component that does not exist yet is refused: the archive could later
/// make that component a link and move the target outside.
fn resolve_inside(dest: &Path, base: &Path, link: &Path, hops: &mut u32) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    let mut exists = true;
    for part in link.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !exists || !out.pop() {
                    return None;
                }
            }
            Component::Normal(name) => {
                out.push(name);
                if !exists {
                    continue;
                }
                match std::fs::symlink_metadata(dest.join(&out)) {
                    Ok(m) if m.file_type().is_symlink() => {
                        *hops += 1;
                        if *hops > MAX_LINK_HOPS {
                            return None;
                        }
                        let next = std::fs::read_link(dest.join(&out)).ok()?;
                        out.pop();
                        out = resolve_inside(dest, &out, &next, hops)?;
                        exists = std::fs::symlink_metadata(dest.join(&out)).is_ok();
                    }
                    Ok(_) => {}
                    Err(_) => exists = false,
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// Total size in bytes of a directory tree (blocking).
pub fn dir_size(path: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(path) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tar(dir: &Path) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        let data = b"#!/bin/sh\necho hi\n";
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "GE-Proton9-1/proton", &data[..])
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "GE-Proton9-1/files/bin/wine", &b"elf"[..])
            .unwrap();
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_cksum();
        builder
            .append_link(&mut link, "GE-Proton9-1/files/bin/wine64", "wine")
            .unwrap();
        let _ = dir;
        builder.into_inner().unwrap()
    }

    fn extract_with(compressed: Vec<u8>, name: &str) {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join(name);
        std::fs::write(&archive, compressed).unwrap();
        let dest = dir.path().join("out");
        extract_tar(
            &archive,
            &dest,
            1,
            ember_core::install::ignore_progress(),
            &CancellationToken::new(),
        )
        .unwrap();
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt as _;
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(dest.join("proton"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }
        assert_eq!(std::fs::read(dest.join("files/bin/wine")).unwrap(), b"elf");
        assert_eq!(
            std::fs::read_link(dest.join("files/bin/wine64")).unwrap(),
            PathBuf::from("wine")
        );
    }

    #[test]
    fn extracts_all_formats() {
        let raw = make_tar(Path::new("."));
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &raw).unwrap();
        extract_with(gz.finish().unwrap(), "a.tar.gz");
        let mut xz = xz2::write::XzEncoder::new(Vec::new(), 1);
        std::io::Write::write_all(&mut xz, &raw).unwrap();
        extract_with(xz.finish().unwrap(), "a.tar.xz");
        extract_with(zstd::encode_all(&raw[..], 1).unwrap(), "a.tar.zst");
        extract_with(raw, "a.tar");
    }

    #[test]
    fn corrupt_archive_fails() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("bad.tar.gz");
        std::fs::write(&archive, [0x1f, 0x8b, 0, 1, 2, 3, 4, 5]).unwrap();
        let res = extract_tar(
            &archive,
            &dir.path().join("out"),
            1,
            ember_core::install::ignore_progress(),
            &CancellationToken::new(),
        );
        assert!(res.is_err());
    }

    #[test]
    fn rejects_escaping_paths() {
        assert!(strip_components(Path::new("a/../../etc"), 1).is_err());
        assert!(strip_components(Path::new("/etc/passwd"), 0).is_err());
        assert_eq!(strip_components(Path::new("top/"), 1).unwrap(), None);
        assert_eq!(
            strip_components(Path::new("./top/x/y"), 1).unwrap(),
            Some(PathBuf::from("x/y"))
        );
    }

    /// One entry of a hand-built archive.
    enum Item<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        Symlink(&'a str, &'a str),
        HardLink(&'a str, &'a str),
    }

    fn build_tar(items: &[Item]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for item in items {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            match *item {
                Item::File(path, data) => {
                    header.set_size(data.len() as u64);
                    builder.append_data(&mut header, path, data).unwrap();
                }
                Item::Dir(path) => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_mode(0o755);
                    header.set_size(0);
                    builder
                        .append_data(&mut header, path, std::io::empty())
                        .unwrap();
                }
                Item::Symlink(path, target) | Item::HardLink(path, target) => {
                    header.set_entry_type(match item {
                        Item::Symlink(..) => tar::EntryType::Symlink,
                        _ => tar::EntryType::Link,
                    });
                    header.set_size(0);
                    builder.append_link(&mut header, path, target).unwrap();
                }
            }
        }
        builder.into_inner().unwrap()
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

        fn extract(&self, items: &[Item], strip: usize) -> Result<()> {
            let archive = self.root.join("a.tar");
            std::fs::write(&archive, build_tar(items)).unwrap();
            extract_tar(
                &archive,
                &self.dest(),
                strip,
                ember_core::install::ignore_progress(),
                &CancellationToken::new(),
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
    }

    #[test]
    fn refuses_writing_through_an_archive_symlink() {
        let sb = Sandbox::new();
        let outside = sb.outside();
        let res = sb.extract(
            &[
                Item::Symlink("dir", &outside),
                Item::File("dir/x", b"pwned"),
                Item::Dir("dir/sub"),
            ],
            0,
        );
        assert!(res.is_err());
        sb.assert_outside_untouched();
    }

    #[test]
    fn refuses_absolute_link_targets() {
        let sb = Sandbox::new();
        let outside = sb.outside();
        assert!(sb.extract(&[Item::Symlink("abs", &outside)], 0).is_err());
        assert!(sb.dest().join("abs").symlink_metadata().is_err());
        assert!(sb.extract(&[Item::Symlink("root", "/")], 0).is_err());
        assert!(sb.dest().join("root").symlink_metadata().is_err());
    }

    #[test]
    fn refuses_link_targets_climbing_out() {
        let sb = Sandbox::new();
        let res = sb.extract(&[Item::Symlink("up", "../outside")], 0);
        assert!(res.is_err());
        assert!(sb.dest().join("up").symlink_metadata().is_err());
        let res = sb.extract(&[Item::Symlink("a/b/up", "../../../outside/secret")], 0);
        assert!(res.is_err());
        // With `strip`, a link climbing out of the top folder leaves `dest`.
        let res = sb.extract(&[Item::Symlink("top/up", "../../outside")], 1);
        assert!(res.is_err());
        sb.assert_outside_untouched();
    }

    #[test]
    fn refuses_hard_links_outside() {
        let sb = Sandbox::new();
        let res = sb.extract(&[Item::HardLink("h", "../outside/secret")], 0);
        assert!(res.is_err());
        sb.assert_outside_untouched();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_links_resolving_out_through_earlier_links() {
        let sb = Sandbox::new();
        // Read lexically, `self/..` is `dest`; on disk it is `dest/..`.
        let res = sb.extract(
            &[
                Item::Symlink("self", "."),
                Item::Symlink("up", "self/.."),
                Item::File("up/x", b"pwned"),
            ],
            0,
        );
        assert!(res.is_err());
        assert!(sb.dest().join("up").symlink_metadata().is_err());
        sb.assert_outside_untouched();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_links_replacing_entries() {
        let sb = Sandbox::new();
        // `up -> a/..` is `dest` while `a` is a folder link, `dest/..` after.
        let res = sb.extract(
            &[
                Item::Dir("d/e"),
                Item::Symlink("a", "d/e"),
                Item::Symlink("up", "a/.."),
                Item::Symlink("a", "."),
                Item::File("up/x", b"pwned"),
            ],
            0,
        );
        assert!(res.is_err());
        assert_eq!(
            std::fs::read_link(sb.dest().join("a")).unwrap(),
            Path::new("d/e")
        );
        sb.assert_outside_untouched();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_writing_through_an_existing_symlink() {
        let sb = Sandbox::new();
        std::os::unix::fs::symlink(sb.outside(), sb.dest().join("dir")).unwrap();
        assert!(sb.extract(&[Item::File("dir/x", b"pwned")], 0).is_err());
        assert!(sb.extract(&[Item::Dir("dir/sub")], 0).is_err());
        assert!(sb.extract(&[Item::Dir("dir")], 0).is_err());
        // Linking to `dir/secret` would share the outside file's inode.
        assert!(sb.extract(&[Item::HardLink("h", "dir/secret")], 0).is_err());
        assert!(sb.dest().join("h").symlink_metadata().is_err());
        assert!(sb.extract(&[Item::Symlink("dir/l", "../l")], 0).is_err());
        sb.assert_outside_untouched();
    }

    #[cfg(unix)]
    #[test]
    fn replaces_a_final_symlink_instead_of_following_it() {
        let sb = Sandbox::new();
        let secret = format!("{}/secret", sb.outside());
        std::os::unix::fs::symlink(&secret, sb.dest().join("f")).unwrap();
        sb.extract(&[Item::File("f", b"mine")], 0).unwrap();
        assert!(!sb.dest().join("f").symlink_metadata().unwrap().is_symlink());
        assert_eq!(std::fs::read(sb.dest().join("f")).unwrap(), b"mine");
        sb.assert_outside_untouched();
    }

    #[cfg(unix)]
    #[test]
    fn keeps_links_inside_the_tree() {
        let sb = Sandbox::new();
        sb.extract(
            &[
                Item::File("GE-Proton/files/lib64/libfoo.so", b"so"),
                Item::Symlink("GE-Proton/files/lib", "lib64"),
                Item::File("GE-Proton/files/bin/wine", b"elf"),
                Item::Symlink("GE-Proton/files/bin/wine64", "wine"),
                Item::Symlink("GE-Proton/files/bin/libfoo.so", "../lib/libfoo.so"),
                Item::HardLink(
                    "GE-Proton/files/bin/wine-preloader",
                    "GE-Proton/files/bin/wine",
                ),
                // A link may name a folder that comes later in the archive.
                Item::Symlink("GE-Proton/files/share/pfx/dosdevices/c:", "../drive_c"),
                Item::Dir("GE-Proton/files/share/pfx/drive_c"),
                Item::File("GE-Proton/files/share/pfx/drive_c/x.txt", b"c"),
            ],
            1,
        )
        .unwrap();
        let files = sb.dest().join("files");
        assert_eq!(std::fs::read(files.join("lib/libfoo.so")).unwrap(), b"so");
        assert_eq!(std::fs::read(files.join("bin/wine64")).unwrap(), b"elf");
        assert_eq!(std::fs::read(files.join("bin/libfoo.so")).unwrap(), b"so");
        assert_eq!(
            std::fs::read(files.join("bin/wine-preloader")).unwrap(),
            b"elf"
        );
        assert_eq!(
            std::fs::read(files.join("share/pfx/dosdevices/c:/x.txt")).unwrap(),
            b"c"
        );
        sb.assert_outside_untouched();
    }

    #[test]
    fn checksum_parsing_is_exact() {
        let digest = "a".repeat(128);
        let other = "b".repeat(128);
        let text = format!("{other}  other.tar.gz\n{digest}  GE-Proton9-1.tar.gz\n");
        assert_eq!(
            parse_sha512sum(&text, "GE-Proton9-1.tar.gz"),
            Some(digest.clone())
        );
        assert_eq!(
            parse_sha512sum(&format!("{digest} *dir/x.tar.xz"), "x.tar.xz"),
            Some(digest.clone())
        );
        assert_eq!(
            parse_sha512sum(&digest.to_uppercase(), "x"),
            Some(digest.clone())
        );
        assert_eq!(parse_sha512sum("not a digest", "x"), None);
        assert_eq!(parse_sha512sum(&text, "missing.tar.gz"), None);
        assert!(checksum_matches(&digest, &digest.to_uppercase()));
        let longer = format!("{digest}{other}");
        assert!(!checksum_matches(&longer, &digest));
    }

    #[test]
    fn progress_math() {
        let p = transfer_progress(50, Some(100), Duration::from_secs(1));
        assert_eq!(p.percent, Some(50.0));
        assert_eq!(p.download_speed, Some(50));
        assert_eq!(p.eta_secs, Some(1));
        assert_eq!(
            transfer_progress(1, None, Duration::from_secs(1)).percent,
            None
        );
    }

    #[test]
    fn detects_compression() {
        assert_eq!(detect_compression(&[0x1f, 0x8b, 8]), Compression::Gzip);
        assert_eq!(
            detect_compression(&[0xfd, b'7', b'z', b'X', b'Z', 0]),
            Compression::Xz
        );
        assert_eq!(
            detect_compression(&[0x28, 0xb5, 0x2f, 0xfd]),
            Compression::Zstd
        );
        assert_eq!(detect_compression(b"ustar"), Compression::None);
    }
}
