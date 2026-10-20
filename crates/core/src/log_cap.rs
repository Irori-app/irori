//! Size limits for log files. A game that prints a Wine warning in a tight
//! loop can write over 100 MiB per second; without a limit its log fills the
//! disk in minutes.
//!
//! Two kinds of log, two mechanisms:
//! - Game logs are written by the game itself (its stdout and stderr point at
//!   the file, so a launcher that quits mid-game cannot break the game's
//!   output). [`bound_log`] watches such a file and cuts it back to its launch
//!   header whenever it passes the limit; the game's appends simply continue
//!   at the new end.
//! - Operation logs (installs, updates, tools) are written by Ember line by
//!   line through [`CappedLog`], which moves a full file aside to `<name>.old`
//!   and starts afresh, so the newest output is always kept.

use std::io::{Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::AsyncWriteExt as _;

/// Largest a game log grows before it is cut back to its header.
pub const GAME_LOG_CAP: u64 = 64 * 1024 * 1024;
/// Largest an operation log grows before it moves to `<name>.old`.
pub const OPERATION_LOG_CAP: u64 = 16 * 1024 * 1024;
/// How often [`bound_log`] checks the size.
const CHECK_EVERY: Duration = Duration::from_secs(1);

/// Keep `path`, which a running process appends to, under `cap` bytes: each
/// time it passes the cap it is cut back to its first `keep` bytes (the
/// launch header) followed by a note saying output was dropped. Runs until the
/// task is dropped or aborted; the caller stops it when the process exits.
pub async fn bound_log(path: PathBuf, keep: u64, cap: u64) {
    loop {
        tokio::time::sleep(CHECK_EVERY).await;
        let path = path.clone();
        let cut = tokio::task::spawn_blocking(move || cut_back(&path, keep, cap)).await;
        if let Ok(Err(e)) = cut {
            tracing::debug!("bounding log: {e:#}");
        }
    }
}

/// One check of [`bound_log`]: cut `path` back to `keep` bytes if it is
/// longer than `cap`. Returns whether it did.
pub fn cut_back(path: &Path, keep: u64, cap: u64) -> std::io::Result<bool> {
    let len = std::fs::metadata(path)?.len();
    if len <= cap {
        return Ok(false);
    }
    let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
    let keep = keep.min(cap / 2);
    file.set_len(keep)?;
    file.seek(SeekFrom::End(0))?;
    writeln!(
        file,
        "\n[Ember] The log passed {} MiB; the {} MiB after the launch details were dropped.\n",
        cap / (1024 * 1024),
        (len - keep) / (1024 * 1024)
    )?;
    Ok(true)
}

/// An append-only log that never grows past its cap: when a write would pass
/// it, the file moves to `<name>.old` (replacing any earlier one) and a new
/// file starts. Disk use stays under twice the cap.
pub struct CappedLog {
    path: PathBuf,
    file: tokio::fs::File,
    written: u64,
    cap: u64,
}

impl CappedLog {
    /// Open `path` for appending, moving it aside first if it is already full.
    pub async fn open(path: &Path, cap: u64) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let written = match tokio::fs::metadata(path).await {
            Ok(m) if m.is_file() && m.len() >= cap => {
                tokio::fs::rename(path, old_path(path)).await?;
                0
            }
            Ok(m) => m.len(),
            Err(_) => 0,
        };
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            written,
            cap,
        })
    }

    /// Append `line` and a newline, rotating first if it would pass the cap.
    pub async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let len = line.len() as u64 + 1;
        if self.written + len > self.cap && self.written > 0 {
            self.rotate().await?;
        }
        self.file.write_all(line.as_bytes()).await?;
        self.file.write_all(b"\n").await?;
        self.written += len;
        Ok(())
    }

    pub async fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush().await
    }

    async fn rotate(&mut self) -> std::io::Result<()> {
        self.file.flush().await?;
        // Only regular files rotate; a log pointed at a device stays put.
        if tokio::fs::metadata(&self.path).await?.is_file() {
            tokio::fs::rename(&self.path, old_path(&self.path)).await?;
            self.file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .await?;
        }
        self.written = 0;
        Ok(())
    }
}

/// `launch.log` → `launch.log.old`.
fn old_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_back_keeps_the_header_and_appends_continue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launch.log");
        std::fs::write(&path, "HEADER\n").unwrap();
        // The game's handle, opened for appending as a spawned game's is.
        let mut game = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        game.write_all(&vec![b'x'; 4096]).unwrap();
        assert!(cut_back(&path, 7, 1024).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("HEADER\n\n[Ember] The log passed"));
        assert!(!text.contains('x'));
        game.write_all(b"after\n").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("dropped.\n\nafter\n"), "{text:?}");
        // Under the cap nothing happens.
        assert!(!cut_back(&path, 7, 1024).unwrap());
    }

    #[tokio::test]
    async fn capped_log_rotates_to_old_and_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.log");
        // The first two lines (33 bytes) fit; the third would pass 40.
        let mut log = CappedLog::open(&path, 40).await.unwrap();
        for line in ["first line here", "second line here", "third line here"] {
            log.write_line(line).await.unwrap();
        }
        log.flush().await.unwrap();
        let current = std::fs::read_to_string(&path).unwrap();
        let old = std::fs::read_to_string(dir.path().join("install.log.old")).unwrap();
        assert_eq!(current, "third line here\n");
        assert_eq!(old, "first line here\nsecond line here\n");
    }

    #[tokio::test]
    async fn a_full_log_moves_aside_when_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("op.log");
        std::fs::write(&path, vec![b'y'; 64]).unwrap();
        let mut log = CappedLog::open(&path, 32).await.unwrap();
        log.write_line("fresh").await.unwrap();
        log.flush().await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh\n");
        assert_eq!(
            std::fs::read(dir.path().join("op.log.old")).unwrap().len(),
            64
        );
    }
}
