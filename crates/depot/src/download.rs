//! The download engine: builds files from chunks with bounded parallelism,
//! resumes interrupted work and verifies everything it writes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use futures::future::BoxFuture;
use futures::{StreamExt as _, TryStreamExt as _};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::{OnceCell, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::hash::{Digest, hash_file};
use crate::path;
use crate::plan::{Chunk, Codec, FileEntry, Part};
use crate::progress::{INTERVAL, Meter, ProgressFn, Stats};

/// Supplies short-lived download addresses for chunks (stores whose CDN
/// needs per-chunk signatures). Called with up to [`SIGN_BATCH`] chunk keys
/// in download order; returns key → URL.
pub trait UrlSigner: Send + Sync {
    fn sign<'a>(&'a self, keys: &'a [String]) -> BoxFuture<'a, Result<HashMap<String, String>>>;
}

/// Chunk keys signed per [`UrlSigner::sign`] call.
pub const SIGN_BATCH: usize = 50;

/// A shared [`UrlSigner`].
#[derive(Clone)]
pub struct Signer(pub Arc<dyn UrlSigner>);

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Signer")
    }
}

/// Tuning of a download.
#[derive(Debug, Clone)]
pub struct Options {
    /// Concurrent HTTP requests (0: 8).
    pub workers: usize,
    /// Bandwidth cap in bytes per second (0: unlimited).
    pub max_bytes_per_sec: u64,
    /// Records finished files so a resumed download skips them without
    /// hashing. Deleted when the download completes.
    pub journal: Option<PathBuf>,
    /// Memory for chunks shared by several files (0: 256 MiB).
    pub cache_bytes: u64,
    /// Attempts per chunk or file after the first.
    pub retries: u32,
    /// Signs chunk addresses instead of using [`Chunk::urls`].
    pub signer: Option<Signer>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            workers: 0,
            max_bytes_per_sec: 0,
            journal: None,
            cache_bytes: 0,
            retries: 5,
            signer: None,
        }
    }
}

/// What to put where.
#[derive(Debug, Clone, Copy)]
pub struct Job<'a> {
    /// Install directory.
    pub root: &'a Path,
    /// Files to write (files already intact on disk are skipped).
    pub files: &'a [FileEntry],
    /// Paths deleted once every file is in place (files an update dropped).
    pub remove: &'a [String],
}

/// How a download ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finished {
    Done,
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("cancelled")]
    Cancelled,
    #[error("the server answered {status} for {url}")]
    Status {
        status: reqwest::StatusCode,
        url: String,
    },
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("{0}")]
    Corrupt(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    fn retryable(&self) -> bool {
        match self {
            Error::Status { status, .. } => {
                status.is_server_error()
                    || *status == reqwest::StatusCode::REQUEST_TIMEOUT
                    || *status == reqwest::StatusCode::TOO_MANY_REQUESTS
            }
            Error::Network(_) | Error::Corrupt(_) => true,
            Error::Cancelled | Error::Io(_) | Error::Other(_) => false,
        }
    }
}

