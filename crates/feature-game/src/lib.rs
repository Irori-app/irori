//! The game page and every per-game workflow: install, import, modify,
//! uninstall, move, cloud saves, logs and help after a failed start.
//!
//! The shell renders [`GamePage`] for `Route::Game(id)`, calls [`init`] once
//! at startup (translations, key bindings), may install [`GameHooks`] to override the
//! compatibility source (default: `ember_extras`) and to provide GOG
//! private-branch passwords, and passes the concrete Epic and GOG stores to
//! [`install_store_handles`] (EOS overlay, REDmod). It routes
//! [`GamePageEvent::RequestEdit`] and [`GamePageEvent::RequestCategories`]
//! to the library's dialogs. Other features open
//! the same workflows through the `open_*` functions and reuse the game
//! commands in [`ops`] and the menu builder [`game_menu`], so a command
//! behaves the same from every entry point.

mod choice;
mod compat_source;
mod dlc;
mod hooks;
mod i18n;
mod import_installers;
mod install;
mod launch_problem;
mod load;
mod logs;
mod manage;
pub mod ops;
mod page;
mod play_with;
mod redmod;
mod saves;
pub mod sizes;
pub mod state;
#[cfg(test)]
mod ui_tests;

use gpui_kit::{App, KeyBinding};

pub use hooks::{
    AnticheatReport, AnticheatStatus, BranchPasswordFn, CompatibilityFn, CompatibilityInfo,
    GameHooks, HowLongToBeat, ReviewScore, SteamDeckStatus, install_hooks, install_store_handles,
};
pub use i18n::CATALOGS;
pub use import_installers::open_import_installers_dialog;
pub use install::{
    InstallFooter, InstallMode, InstallSheet, installable_platforms, open_install_sheet,
    open_modify_sheet,
};
pub use launch_problem::{LaunchHint, game_output_start, launch_hints, open_launch_problem_sheet};
pub use logs::{LineLevel, LogTail, LogViewer, open_log_sheet, parse_tail};
pub use manage::{
    ImportForm, UninstallOptions, open_cancel_download_dialog, open_import_dialog,
    open_move_dialog, open_uninstall_dialog,
};
pub use page::{AchievementFilter, GAME_PAGE_CONTEXT, GamePage, GamePageEvent, InfoTab, game_menu};
pub use play_with::open_play_with_dialog;
pub use redmod::{RedmodPanel, open_redmod_sheet};
pub use saves::{CloudSavesPanel, CloudSupport, open_cloud_saves_sheet};
pub use state::PrimaryAction;

gpui_kit::actions!(
    game_page,
    [
        /// Run the game page's primary action (Play, Install, Stop…).
        RunPrimaryAction,
        /// Add or remove the game from favourites.
        ToggleFavourite,
        /// Show the log of the last session.
        OpenGameLog,
        /// Open the per-game settings.
        OpenGameSettingsAction,
    ]
);

/// Register the game page's translations and key bindings. Call once after
/// `gpui_kit::init`.
pub fn init(cx: &mut App) {
    i18n::register();
    let ctx = Some(GAME_PAGE_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("ctrl-enter", RunPrimaryAction, ctx),
        KeyBinding::new("ctrl-d", ToggleFavourite, ctx),
        KeyBinding::new("ctrl-shift-l", OpenGameLog, ctx),
        KeyBinding::new("ctrl-shift-s", OpenGameSettingsAction, ctx),
    ]);
}
