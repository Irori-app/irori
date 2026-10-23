//! The main window: title bar, sidebar navigation and the routed content area.

use ember_core::{GameId, NoticeLevel, Runner, StatusKind};
use ember_ui::i18n::{t, t_args};
use ember_ui::models::{self, models};
use ember_ui::nav::{self, Route, navigator};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, TitleBar,
    WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, rems,
};

use crate::actions::*;
use crate::pages::Pages;
use crate::palette::Palette;
use crate::sidebar;

/// How far a newly shown page rises into place, in pixels.
const PAGE_RISE_PX: f32 = 8.0;

pub struct AppShell {
    focus: FocusHandle,
    palette: Palette,
    pages: Pages,
    sidebar_collapsed: bool,
    /// The sidebar was opened by hand while the window is narrow, where it
    /// otherwise collapses on its own. Not saved: a wide window forgets it.
    narrow_expanded: bool,
    recent: Entity<sidebar::RecentGames>,
    /// Games we minimized the window for, to restore when they exit.
    minimized_for: Option<GameId>,
    /// Refreshes the title bar's elapsed play time while a game runs.
    playing_ticker: Option<Task<()>>,
    /// Appearance preferences last applied to the theme.
    applied_ui: Option<ember_core::settings::UiPrefs>,
    _subscriptions: Vec<Subscription>,
}