/// Download `job`. Files already intact are skipped, partial whole-file
/// downloads resume, and every file is checked against its digest before it
/// replaces the installed one.
pub async fn download(
    client: &reqwest::Client,
    job: Job<'_>,
    opts: &Options,
    progress: ProgressFn,
    cancel: &CancellationToken,
) -> Result<Finished> {
    for f in job.files {
        if path::normalize(&f.path)? != f.path {
            bail!("file path {:?} is not normalised", f.path);
        }
    }
    tokio::fs::create_dir_all(job.root)
        .await
        .with_context(|| format!("Could not create {}", job.root.display()))?;
    check_free_space(job.root, job.files).await?;

    let total = job.files.iter().map(|f| f.size).sum();
    let stats = Arc::new(Stats::new(total));
    let workers = if opts.workers == 0 { 8 } else { opts.workers };
    let cancel = cancel.child_token();
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        root: job.root.to_path_buf(),
        requests: Semaphore::new(workers),
        limiter: (opts.max_bytes_per_sec > 0).then(|| RateLimiter::new(opts.max_bytes_per_sec)),
        cache: ChunkCache::new(
            job.files,
            if opts.cache_bytes == 0 {
                256 << 20
            } else {
                opts.cache_bytes
            },
        ),
        stats: stats.clone(),
        cancel: cancel.clone(),
        journal: match &opts.journal {
            Some(p) => Some(Journal::open(p).await?),
            None => None,
        },
        retries: opts.retries,
        signing: opts
            .signer
            .as_ref()
            .map(|s| Signing::new(s.0.clone(), job.files)),
        prefetch: (workers / 2).clamp(4, 16),
    });

    // Owned items keep the future `Send` (borrowing closures trip up
    // higher-ranked lifetime inference).
    let files: Arc<[FileEntry]> = job.files.into();
    let work = futures::stream::iter(0..files.len())
        .map(move |i| {
            let (ctx, files) = (ctx.clone(), files.clone());
            async move { ctx.process_file(&files[i]).await }
        })
        .buffer_unordered(workers)
        .try_for_each(|()| async { Ok(()) });
    let result = with_progress(stats, progress, work).await;
    match result {
        Ok(()) => {}
        Err(_) if cancel.is_cancelled() => return Ok(Finished::Cancelled),
        Err(e) => {
            cancel.cancel();
            return Err(anyhow::Error::from(e).context("Download failed"));
        }
    }

    for rel in job.remove {
        let rel = path::normalize(rel)?;
        let p = path::join(job.root, &rel);
        match tokio::fs::remove_file(&p).await {
            Ok(()) => path::prune_empty_parents(job.root, &rel),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("removing {}: {e}", p.display()),
        }
    }
    if let Some(journal) = &opts.journal
        && let Err(e) = tokio::fs::remove_file(journal).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("removing {}: {e}", journal.display());
    }
    Ok(Finished::Done)
}

/// Run `fut`, reporting progress on a timer and once at the end.
pub(crate) async fn with_progress<F: std::future::Future>(
    stats: Arc<Stats>,
    sink: ProgressFn,
    fut: F,
) -> F::Output {
    let mut meter = Meter::new(stats);
    let mut tick = tokio::time::interval(INTERVAL);
    tokio::pin!(fut);
    loop {
        tokio::select! {
            out = &mut fut => {
                sink(meter.sample());
                return out;
            }
            _ = tick.tick() => sink(meter.sample()),
        }
    }
}

async fn check_free_space(root: &Path, files: &[FileEntry]) -> Result<()> {
    let root = root.to_path_buf();
    let needed: u64 = {
        let sizes: Vec<(String, u64)> = files.iter().map(|f| (f.path.clone(), f.size)).collect();
        let root = root.clone();
        tokio::task::spawn_blocking(move || {
            sizes
                .iter()
                .map(|(rel, size)| {
                    let have = std::fs::metadata(path::join(&root, rel)).map_or(0, |m| m.len());
                    size.saturating_sub(have)
                })
                .sum()
        })
        .await?
    };
    match fs4::available_space(&root) {
        Ok(free) if free < needed => bail!(
            "Not enough free disk space in {}: {} MiB needed, {} MiB available",
            root.display(),
            needed.div_ceil(1 << 20),
            free >> 20
        ),
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::warn!("free space of {}: {e}", root.display());
            Ok(())
        }
    }
}

struct Ctx {
    client: reqwest::Client,
    root: PathBuf,
    requests: Semaphore,
    limiter: Option<RateLimiter>,
    cache: ChunkCache,
    stats: Arc<Stats>,
    cancel: CancellationToken,
    journal: Option<Journal>,
    retries: u32,
    signing: Option<Signing>,
    /// Chunks fetched ahead of the one being written, per file.
    prefetch: usize,
}

/// Where the bytes of one part come from.
enum PartData {
    Chunk(Arc<Vec<u8>>),
    Local(Vec<u8>),
}

