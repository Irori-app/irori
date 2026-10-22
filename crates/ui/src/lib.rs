//! Ember's shared UI layer. Feature crates depend on this crate for backend
//! access ([`state`]), observable models ([`models`]), navigation ([`nav`]),
//! game art ([`art`]), theming ([`theme`]), motion ([`motion`]) and domain
//! components.

pub mod art;
pub mod components;
pub mod hooks;
pub mod i18n;
pub mod models;
pub mod motion;
pub mod nav;
pub mod state;
pub mod style;
pub mod theme;

pub use models::{Models, models};
pub use nav::{Route, navigate};
pub use state::{backend, run};

use ember_services::Backend;
use gpui_kit::App;

/// Install backend access, models, navigation, art and themes. Call after
/// `gpui_kit::init` and before opening windows.
pub fn init(backend: Backend, cx: &mut App) {
    i18n::register(i18n::CATALOGS);
    i18n::set_language(&backend.settings.app().ui_language);
    state::AppState::install(backend, cx);
    theme::install(cx);
    art::install(cx);
    nav::install(cx);
    models::install(cx);
}
