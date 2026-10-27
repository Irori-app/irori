//! What Ember installed and what the user chose, in `<state>/engines.json`:
//! managed versions per engine, the installation each engine uses by
//! default, custom program paths and installed RetroArch cores.
//!
//! The default installation of an engine changes only when the user picks
//! another one or presses Update; a game pinned to a version keeps it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use ember_core::{Paths, json_store};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// A version Ember downloaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedInstall {
    pub version: String,
    /// The install folder.
    pub dir: PathBuf,
    /// The program, inside `dir`.
    pub program: PathBuf,
    /// Size on disk in bytes.
    #[serde(default)]
    pub size: u64,
    /// RFC 3339.
    #[serde(default)]
    pub installed_at: String,
    /// Where it came from.
    #[serde(default)]
    pub url: String,
}

/// A RetroArch core Ember downloaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreInstall {
    pub path: PathBuf,
    /// The buildbot's date of the build, `YYYY-MM-DD`.
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EngineState {
    /// Managed versions by engine id, oldest first.
    pub managed: BTreeMap<String, Vec<ManagedInstall>>,
    /// The installation each engine uses unless a game says otherwise
    /// (an [`Installation::id`](crate::Installation::id)).
    pub preferred: BTreeMap<String, String>,
    /// Programs the user pointed Ember at, by engine id.
    pub custom: BTreeMap<String, PathBuf>,
    /// Installed cores by core id.
    pub cores: BTreeMap<String, CoreInstall>,
    /// Engines whose exit hotkey the user has been shown.
    pub hotkey_shown: std::collections::BTreeSet<String>,
}

fn file() -> PathBuf {
    Paths::get().state_dir().join("engines.json")
}

static LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// The saved state (empty when there is none or it is damaged).
pub fn load() -> EngineState {
    json_store::read_or_default(&file())
}

/// Change the saved state under a lock, so concurrent installs don't lose
/// each other's entries. Blocking: call from the blocking pool.
pub fn update<T>(change: impl FnOnce(&mut EngineState) -> T) -> Result<T> {
    let _guard = LOCK.lock();
    let mut state = load();
    let out = change(&mut state);
    json_store::write(&file(), &state)?;
    Ok(out)
}

impl EngineState {
    pub fn managed_versions(&self, engine: &str) -> &[ManagedInstall] {
        self.managed.get(engine).map(Vec::as_slice).unwrap_or(&[])
    }
}