impl Ctx {
    fn check_cancel(&self) -> Result<(), Error> {
        if self.cancel.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    async fn backoff(&self, attempt: u32) -> Result<(), Error> {
        let delay = Duration::from_millis(500 << attempt.min(5));
        tokio::select! {
            _ = self.cancel.cancelled() => Err(Error::Cancelled),
            _ = tokio::time::sleep(delay) => Ok(()),
        }
    }

    async fn throttle(&self, n: usize) -> Result<(), Error> {
        let Some(limiter) = &self.limiter else {
            return Ok(());
        };
        let wait = limiter.reserve(n as u64);
        if wait.is_zero() {
            return Ok(());
        }
        tokio::select! {
            _ = self.cancel.cancelled() => Err(Error::Cancelled),
            _ = tokio::time::sleep(wait) => Ok(()),
        }
    }

    async fn process_file(&self, f: &FileEntry) -> Result<(), Error> {
        self.check_cancel()?;
        let dest = path::join(&self.root, &f.path);
        let on_disk = file_len(&dest).await;
        let journaled = self.journal.as_ref().is_some_and(|j| j.has(f));
        if on_disk == Some(f.size) && (journaled || self.intact(&dest, f).await?) {
            self.skip(f);
            if !journaled {
                self.record(f).await;
            }
            return Ok(());
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let mut attempt = 0;
        loop {
            let result = if f.parts.is_empty() {
                self.write_empty(f, &dest).await
            } else if f.is_stream() {
                self.stream_file(f, &dest, attempt).await
            } else {
                // Local copies are only trusted on the first attempt.
                self.assemble(f, &dest, attempt == 0).await
            };
            match result {
                Ok(()) => break,
                Err(e)
                    if e.retryable() && attempt < self.retries && !self.cancel.is_cancelled() =>
                {
                    tracing::warn!("{} (attempt {}): {e}", f.path, attempt + 1);
                    attempt += 1;
                    self.backoff(attempt).await?;
                }
                Err(e) => {
                    return Err(match e {
                        Error::Cancelled => e,
                        e => Error::Other(anyhow::Error::from(e).context(f.path.clone())),
                    });
                }
            }
        }
        if f.executable {
            set_executable(&dest).await;
        }
        self.record(f).await;
        Ok(())
    }

    /// Whether the file on disk (of the right size) matches its digest.
    async fn intact(&self, dest: &Path, f: &FileEntry) -> Result<bool, Error> {
        let Some(hash) = f.hash.clone() else {
            return Ok(true);
        };
        let dest = dest.to_path_buf();
        let cancel = self.cancel.clone();
        let res = tokio::task::spawn_blocking(move || {
            hash_file(&dest, &hash, |_| !cancel.is_cancelled())
        })
        .await
        .map_err(|e| Error::Other(e.into()))?;
        match res {
            Ok(ok) => Ok(ok),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Err(Error::Cancelled),
            Err(e) => Err(e.into()),
        }
    }

    fn skip(&self, f: &FileEntry) {
        self.stats.add_done(f.size);
        for p in &f.parts {
            self.cache.release(&p.chunk.key);
        }
    }

    async fn record(&self, f: &FileEntry) {
        if let Some(j) = &self.journal {
            j.record(f).await;
        }
    }

    async fn write_empty(&self, f: &FileEntry, dest: &Path) -> Result<(), Error> {
        if f.size != 0 {
            return Err(Error::Other(anyhow::anyhow!(
                "the manifest lists no content for a {}-byte file",
                f.size
            )));
        }
        tokio::fs::File::create(dest).await?;
        Ok(())
    }

    /// Download a whole-file object into `<dest>.ember-part`, resuming what
    /// an earlier attempt left, then verify and move it into place.
    async fn stream_file(&self, f: &FileEntry, dest: &Path, attempt: u32) -> Result<(), Error> {
        let part = path::part_path(dest);
        let chunk = &f.parts[0].chunk;
        if chunk.urls.is_empty() && self.signing.is_none() {
            return Err(Error::Other(anyhow::anyhow!("no download address")));
        }
        let mut counted = 0u64;
        let result = async {
            if let Some(s) = &self.signing {
                let url = s.url(&chunk.key).await?;
                let res = self
                    .stream_once(&url, &part, chunk.download_size, chunk.range, &mut counted)
                    .await;
                if matches!(&res, Err(Error::Status { status, .. }) if matches!(status.as_u16(), 401 | 403 | 404 | 410))
                {
                    // An expired signature or link: sign again on the next attempt.
                    s.forget(&chunk.key);
                    return Err(Error::Corrupt("the download address expired".into()));
                }
                res?;
            } else {
                // Other mirrors are tried right away when one refuses the request.
                let n = chunk.urls.len();
                let mut tried = 0;
                loop {
                    let url = &chunk.urls[(attempt as usize + tried) % n];
                    match self
                        .stream_once(url, &part, chunk.download_size, chunk.range, &mut counted)
                        .await
                    {
                        Err(Error::Status { .. }) if tried + 1 < n => {
                            self.stats.sub_done(counted);
                            counted = 0;
                            tried += 1;
                        }
                        other => break other,
                    }
                }?;
            }
            let finished = if matches!(chunk.codec, Codec::Deflate) {
                let inflated = part.with_extension("ember-inflate");
                let (src, dst) = (part.clone(), inflated.clone());
                tokio::task::spawn_blocking(move || inflate_file(&src, &dst))
                    .await
                    .map_err(|e| Error::Other(e.into()))?
                    .map_err(|e| Error::Corrupt(format!("{}: {e}", f.path)))?;
                let _ = tokio::fs::remove_file(&part).await;
                // Progress counted compressed bytes; account for the file.
                self.stats.sub_done(counted);
                self.stats.add_done(f.size);
                counted = f.size;
                inflated
            } else {
                part.clone()
            };
            if !self.intact(&finished, f).await? {
                let _ = tokio::fs::remove_file(&finished).await;
                return Err(Error::Corrupt(format!(
                    "{} does not match its checksum",
                    f.path
                )));
            }
            tokio::fs::rename(&finished, dest).await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            // The next attempt counts whatever of the part file survives.
            self.stats.sub_done(counted);
        }
        result
    }

    async fn stream_once(
        &self,
        url: &str,
        part: &Path,
        size: u64,
        range: Option<(u64, u64)>,
        counted: &mut u64,
    ) -> Result<(), Error> {
        let mut have = file_len(part).await.unwrap_or(0);
        if have > size {
            tokio::fs::remove_file(part).await?;
            have = 0;
        }
        // What an earlier attempt left on disk counts as done.
        self.stats.add_done(have);
        *counted = have;
        if have == size {
            return Ok(());
        }

        let _permit = self
            .requests
            .acquire()
            .await
            .map_err(|e| Error::Other(e.into()))?;
        let mut req = self.client.get(url);
        match range {
            Some((start, len)) => {
                req = req.header(
                    reqwest::header::RANGE,
                    format!("bytes={}-{}", start + have, start + len - 1),
                )
            }
            None if have > 0 => req = req.header(reqwest::header::RANGE, format!("bytes={have}-")),
            None => {}
        }
        let resp = tokio::select! {
            _ = self.cancel.cancelled() => return Err(Error::Cancelled),
            r = req.send() => r?,
        };
        let status = resp.status();
        let append = match status {
            reqwest::StatusCode::PARTIAL_CONTENT => have > 0,
            s if s.is_success() && range.is_some() => {
                return Err(Error::Other(anyhow::anyhow!(
                    "the server does not support partial downloads"
                )));
            }
            s if s.is_success() => false,
            status => {
                return Err(Error::Status {
                    status,
                    url: redact(url),
                });
            }
        };
        let file = if append {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(part)
                .await?
        } else {
            self.stats.sub_done(*counted);
            *counted = 0;
            have = 0;
            tokio::fs::File::create(part).await?
        };
        let mut out = tokio::io::BufWriter::with_capacity(1 << 20, file);
        let mut body = resp.bytes_stream();
        loop {
            let next = tokio::select! {
                _ = self.cancel.cancelled() => {
                    out.flush().await?;
                    return Err(Error::Cancelled);
                }
                n = body.next() => n,
            };
            let bytes = match next {
                None => break,
                Some(Ok(b)) => b,
                Some(Err(e)) => {
                    // Keep what arrived for the next attempt.
                    out.flush().await?;
                    return Err(e.into());
                }
            };
            let n = bytes.len() as u64;
            if have + n > size {
                drop(out);
                tokio::fs::remove_file(part).await?;
                return Err(Error::Corrupt(
                    "the server sent more data than expected".into(),
                ));
            }
            self.throttle(bytes.len()).await?;
            out.write_all(&bytes).await?;
            have += n;
            *counted += n;
            self.stats.add_received(n);
            self.stats.add_written(n);
            self.stats.add_done(n);
        }
        out.flush().await?;
        if have != size {
            return Err(Error::Corrupt(format!(
                "the connection closed after {have} of {size} bytes"
            )));
        }
        Ok(())
    }

    /// Build a file from chunk slices (or local copies of them), verify it
    /// and move it into place.
    async fn assemble(&self, f: &FileEntry, dest: &Path, use_local: bool) -> Result<(), Error> {
        let part = path::part_path(dest);
        let mut out =
            tokio::io::BufWriter::with_capacity(1 << 20, tokio::fs::File::create(&part).await?);
        let mut hasher = f.hash.as_ref().map(Digest::hasher);
        let mut written = 0u64;
        let result = async {
            let mut parts = futures::stream::iter(f.parts.clone())
                .map(|p| async move {
                    let data = self.part_data(&p, use_local).await?;
                    Ok::<_, Error>((p, data))
                })
                .buffered(self.prefetch);
            while let Some(next) = parts.next().await {
                let (p, data) = next?;
                let slice = match &data {
                    PartData::Local(bytes) => bytes.as_slice(),
                    PartData::Chunk(chunk) => usize::try_from(p.offset)
                        .ok()
                        .zip(usize::try_from(p.offset + p.size).ok())
                        .and_then(|(start, end)| chunk.get(start..end))
                        .ok_or_else(|| {
                            Error::Corrupt("a file part lies outside its chunk".into())
                        })?,
                };
                out.write_all(slice).await?;
                if let Some(h) = &mut hasher {
                    h.update(slice);
                }
                self.cache.release(&p.chunk.key);
                let n = slice.len() as u64;
                written += n;
                self.stats.add_written(n);
                self.stats.add_done(n);
            }
            out.flush().await?;
            drop(out);
            if written != f.size {
                return Err(Error::Corrupt(format!(
                    "{} has {written} bytes instead of {}",
                    f.path, f.size
                )));
            }
            if let (Some(h), Some(expected)) = (hasher, &f.hash)
                && h.finish() != expected.value
            {
                return Err(Error::Corrupt(format!(
                    "{} does not match its checksum",
                    f.path
                )));
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            self.stats.sub_done(written);
            if !matches!(e, Error::Cancelled) {
                let _ = tokio::fs::remove_file(&part).await;
            }
            return Err(e);
        }
        tokio::fs::rename(&part, dest).await?;
        Ok(())
    }

    /// The bytes of a part: read from the local copy when allowed and
    /// readable, else from its (shared, cached) chunk.
    async fn part_data(&self, p: &Part, use_local: bool) -> Result<PartData, Error> {
        if use_local && let Some(reuse) = &p.reuse {
            let src = path::join(&self.root, &reuse.path);
            let (offset, size) = (reuse.offset, p.size);
            let read = tokio::task::spawn_blocking(move || read_at(&src, offset, size))
                .await
                .map_err(|e| Error::Other(e.into()))?;
            match read {
                Ok(bytes) => return Ok(PartData::Local(bytes)),
                Err(e) => tracing::debug!("reusing {}: {e}; downloading instead", reuse.path),
            }
        }
        Ok(PartData::Chunk(self.chunk(&p.chunk).await?))
    }

    /// A decoded chunk, downloaded once while other files still need it.
    async fn chunk(&self, chunk: &Arc<Chunk>) -> Result<Arc<Vec<u8>>, Error> {
        let cell = self.cache.cell(&chunk.key);
        cell.get_or_try_init(|| async {
            let data = self.fetch_chunk(chunk).await?;
            self.cache.add_bytes(data.len() as u64);
            Ok::<_, Error>(Arc::new(data))
        })
        .await
        .cloned()
    }

    async fn fetch_chunk(&self, chunk: &Chunk) -> Result<Vec<u8>, Error> {
        if chunk.urls.is_empty() && self.signing.is_none() {
            return Err(Error::Other(anyhow::anyhow!(
                "chunk {} has no address",
                chunk.key
            )));
        }
        let mut attempt = 0;
        let mut mirror = 0;
        loop {
            let url = match &self.signing {
                Some(s) => s.url(&chunk.key).await?,
                None => chunk.urls[mirror % chunk.urls.len()].clone(),
            };
            match self.fetch_chunk_once(chunk, &url).await {
                Ok(data) => return Ok(data),
                // An expired signature: sign again.
                Err(Error::Status { status, .. })
                    if self.signing.is_some()
                        && matches!(status.as_u16(), 401 | 403)
                        && attempt < self.retries =>
                {
                    if let Some(s) = &self.signing {
                        s.forget(&chunk.key);
                    }
                    attempt += 1;
                }
                // Other mirrors are tried right away when one refuses the request.
                Err(Error::Status { .. })
                    if self.signing.is_none() && mirror + 1 < chunk.urls.len() =>
                {
                    mirror += 1
                }
                Err(e)
                    if e.retryable() && attempt < self.retries && !self.cancel.is_cancelled() =>
                {
                    tracing::warn!("chunk {} (attempt {}): {e}", chunk.key, attempt + 1);
                    attempt += 1;
                    mirror += 1;
                    self.backoff(attempt).await?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn fetch_chunk_once(&self, chunk: &Chunk, url: &str) -> Result<Vec<u8>, Error> {
        let raw = {
            let _permit = self
                .requests
                .acquire()
                .await
                .map_err(|e| Error::Other(e.into()))?;
            let mut req = self.client.get(url);
            if let Some((start, len)) = chunk.range {
                req = req.header(
                    reqwest::header::RANGE,
                    format!("bytes={start}-{}", start + len - 1),
                );
            }
            let resp = tokio::select! {
                _ = self.cancel.cancelled() => return Err(Error::Cancelled),
                r = req.send() => r?,
            };
            if !resp.status().is_success() {
                return Err(Error::Status {
                    status: resp.status(),
                    url: redact(url),
                });
            }
            if chunk.range.is_some() && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                return Err(Error::Other(anyhow::anyhow!(
                    "the server does not support partial downloads"
                )));
            }
            let mut raw = Vec::with_capacity(chunk.download_size as usize);
            let mut body = resp.bytes_stream();
            loop {
                let next = tokio::select! {
                    _ = self.cancel.cancelled() => return Err(Error::Cancelled),
                    n = body.next() => n,
                };
                let Some(bytes) = next else { break };
                let bytes = bytes?;
                self.throttle(bytes.len()).await?;
                self.stats.add_received(bytes.len() as u64);
                raw.extend_from_slice(&bytes);
            }
            raw
        };
        // Decoding and hashing are CPU-bound: keep them off the async workers.
        let (codec, size, hash) = (chunk.codec.clone(), chunk.size, chunk.hash.clone());
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
            let data = decode(&codec, raw).map_err(|e| format!("{e:#}"))?;
            if data.len() as u64 != size {
                return Err(format!("{} bytes instead of {size}", data.len()));
            }
            if hash.is_some_and(|h| !h.matches(&data)) {
                return Err("does not match its checksum".into());
            }
            Ok(data)
        })
        .await
        .map_err(|e| Error::Other(e.into()))?
        .map_err(|e| Error::Corrupt(format!("chunk {}: {e}", chunk.key)))
    }
}

fn decode(codec: &Codec, raw: Vec<u8>) -> Result<Vec<u8>> {
    match codec {
        Codec::Raw => Ok(raw),
        Codec::Zlib => {
            use std::io::Read as _;
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(raw.as_slice()).read_to_end(&mut out)?;
            Ok(out)
        }
        Codec::Deflate => {
            use std::io::Read as _;
            let mut out = Vec::new();
            flate2::read::DeflateDecoder::new(raw.as_slice()).read_to_end(&mut out)?;
            Ok(out)
        }
        Codec::Custom(d) => d.decode(raw),
    }
}

/// Inflate a raw-deflate file.
fn inflate_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    let input = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(src)?);
    let mut decoder = flate2::read::DeflateDecoder::new(input);
    let mut out = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(dst)?);
    std::io::copy(&mut decoder, &mut out)?;
    std::io::Write::flush(&mut out)
}

/// `size` bytes of `path` from `offset`.
fn read_at(path: &Path, offset: u64, size: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read as _, Seek as _};
    let mut file = std::fs::File::open(path)?;
    file.seek(std::io::SeekFrom::Start(offset))?;
    let mut buf = vec![0; usize::try_from(size).map_err(std::io::Error::other)?];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

/// Signed chunk addresses, fetched in batches in download order.
struct Signing {
    signer: Arc<dyn UrlSigner>,
    /// Chunk keys in the order files need them.
    order: Vec<String>,
    index: HashMap<String, usize>,
    urls: std::sync::Mutex<HashMap<String, String>>,
    gate: tokio::sync::Mutex<()>,
}

impl Signing {
    fn new(signer: Arc<dyn UrlSigner>, files: &[FileEntry]) -> Self {
        let mut order = Vec::new();
        let mut index = HashMap::new();
        for p in files.iter().flat_map(|f| &f.parts) {
            if !index.contains_key(&p.chunk.key) {
                index.insert(p.chunk.key.clone(), order.len());
                order.push(p.chunk.key.clone());
            }
        }
        Self {
            signer,
            order,
            index,
            urls: Default::default(),
            gate: Default::default(),
        }
    }

