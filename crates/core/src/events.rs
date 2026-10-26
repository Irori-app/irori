use std::sync::Arc;

use tokio::sync::broadcast;

use crate::runner::{GameId, Runner};
use crate::status::GameStatus;

/// Severity of a user-facing notice raised by the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// Something the UI should show without the user asking (a finished download,
/// a failed launch, an expired login).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: NoticeLevel,
    /// English text (logs, and the fallback when no key is set).
    pub title: String,
    pub message: Option<String>,
    pub game: Option<GameId>,
    /// The store account the notice is about; clicking it opens Accounts.
    pub account: Option<Runner>,
    /// Translation keys for `title`/`message` and their `{placeholder}`
    /// values, resolved by the UI in the active language.
    pub title_key: Option<&'static str>,
    pub message_key: Option<&'static str>,
    pub args: Vec<(&'static str, String)>,
}

impl Notice {
    pub fn new(level: NoticeLevel, title: impl Into<String>) -> Self {
        Self {
            level,
            title: title.into(),
            message: None,
            game: None,
            account: None,
            title_key: None,
            message_key: None,
            args: Vec::new(),
        }
    }

    /// Attach translation keys (`message_key` may be `None` when the message
    /// is free text such as a tool error) and placeholder values.
    pub fn keys(
        mut self,
        title_key: &'static str,
        message_key: Option<&'static str>,
        args: &[(&'static str, &str)],
    ) -> Self {
        self.title_key = Some(title_key);
        self.message_key = message_key;
        self.args = args.iter().map(|(k, v)| (*k, v.to_string())).collect();
        self
    }

    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub fn game(mut self, id: GameId) -> Self {
        self.game = Some(id);
        self
    }

    pub fn account(mut self, runner: Runner) -> Self {
        self.account = Some(runner);
        self
    }
}

/// Backend → UI change notifications. Events carry identities, not data: the
/// receiver re-reads current state from the owning service, so a lagging
/// receiver never applies stale snapshots.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A store's game list changed (refresh, install, uninstall). Raw store
    /// signal; UIs should wait for [`Event::LibraryUpdated`].
    LibraryChanged(Runner),
    /// The aggregated library was rebuilt; carries its new revision.
    LibraryUpdated(u64),
    /// One game's info changed.
    GameChanged(GameId),
    /// Live status of a game (install progress, playing…).
    Status(GameStatus),
    /// The game returned to idle.
    StatusCleared(GameId),
    /// Login state or user info of a store changed.
    AccountChanged(Runner),
    DownloadQueueChanged,
    WineVersionsChanged,
    SettingsChanged,
    /// Per-game settings changed.
    GameSettingsChanged(GameId),
    Connectivity(bool),
    Notice(Notice),
    /// A game process exited after `seconds` of play. `crashed` means it
    /// failed to start: it exited with an error within seconds, unasked.
    GameExited {
        id: GameId,
        seconds: u64,
        crashed: bool,
        /// The process's exit code; `None` when a signal ended it.
        exit_code: Option<i32>,
    },
}

/// Fan-out channel for [`Event`]s. Cheap to clone.
#[derive(Clone)]
pub struct EventBus {
    tx: Arc<broadcast::Sender<Event>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self { tx: Arc::new(tx) }
    }

    /// Publish an event. Dropped silently when nobody listens.
    pub fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn notice(&self, notice: Notice) {
        self.emit(Event::Notice(notice));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}
