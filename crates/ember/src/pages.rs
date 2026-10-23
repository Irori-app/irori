//! Route → view mapping. Long-lived views (library, downloads) are kept across
//! navigation so scroll position and filters survive; detail views are
//! rebuilt when their subject changes.

use ember_ui::Route;
use gpui_kit::{AnyView, AppContext as _, Context, Entity, Focusable as _, Subscription, Window};

#[derive(Default)]
pub struct Pages {
    current: Option<(Route, AnyView)>,
    downloads: Option<AnyView>,
    wine: Option<AnyView>,
    accounts: Option<AnyView>,
    stores: Option<AnyView>,
    settings: Option<Entity<ember_settings::SettingsView>>,
    library: Option<Entity<ember_library::LibraryView>>,
    /// Subscription to the current game page (replaced with the page).
    game_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl Pages {
    pub fn view<V: 'static>(
        &mut self,
        route: &Route,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> AnyView {
        if let Some((r, v)) = &self.current
            && r == route
        {
            return v.clone();
        }
        let view = self.build(route, window, cx);
        self.current = Some((route.clone(), view.clone()));
        view
    }

    fn build<V: 'static>(
        &mut self,
        route: &Route,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> AnyView {
        match route {
            Route::Library => {
                let view = match &self.library {
                    Some(v) => v.clone(),
                    None => {
                        let v = cx.new(|cx| ember_library::LibraryView::new(window, cx));
                        self._subscriptions.push(cx.subscribe_in(
                            &v,
                            window,
                            |_, _, event, window, cx| {
                                crate::routing::library_event(event, window, cx)
                            },
                        ));
                        self.library = Some(v.clone());
                        v
                    }
                };
                let focus = view.read(cx).focus_handle(cx);
                window.defer(cx, move |window, cx| focus.focus(window, cx));
                view.into()
            }
            Route::Downloads => cached(&mut self.downloads, || {
                cx.new(|cx| ember_manage::DownloadsView::new(window, cx))
                    .into()
            }),
            Route::WineManager => cached(&mut self.wine, || {
                cx.new(|cx| ember_manage::WineManagerView::new(window, cx))
                    .into()
            }),
            Route::Accounts => cached(&mut self.accounts, || {
                cx.new(|cx| ember_manage::AccountsView::new(window, cx))
                    .into()
            }),
            Route::Stores => cached(&mut self.stores, || {
                cx.new(|cx| ember_manage::StoresView::new(window, cx))
                    .into()
            }),
            Route::Settings(section) => {
                let section = section.clone();
                match &self.settings {
                    Some(view) => {
                        view.update(cx, |v, cx| v.set_section(&section, window, cx));
                        view.clone().into()
                    }
                    None => {
                        let view =
                            cx.new(|cx| ember_settings::SettingsView::new(section, window, cx));
                        self.settings = Some(view.clone());
                        view.into()
                    }
                }
            }
            Route::Game(id) => {
                let id = id.clone();
                let page = cx.new(|cx| ember_game::GamePage::new(id, window, cx));
                self.game_subscription =
                    Some(
                        cx.subscribe_in(&page, window, |_, page, event, window, cx| {
                            crate::routing::game_event(page, event, window, cx)
                        }),
                    );
                page.into()
            }
            // Rebuilt on each visit so the numbers are fresh.
            Route::Stats => cx
                .new(|cx| ember_library::StatsView::new(window, cx))
                .into(),
            Route::Onboarding => cx
                .new(|cx| ember_manage::OnboardingView::new(window, cx))
                .into(),
            Route::BigPicture => cx
                .new(|cx| ember_bigpicture::BigPictureView::new(window, cx))
                .into(),
            Route::GameSettings(id, section) => {
                let (id, section) = (id.clone(), section.clone());
                cx.new(|cx| ember_settings::GameSettingsView::new(id, section, window, cx))
                    .into()
            }
        }
    }
}

fn cached(slot: &mut Option<AnyView>, make: impl FnOnce() -> AnyView) -> AnyView {
    slot.get_or_insert_with(make).clone()
}