    fn cached(&self, key: &str) -> Option<String> {
        self.urls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }

    fn forget(&self, key: &str) {
        self.urls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }

    /// The signed address of `key`, signing it together with the next
    /// unsigned keys.
    async fn url(&self, key: &str) -> Result<String, Error> {
        if let Some(url) = self.cached(key) {
            return Ok(url);
        }
        let _gate = self.gate.lock().await;
        if let Some(url) = self.cached(key) {
            return Ok(url);
        }
        let start = self.index.get(key).copied();
        let mut batch = vec![key.to_string()];
        if let Some(start) = start {
            let urls = self.urls.lock().unwrap_or_else(|e| e.into_inner());
            batch.extend(
                self.order[start + 1..]
                    .iter()
                    .filter(|k| !urls.contains_key(*k))
                    .take(SIGN_BATCH - 1)
                    .cloned(),
            );
        }
        let signed = self
            .signer
            .sign(&batch)
            .await
            .map_err(|e| Error::Other(e.context("Could not get chunk download addresses")))?;
        let mut urls = self.urls.lock().unwrap_or_else(|e| e.into_inner());
        urls.extend(signed);
        urls.get(key).cloned().ok_or_else(|| {
            Error::Other(anyhow::anyhow!(
                "no download address was issued for chunk {key}"
            ))
        })
    }
}

/// Signed CDN addresses carry credentials in the query; keep them out of logs.
fn redact(url: &str) -> String {
    url.split('?').next().unwrap_or(url).to_string()
}

async fn file_len(p: &Path) -> Option<u64> {
    tokio::fs::metadata(p)
        .await
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len())
}

