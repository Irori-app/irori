//! Presentation of the download queue: which item is current, the waiting
//! items with their reorder targets, and the finished history. Pure data so it
//! is derived once per queue or library change, not per frame.

use std::sync::Arc;

use ember_core::{GameId, GameInfo, Runner};
use ember_services::{DownloadItem, DownloadKind, DownloadQueueSnapshot, DownloadState};
use ember_ui::i18n::t;
use gpui_kit::SharedString;

/// The download being worked on (running, or paused with the queue).
#[derive(Debug, Clone)]
pub(crate) struct CurrentRow {
    pub item: DownloadItem,
    pub game: Arc<GameInfo>,
    /// The queue is paused and this item waits to resume.
    pub paused: bool,
}

/// One waiting item.
#[derive(Debug, Clone)]
pub(crate) struct QueueRow {
    pub item: DownloadItem,
    pub game: Arc<GameInfo>,
    /// Zero-based position among the waiting items.
    pub waiting_ix: usize,
    pub is_first: bool,
    pub is_last: bool,
}

/// One finished item.
#[derive(Debug, Clone)]
pub(crate) struct HistoryRow {
    pub item: DownloadItem,
    pub game: Arc<GameInfo>,
}

impl HistoryRow {
    /// Failed and canceled downloads can be started again.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.item.state,
            DownloadState::Failed | DownloadState::Canceled
        )
    }
}

/// Everything the downloads page renders about the queue.
#[derive(Debug, Clone, Default)]
pub(crate) struct QueuePresentation {
    pub current: Option<CurrentRow>,
    pub waiting: Vec<QueueRow>,
    pub history: Vec<HistoryRow>,
    pub paused: bool,
    /// Added to a waiting position to get the backend's reorder index: the
    /// backend counts a paused current item as movable, a running one not.
    index_offset: usize,
}

/// Direction of a one-step move in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Up,
    Down,
}

