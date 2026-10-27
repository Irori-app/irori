//! Application-level operations that need crates the UI layer does not depend
//! on (the concrete store crates). The shell installs them at startup; feature
//! crates call them.

use std::sync::Arc;

use anyhow::Result;
use gpui_kit::{App, Global, Task, Window};

/// What a Heroic import brought over, for the confirmation message.
#[derive(Debug, Clone, Default)]
pub struct ImportSummary {
    pub stores: Vec<String>,
    pub games_with_settings: usize,
    pub favourites: usize,
    pub categories: usize,
}

/// A Lutris game that was not imported, and why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LutrisSkip {
    pub title: String,
    /// A stable reason id (`notInstalled`, `unknownRunner`, `fileMissing`,
    /// …) that the summary translates.
    pub reason: String,
    /// The runner, core, file or error the reason is about.
    pub detail: Option<String>,
}

/// What a Lutris import did, for its summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LutrisSummary {
    /// Games in Lutris's library.
    pub found: usize,
    /// Games added to Irori (apps and ROM library entries).
    pub imported: usize,
    /// Games Irori already had through its own stores or Steam; their
    /// favourites, categories and playtime were brought over.
    pub matched: usize,
    /// Store games Irori doesn't have yet, with the store to log in to.
    pub store_games: Vec<(String, String)>,
    pub skipped: Vec<LutrisSkip>,
    /// Games whose Wine build Irori doesn't have, with the build's name;
    /// they use the default build.
    pub wine_missing: Vec<(String, String)>,
    pub favourites: usize,
    pub categories: usize,
    pub playtime_games: usize,
    /// Lutris files that could not be read.
    pub warnings: Vec<String>,
}

type ImportFn = Arc<dyn Fn(&mut App) -> Task<Result<ImportSummary>>>;
type LutrisFn = Arc<dyn Fn(&mut App) -> Task<Result<LutrisSummary>>>;
type OpenFn = Arc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone, Default)]
pub struct Hooks {
    pub import_from_heroic: Option<ImportFn>,
    /// Open the "Set up emulation" guide (it lives in the management
    /// screens, which the library and Big Picture don't depend on).
    pub open_emulation_setup: Option<OpenFn>,
    pub import_from_lutris: Option<LutrisFn>,
}

impl Global for Hooks {}

pub fn install(hooks: Hooks, cx: &mut App) {
    cx.set_global(hooks);
}

/// Import accounts, installs, settings and library data from Heroic.
pub fn import_from_heroic(cx: &mut App) -> Task<Result<ImportSummary>> {
    let hook = cx
        .try_global::<Hooks>()
        .and_then(|h| h.import_from_heroic.clone());
    match hook {
        Some(f) => f(cx),
        None => Task::ready(Err(anyhow::anyhow!("import is not available"))),
    }
}

/// Open the "Set up emulation" guide; without the hook, the Emulators page.
pub fn open_emulation_setup(window: &mut Window, cx: &mut App) {
    match cx
        .try_global::<Hooks>()
        .and_then(|h| h.open_emulation_setup.clone())
    {
        Some(open) => open(window, cx),
        None => crate::navigate(crate::Route::Emulators, cx),
    }
}

/// Import games and library data from Lutris.
pub fn import_from_lutris(cx: &mut App) -> Task<Result<LutrisSummary>> {
    let hook = cx
        .try_global::<Hooks>()
        .and_then(|h| h.import_from_lutris.clone());
    match hook {
        Some(f) => f(cx),
        None => Task::ready(Err(anyhow::anyhow!("import is not available"))),
    }
}

/// Whether a Lutris library exists on this computer (native or Flatpak).
pub fn lutris_detected() -> bool {
    let home = dirs::home_dir().unwrap_or_default();
    [
        home.join(".local/share/lutris/pga.db"),
        home.join(".var/app/net.lutris.Lutris/data/lutris/pga.db"),
    ]
    .iter()
    .any(|p| p.is_file())
        || std::env::var_os("XDG_DATA_HOME")
            .is_some_and(|d| std::path::Path::new(&d).join("lutris/pga.db").is_file())
}

/// Whether a Heroic installation with importable data exists.
pub fn heroic_detected() -> bool {
    irori_core::paths::heroic_config_dir().is_some()
}