#[cfg(unix)]
async fn set_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Ok(meta) = tokio::fs::metadata(p).await {
        let mut perm = meta.permissions();
        perm.set_mode(perm.mode() | 0o755);
        if let Err(e) = tokio::fs::set_permissions(p, perm).await {
            tracing::warn!("making {} executable: {e}", p.display());
        }
    }
}

#[cfg(not(unix))]
async fn set_executable(_: &Path) {}

/// Token bucket over virtual time: each reservation pushes the next free
/// moment forward by its transfer time.
struct RateLimiter {
    bytes_per_sec: f64,
    next: std::sync::Mutex<Instant>,
}

impl RateLimiter {
    fn new(bytes_per_sec: u64) -> Self {
        Self {
            bytes_per_sec: bytes_per_sec as f64,
            next: std::sync::Mutex::new(Instant::now()),
        }
    }

    /// How long to wait before sending `n` bytes. Unused capacity carries
    /// over for up to one second.
    fn reserve(&self, n: u64) -> Duration {
        let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let floor = now.checked_sub(Duration::from_secs(1)).unwrap_or(now);
        let start = (*next).max(floor);
        *next = start + Duration::from_secs_f64(n as f64 / self.bytes_per_sec);
        next.saturating_duration_since(now)
    }
}

/// Decoded chunks kept while files that still need them are pending.
struct ChunkCache {
    slots: std::sync::Mutex<HashMap<String, Slot>>,
    budget: u64,
    bytes: std::sync::atomic::AtomicU64,
}

