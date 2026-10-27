//! Downloads into the install cache, resumable through `ember-depot`, and
//! checksum verification. Finished files stay in the cache, so a retry after
//! a failure or a cancel reuses them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use ember_core::process::CancellationToken;
use ember_runtime::Cancelled;
use futures::StreamExt as _;
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use tokio::io::AsyncWriteExt as _;

use crate::context::{Progress, ProgressFn};
use crate::error::ScriptError;
use crate::model::{Checksum, HashAlgorithm};

fn hash_with<D: Digest>(path: &Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = D::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// The hex digest of a file. Blocking.
pub(crate) fn file_digest(path: &Path, algorithm: HashAlgorithm) -> std::io::Result<String> {
    match algorithm {
        HashAlgorithm::Md5 => hash_with::<Md5>(path),
        HashAlgorithm::Sha1 => hash_with::<Sha1>(path),
        HashAlgorithm::Sha256 => hash_with::<Sha256>(path),
        HashAlgorithm::Sha512 => hash_with::<Sha512>(path),
    }
}

/// Fail with [`ScriptError::ChecksumMismatch`] unless `path` matches.
pub(crate) async fn verify(path: &Path, checksum: &Checksum, label: &str) -> Result<()> {
    let p = path.to_path_buf();
    let alg = checksum.algorithm;
    let actual = tokio::task::spawn_blocking(move || file_digest(&p, alg))
        .await?
        .with_context(|| format!("reading {}", path.display()))?;
    if actual != checksum.hex {
        return Err(ScriptError::ChecksumMismatch {
            file: label.into(),
            expected: checksum.to_string(),
            actual: format!("{}:{actual}", alg.name()),
        }
        .into());
    }
    Ok(())
}

/// The file name for a download: the script's, else the address's last
/// part.
pub(crate) fn file_name_for(url: &str, filename: Option<&str>) -> String {
    if let Some(f) = filename {
        return f.to_string();
    }
    let last = url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut s| s.next_back().map(str::to_string))
        })
        .unwrap_or_default();
    let decoded = percent_decode(&last);
    let clean: String = decoded
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if clean.is_empty() || clean == "." || clean == ".." {
        "download".into()
    } else {
        clean
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn client_for(referer: Option<&str>) -> Result<reqwest::Client> {
    let Some(referer) = referer else {
        return Ok(ember_core::http::download_client().clone());
    };
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::REFERER,
        reqwest::header::HeaderValue::from_str(referer)
            .context("the script's referer is not valid")?,
    );
    Ok(reqwest::Client::builder()
        .user_agent(ember_core::http::USER_AGENT)
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()?)
}

/// The size the server announces, if it answers a HEAD request.
async fn remote_size(client: &reqwest::Client, url: &str) -> Option<u64> {
    let resp = client.head(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.headers()
        .get(reqwest::header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
        .filter(|&n| n > 0)
}

/// Sizes of the given downloads (for the consent screen), by HEAD request.
pub(crate) async fn probe_size(url: &str, referer: Option<&str>) -> Option<u64> {
    let client = client_for(referer).ok()?;
    remote_size(&client, url).await
}

/// Download `url` to `<cache>/<id>/<name>`, reusing a finished file.
pub(crate) async fn download(
    id: &str,
    url: &str,
    name: &str,
    referer: Option<&str>,
    cache_dir: &Path,
    cancel: &CancellationToken,
    progress: &ProgressFn,
) -> Result<PathBuf> {
    let dest = cache_dir.join(id).join(name);
    if tokio::fs::metadata(&dest).await.is_ok_and(|m| m.is_file()) {
        return Ok(dest);
    }
    let client = client_for(referer)?;
    let size = remote_size(&client, url).await;
    let rel = format!("{id}/{name}");
    let normalised = ember_depot::path::normalize(&rel)
        .ok()
        .filter(|n| *n == rel);
    match (size, normalised) {
        (Some(size), Some(rel)) => {
            let file_id = id.to_string();
            let report = progress.clone();
            let depot_progress: ember_depot::ProgressFn = Arc::new(move |p| {
                report(Progress::Download {
                    file_id: file_id.clone(),
                    bytes_done: p.bytes_done,
                    bytes_total: Some(p.bytes_total),
                    bytes_per_sec: p.download_speed,
                });
            });
            let files = [ember_depot::FileEntry::whole(
                rel,
                size,
                None,
                vec![url.to_string()],
            )];
            let job = ember_depot::Job {
                root: cache_dir,
                files: &files,
                remove: &[],
            };
            let finished = ember_depot::download(
                &client,
                job,
                &ember_depot::Options::default(),
                depot_progress,
                cancel,
            )
            .await
            .with_context(|| format!("Could not download {url}"))?;
            if finished == ember_depot::Finished::Cancelled {
                return Err(Cancelled.into());
            }
        }
        _ => stream(&client, id, url, &dest, cancel, progress).await?,
    }
    Ok(dest)
}

/// Plain streaming download for servers that announce no size, resuming a
/// partial file when the server honours ranges.
async fn stream(
    client: &reqwest::Client,
    id: &str,
    url: &str,
    dest: &Path,
    cancel: &CancellationToken,
    progress: &ProgressFn,
) -> Result<()> {
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("Could not create {}", dir.display()))?;
    }
    let part = ember_depot::path::part_path(dest);
    let have = tokio::fs::metadata(&part)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let mut req = client.get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = tokio::select! {
        r = req.send() => r.with_context(|| format!("Could not download {url}"))?,
        () = cancel.cancelled() => return Err(Cancelled.into()),
    };
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("Could not download {url}: the server answered {status}");
    }
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
    let total = resp
        .content_length()
        .map(|n| if resumed { n + have } else { n });
    let mut file = if resumed {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(&part)
            .await?
    } else {
        tokio::fs::File::create(&part).await?
    };
    let mut done = if resumed { have } else { 0 };
    let mut body = resp.bytes_stream();
    loop {
        let chunk = tokio::select! {
            c = body.next() => c,
            () = cancel.cancelled() => {
                file.flush().await?;
                return Err(Cancelled.into());
            }
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.with_context(|| format!("Could not download {url}"))?;
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        progress(Progress::Download {
            file_id: id.to_string(),
            bytes_done: done,
            bytes_total: total,
            bytes_per_sec: 0,
        });
    }
    file.flush().await?;
    drop(file);
    if let Some(total) = total
        && done != total
    {
        anyhow::bail!(
            "Could not download {url}: the connection closed after {done} of {total} bytes"
        );
    }
    tokio::fs::rename(&part, dest).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_downloads() {
        assert_eq!(
            file_name_for("https://x.org/a/My%20Game.zip?x=1", None),
            "My Game.zip"
        );
        assert_eq!(file_name_for("https://x.org/", None), "download");
        assert_eq!(file_name_for("https://x.org/a.zip", Some("b.zip")), "b.zip");
    }

    #[test]
    fn digests_match_known_values() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            file_digest(&p, HashAlgorithm::Md5).unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            file_digest(&p, HashAlgorithm::Sha1).unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            file_digest(&p, HashAlgorithm::Sha256).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(
            file_digest(&p, HashAlgorithm::Sha512)
                .unwrap()
                .starts_with("ddaf35a193617aba")
        );
    }
}
