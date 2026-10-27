//! In-window navigation with back/forward history. Features request
//! navigation through [`navigate`]; the shell renders the current route.

use ember_core::GameId;
use gpui_kit::{App, AppContext as _, Context, Entity, Global, SharedString};

/// A destination in the main window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The library, optionally scoped to a named view (e.g. a category).
    Library,
    Game(GameId),
    Downloads,
    /// Settings, opened on a section id (e.g. "general", "wine").
    Settings(SharedString),
    /// Settings of one game, opened on a section id.
    GameSettings(GameId, SharedString),
    WineManager,
    /// Emulators, their versions and cores, and each system's default.
    Emulators,
    Accounts,
    Stores,
    /// Play time per week, store and game, and streaks.
    Stats,
    /// Gamepad-first fullscreen mode.
    BigPicture,
    /// First-run welcome and setup.
    Onboarding,
}

impl Route {
    /// Top-level section this route belongs to, for sidebar highlighting.
    pub fn section(&self) -> &'static str {
        match self {
            Route::Library | Route::Game(_) | Route::GameSettings(..) => "library",
            Route::Downloads => "downloads",
            Route::Settings(_) => "settings",
            Route::WineManager => "wine",
            Route::Emulators => "emulators",
            Route::Accounts => "accounts",
            Route::Stores => "stores",
            Route::Stats => "stats",
            Route::BigPicture => "big-picture",
            Route::Onboarding => "onboarding",
        }
    }
}

pub struct Navigator {
    current: Route,
    back: Vec<Route>,
    forward: Vec<Route>,
}

impl Global for NavigatorHandle {}

#[derive(Clone)]
pub struct NavigatorHandle(pub Entity<Navigator>);

impl Navigator {
    pub fn current(&self) -> &Route {
        &self.current
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// The route `back` returns to.
    pub fn previous(&self) -> Option<&Route> {
        self.back.last()
    }

    pub fn go(&mut self, route: Route, cx: &mut Context<Self>) {
        if route == self.current {
            return;
        }
        let prev = std::mem::replace(&mut self.current, route);
        self.back.push(prev);
        if self.back.len() > 64 {
            self.back.remove(0);
        }
        self.forward.clear();
        cx.notify();
    }

    /// Replace the current route without adding history.
    pub fn replace(&mut self, route: Route, cx: &mut Context<Self>) {
        self.current = route;
        cx.notify();
    }

    pub fn back(&mut self, cx: &mut Context<Self>) {
        if let Some(prev) = self.back.pop() {
            let cur = std::mem::replace(&mut self.current, prev);
            self.forward.push(cur);
            cx.notify();
        }
    }

    pub fn forward(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.forward.pop() {
            let cur = std::mem::replace(&mut self.current, next);
            self.back.push(cur);
            cx.notify();
        }
    }
}

pub(crate) fn install(cx: &mut App) {
    let nav = cx.new(|_| Navigator {
        current: Route::Library,
        back: Vec::new(),
        forward: Vec::new(),
    });
    cx.set_global(NavigatorHandle(nav));
}

/// The navigator entity.
pub fn navigator(cx: &App) -> Entity<Navigator> {
    cx.global::<NavigatorHandle>().0.clone()
}

/// Navigate to a route.
pub fn navigate(route: Route, cx: &mut App) {
    navigator(cx).update(cx, |n, cx| n.go(route, cx));
}

pub fn go_back(cx: &mut App) {
    navigator(cx).update(cx, |n, cx| n.back(cx));
}

pub fn go_forward(cx: &mut App) {
    navigator(cx).update(cx, |n, cx| n.forward(cx));
}