struct Slot {
    remaining: usize,
    size: u64,
    cell: Arc<OnceCell<Arc<Vec<u8>>>>,
}

impl ChunkCache {
    fn new(files: &[FileEntry], budget: u64) -> Self {
        let mut slots: HashMap<String, Slot> = HashMap::new();
        for p in files.iter().flat_map(|f| &f.parts) {
            slots
                .entry(p.chunk.key.clone())
                .or_insert_with(|| Slot {
                    remaining: 0,
                    size: p.chunk.size,
                    cell: Arc::default(),
                })
                .remaining += 1;
        }
        Self {
            slots: std::sync::Mutex::new(slots),
            budget,
            bytes: Default::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Slot>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cell(&self, key: &str) -> Arc<OnceCell<Arc<Vec<u8>>>> {
        self.lock()
            .entry(key.to_string())
            .or_insert_with(|| Slot {
                remaining: 1,
                size: 0,
                cell: Arc::default(),
            })
            .cell
            .clone()
    }

    fn add_bytes(&self, n: u64) {
        self.bytes
            .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }

    fn drop_bytes(&self, slot: &Slot) {
        if slot.cell.initialized() {
            let n = slot.cell.get().map_or(slot.size, |d| d.len() as u64);
            let _ = self.bytes.try_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |b| Some(b.saturating_sub(n)),
            );
        }
    }

    /// One use of the chunk is done: forget it when nothing else needs it,
    /// or when the cache is over budget (it is downloaded again if needed).
    fn release(&self, key: &str) {
        let mut slots = self.lock();
        let Some(slot) = slots.get_mut(key) else {
            return;
        };
        slot.remaining = slot.remaining.saturating_sub(1);
        if slot.remaining == 0 {
            if let Some(slot) = slots.remove(key) {
                self.drop_bytes(&slot);
            }
        } else if self.bytes.load(std::sync::atomic::Ordering::Relaxed) > self.budget
            && slot.cell.initialized()
        {
            let old = std::mem::take(&mut slot.cell);
            let size = slot.size;
            self.drop_bytes(&Slot {
                remaining: 0,
                size,
                cell: old,
            });
        }
    }
}

/// Finished files of an interrupted download.
struct Journal {
    done: HashMap<String, (u64, Option<Digest>)>,
    file: tokio::sync::Mutex<Option<tokio::fs::File>>,
}

#[derive(Serialize, Deserialize)]
struct JournalEntry {
    path: String,
    size: u64,
    hash: Option<Digest>,
}

impl Journal {
    async fn open(path: &Path) -> Result<Self> {
        let done = match tokio::fs::read_to_string(path).await {
            Ok(text) => text
                .lines()
                .filter_map(|l| serde_json::from_str::<JournalEntry>(l).ok())
                .map(|e| (e.path, (e.size, e.hash)))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Self {
            done,
            file: tokio::sync::Mutex::new(Some(file)),
        })
    }

    fn has(&self, f: &FileEntry) -> bool {
        self.done
            .get(&f.path)
            .is_some_and(|(size, hash)| *size == f.size && *hash == f.hash)
    }

    async fn record(&self, f: &FileEntry) {
        let entry = JournalEntry {
            path: f.path.clone(),
            size: f.size,
            hash: f.hash.clone(),
        };
        let Ok(mut line) = serde_json::to_vec(&entry) else {
            return;
        };
        line.push(b'\n');
        let mut guard = self.file.lock().await;
        if let Some(file) = guard.as_mut()
            && let Err(e) = file.write_all(&line).await
        {
            tracing::warn!("download journal: {e}");
            *guard = None;
        }
    }
}
