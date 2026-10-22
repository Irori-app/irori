//! Live game statuses. Progress updates arrive many times per second from tool
//! output; events are throttled per game so the UI repaints at a bounded rate.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ember_core::{Event, EventBus, GameId, GameStatus, InstallProgress, StatusKind};
use parking_lot::Mutex;

/// Minimum interval between progress events of one game.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

struct Entry {
    status: GameStatus,
    last_emit: Instant,
}

#[derive(Clone)]
pub struct StatusRegistry {
    inner: Arc<Mutex<HashMap<GameId, Entry>>>,
    bus: EventBus,
}

impl StatusRegistry {
    pub fn new(bus: EventBus) -> Self {
        Self {
            inner: Arc::default(),
            bus,
        }
    }

    pub fn get(&self, id: &GameId) -> Option<GameStatus> {
        self.inner.lock().get(id).map(|e| e.status.clone())
    }

    pub fn all(&self) -> Vec<GameStatus> {
        self.inner
            .lock()
            .values()
            .map(|e| e.status.clone())
            .collect()
    }

    pub fn is_busy(&self, id: &GameId) -> bool {
        self.inner
            .lock()
            .get(id)
            .is_some_and(|e| e.status.kind.is_busy())
    }

    /// Enter a status (resets progress and context).
    pub fn set(&self, id: &GameId, kind: StatusKind) {
        let status = GameStatus::new(id.clone(), kind);
        self.inner.lock().insert(
            id.clone(),
            Entry {
                status: status.clone(),
                last_emit: Instant::now(),
            },
        );
        self.bus.emit(Event::Status(status));
    }

    /// Update progress of the current status (throttled).
    pub fn progress(&self, id: &GameId, progress: InstallProgress) {
        let emit = {
            let mut map = self.inner.lock();
            let Some(entry) = map.get_mut(id) else { return };
            entry.status.progress = Some(progress);
            let due = entry.last_emit.elapsed() >= PROGRESS_INTERVAL
                || progress.percent.is_some_and(|p| p >= 100.0);
            if due {
                entry.last_emit = Instant::now();
                Some(entry.status.clone())
            } else {
                None
            }
        };
        if let Some(s) = emit {
            self.bus.emit(Event::Status(s));
        }
    }

    /// Update the detail line of the current status.
    pub fn context(&self, id: &GameId, context: &str) {
        let emit = {
            let mut map = self.inner.lock();
            let Some(entry) = map.get_mut(id) else { return };
            if entry.status.context.as_deref() == Some(context) {
                return;
            }
            entry.status.context = Some(context.to_string());
            entry.last_emit = Instant::now();
            entry.status.clone()
        };
        self.bus.emit(Event::Status(emit));
    }

    /// Return the game to idle.
    pub fn clear(&self, id: &GameId) {
        if self.inner.lock().remove(id).is_some() {
            self.bus.emit(Event::StatusCleared(id.clone()));
        }
    }

    /// RAII guard that clears the status when dropped (error paths included).
    pub fn guard(&self, id: &GameId, kind: StatusKind) -> StatusGuard {
        self.set(id, kind);
        StatusGuard {
            registry: self.clone(),
            id: id.clone(),
        }
    }
}

pub struct StatusGuard {
    registry: StatusRegistry,
    id: GameId,
}

impl StatusGuard {
    pub fn set(&self, kind: StatusKind) {
        self.registry.set(&self.id, kind);
    }
}

impl Drop for StatusGuard {
    fn drop(&mut self) {
        self.registry.clear(&self.id);
    }
}
