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
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use crate::adapt::{self, Gate, Hosts, Signals, Tuner, Watchdog, Window, classify};
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
    /// Most HTTP requests at once (0: up to 32). The engine measures how
    /// many make the download fastest and stays below this.
    pub workers: usize,
    /// Bandwidth cap in bytes per second (0: unlimited).
    pub max_bytes_per_sec: u64,
    /// Records finished files so a resumed download skips them without
    /// hashing. Deleted when the download completes.
    pub journal: Option<PathBuf>,
    /// Memory for chunks shared by several files (0: 256 MiB).
    pub cache_bytes: u64,
    /// Attempts after the first when a server sends damaged data or
    /// refuses a signed address. Dropped connections, stalls and busy
    /// servers are retried until the download is cancelled.
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
    /// The connection stalled or closed early.
    #[error("{0}")]
    Interrupted(String),
    #[error("{0}")]
    Corrupt(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// How a failed request is retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The connection or a busy server: retried until cancelled.
    Transient,
    /// Damaged data: retried [`Options::retries`] times.
    Corrupt,
    /// Retrying cannot help.
    Fatal,
}

impl Error {
    fn kind(&self) -> Kind {
        match self {
            Error::Status { status, .. }
                if status.is_server_error()
                    || *status == reqwest::StatusCode::REQUEST_TIMEOUT
                    || *status == reqwest::StatusCode::TOO_MANY_REQUESTS =>
            {
                Kind::Transient
            }
            Error::Network(e) if e.is_builder() || e.is_redirect() => Kind::Fatal,
            Error::Network(_) | Error::Interrupted(_) => Kind::Transient,
            Error::Corrupt(_) => Kind::Corrupt,
            Error::Status { .. } | Error::Cancelled | Error::Io(_) | Error::Other(_) => Kind::Fatal,
        }
    }
}

/// A connection that delivers nothing (or a trickle) for this long is
/// dropped for another server.
pub(crate) const STALL: Duration = if cfg!(test) {
    Duration::from_millis(400)
} else {
    Duration::from_secs(8)
};
/// The first wait after a failure; it doubles per failure up to a minute.
const BACKOFF: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_millis(500)
};
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How often the number of requests at once is tuned.
const TUNE_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(2)
};
/// Files hashed at once while checking what is already on disk.
const CHECKERS: usize = 4;

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
    let on_disk = disk_sizes(job.root, job.files).await?;
    check_free_space(job.root, job.files, &on_disk)?;

    let total = job.files.iter().map(|f| f.size).sum();
    let stats = Arc::new(Stats::new(total));
    let max = if opts.workers == 0 {
        adapt::DEFAULT_MAX
    } else {
        opts.workers
    };
    let tuner = Tuner::new(max);
    let cancel = cancel.child_token();
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        root: job.root.to_path_buf(),
        requests: Gate::new(tuner.limit()),
        hosts: Hosts::default(),
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
        prefetch: (max / 4).clamp(4, 8),
    });

    // Owned items keep the future `Send` (borrowing closures trip up
    // higher-ranked lifetime inference).
    let files: Arc<[FileEntry]> = job.files.into();
    let work = {
        let ctx = ctx.clone();
        async move {
            let pending = ctx.check_existing(&files, &on_disk).await?;
            futures::stream::iter(pending)
                .map(move |i| {
                    let (ctx, files) = (ctx.clone(), files.clone());
                    async move { ctx.process_file(&files[i]).await }
                })
                .buffer_unordered(max)
                .try_for_each(|()| async { Ok(()) })
                .await
        }
    };
    let limit = (opts.max_bytes_per_sec > 0).then_some(opts.max_bytes_per_sec);
    let result = with_progress(stats, progress, async {
        tokio::select! {
            biased;
            r = work => r,
            () = tune(&ctx, tuner, limit) => unreachable!("tuning never ends"),
        }
    })
    .await;
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

/// Sizes of the files already below `root`.
async fn disk_sizes(root: &Path, files: &[FileEntry]) -> Result<Vec<Option<u64>>> {
    let root = root.to_path_buf();
    let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    Ok(tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .map(|rel| {
                std::fs::metadata(path::join(&root, rel))
                    .ok()
                    .filter(|m| m.is_file())
                    .map(|m| m.len())
            })
            .collect()
    })
    .await?)
}

