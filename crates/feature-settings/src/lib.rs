//! Irori's settings feature: the application settings page, the game
//! defaults form, per-game settings with visible inheritance, the launcher
//! log viewer and system information.
//!
//! Public seam:
//! - [`SettingsView`] renders `Route::Settings(section)`.
//! - [`GameSettingsView`] renders `Route::GameSettings(id, section)`.
//! - [`GameSettingsForm`] is the shared launch-settings form, bound either to
//!   the global defaults or to one game's sparse overrides.
//! - [`registry`] describes every editable field (label, keywords, section)
//!   and provides the search used by the settings search field.
//! - [`install_store_handles`] hands over the concrete Epic store for the
//!   EOS overlay and Epic Games Launcher sync panels.
//!
//! - [`init`] registers the translation catalogs ([`CATALOGS`]); call it
//!   once at startup, before the settings views are first rendered.
//!
//! Every write goes through `irori_services::SettingsService`
//! (`update_app` / `set_game_value`). Text fields commit on blur or Enter,
//! never per keystroke.

mod backup;
mod controllers;
mod epic;
mod form;
mod game_view;
mod i18n;
mod logs;
pub mod registry;
mod saves;
mod stores;
pub mod value;
mod view;
mod widgets;
mod winetricks;

pub use form::{FormTarget, GameSettingsForm, SettingsForm};
pub use game_view::{GameSection, GameSettingsView};
pub use i18n::CATALOGS;
pub use logs::LogViewer;
pub use stores::install_store_handles;
pub use view::{SettingsSection, SettingsView};

/// Register the settings feature's translations with `irori_ui::i18n`.
/// Safe to call more than once; only the first call registers.
pub fn init(_cx: &mut gpui_kit::App) {
    i18n::register();
}

#[cfg(test)]
mod ui_tests;