impl QueuePresentation {
    /// Derive the presentation from a queue snapshot. `lookup` resolves a game
    /// for its art; items whose game is unknown (redistributables, games of a
    /// store that is gone) still get a row with their queued title, so every
    /// store's items are visible.
    pub fn build(
        snapshot: &DownloadQueueSnapshot,
        lookup: impl Fn(&GameId) -> Option<Arc<GameInfo>>,
    ) -> Self {
        let game_of = |item: &DownloadItem| {
            lookup(&item.id).unwrap_or_else(|| placeholder_game(&item.id, &item.title))
        };
        let current = snapshot.current().map(|item| CurrentRow {
            item: item.clone(),
            game: game_of(item),
            paused: item.state == DownloadState::Paused || snapshot.paused,
        });
        let skip = usize::from(current.is_some());
        let waiting_items: Vec<&DownloadItem> = snapshot.active.iter().skip(skip).collect();
        let count = waiting_items.len();
        let waiting = waiting_items
            .into_iter()
            .enumerate()
            .map(|(ix, item)| QueueRow {
                item: item.clone(),
                game: game_of(item),
                waiting_ix: ix,
                is_first: ix == 0,
                is_last: ix + 1 == count,
            })
            .collect();
        let history = snapshot
            .finished
            .iter()
            .map(|item| HistoryRow {
                item: item.clone(),
                game: game_of(item),
            })
            .collect();
        let index_offset = usize::from(
            current
                .as_ref()
                .is_some_and(|c| c.item.state != DownloadState::Running),
        );
        Self {
            current,
            waiting,
            history,
            paused: snapshot.paused,
            index_offset,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.current.is_none() && self.waiting.is_empty() && self.history.is_empty()
    }

    pub fn has_active(&self) -> bool {
        self.current.is_some() || !self.waiting.is_empty()
    }

    /// Backend reorder index that moves `id` one step, or `None` when it is
    /// already at that end of the queue or not waiting.
    pub fn step_target(&self, id: &GameId, step: Step) -> Option<usize> {
        let row = self.waiting.iter().find(|r| &r.item.id == id)?;
        let target = match step {
            Step::Up => row.waiting_ix.checked_sub(1)?,
            Step::Down => {
                let next = row.waiting_ix + 1;
                (next < self.waiting.len()).then_some(next)?
            }
        };
        Some(target + self.index_offset)
    }

    /// Backend reorder index that puts `dragged` where `onto` is now, or `None`
    /// when either is not waiting or they are the same item.
    pub fn drop_target(&self, dragged: &GameId, onto: &GameId) -> Option<usize> {
        if dragged == onto {
            return None;
        }
        self.waiting.iter().find(|r| &r.item.id == dragged)?;
        let target = self.waiting.iter().find(|r| &r.item.id == onto)?;
        Some(target.waiting_ix + self.index_offset)
    }

    /// Backend reorder index that restores an item to waiting position `ix`.
    pub fn restore_index(&self, waiting_ix: usize) -> usize {
        waiting_ix + self.index_offset
    }
}

/// "Install" / "Update".
pub(crate) fn kind_label(kind: &DownloadKind) -> SharedString {
    match kind {
        DownloadKind::Install(_) => t("manage.downloads.kind.install"),
        DownloadKind::Update(_) => t("manage.downloads.kind.update"),
    }
}

/// Store name for a download row; manually added apps read "Other".
pub(crate) fn store_label(runner: Runner) -> SharedString {
    match runner {
        Runner::Sideload => t("manage.downloads.store_other"),
        r => SharedString::from(r.store_name()),
    }
}

/// Library entry stand-in for queue items whose game is not in the library.
pub(crate) fn placeholder_game(id: &GameId, title: &str) -> Arc<GameInfo> {
    Arc::new(
        GameInfo {
            runner: Some(id.runner),
            app_name: id.app_name.to_string(),
            title: if title.is_empty() {
                id.app_name.to_string()
            } else {
                title.to_string()
            },
            ..Default::default()
        }
        .finalize(),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ember_core::{InstallRequest, UpdateRequest};

    pub(crate) fn item(runner: Runner, app: &str, state: DownloadState) -> DownloadItem {
        DownloadItem {
            id: GameId::new(runner, app),
            title: app.to_uppercase(),
            kind: DownloadKind::Install(InstallRequest::default()),
            state,
            added_at: 100,
            started_at: None,
            finished_at: None,
            download_size: 0,
            error: None,
            progress: None,
        }
    }

    fn known(id: &GameId) -> Option<Arc<GameInfo>> {
        (id.runner != Runner::Zoom).then(|| {
            Arc::new(GameInfo {
                runner: Some(id.runner),
                app_name: id.app_name.to_string(),
                title: format!("Known {}", id.app_name),
                ..Default::default()
            })
        })
    }

    fn id(runner: Runner, app: &str) -> GameId {
        GameId::new(runner, app)
    }

    #[test]
    fn running_item_is_current_and_pinned() {
        let snap = DownloadQueueSnapshot {
            active: vec![
                item(Runner::Gog, "a", DownloadState::Running),
                item(Runner::Zoom, "b", DownloadState::Queued),
                item(Runner::Sideload, "c", DownloadState::Queued),
            ],
            finished: vec![],
            paused: false,
        };
        let p = QueuePresentation::build(&snap, known);
        let current = p.current.as_ref().unwrap();
        assert_eq!(current.item.id, id(Runner::Gog, "a"));
        assert!(!current.paused);
        assert_eq!(p.waiting.len(), 2);
        assert!(p.waiting[0].is_first && !p.waiting[0].is_last);
        assert!(p.waiting[1].is_last);
        // Items of every store are shown; unknown games keep their queued title.
        assert_eq!(p.waiting[0].game.title, "B");
        assert_eq!(p.waiting[1].game.title, "Known c");
        // The backend skips the running item itself, so positions map 1:1.
        assert_eq!(p.step_target(&id(Runner::Zoom, "b"), Step::Down), Some(1));
        assert_eq!(p.step_target(&id(Runner::Zoom, "b"), Step::Up), None);
        assert_eq!(p.step_target(&id(Runner::Sideload, "c"), Step::Up), Some(0));
        assert_eq!(p.step_target(&id(Runner::Sideload, "c"), Step::Down), None);
        assert_eq!(p.step_target(&id(Runner::Gog, "a"), Step::Down), None);
    }

    #[test]
    fn paused_current_offsets_backend_index() {
        let snap = DownloadQueueSnapshot {
            active: vec![
                item(Runner::Gog, "a", DownloadState::Paused),
                item(Runner::Gog, "b", DownloadState::Queued),
                item(Runner::Gog, "c", DownloadState::Queued),
            ],
            finished: vec![],
            paused: true,
        };
        let p = QueuePresentation::build(&snap, known);
        assert!(p.current.as_ref().unwrap().paused);
        // A paused item occupies backend index 0 and is movable there, so
        // waiting rows start at 1 to keep the paused item first.
        assert_eq!(p.step_target(&id(Runner::Gog, "c"), Step::Up), Some(1));
        assert_eq!(p.step_target(&id(Runner::Gog, "b"), Step::Down), Some(2));
        assert_eq!(
            p.drop_target(&id(Runner::Gog, "c"), &id(Runner::Gog, "b")),
            Some(1)
        );
        assert_eq!(p.restore_index(0), 1);
    }

    #[test]
    fn queue_without_current_and_drag_targets() {
        let snap = DownloadQueueSnapshot {
            active: vec![
                item(Runner::Gog, "a", DownloadState::Queued),
                item(Runner::Nile, "b", DownloadState::Queued),
                item(Runner::Legendary, "c", DownloadState::Queued),
            ],
            finished: vec![],
            paused: true,
        };
        let p = QueuePresentation::build(&snap, known);
        assert!(p.current.is_none());
        assert_eq!(p.waiting.len(), 3);
        assert_eq!(
            p.drop_target(&id(Runner::Legendary, "c"), &id(Runner::Gog, "a")),
            Some(0)
        );
        assert_eq!(
            p.drop_target(&id(Runner::Gog, "a"), &id(Runner::Legendary, "c")),
            Some(2)
        );
        assert_eq!(
            p.drop_target(&id(Runner::Gog, "a"), &id(Runner::Gog, "a")),
            None
        );
        assert_eq!(
            p.drop_target(&id(Runner::Zoom, "x"), &id(Runner::Gog, "a")),
            None
        );
        assert!(p.has_active());
    }

    #[test]
    fn history_rows_and_retry() {
        crate::i18n::register();
        let mut failed = item(Runner::Zoom, "f", DownloadState::Failed);
        failed.error = Some("disk full".into());
        let mut update = item(Runner::Gog, "u", DownloadState::Done);
        update.kind = DownloadKind::Update(UpdateRequest::default());
        let snap = DownloadQueueSnapshot {
            active: vec![],
            finished: vec![
                failed,
                update,
                item(Runner::Gog, "c", DownloadState::Canceled),
            ],
            paused: false,
        };
        let p = QueuePresentation::build(&snap, known);
        assert!(!p.has_active());
        assert!(!p.is_empty());
        assert_eq!(p.history.len(), 3);
        assert!(p.history[0].is_retryable());
        assert!(!p.history[1].is_retryable());
        assert!(p.history[2].is_retryable());
        assert_eq!(kind_label(&p.history[1].item.kind), "Update");
        assert_eq!(store_label(Runner::Sideload), "Other");
        assert_eq!(store_label(Runner::Nile), "Amazon Games");
    }
}
