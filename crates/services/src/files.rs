//! Downloads of single files offered beside games (offline installers,
//! patches, goodies such as manuals and soundtracks). They run one at a time
//! apart from the install queue, so fetching a soundtrack never waits behind
//! an install or marks the game itself as busy. An interrupted file resumes
//! when it is downloaded again.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use irori_core::process::CancellationToken;
use irori_core::{Event, ExtraFile, GameId};
use irori_depot::{FileEntry, Finished, Job, Options};
use serde::{Deserialize, Serialize};

use crate::Backend;

/// Finished entries kept in the list.
const FINISHED_LIMIT: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileState {
    Queued,
    Running,
    Done,
    Failed,
    Canceled,
}

/// One file to download.
#[derive(Debug, Clone)]
pub struct FileDownload {
    /// Identifies the entry: the game and the store's file handle.
    pub key: String,
    pub game: GameId,
    pub file: ExtraFile,
    /// Where the file goes.
    pub folder: PathBuf,
    /// The saved file's name, known once the download starts.
    pub name: Option<String>,
    pub state: FileState,
    pub downloaded: u64,
    /// Network bytes per second while running.
    pub speed: u64,
    pub error: Option<String>,
}

impl FileDownload {
    /// The saved file, once its name is known.
    pub fn path(&self) -> Option<PathBuf> {
        self.name.as_ref().map(|n| self.folder.join(n))
    }

    pub fn is_active(&self) -> bool {
        matches!(self.state, FileState::Queued | FileState::Running)
    }
}

#[derive(Default)]
pub(crate) struct FilesState {
    list: Vec<FileDownload>,
    running: Option<(String, CancellationToken)>,
}

/// The [`FileDownload::key`] of `file` of `game`.
pub fn key_of(game: &GameId, file: &ExtraFile) -> String {
    format!("{}|{}", game.key(), file.id)
}

impl Backend {
    /// Queued, running and recently finished file downloads, oldest first.
    pub fn file_downloads(&self) -> Vec<FileDownload> {
        self.files.lock().list.clone()
    }

    /// Download `file` of `game` into `folder`. A file already queued or
    /// running is left alone; a finished one is fetched again (resuming a
    /// partial copy).
    pub fn download_extra(&self, game: GameId, file: ExtraFile, folder: PathBuf) {
        let key = key_of(&game, &file);
        {
            let mut files = self.files.lock();
            if files.list.iter().any(|f| f.key == key && f.is_active()) {
                return;
            }
            files.list.retain(|f| f.key != key);
            files.list.push(FileDownload {
                key,
                game,
                file,
                folder,
                name: None,
                state: FileState::Queued,
                downloaded: 0,
                speed: 0,
                error: None,
            });
        }
        self.files_wake.notify_one();
        self.bus.emit(Event::DownloadQueueChanged);
    }

    /// Stop a running file or drop a queued one. The partial file stays, so
    /// downloading it again resumes.
    pub fn cancel_extra(&self, key: &str) {
        let mut files = self.files.lock();
        if let Some((running, cancel)) = &files.running
            && running == key
        {
            cancel.cancel();
        }
        if let Some(f) = files
            .list
            .iter_mut()
            .find(|f| f.key == key && f.state == FileState::Queued)
        {
            f.state = FileState::Canceled;
        }
        drop(files);
        self.bus.emit(Event::DownloadQueueChanged);
    }

    /// Forget finished file downloads.
    pub fn clear_finished_extras(&self) {
        self.files.lock().list.retain(FileDownload::is_active);
        self.bus.emit(Event::DownloadQueueChanged);
    }

    /// Runs file downloads one after another, for the backend's lifetime.
    pub(crate) async fn file_worker(&self) {
        loop {
            let next = {
                let mut files = self.files.lock();
                let next = files
                    .list
                    .iter_mut()
                    .find(|f| f.state == FileState::Queued)
                    .map(|f| {
                        f.state = FileState::Running;
                        f.clone()
                    });
                if let Some(f) = &next {
                    files.running = Some((f.key.clone(), CancellationToken::new()));
                }
                next
            };
            let Some(item) = next else {
                self.files_wake.notified().await;
                continue;
            };
            self.bus.emit(Event::DownloadQueueChanged);
            let cancel = self
                .files
                .lock()
                .running
                .as_ref()
                .map(|(_, c)| c.clone())
                .unwrap_or_default();
            let result = self.fetch_extra(&item, &cancel).await;
            {
                let mut files = self.files.lock();
                files.running = None;
                if let Some(f) = files.list.iter_mut().find(|f| f.key == item.key) {
                    f.speed = 0;
                    match result {
                        Ok(Finished::Done) => {
                            f.state = FileState::Done;
                            f.downloaded = f.file.size.max(f.downloaded);
                        }
                        Ok(Finished::Cancelled) => f.state = FileState::Canceled,
                        Err(e) => {
                            tracing::warn!("downloading {}: {e:#}", f.file.name);
                            f.state = FileState::Failed;
                            f.error = Some(format!("{e:#}"));
                        }
                    }
                }
                // Keep the newest finished entries.
                let finished = files.list.iter().filter(|f| !f.is_active()).count();
                let mut drop_first = finished.saturating_sub(FINISHED_LIMIT);
                files.list.retain(|f| {
                    if drop_first > 0 && !f.is_active() {
                        drop_first -= 1;
                        false
                    } else {
                        true
                    }
                });
            }
            self.bus.emit(Event::DownloadQueueChanged);
        }
    }

    async fn fetch_extra(
        &self,
        item: &FileDownload,
        cancel: &CancellationToken,
    ) -> Result<Finished> {
        let store = self
            .store(item.game.runner)
            .context("This store is not available")?;
        let (url, name) = store.extra_url(&item.game.app_name, &item.file).await?;
        {
            let mut files = self.files.lock();
            if let Some(f) = files.list.iter_mut().find(|f| f.key == item.key) {
                f.name = Some(name.clone());
            }
        }
        let entry = FileEntry::whole(name, item.file.size, None, vec![url]);
        let opts = Options {
            max_bytes_per_sec: u64::from(self.settings.app().max_download_speed) * 1024,
            ..Options::default()
        };
        let (b, key) = (self.clone(), item.key.clone());
        let progress = Arc::new(move |p: irori_depot::Progress| {
            {
                let mut files = b.files.lock();
                if let Some(f) = files.list.iter_mut().find(|f| f.key == key) {
                    f.downloaded = p.bytes_done;
                    f.speed = p.download_speed;
                }
            }
            b.bus.emit(Event::DownloadQueueChanged);
        });
        irori_depot::download(
            irori_core::http::client(),
            Job {
                root: &item.folder,
                files: &[entry],
                remove: &[],
            },
            &opts,
            progress,
            cancel,
        )
        .await
    }
}
