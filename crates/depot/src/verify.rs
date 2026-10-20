//! Checking installed files against a manifest.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt as _;
use tokio_util::sync::CancellationToken;

use crate::download::with_progress;
use crate::hash::hash_file;
use crate::path;
use crate::plan::FileEntry;
use crate::progress::{ProgressFn, Stats};

/// Why a file failed verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    Missing,
    WrongSize,
    WrongHash,
}

/// Outcome of [`verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verified {
    /// Indexes into the verified list, with what is wrong.
    Checked(Vec<(usize, Problem)>),
    Cancelled,
}

/// Hash every file under `root` and report the ones that are missing or
/// differ from the manifest. Progress counts bytes hashed.
pub async fn verify(
    root: &Path,
    files: &[FileEntry],
    workers: usize,
    progress: ProgressFn,
    cancel: &CancellationToken,
) -> Result<Verified> {
    let workers = if workers == 0 {
        std::thread::available_parallelism().map_or(4, |n| n.get().min(8))
    } else {
        workers
    };
    let stats = Arc::new(Stats::new(files.iter().map(|f| f.size).sum()));
    // Owned items keep the future `Send` (see `download`).
    let items: Vec<_> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (i, path::join(root, &f.path), f.size, f.hash.clone()))
        .collect();
    let checks = futures::stream::iter(items)
        .map(|(i, dest, size, hash)| {
            let (stats, cancel) = (stats.clone(), cancel.clone());
            async move {
                let problem =
                    tokio::task::spawn_blocking(move || -> std::io::Result<Option<Problem>> {
                        let meta = match std::fs::metadata(&dest) {
                            Ok(m) if m.is_file() => m,
                            _ => {
                                stats.add_done(size);
                                return Ok(Some(Problem::Missing));
                            }
                        };
                        if meta.len() != size {
                            stats.add_done(size);
                            return Ok(Some(Problem::WrongSize));
                        }
                        let Some(hash) = hash else {
                            stats.add_done(size);
                            return Ok(None);
                        };
                        let ok = hash_file(&dest, &hash, |n| {
                            stats.add_done(n);
                            !cancel.is_cancelled()
                        })?;
                        Ok((!ok).then_some(Problem::WrongHash))
                    })
                    .await?;
                match problem {
                    Ok(p) => anyhow::Ok(p.map(|p| (i, p))),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Ok(None),
                    Err(e) => Ok(Some((i, problem_of(&e)))),
                }
            }
        })
        .buffer_unordered(workers)
        .collect::<Vec<_>>();
    let results = with_progress(stats.clone(), progress, checks).await;
    if cancel.is_cancelled() {
        return Ok(Verified::Cancelled);
    }
    let mut bad = Vec::new();
    for r in results {
        if let Some(b) = r? {
            bad.push(b);
        }
    }
    bad.sort_unstable_by_key(|(i, _)| *i);
    Ok(Verified::Checked(bad))
}

fn problem_of(e: &std::io::Error) -> Problem {
    if e.kind() == std::io::ErrorKind::NotFound {
        Problem::Missing
    } else {
        Problem::WrongHash
    }
}
