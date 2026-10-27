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

type ImportFn = Arc<dyn Fn(&mut App) -> Task<Result<ImportSummary>>>;
type OpenFn = Arc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone, Default)]
pub struct Hooks {
    pub import_from_heroic: Option<ImportFn>,
    /// Open the "Set up emulation" guide (it lives in the management
    /// screens, which the library and Big Picture don't depend on).
    pub open_emulation_setup: Option<OpenFn>,
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

/// Whether a Heroic installation with importable data exists.
pub fn heroic_detected() -> bool {
    ember_core::paths::heroic_config_dir().is_some()
}
