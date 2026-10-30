//! Irori's shared UI layer. Feature crates depend on this crate for backend
//! access ([`state`]), observable models ([`models`]), navigation ([`nav`]),
//! game art ([`art`]), theming ([`theme`]), motion ([`motion`]), desktop
//! notifications ([`system_notify`]) and domain components.

pub mod art;
pub mod components;
mod engine_problem;
pub mod glass;
pub mod hooks;
pub mod i18n;
pub mod lutris_summary;
pub mod models;
pub mod motion;
pub mod nav;
pub mod save_conflict;
pub mod state;
pub mod style;
pub mod system_notify;
pub mod theme;

pub use models::{Models, models};
pub use nav::{Route, navigate};
pub use state::{backend, run};

/// `url` when it is a web address. Links from stores and community data go
/// through this: the system opens anything else (`file:`, a network share, a
/// custom scheme) with whatever handles it.
pub fn web_url(url: &str) -> Option<String> {
    let url = url.trim();
    let scheme = url.split_once("://")?.0;
    (scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http"))
        .then(|| url.to_string())
}

use gpui_kit::App;
use irori_services::Backend;

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