fn check_free_space(root: &Path, files: &[FileEntry], on_disk: &[Option<u64>]) -> Result<()> {
    let needed: u64 = files
        .iter()
        .zip(on_disk)
        .map(|(f, have)| f.size.saturating_sub(have.unwrap_or(0)))
        .sum();
    match fs4::available_space(root) {
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

/// Size the request gate to what the connection takes, and tell what
/// holds the download back. Runs until dropped.
async fn tune(ctx: &Ctx, mut tuner: Tuner, limit: Option<u64>) {
    use std::sync::atomic::Ordering::Relaxed;
    let stats = &ctx.stats;
    let snapshot = || {
        (
            stats.received.load(Relaxed),
            stats.failures.load(Relaxed),
            stats.net_wait_us.load(Relaxed),
            stats.disk_wait_us.load(Relaxed),
        )
    };
    let mut last = (Instant::now(), snapshot());
    // A verdict shows once two windows in a row agree.
    let mut candidate = None;
    loop {
        tokio::time::sleep(TUNE_EVERY).await;
        let now = (Instant::now(), snapshot());
        let secs = now.0.duration_since(last.0).as_secs_f64().max(0.001);
        let (received, failures, net_us, disk_us) = (
            now.1.0 - last.1.0,
            now.1.1 - last.1.1,
            now.1.2 - last.1.2,
            now.1.3 - last.1.3,
        );
        last = now;
        let rate = received as f64 / secs;
        let contended = ctx.requests.take_contended();
        let throttled =
            stats.throttling.load(Relaxed) > 0 || limit.is_some_and(|l| rate >= l as f64 * 0.85);
        let n = tuner.step(Window {
            rate,
            failures,
            contended,
            throttled,
        });
        ctx.requests.set_limit(n);
        let verdict = if throttled {
            Some(ember_core::Bottleneck::SpeedLimit)
        } else {
            classify(Signals {
                rate,
                failures,
                backing_off: stats.backing_off.load(Relaxed),
                limit,
                net_wait: Duration::from_micros(net_us),
                disk_wait: Duration::from_micros(disk_us),
                contended,
                capped: tuner.capped(),
            })
        };
        if verdict == candidate {
            stats.set_bottleneck(verdict);
        }
        candidate = verdict;
    }
}

struct Ctx {
    client: reqwest::Client,
    root: PathBuf,
    /// Requests at once, sized by [`tune`].
    requests: Gate,
    hosts: Hosts,
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

    /// Wait out failure number `failures` (1-based) of a request.
    async fn backoff(&self, failures: u32) -> Result<(), Error> {
        let delay = BACKOFF
            .saturating_mul(1 << failures.min(8))
            .min(MAX_BACKOFF);
        let _waiting = Waiting::new(&self.stats.backing_off);
        tokio::select! {
            _ = self.cancel.cancelled() => Err(Error::Cancelled),
            _ = tokio::time::sleep(delay) => Ok(()),
        }
    }

    /// Wait for the speed limit to allow `n` more bytes; returns the wait.
    async fn throttle(&self, n: usize) -> Result<Duration, Error> {
        let Some(limiter) = &self.limiter else {
            return Ok(Duration::ZERO);
        };
        let wait = limiter.reserve(n as u64);
        if wait.is_zero() {
            return Ok(wait);
        }
        let _waiting = Waiting::new(&self.stats.throttling);
        tokio::select! {
            _ = self.cancel.cancelled() => Err(Error::Cancelled),
            _ = tokio::time::sleep(wait) => Ok(wait),
        }
    }

    /// Note how a request to `url` that ran for `elapsed` ended.
    fn score<T>(&self, url: &str, result: &Result<T, Error>, bytes: u64, elapsed: Duration) {
        match result {
            Ok(_) => self.hosts.record_ok(url, bytes, elapsed),
            Err(e) if e.kind() == Kind::Transient => {
                self.hosts.record_failure(url);
                self.stats.add_failure();
            }
            Err(e) if e.kind() == Kind::Corrupt => self.hosts.record_failure(url),
            Err(_) => {}
        }
    }

    /// Skip files already in place: recorded in the journal, or of the
    /// right size and matching their digest. Returns the indices of the
    /// files still to download.
    async fn check_existing(
        &self,
        files: &[FileEntry],
        on_disk: &[Option<u64>],
    ) -> Result<Vec<usize>, Error> {
        let mut pending = Vec::new();
        let mut to_hash = Vec::new();
        for (i, f) in files.iter().enumerate() {
            if on_disk[i] != Some(f.size) {
                pending.push(i);
            } else if self.journal.as_ref().is_some_and(|j| j.has(f)) {
                self.skip(f);
            } else if f.hash.is_none() {
                self.skip(f);
                self.record(f).await;
            } else {
                to_hash.push(i);
            }
        }
        use std::sync::atomic::Ordering::Relaxed;
        self.stats.check_total.store(to_hash.len() as u64, Relaxed);
        let checked: Vec<(usize, bool)> = futures::stream::iter(to_hash)
            .map(|i| async move {
                let f = &files[i];
                let ok = self.intact(&path::join(&self.root, &f.path), f).await?;
                self.stats.checked.fetch_add(1, Relaxed);
                Ok::<_, Error>((i, ok))
            })
            .buffer_unordered(CHECKERS)
            .try_collect()
            .await?;
        for (i, ok) in checked {
            if ok {
                self.skip(&files[i]);
                self.record(&files[i]).await;
            } else {
                pending.push(i);
            }
        }
        pending.sort_unstable();
        Ok(pending)
    }

    async fn process_file(&self, f: &FileEntry) -> Result<(), Error> {
        self.check_cancel()?;
        let dest = path::join(&self.root, &f.path);
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Chunks retry on their own; whole files retry here, moving to the
        // next server after each failure.
        let order = match f.parts.first() {
            Some(p) if f.is_stream() => self.hosts.order(&p.chunk.urls),
            _ => Vec::new(),
        };
        let mirrors = order.len().max(1) as u32;
        let (mut attempt, mut failures, mut corrupt) = (0, 0, 0);
        loop {
            let result = if f.parts.is_empty() {
                self.write_empty(f, &dest).await
            } else if f.is_stream() {
                self.stream_file(f, &dest, &order, attempt).await
            } else {
                // Local copies are only trusted on the first attempt.
                self.assemble(f, &dest, attempt == 0).await
            };
            let e = match result {
                Ok(()) => break,
                Err(e) => e,
            };
            let kind = e.kind();
            if self.cancel.is_cancelled()
                || kind == Kind::Fatal
                || (kind == Kind::Corrupt && corrupt >= self.retries)
            {
                return Err(match e {
                    Error::Cancelled => e,
                    e => Error::Other(anyhow::Error::from(e).context(f.path.clone())),
                });
            }
            tracing::warn!("{} (attempt {}): {e}", f.path, attempt + 1);
            attempt += 1;
            if kind == Kind::Corrupt {
                corrupt += 1;
                self.backoff(corrupt).await?;
            } else {
                failures += 1;
                // Every server gets a try before waiting.
                if failures >= mirrors {
                    self.backoff(failures + 1 - mirrors).await?;
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
    /// an earlier attempt left, then verify and move it into place. Servers
    /// are tried in `order`, starting at the `attempt`th.
    async fn stream_file(
        &self,
        f: &FileEntry,
        dest: &Path,
        order: &[usize],
        attempt: u32,
    ) -> Result<(), Error> {
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
                let n = order.len();
                let mut tried = 0;
                loop {
                    let url = &chunk.urls[order[(attempt as usize + tried) % n]];
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

        let _permit = self.requests.acquire().await;
        let started = Instant::now();
        let before = *counted;
        let result = self
            .stream_body(url, part, size, range, have, counted)
            .await;
        self.score(
            url,
            &result,
            counted.saturating_sub(before),
            started.elapsed(),
        );
        result
    }

    /// The request of [`Self::stream_once`], appending to the `have` bytes
    /// of `part`.
    async fn stream_body(
        &self,
        url: &str,
        part: &Path,
        size: u64,
        range: Option<(u64, u64)>,
        mut have: u64,
        counted: &mut u64,
    ) -> Result<(), Error> {
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
        let resp = self.send(req, url).await?;
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
        let mut watch = Watchdog::new(STALL, Instant::now());
        loop {
            let waited = Instant::now();
            let next = tokio::select! {
                _ = self.cancel.cancelled() => {
                    out.flush().await?;
                    return Err(Error::Cancelled);
                }
                n = tokio::time::timeout(STALL, body.next()) => n,
            };
            self.stats.add_net_wait(waited.elapsed());
            let bytes = match next {
                Ok(None) => break,
                Ok(Some(Ok(b))) => b,
                Ok(Some(Err(e))) => {
                    // Keep what arrived for the next attempt.
                    out.flush().await?;
                    return Err(e.into());
                }
                Err(_) => {
                    out.flush().await?;
                    return Err(stalled(url));
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
            watch.pause(self.throttle(bytes.len()).await?);
            let writing = Instant::now();
            out.write_all(&bytes).await?;
            self.stats.add_disk_wait(writing.elapsed());
            have += n;
            *counted += n;
            self.stats.add_received(n);
            self.stats.add_written(n);
            self.stats.add_done(n);
            if let Err(e) = watch.feed(n, Instant::now()) {
                out.flush().await?;
                return Err(Error::Interrupted(format!("{}: {e}", redact(url))));
            }
        }
        out.flush().await?;
        if have != size {
            return Err(Error::Interrupted(format!(
                "the connection closed after {have} of {size} bytes"
            )));
        }
        Ok(())
    }

    /// Send `req`, giving up on a server that does not answer in time.
    async fn send(
        &self,
        req: reqwest::RequestBuilder,
        url: &str,
    ) -> Result<reqwest::Response, Error> {
        tokio::select! {
            _ = self.cancel.cancelled() => Err(Error::Cancelled),
            r = tokio::time::timeout(STALL * 2, req.send()) => match r {
                Ok(r) => Ok(r?),
                Err(_) => Err(stalled(url)),
            },
        }
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
            loop {
                let waited = Instant::now();
                let Some(next) = parts.next().await else {
                    break;
                };
                self.stats.add_net_wait(waited.elapsed());
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
                let writing = Instant::now();
                out.write_all(slice).await?;
                self.stats.add_disk_wait(writing.elapsed());
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

    /// A chunk from the best server that has it. Failing servers are left
    /// for the next one right away; once every server failed, the engine
    /// waits a little longer each round and keeps trying.
    async fn fetch_chunk(&self, chunk: &Chunk) -> Result<Vec<u8>, Error> {
        if chunk.urls.is_empty() && self.signing.is_none() {
            return Err(Error::Other(anyhow::anyhow!(
                "chunk {} has no address",
                chunk.key
            )));
        }
        let order = match &self.signing {
            Some(_) => Vec::new(),
            None => self.hosts.order(&chunk.urls),
        };
        let mirrors = order.len().max(1);
        let (mut next, mut refused) = (0, 0);
        let (mut failures, mut corrupt, mut resigned) = (0u32, 0u32, 0u32);
        loop {
            let (url, alt) = match &self.signing {
                Some(s) => {
                    let url = s.url(&chunk.key).await?;
                    (url.clone(), url)
                }
                None => (
                    chunk.urls[order[next % mirrors]].clone(),
                    chunk.urls[order[(next + 1) % mirrors]].clone(),
                ),
            };
            let e = match self.fetch_raced(chunk, &url, &alt).await {
                Ok(data) => return Ok(data),
                Err(e) => e,
            };
            if self.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            match e.kind() {
                // An expired signature: sign again.
                Kind::Fatal
                    if self.signing.is_some()
                        && matches!(&e, Error::Status { status, .. } if matches!(status.as_u16(), 401 | 403))
                        && resigned < self.retries =>
                {
                    if let Some(s) = &self.signing {
                        s.forget(&chunk.key);
                    }
                    resigned += 1;
                }
                // Other servers may have what this one refuses.
                Kind::Fatal if matches!(e, Error::Status { .. }) && refused + 1 < mirrors => {
                    refused += 1;
                    next += 1;
                }
                Kind::Fatal => return Err(e),
                Kind::Corrupt => {
                    corrupt += 1;
                    if corrupt > self.retries.max(mirrors as u32 - 1) {
                        return Err(e);
                    }
                    tracing::warn!("chunk {} (attempt {corrupt}): {e}", chunk.key);
                    next += 1;
                    self.backoff(corrupt).await?;
                }
                Kind::Transient => {
                    failures += 1;
                    tracing::warn!("chunk {} (failure {failures}): {e}", chunk.key);
                    next += 1;
                    if failures as usize >= mirrors {
                        self.backoff(failures + 1 - mirrors as u32).await?;
                    }
                }
            }
        }
    }

    /// Fetch from `url`; when that lags while request slots sit idle (the
    /// end of a download), race the same chunk from `alt` and take
    /// whichever arrives first.
    async fn fetch_raced(&self, chunk: &Chunk, url: &str, alt: &str) -> Result<Vec<u8>, Error> {
        let first = self.fetch_chunk_once(chunk, url);
        tokio::pin!(first);
        let patience = self
            .hosts
            .chunk_time()
            .map_or(STALL, |t| (t * 3).clamp(STALL / 4, STALL * 2));
        tokio::select! {
            r = &mut first => return r,
            () = tokio::time::sleep(patience) => {}
        }
        if !self.requests.has_spare() {
            return first.await;
        }
        tracing::debug!("chunk {} is slow; racing a second request", chunk.key);
        let second = self.fetch_chunk_once(chunk, alt);
        tokio::pin!(second);
        tokio::select! {
            r = &mut first => match r {
                Err(e) if !matches!(e, Error::Cancelled) => second.await,
                r => r,
            },
            r = &mut second => match r {
                Err(e) if !matches!(e, Error::Cancelled) => first.await,
                r => r,
            },
        }
    }

    async fn fetch_chunk_once(&self, chunk: &Chunk, url: &str) -> Result<Vec<u8>, Error> {
        let raw = {
            let _permit = self.requests.acquire().await;
            let started = Instant::now();
            let raw = self.request_chunk(chunk, url).await;
            let bytes = raw.as_ref().map_or(0, |r| r.len() as u64);
            self.score(url, &raw, bytes, started.elapsed());
            raw?
        };
        // Decoding and hashing are CPU-bound: keep them off the async workers.
        let (codec, size, hash) = (chunk.codec.clone(), chunk.size, chunk.hash.clone());
        let decoded = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
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
        .map_err(|e| Error::Corrupt(format!("chunk {}: {e}", chunk.key)));
        if decoded.is_err() {
            self.hosts.record_failure(url);
        }
        decoded
    }

    /// The raw bytes of `chunk` from `url`.
    async fn request_chunk(&self, chunk: &Chunk, url: &str) -> Result<Vec<u8>, Error> {
        let mut req = self.client.get(url);
        if let Some((start, len)) = chunk.range {
            req = req.header(
                reqwest::header::RANGE,
                format!("bytes={start}-{}", start + len - 1),
            );
        }
        let resp = self.send(req, url).await?;
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
        let mut watch = Watchdog::new(STALL, Instant::now());
        loop {
            let next = tokio::select! {
                _ = self.cancel.cancelled() => return Err(Error::Cancelled),
                n = tokio::time::timeout(STALL, body.next()) => n.map_err(|_| stalled(url))?,
            };
            let Some(bytes) = next else { break };
            let bytes = bytes?;
            watch.pause(self.throttle(bytes.len()).await?);
            self.stats.add_received(bytes.len() as u64);
            raw.extend_from_slice(&bytes);
            watch
                .feed(bytes.len() as u64, Instant::now())
                .map_err(|e| Error::Interrupted(format!("{}: {e}", redact(url))))?;
        }
        Ok(raw)
    }
}

/// The error for a server that stopped sending.
fn stalled(url: &str) -> Error {
    Error::Interrupted(format!(
        "{} sent nothing for {} seconds",
        adapt::host(url),
        STALL.as_secs_f32()
    ))
}

/// Counts a task in a gauge while it lives.
struct Waiting<'a>(&'a std::sync::atomic::AtomicU64);

impl<'a> Waiting<'a> {
    fn new(gauge: &'a std::sync::atomic::AtomicU64) -> Self {
        gauge.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(gauge)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
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
        let signed = self.signer.sign(&batch).await.map_err(sign_error)?;
        let mut urls = self.urls.lock().unwrap_or_else(|e| e.into_inner());
        urls.extend(signed);
        urls.get(key).cloned().ok_or_else(|| {
            Error::Other(anyhow::anyhow!(
                "no download address was issued for chunk {key}"
            ))
        })
    }
}

/// A failure to sign addresses: retried like a dropped connection when
/// the network was at fault, else final.
fn sign_error(e: anyhow::Error) -> Error {
    let network = e.chain().any(|c| {
        c.downcast_ref::<reqwest::Error>()
            .is_some_and(|r| !r.is_builder() && r.status().is_none_or(|s| s.is_server_error()))
    });
    let e = e.context("Could not get chunk download addresses");
    if network {
        Error::Interrupted(format!("{e:#}"))
    } else {
        Error::Other(e)
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