impl AppShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let m = models(cx).clone();
        let nav = navigator(cx);
        let subs = vec![
            cx.observe(&nav, |_, _, cx| cx.notify()),
            cx.observe(&m.downloads, |_, _, cx| cx.notify()),
            cx.observe(&m.accounts, |_, _, cx| cx.notify()),
            cx.observe_in(&m.notices, window, |this, notices, window, cx| {
                this.show_notices(&notices, window, cx);
            }),
            cx.observe_in(&m.statuses, window, |this, _, window, cx| {
                this.minimize_on_launch(window, cx);
                this.sync_playing_ticker(cx);
            }),
            cx.observe_in(&m.sessions, window, |this, sessions, window, cx| {
                this.on_sessions(&sessions, window, cx);
            }),
            cx.observe_in(&m.settings, window, |this, settings, window, cx| {
                let lang = settings.read(cx).app.ui_language.clone();
                if lang != ember_ui::i18n::language() {
                    ember_ui::i18n::set_language(&lang);
                    window.refresh();
                }
                let prefs = settings.read(cx).app.ui.clone();
                // GPUI holds every animation still while this is set.
                let reduce = ember_ui::style::reduce_motion(cx);
                cx.set_reduce_motion(reduce);
                // Settings notify for every change; re-theme only when the
                // appearance preferences did.
                if this.applied_ui.as_ref() != Some(&prefs) {
                    ember_ui::theme::apply(&prefs, Some(window), cx);
                    this.applied_ui = Some(prefs);
                }
            }),
        ];
        let collapsed = m.settings.read(cx).app.ui.sidebar_collapsed;
        let reduce = ember_ui::style::reduce_motion(cx);
        cx.set_reduce_motion(reduce);
        Self {
            focus: cx.focus_handle(),
            palette: Palette::new(window, cx),
            pages: Pages::default(),
            sidebar_collapsed: collapsed,
            narrow_expanded: false,
            recent: cx.new(sidebar::RecentGames::new),
            minimized_for: None,
            playing_ticker: None,
            applied_ui: Some(m.settings.read(cx).app.ui.clone()),
            _subscriptions: subs,
        }
    }

    fn show_notices(
        &mut self,
        notices: &gpui_kit::Entity<models::NoticeModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pending = notices.update(cx, |n, _| n.drain());
        for n in pending {
            let args: Vec<(&str, &str)> = n.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let title = match n.title_key {
                Some(key) => t_args(key, &args),
                None => SharedString::from(n.title.clone()),
            };
            let message = match (n.message_key, &n.message) {
                (Some(key), _) => Some(t_args(key, &args)),
                (None, Some(m)) => Some(SharedString::from(m.clone())),
                (None, None) => None,
            };
            let mut note = match n.level {
                NoticeLevel::Info => Notification::info(title.clone()),
                NoticeLevel::Success => Notification::success(title.clone()),
                NoticeLevel::Warning => Notification::warning(title.clone()),
                NoticeLevel::Error => Notification::error(title.clone()),
            };
            if let Some(msg) = message {
                note = note.title(title).message(msg);
            }
            if let Some(id) = n.game.clone() {
                note = note.on_click(move |_, _, cx| {
                    nav::navigate(Route::Game(id.clone()), cx);
                });
            }
            window.push_notification(note, cx);
        }
    }

    fn minimize_on_launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.minimized_for.is_some() || !models(cx).settings.read(cx).app.minimize_on_launch {
            return;
        }
        let playing = models(cx).statuses.read(cx).playing().next().cloned();
        if let Some(id) = playing {
            self.minimized_for = Some(id);
            window.minimize_window();
        }
    }

    fn on_sessions(
        &mut self,
        sessions: &gpui_kit::Entity<models::SessionModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let exited: Vec<_> = sessions.update(cx, |s, _| s.exited.drain(..).collect());
        for (id, seconds, crashed) in exited {
            if self.minimized_for.as_ref() == Some(&id) {
                self.minimized_for = None;
                window.activate_window();
            }
            if let Some(note) = crate::playing::session_note(&id, seconds, crashed, cx) {
                window.push_notification(note, cx);
            }
        }
    }

    /// Run the title bar's play-time ticker exactly while a game is playing,
    /// so an idle launcher keeps using no CPU.
    fn sync_playing_ticker(&mut self, cx: &mut Context<Self>) {
        let playing = models(cx).statuses.read(cx).playing().next().is_some();
        if !playing {
            self.playing_ticker = None;
        } else if self.playing_ticker.is_none() {
            self.playing_ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(crate::playing::TICK).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
    }

    /// Whether the window is too narrow for the full sidebar beside the page.
    fn is_narrow(window: &Window) -> bool {
        window.viewport_size().width / window.rem_size() < sidebar::NARROW_WINDOW_REM
    }

    /// Whether the sidebar shows collapsed right now: the saved choice, except
    /// in narrow windows, where it collapses unless opened by hand.
    fn sidebar_collapsed_now(&self, window: &Window) -> bool {
        if Self::is_narrow(window) {
            !self.narrow_expanded
        } else {
            self.sidebar_collapsed
        }
    }

    /// Ctrl+B and the sidebar button. In a narrow window this only opens the
    /// sidebar for now; the saved preference is for wide windows.
    fn toggle_sidebar(&mut self, window: &Window, cx: &mut Context<Self>) {
        if Self::is_narrow(window) {
            self.narrow_expanded = !self.narrow_expanded;
            cx.notify();
        } else {
            let c = !self.sidebar_collapsed;
            self.set_sidebar_collapsed(c, cx);
        }
    }

    fn set_sidebar_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.sidebar_collapsed = collapsed;
        let b = ember_ui::backend(cx);
        b.settings
            .update_app(|s| s.ui.sidebar_collapsed = collapsed);
        cx.notify();
    }

    fn render_title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = navigator(cx).read(cx);
        let (can_back, can_forward) = (nav.can_go_back(), nav.can_go_forward());
        let downloads = &models(cx).downloads.read(cx).queue;
        let statuses = models(cx).statuses.read(cx);
        let current = downloads.current().map(|item| {
            let pct = statuses
                .get(&item.id)
                .and_then(|s| s.progress)
                .and_then(|p| p.percent)
                .unwrap_or(0.0);
            (item.title.clone(), pct)
        });
        let queued = downloads.active.len();
        let online = models(cx).accounts.read(cx).online;
        let playing = crate::playing::pill(cx);
        let _ = window;

        TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_2()
                .gap_1()
                .justify_between()
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("nav-back")
                                .ghost()
                                .small()
                                .icon(IconName::ChevronLeft)
                                .tooltip(t("shell.nav.back"))
                                .disabled(!can_back)
                                .on_click(|_, _, cx| nav::go_back(cx)),
                        )
                        .child(
                            Button::new("nav-forward")
                                .ghost()
                                .small()
                                .icon(IconName::ChevronRight)
                                .tooltip(t("shell.nav.forward"))
                                .disabled(!can_forward)
                                .on_click(|_, _, cx| nav::go_forward(cx)),
                        ),
                )
                .child(
                    Button::new("open-palette")
                        .outline()
                        .small()
                        .w(rems(22.))
                        .icon(IconName::Search)
                        .label(t("shell.search.placeholder"))
                        .tooltip("Ctrl+K")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.palette.open(window, cx);
                        })),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .children(playing)
                        .when(!online, |this| {
                            this.child(
                                h_flex()
                                    .id("offline")
                                    .gap_1()
                                    .px_2()
                                    .text_xs()
                                    .text_color(cx.theme().warning)
                                    .child(Icon::new(IconName::WifiOff).xsmall())
                                    .child(t("shell.offline"))
                                    .tooltip(|window, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new(t(
                                            "shell.offline.tooltip",
                                        ))
                                        .build(window, cx)
                                    }),
                            )
                        })
                        .child(
                            Button::new("title-downloads")
                                .ghost()
                                .small()
                                .map(|b| match &current {
                                    Some((title, pct)) => b
                                        .child(
                                            ProgressCircle::new("title-download-progress")
                                                .value(*pct)
                                                .xsmall(),
                                        )
                                        .tooltip(t_args(
                                            "shell.downloads.progress",
                                            &[("title", title), ("percent", &format!("{pct:.0}"))],
                                        )),
                                    None => b.icon(IconName::Download).tooltip(if queued > 0 {
                                        t("shell.downloads.paused")
                                    } else {
                                        t("shell.nav.downloads")
                                    }),
                                })
                                .on_click(|_, _, cx| nav::navigate(Route::Downloads, cx)),
                        ),
                ),
        )
    }

    fn render_sidebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let section = navigator(cx).read(cx).current().section();
        let downloads = models(cx).downloads.read(cx).queue.active.len();
        let collapsed = self.sidebar_collapsed_now(window);
        let any_account = models(cx).accounts.read(cx).any_logged_in();
        let theme = cx.theme();
        let item = |id, label: &'static str, icon, key: &str, route, suffix, cx: &App| {
            sidebar::nav_item(
                sidebar::NavItem {
                    id,
                    label: t(label),
                    icon,
                    route,
                    active: section == key,
                    suffix,
                },
                collapsed,
                cx,
            )
        };
        let download_count = (downloads > 0).then(|| {
            div()
                .text_xs()
                .font_bold()
                .px_1p5()
                .rounded_full()
                .bg(theme.primary)
                .text_color(theme.primary_foreground)
                .child(downloads.to_string())
                .into_any_element()
        });

        v_flex()
            .id("main-sidebar")
            .h_full()
            .w(rems(if collapsed {
                sidebar::COLLAPSED_REM
            } else {
                sidebar::WIDTH_REM
            }))
            .flex_shrink_0()
            .px_3()
            .pb_3()
            .gap_0p5()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex()
                    .h(rems(4.5))
                    .gap_2p5()
                    .px_1()
                    .when(collapsed, |d| d.justify_center().px_0())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size_8()
                            .flex_shrink_0()
                            .rounded(theme.radius)
                            .bg(gpui_kit::linear_gradient(
                                135.,
                                gpui_kit::linear_color_stop(theme.warning, 0.),
                                gpui_kit::linear_color_stop(theme.sidebar_primary, 0.6),
                            ))
                            .text_color(theme.sidebar_primary_foreground)
                            .child(Icon::new(IconName::Flame)),
                    )
                    .when(!collapsed, |this| {
                        this.child(div().font_extrabold().text_xl().child("Ember"))
                    }),
            )
            .child(item(
                "nav-library",
                "shell.nav.library",
                IconName::LibraryBig,
                "library",
                Route::Library,
                None,
                cx,
            ))
            .child(item(
                "nav-downloads",
                "shell.nav.downloads",
                IconName::Download,
                "downloads",
                Route::Downloads,
                download_count,
                cx,
            ))
            .child(item(
                "nav-stores",
                "shell.nav.stores",
                IconName::Store,
                "stores",
                Route::Stores,
                None,
                cx,
            ))
            .child(item(
                "nav-stats",
                "shell.nav.stats",
                IconName::ChartColumn,
                "stats",
                Route::Stats,
                None,
                cx,
            ))
            .when(!collapsed, |d| d.child(self.recent.clone()))
            .child(div().flex_1())
            .child(item(
                "nav-wine",
                "shell.nav.wine",
                IconName::Wine,
                "wine",
                Route::WineManager,
                None,
                cx,
            ))
            .child(item(
                "nav-accounts",
                if any_account {
                    "shell.nav.accounts"
                } else {
                    "shell.nav.login"
                },
                IconName::CircleUser,
                "accounts",
                Route::Accounts,
                None,
                cx,
            ))
            .child(item(
                "nav-settings",
                "shell.nav.settings",
                IconName::Settings,
                "settings",
                Route::Settings("general".into()),
                None,
                cx,
            ))
            .child(
                h_flex()
                    .pt_3()
                    .gap_1()
                    .when(collapsed, |d| d.flex_col())
                    .child(sidebar::big_picture_button(collapsed, cx))
                    .child(
                        Button::new("toggle-sidebar")
                            .ghost()
                            .icon(if collapsed {
                                IconName::PanelLeftOpen
                            } else {
                                IconName::PanelLeftClose
                            })
                            .tooltip(t("shell.nav.toggle_sidebar"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx)),
                            ),
                    ),
            )
    }
}

