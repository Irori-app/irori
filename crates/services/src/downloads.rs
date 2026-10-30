//! Download queue: one install/update runs at a time, the rest wait in order.
//! The queue survives restarts; an interrupted download resumes (stores
//! continue partial downloads).

use std::sync::Arc;
use std::time::Duration;

use irori_core::process::CancellationToken;
use irori_core::{
    Event, GameId, InstallProgress, InstallRequest, Notice, NoticeLevel, OpContext, Outcome, Paths,
    RemoveArgs, StatusKind, UpdateRequest, json_store, util,
};
use serde::{Deserialize, Serialize};

use crate::Backend;

const FINISHED_LIMIT: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DownloadKind {
    Install(InstallRequest),
    Update(UpdateRequest),
}

impl DownloadKind {
    pub fn label(&self) -> &'static str {
        match self {
            DownloadKind::Install(_) => "Install",
            DownloadKind::Update(_) => "Update",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadState {
    Queued,
    Running,
    Paused,
    Done,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadItem {
    pub id: GameId,
    pub title: String,
    pub kind: DownloadKind,
    pub state: DownloadState,
    /// Unix seconds.
    pub added_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    /// Expected download size in bytes (0 = unknown).
    pub download_size: u64,
    pub error: Option<String>,
    #[serde(skip)]
    pub progress: Option<InstallProgress>,
}

/// Read-only view for the UI.
#[derive(Debug, Clone, Default)]
pub struct DownloadQueueSnapshot {
    /// Running or paused item first, then queued items in order.
    pub active: Vec<DownloadItem>,
    /// Most recent first.
    pub finished: Vec<DownloadItem>,
    pub paused: bool,
}

impl DownloadQueueSnapshot {
    pub fn current(&self) -> Option<&DownloadItem> {
        self.active
            .first()
            .filter(|i| matches!(i.state, DownloadState::Running | DownloadState::Paused))
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct QueueState {
    active: Vec<DownloadItem>,
    finished: Vec<DownloadItem>,
    paused: bool,
    #[serde(skip)]
    running: Option<Running>,
}

struct Running {
    id: GameId,
    cancel: CancellationToken,
    /// Why the run was interrupted.
    stop: Option<StopReason>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StopReason {
    Pause,
    Cancel { keep_files: bool },
}

fn file() -> std::path::PathBuf {
    Paths::get().state_dir().join("downloads.json")
}

impl QueueState {
    pub(crate) fn load() -> Self {
        let mut s: QueueState = json_store::read_or_default(&file());
        for item in &mut s.active {
            if item.state == DownloadState::Running {
                item.state = DownloadState::Queued;
            }
        }
        s
    }

    fn save(&self) {
        if let Err(e) = json_store::write(&file(), self) {
            tracing::error!("saving download queue: {e:#}");
        }
    }

    fn snapshot(&self) -> DownloadQueueSnapshot {
        DownloadQueueSnapshot {
            active: self.active.clone(),
            finished: self.finished.clone(),
            paused: self.paused,
        }
    }

    fn finish(&mut self, mut item: DownloadItem, state: DownloadState, error: Option<String>) {
        item.state = state;
        item.error = error;
        item.finished_at = Some(util::unix_now());
        item.progress = None;
        self.finished.insert(0, item);
        self.finished.truncate(FINISHED_LIMIT);
    }
}

impl Backend {
    pub fn downloads(&self) -> DownloadQueueSnapshot {
        self.downloads.lock().snapshot()
    }

    /// Whether a game is queued or downloading.
    pub fn is_queued(&self, id: &GameId) -> bool {
        self.downloads.lock().active.iter().any(|i| &i.id == id)
    }

    /// Queue an install. An already-installed game is refused: reinstalling
    /// over a live install would replace its store records (use repair or
    /// modify instead).
    pub fn enqueue_install(&self, id: GameId, title: String, request: InstallRequest, size: u64) {
        if self.game(&id).is_some_and(|g| g.is_installed) {
            tracing::warn!("refusing to install {id}: already installed");
            self.bus.notice(
                Notice::new(
                    NoticeLevel::Warning,
                    format!("{title} is already installed"),
                )
                .keys("shell.notice.already_installed", None, &[("title", &title)])
                .game(id),
            );
            return;
        }
        self.enqueue(id, title, DownloadKind::Install(request), size);
    }

    pub fn enqueue_update(&self, id: GameId, title: String, request: UpdateRequest) {
        self.enqueue(id, title, DownloadKind::Update(request), 0);
    }

    fn enqueue(&self, id: GameId, title: String, kind: DownloadKind, size: u64) {
        {
            let mut q = self.downloads.lock();
            if q.active.iter().any(|i| i.id == id) {
                return;
            }
            q.active.push(DownloadItem {
                id: id.clone(),
                title,
                kind,
                state: DownloadState::Queued,
                added_at: util::unix_now(),
                started_at: None,
                finished_at: None,
                download_size: size,
                error: None,
                progress: None,
            });
            q.save();
        }
        self.statuses.set(&id, StatusKind::Queued);
        self.bus.emit(Event::DownloadQueueChanged);
        self.downloads_wake.notify_one();
    }

    /// Queue updates for every installed game with one available (from the
    /// update set computed by the last refresh).
    pub async fn enqueue_all_updates(&self) {
        for id in self.updates().iter() {
            if self.settings.game(id).ignore_game_updates {
                continue;
            }
            let title = self
                .game(id)
                .map(|g| g.title.clone())
                .unwrap_or_else(|| id.app_name.to_string());
            self.enqueue_update(id.clone(), title, UpdateRequest::default());
        }
    }

    /// Pause the queue; the running download stops and resumes later.
    pub fn pause_downloads(&self) {
        let mut q = self.downloads.lock();
        q.paused = true;
        if let Some(r) = q.running.as_mut() {
            r.stop = Some(StopReason::Pause);
            r.cancel.cancel();
        }
        q.save();
        drop(q);
        self.bus.emit(Event::DownloadQueueChanged);
    }

    pub fn resume_downloads(&self) {
        {
            let mut q = self.downloads.lock();
            q.paused = false;
            q.save();
        }
        self.bus.emit(Event::DownloadQueueChanged);
        self.downloads_wake.notify_one();
    }

    /// Remove a queued item, or stop the running one. `keep_files` keeps the
    /// partial download so a later install can resume it.
    pub fn cancel_download(&self, id: &GameId, keep_files: bool) {
        let mut q = self.downloads.lock();
        if let Some(r) = q.running.as_mut().filter(|r| &r.id == id) {
            r.stop = Some(StopReason::Cancel { keep_files });
            r.cancel.cancel();
            return;
        }
        if let Some(pos) = q.active.iter().position(|i| &i.id == id) {
            let item = q.active.remove(pos);
            q.finish(item, DownloadState::Canceled, None);
            q.save();
            drop(q);
            self.statuses.clear(id);
            self.bus.emit(Event::DownloadQueueChanged);
        }
    }

    /// Move a queued item to `index` within the waiting items (the running item
    /// keeps its place).
    pub fn reorder_download(&self, id: &GameId, index: usize) {
        let mut q = self.downloads.lock();
        let first_movable = usize::from(q.running.is_some() && !q.active.is_empty());
        let Some(pos) = q.active.iter().position(|i| &i.id == id) else {
            return;
        };
        if pos < first_movable {
            return;
        }
        let item = q.active.remove(pos);
        let target = (index + first_movable).min(q.active.len());
        q.active.insert(target, item);
        q.save();
        drop(q);
        self.bus.emit(Event::DownloadQueueChanged);
    }

    pub fn clear_finished_downloads(&self) {
        let mut q = self.downloads.lock();
        q.finished.clear();
        q.save();
        drop(q);
        self.bus.emit(Event::DownloadQueueChanged);
    }

    pub fn remove_finished_download(&self, id: &GameId) {
        let mut q = self.downloads.lock();
        q.finished.retain(|i| &i.id != id);
        q.save();
        drop(q);
        self.bus.emit(Event::DownloadQueueChanged);
    }

    pub(crate) async fn download_worker(&self) {
        loop {
            let next = {
                let mut q = self.downloads.lock();
                if q.paused || q.running.is_some() {
                    None
                } else if let Some(item) = q
                    .active
                    .iter_mut()
                    .find(|i| matches!(i.state, DownloadState::Queued | DownloadState::Paused))
                {
                    item.state = DownloadState::Running;
                    item.started_at.get_or_insert(util::unix_now());
                    let item = item.clone();
                    let cancel = CancellationToken::new();
                    q.running = Some(Running {
                        id: item.id.clone(),
                        cancel: cancel.clone(),
                        stop: None,
                    });
                    q.save();
                    Some((item, cancel))
                } else {
                    None
                }
            };
            match next {
                Some((item, cancel)) => {
                    self.bus.emit(Event::DownloadQueueChanged);
                    self.run_download(item, cancel).await;
                }
                None => self.downloads_wake.notified().await,
            }
        }
    }

    async fn run_download(&self, item: DownloadItem, cancel: CancellationToken) {
        let id = item.id.clone();
        let kind = match item.kind {
            DownloadKind::Install(_) => StatusKind::Installing,
            DownloadKind::Update(_) => StatusKind::Updating,
        };
        self.statuses.set(&id, kind);

        let ctx = self.op_context(&id, cancel.clone());
        let work = async {
            match self.store(id.runner) {
                None => Err(anyhow::anyhow!("{} is not available", id.runner)),
                Some(store) => match &item.kind {
                    DownloadKind::Install(_)
                        if store.game(&id.app_name).is_some_and(|g| g.is_installed) =>
                    {
                        Ok(Outcome::Error(format!(
                            "{} is already installed",
                            item.title
                        )))
                    }
                    DownloadKind::Install(req) => store.install(&id.app_name, req, ctx).await,
                    DownloadKind::Update(req) => store.update(&id.app_name, req, ctx).await,
                },
            }
        };
        let result = match stop_within(work, &cancel, STOP_GRACE).await {
            Some(result) => result,
            None => {
                tracing::warn!("{id} did not stop within {STOP_GRACE:?} of being asked; dropped");
                Ok(Outcome::Abort)
            }
        };

        let stop = {
            let mut q = self.downloads.lock();
            q.running.take().and_then(|r| r.stop)
        };
        let outcome = match result {
            Ok(o) => o,
            Err(e) => Outcome::Error(format!("{e:#}")),
        };

        enum After {
            Paused,
            Canceled { delete_files: bool },
            Done,
            Failed(String),
        }
        let after = {
            let mut q = self.downloads.lock();
            let pos = q.active.iter().position(|i| i.id == id);
            let (after, state, error) = match (stop, outcome) {
                (Some(StopReason::Pause), _) => (After::Paused, DownloadState::Paused, None),
                (Some(StopReason::Cancel { keep_files }), _) => (
                    After::Canceled {
                        delete_files: !keep_files,
                    },
                    DownloadState::Canceled,
                    None,
                ),
                (None, Outcome::Abort) => (
                    After::Canceled {
                        delete_files: false,
                    },
                    DownloadState::Canceled,
                    None,
                ),
                (None, Outcome::Done) => (After::Done, DownloadState::Done, None),
                (None, Outcome::Error(e)) => {
                    (After::Failed(e.clone()), DownloadState::Failed, Some(e))
                }
            };
            if let Some(p) = pos {
                if state == DownloadState::Paused {
                    q.active[p].state = DownloadState::Paused;
                } else {
                    let it = q.active.remove(p);
                    q.finish(it, state, error);
                }
            }
            q.save();
            after
        };

        match after {
            After::Paused => self.statuses.set(&id, StatusKind::Queued),
            After::Canceled { delete_files } => {
                self.statuses.clear(&id);
                if delete_files
                    && matches!(item.kind, DownloadKind::Install(_))
                    && let Some(store) = self.store(id.runner)
                {
                    let _ = store
                        .uninstall(
                            &id.app_name,
                            RemoveArgs {
                                delete_files: true,
                                ..Default::default()
                            },
                        )
                        .await;
                }
            }
            After::Done => {
                self.statuses.clear(&id);
                self.forget_update(&id);
                self.bus.emit(Event::LibraryChanged(id.runner));
                let (verb, key) = match item.kind {
                    DownloadKind::Install(_) => ("installed", "shell.notice.installed"),
                    DownloadKind::Update(_) => ("updated", "shell.notice.updated"),
                };
                self.bus.notice(
                    Notice::new(NoticeLevel::Success, format!("{} {verb}", item.title))
                        .keys(key, None, &[("title", &item.title)])
                        .game(id.clone()),
                );
                if matches!(item.kind, DownloadKind::Install(_)) {
                    self.after_install(&id).await;
                }
            }
            After::Failed(err) => {
                self.statuses.clear(&id);
                self.bus.notice(
                    Notice::new(
                        NoticeLevel::Error,
                        format!("{} failed to download", item.title),
                    )
                    .keys(
                        "shell.notice.download_failed",
                        None,
                        &[("title", &item.title)],
                    )
                    .message(err)
                    .game(id.clone()),
                );
            }
        }
        self.bus.emit(Event::DownloadQueueChanged);
    }

    /// Operation context wired to the status registry.
    pub(crate) fn op_context(&self, id: &GameId, cancel: CancellationToken) -> OpContext {
        let statuses = self.statuses.clone();
        let progress_id = id.clone();
        let step_statuses = self.statuses.clone();
        let step_id = id.clone();
        OpContext {
            progress: Arc::new(move |p| statuses.progress(&progress_id, p)),
            cancel,
            step: Arc::new(move |s| step_statuses.context(&step_id, s)),
            // "Turn off logs" covers installs, updates and tools too.
            log_file: (!self.settings.app().disable_logs)
                .then(|| Paths::get().operation_log_file(id)),
            settings: Some(Arc::new(self.settings.game(id))),
        }
    }
}

/// How long a pause or cancel waits for the store to stop on its own.
const STOP_GRACE: Duration = Duration::from_secs(15);

/// Run `work` until it finishes, or until `grace` after `cancel` fired. A
/// store that misses the cancel (stuck fetching a manifest, a request with no
/// timeout) must not hold up the whole queue; dropping the future stops it.
async fn stop_within<T>(
    work: impl Future<Output = T>,
    cancel: &CancellationToken,
    grace: Duration,
) -> Option<T> {
    tokio::select! {
        biased;
        result = work => Some(result),
        () = async {
            cancel.cancelled().await;
            tokio::time::sleep(grace).await;
        } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn gives_up_on_work_that_ignores_a_stop() {
        let cancel = CancellationToken::new();
        let grace = Duration::from_millis(50);
        let quick = stop_within(async { 7 }, &cancel, grace).await;
        assert_eq!(quick, Some(7));
        cancel.cancel();
        let stuck = tokio::time::timeout(
            Duration::from_secs(2),
            stop_within(std::future::pending::<()>(), &cancel, grace),
        )
        .await
        .expect("returns after the grace period");
        assert_eq!(stuck, None);
    }
}
