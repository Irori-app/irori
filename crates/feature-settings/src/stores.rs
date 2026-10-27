//! Concrete store handles the shell hands to the settings feature for
//! store-specific panels (the backend only exposes `dyn Store`).

use std::sync::Arc;

use gpui_kit::{App, Global};
use irori_epic::EpicStore;

struct StoreHandles {
    epic: Option<Arc<EpicStore>>,
}

impl Global for StoreHandles {}

/// Give the settings feature the concrete Epic store, enabling the "Epic
/// Games" group (EOS overlay and Epic Games Launcher sync) in Advanced.
///
/// Call once at startup, after `irori_ui::init` and before the settings page
/// is first opened; forms built earlier keep hiding the group. Passing `None`
/// (Epic disabled) hides the group. A later call replaces the handle.
pub fn install_store_handles(epic: Option<Arc<EpicStore>>, cx: &mut App) {
    cx.set_global(StoreHandles { epic });
}

/// The Epic store, when the shell installed it.
pub(crate) fn epic(cx: &App) -> Option<Arc<EpicStore>> {
    cx.try_global::<StoreHandles>().and_then(|h| h.epic.clone())
}
