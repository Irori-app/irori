//! Management screens of Ember: the download queue, the Wine/Proton manager,
//! store accounts (with the browser login flow), store links and deals, and the
//! first-run onboarding.
//!
//! The shell owns routing and mounts these views for [`ember_ui::Route::Downloads`],
//! [`ember_ui::Route::WineManager`], [`ember_ui::Route::Accounts`] and
//! [`ember_ui::Route::Stores`]. It shows [`OnboardingView`] instead of the
//! library while [`should_show_onboarding`] is true, and removes it on
//! [`OnboardingEvent::Finished`]. Call [`init`] once at startup to register
//! this crate's translations.

mod accounts;
mod common;
mod deals;
mod downloads;
mod emulators;
mod format;
mod i18n;
mod launchers;
mod login;
mod onboarding;
mod stores;
mod wine;

pub use accounts::AccountsView;
pub use downloads::DownloadsView;
pub use emulators::{
    EmulatorsView, InstallTarget, choice_name, open_emulation_setup, open_engine_settings,
    start_install,
};
pub use launchers::install_launchers_store;
pub use login::{open_account_login_dialog, open_login_dialog};
pub use onboarding::{
    OnboardingEvent, OnboardingView, mark_onboarding_done, should_show_onboarding,
};
pub use stores::StoresView;
pub use wine::WineManagerView;

pub use i18n::CATALOGS;

/// Register this crate's translations (`manage.` keys). Idempotent; the
/// views also call it on construction.
pub fn init(_cx: &mut gpui_kit::App) {
    i18n::register();
}

#[cfg(test)]
mod ui_tests;
