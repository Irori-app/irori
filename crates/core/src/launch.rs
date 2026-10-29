//! The contract between the runtime (Wine/Proton/wrappers), the stores (which
//! know their own launch tool flags) and the services layer (which spawns and
//! tracks the game).

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::game::LaunchOption;
use crate::process::CommandSpec;
use crate::settings::{GameSettings, WineKind};

/// How to run Windows binaries for this launch.
#[derive(Debug, Clone)]
pub struct WineLaunch {
    /// Program to execute Windows binaries: `wine`, the `proton` script, or `umu-run`.
    pub bin: PathBuf,
    /// The configured Wine/Proton build (`bin` of the settings), even when `bin` is umu-run.
    pub build_bin: PathBuf,
    pub kind: WineKind,
    pub prefix: PathBuf,
    /// Launch goes through umu-run (Proton with the Steam Linux Runtime).
    pub uses_umu: bool,
    /// Proton verb to use when `kind` is Proton without umu (`waitforexitandrun`).
    pub proton_verb: &'static str,
}

impl WineLaunch {
    /// Command prefix that runs a Windows executable: `[wine]`, `[proton, verb]` or `[umu-run]`.
    pub fn exec_prefix(&self) -> Vec<String> {
        let bin = self.bin.to_string_lossy().into_owned();
        if !self.uses_umu && self.kind == WineKind::Proton {
            vec![bin, self.proton_verb.to_string()]
        } else {
            vec![bin]
        }
    }
}

/// Everything the runtime prepared for a launch: environment, wrapper chain
/// (gamescope, user wrappers, mangohud, gamemoderun — outermost first) and the
/// Wine layer for Windows games.
///
/// `umu-run` is never part of `wrappers`: when umu is used it is
/// [`WineLaunch::bin`] with `uses_umu` set, so stores add it exactly once (as
/// the Proton wrapper of their launch tool, or as the program for direct runs).
#[derive(Debug, Clone, Default)]
pub struct LaunchEnvironment {
    pub env: BTreeMap<String, String>,
    pub wrappers: Vec<Vec<String>>,
    pub wine: Option<WineLaunch>,
}

impl LaunchEnvironment {
    /// Wrapper chain flattened into one argv prefix.
    pub fn wrapper_argv(&self) -> Vec<String> {
        self.wrappers.iter().flatten().cloned().collect()
    }
}

/// What the services layer asks a store to launch.
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub settings: GameSettings,
    pub environment: LaunchEnvironment,
    pub option: Option<LaunchOption>,
    /// Extra arguments (from a protocol URL or the user).
    pub extra_args: Vec<String>,
    pub offline: bool,
    /// Skip the store's version check (launch outdated install).
    pub skip_version_check: bool,
    pub log_file: PathBuf,
}

/// The command a store wants run for a launch.
#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub command: CommandSpec,
    /// Seconds to wait after the launcher process exits for detached game
    /// processes in the prefix before treating the session as over.
    pub track_wineserver: bool,
    /// For games another program starts and owns (Steam): the command only
    /// hands the game over and exits, so the session lasts while this watch
    /// sees the game, and stopping goes through it.
    pub watch: Option<Arc<dyn GameWatch>>,
}

/// Follows a game that another program started. Both methods block (they may
/// look through the running processes), so callers run them off the async
/// runtime's workers.
pub trait GameWatch: Send + Sync {
    /// Whether the game runs, or is still being prepared to run (a launch-time
    /// update), right now.
    fn running(&self) -> bool;

    /// Ask the game to quit, never the program that started it.
    fn stop(&self, force: bool);
}

impl fmt::Debug for dyn GameWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GameWatch")
    }
}
