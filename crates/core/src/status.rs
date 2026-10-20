use serde::{Deserialize, Serialize};

use crate::install::InstallProgress;
use crate::runner::GameId;

/// What is happening to a game right now. Mirrors Heroic's status strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StatusKind {
    Installing,
    Importing,
    Updating,
    Launching,
    Playing,
    Uninstalling,
    Repairing,
    Moving,
    Queued,
    SyncingSaves,
    Redist,
    Extracting,
    Winetricks,
    /// Idle states, reported when an operation ends.
    Done,
    Canceled,
    Error,
}

impl StatusKind {
    /// Whether this status blocks starting another operation on the game.
    pub fn is_busy(self) -> bool {
        !matches!(
            self,
            StatusKind::Done | StatusKind::Canceled | StatusKind::Error
        )
    }

    /// Present-progressive label for status chips.
    pub fn label(self) -> &'static str {
        match self {
            StatusKind::Installing => "Installing",
            StatusKind::Importing => "Importing",
            StatusKind::Updating => "Updating",
            StatusKind::Launching => "Launching",
            StatusKind::Playing => "Playing",
            StatusKind::Uninstalling => "Uninstalling",
            StatusKind::Repairing => "Verifying",
            StatusKind::Moving => "Moving",
            StatusKind::Queued => "Queued",
            StatusKind::SyncingSaves => "Syncing saves",
            StatusKind::Redist => "Installing dependencies",
            StatusKind::Extracting => "Extracting",
            StatusKind::Winetricks => "Running Winetricks",
            StatusKind::Done => "Done",
            StatusKind::Canceled => "Canceled",
            StatusKind::Error => "Error",
        }
    }
}

/// Live status of one game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameStatus {
    pub id: GameId,
    pub kind: StatusKind,
    pub progress: Option<InstallProgress>,
    /// Extra detail such as the current step ("Installing DirectX").
    pub context: Option<String>,
}

impl GameStatus {
    pub fn new(id: GameId, kind: StatusKind) -> Self {
        Self {
            id,
            kind,
            progress: None,
            context: None,
        }
    }
}
