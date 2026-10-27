//! The library screen: every game of every store in a virtualized cover grid
//! or list, with instant search, store switcher, filters, sorting, a
//! "Continue playing" row, keyboard navigation, per-game context menus, custom
//! categories, manually added apps and per-game title/art overrides.
//!
//! # Wiring (shell)
//!
//! 1. Call [`init`] once at startup (binds the library's keys and registers
//!    its translations) and
//!    [`install_sideload_store`] with the same `Arc<SideloadStore>` handed to the
//!    backend, so the "Add game" and "Edit app" dialogs can reach it.
//! 2. Create the view with [`LibraryView::new`] and render it for
//!    [`ember_ui::Route::Library`].
//! 3. Subscribe to [`LibraryEvent`]s: the library asks the game feature to open
//!    its install sheet, uninstall confirmation and log viewer, which it does
//!    not own.
//!
//! Other features can open the library's dialogs directly:
//! [`open_add_game_dialog`], [`open_edit_game_dialog`] (a game page's
//! "Edit…") and [`open_categories_dialog`] (`Some(id)` for a game page's
//! "Categories…", `None` for the category manager).

mod availability;
mod commands;
mod dialogs;
mod filter;
mod filters_panel;
mod format;
mod grid;
mod i18n;
mod launch;
mod random;
mod selection;
mod stats;
mod top;
mod view;

use std::sync::Arc;

use ember_core::GameId;
use ember_sideload::SideloadStore;
use gpui_kit::{App, Global, KeyBinding, Window};

pub use commands::{GameCommand, GameState};
pub use dialogs::categories::open_categories_dialog;
pub use dialogs::edit_game::open_edit_game_dialog;
pub use dialogs::sideload::open_add_game_dialog;
pub use filter::{CategoryFilter, HiddenMode, InstallFilter, ViewFilter};
pub use i18n::CATALOGS;
pub use random::open_random_game;
pub use stats::StatsView;
pub use view::LibraryView;

/// Requests for actions owned by other features. The shell routes them (for
/// example to the game feature's install sheet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryEvent {
    /// Open the install sheet for a game.
    RequestInstall(GameId),
    /// Open the uninstall confirmation for a game (sideloaded apps included).
    RequestUninstall(GameId),
    /// Open the log viewer of a game.
    RequestLogs(GameId),
    /// Pick a folder of GOG offline installers and install from it.
    RequestImportInstallers,
}

gpui_kit::actions!(
    library,
    [
        /// Move keyboard focus to the search field.
        FocusSearch,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectFirst,
        SelectLast,
        SelectPageUp,
        SelectPageDown,
        /// Open the focused game's page.
        OpenSelected,
        /// Run the focused game's primary action (Play, Stop or Install).
        ActivateSelected,
        /// Pick every shown game for bulk actions.
        SelectAllGames,
        /// Leave selection mode.
        EndSelection,
    ]
);

/// Key context of the whole library screen.
pub(crate) const LIBRARY_CONTEXT: &str = "Library";
/// Key context of the focused cover grid or list.
pub(crate) const GRID_CONTEXT: &str = "LibraryGrid";

struct KeysBound;
impl Global for KeysBound {}

/// Register the library's translations and bind its keys. Idempotent;
/// [`LibraryView::new`] calls it too.
pub fn init(cx: &mut App) {
    i18n::register();
    if cx.has_global::<KeysBound>() {
        return;
    }
    cx.set_global(KeysBound);
    let grid = Some(GRID_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("ctrl-f", FocusSearch, Some(LIBRARY_CONTEXT)),
        KeyBinding::new("/", FocusSearch, grid),
        KeyBinding::new("left", SelectLeft, grid),
        KeyBinding::new("right", SelectRight, grid),
        KeyBinding::new("up", SelectUp, grid),
        KeyBinding::new("down", SelectDown, grid),
        KeyBinding::new("home", SelectFirst, grid),
        KeyBinding::new("end", SelectLast, grid),
        KeyBinding::new("pageup", SelectPageUp, grid),
        KeyBinding::new("pagedown", SelectPageDown, grid),
        KeyBinding::new("enter", OpenSelected, grid),
        KeyBinding::new("space", ActivateSelected, grid),
        KeyBinding::new("ctrl-enter", ActivateSelected, grid),
        KeyBinding::new("ctrl-a", SelectAllGames, grid),
        KeyBinding::new("escape", EndSelection, grid),
    ]);
}

struct SideloadHandle(Arc<SideloadStore>);
impl Global for SideloadHandle {}

/// Give the library access to the sideload store (the backend only exposes
/// stores as `dyn Store`). Without it, adding and editing apps reports that
/// the feature is unavailable.
pub fn install_sideload_store(store: Arc<SideloadStore>, cx: &mut App) {
    cx.set_global(SideloadHandle(store));
}

pub(crate) fn sideload_store(cx: &App) -> Option<Arc<SideloadStore>> {
    cx.try_global::<SideloadHandle>().map(|h| h.0.clone())
}

struct EmulationHandle(Arc<ember_emulation::EmulationStore>);
impl Global for EmulationHandle {}

/// Give the library access to the ROM library store, so Add game can put a
/// ROM there.
pub fn install_emulation_store(store: Arc<ember_emulation::EmulationStore>, cx: &mut App) {
    cx.set_global(EmulationHandle(store));
}

pub(crate) fn emulation_store(cx: &App) -> Option<Arc<ember_emulation::EmulationStore>> {
    cx.try_global::<EmulationHandle>().map(|h| h.0.clone())
}

/// Show an error notification in `window`.
pub(crate) fn notify_error(
    title: impl Into<gpui_kit::SharedString>,
    error: &anyhow::Error,
    window: &mut Window,
    cx: &mut App,
) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::notification::Notification;
    window.push_notification(
        Notification::error(format!("{error:#}")).title(title.into()),
        cx,
    );
}

#[cfg(test)]
mod ui_tests;