impl Focusable for AppShell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !Self::is_narrow(window) {
            self.narrow_expanded = false;
        }
        let route = navigator(cx).read(cx).current().clone();
        let fullscreen_mode = matches!(route, Route::BigPicture);
        let show_sidebar = !matches!(route, Route::BigPicture | Route::Onboarding);
        let content = self.pages.view(&route, window, cx);
        // Each new page settles in from just below; its id changes with the route.
        let content = ember_ui::motion::rise_in(
            SharedString::from(format!("page:{route:?}")),
            div().size_full().child(content),
            PAGE_RISE_PX,
        );

        v_flex()
            .id("ember-shell")
            .key_context(SHELL_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|_, _: &GoBack, _, cx| nav::go_back(cx)))
            .on_action(cx.listener(|_, _: &GoForward, _, cx| nav::go_forward(cx)))
            .on_action(cx.listener(|this, _: &OpenPalette, window, cx| {
                this.palette.open(window, cx);
            }))
            .on_action(cx.listener(|_, _: &GoLibrary, _, cx| nav::navigate(Route::Library, cx)))
            .on_action(cx.listener(|_, _: &GoDownloads, _, cx| nav::navigate(Route::Downloads, cx)))
            .on_action(cx.listener(|_, _: &GoStores, _, cx| nav::navigate(Route::Stores, cx)))
            .on_action(cx.listener(|_, _: &GoStats, _, cx| nav::navigate(Route::Stats, cx)))
            .on_action(cx.listener(|_, _: &GoWine, _, cx| nav::navigate(Route::WineManager, cx)))
            .on_action(cx.listener(|_, _: &GoAccounts, _, cx| nav::navigate(Route::Accounts, cx)))
            .on_action(cx.listener(|_, _: &GoSettings, _, cx| {
                nav::navigate(Route::Settings("general".into()), cx)
            }))
            .on_action(
                cx.listener(|_, _: &RefreshLibrary, _, cx| models::refresh_library(false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleSidebar, window, cx| this.toggle_sidebar(window, cx)),
            )
            .on_action(cx.listener(|_, _: &ToggleFullscreen, window, _| window.toggle_fullscreen()))
            .on_action(
                cx.listener(|_, _: &EnterBigPicture, _, cx| nav::navigate(Route::BigPicture, cx)),
            )
            .when(!fullscreen_mode, |this| {
                this.child(self.render_title_bar(window, cx))
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .when(show_sidebar, |this| {
                        this.child(self.render_sidebar(window, cx))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            // A faint warm glow at the top gives pages depth.
                            .bg(gpui_kit::linear_gradient(
                                180.,
                                gpui_kit::linear_color_stop(cx.theme().secondary, 0.),
                                gpui_kit::linear_color_stop(cx.theme().background, 0.45),
                            ))
                            .child(content),
                    ),
            )
    }
}

/// Launch a game, reporting failures as a notification.
pub fn launch_game(id: GameId, window: &mut Window, cx: &mut App) {
    let b = ember_ui::backend(cx);
    let busy = models(cx)
        .statuses
        .read(cx)
        .kind(&id)
        .is_some_and(StatusKind::is_busy);
    if busy {
        return;
    }
    let launch_id = id.clone();
    let task = ember_ui::run(
        cx,
        async move { b.launch(&launch_id, None, Vec::new()).await },
    );
    window
        .spawn(cx, async move |cx| {
            if let Err(e) = task.await {
                let _ = cx.update(|window, cx| {
                    window.push_notification(
                        Notification::error(format!("{e:#}")).title(t("shell.launch_failed")),
                        cx,
                    );
                });
            }
        })
        .detach();
}

/// Default route at startup: the welcome flow until the user finished or
/// skipped it, else the library.
pub fn initial_route(cx: &App) -> Route {
    let has_sideload = models(cx).library.read(cx).snapshot.count(Runner::Sideload) > 0;
    if ember_manage::should_show_onboarding(cx) && !has_sideload {
        Route::Onboarding
    } else {
        Route::Library
    }
}
