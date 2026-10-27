//! The sidebar: Irori's mark, the main destinations, recently played games,
//! the management pages and the way into Big Picture.

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, ElementId, Entity, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Subscription, Window, div, px, rems,
};
use irori_core::{GameId, GameInfo, Runner};
use irori_ui::art::LruImageCache;
use irori_ui::components::art_img;
use irori_ui::i18n::t;
use irori_ui::models::models;
use irori_ui::nav::{self, Route, navigator};
use irori_ui::style::{KeyboardFocusable as _, SidebarPalette, caption};

/// Windows narrower than this (in rem; 1088 px at the default text size)
/// collapse the sidebar so the page keeps room for its content.
pub(crate) const NARROW_WINDOW_REM: f32 = 68.;
/// Width of the expanded and collapsed sidebar.
pub(crate) const WIDTH_REM: f32 = 15.;
pub(crate) const COLLAPSED_REM: f32 = 4.25;
/// How many recently played games are listed.
const RECENT_GAMES: usize = 5;

/// One destination of the sidebar.
pub(crate) struct NavItem {
    pub id: &'static str,
    pub label: SharedString,
    pub icon: IconName,
    pub route: Route,
    pub active: bool,
    /// Shown after the label, such as a count.
    pub suffix: Option<AnyElement>,
}

/// A destination row. The active one carries an irori marker; the others stay
/// quiet until pointed at.
pub(crate) fn nav_item(item: NavItem, collapsed: bool, cx: &App) -> AnyElement {
    let NavItem {
        id,
        label,
        icon,
        route,
        active,
        suffix,
    } = item;
    let theme = cx.theme();
    // Held readable whatever the theme and glass.
    let palette = SidebarPalette::new(cx);
    let (fg, icon_color) = if active {
        (palette.selected_text, palette.selected_icon)
    } else {
        (palette.text, palette.icon)
    };
    let hover_bg = palette.highlight;
    let hover_fg = palette.selected_text;
    let tooltip = label.clone();
    h_flex()
        .id(id)
        .keyboard_focusable(cx)
        .relative()
        .h(rems(2.5))
        .px_3()
        .gap_3()
        .rounded(theme.radius)
        .text_color(fg)
        .when(collapsed, |d| d.justify_center().px_0())
        .when(active, |d| d.bg(hover_bg).font_bold())
        .when(!active, |d| {
            d.font_medium()
                .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
        })
        .cursor_pointer()
        .when(active, |d| {
            d.child(
                div()
                    .absolute()
                    .left_0()
                    .top(rems(0.6))
                    .bottom(rems(0.6))
                    .w(px(3.))
                    .rounded_full()
                    .bg(palette.selected_icon),
            )
        })
        .child(Icon::new(icon).text_color(icon_color))
        .when(!collapsed, |d| {
            d.child(div().flex_1().min_w_0().truncate().child(label))
                .children(suffix)
        })
        .when(collapsed, |d| {
            d.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        })
        .on_click(move |_, _, cx| nav::navigate(route.clone(), cx))
        .into_any_element()
}

/// Recently played games, newest first: Irori's own history, or the recorded
/// playtime when that is empty (such as right after importing from Heroic).
fn recent_games(cx: &App) -> Vec<Arc<GameInfo>> {
    let library = models(cx).library.read(cx);
    let user = &library.user;
    let keys: Vec<String> = if user.recent.is_empty() {
        let mut played: Vec<(&String, &str)> = user
            .playtime
            .iter()
            .filter_map(|(k, r)| Some((k, r.last_played.as_deref()?)))
            .collect();
        played.sort_by(|a, b| b.1.cmp(a.1));
        played.into_iter().map(|(k, _)| k.clone()).collect()
    } else {
        user.recent.clone()
    };
    keys.iter()
        .filter(|k| !user.hidden.contains(*k))
        .filter_map(|k| {
            let (runner, app) = k.split_once(':')?;
            library
                .snapshot
                .get(&GameId::new(Runner::from_id(runner)?, app))
                .cloned()
        })
        .take(RECENT_GAMES)
        .collect()
}

/// The "Recent" list, a view of its own so cover art arriving redraws only it.
pub(crate) struct RecentGames {
    images: Entity<LruImageCache>,
    _subscriptions: Vec<Subscription>,
}

impl RecentGames {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let m = models(cx).clone();
        Self {
            images: LruImageCache::new(RECENT_GAMES * 2, cx),
            _subscriptions: vec![
                cx.observe(&m.library, |_, _, cx| cx.notify()),
                cx.observe(&irori_ui::art::art_store(cx), |_, _, cx| cx.notify()),
                cx.observe(&navigator(cx), |_, _, cx| cx.notify()),
            ],
        }
    }
}

impl Render for RecentGames {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let games = recent_games(cx);
        let current = navigator(cx).read(cx).current().clone();
        let thumb_width = irori_ui::components::physical_width(rems(2.), window);
        let theme = cx.theme();
        let palette = SidebarPalette::new(cx);
        let hover_bg = palette.highlight;
        let muted_bg = theme.muted;
        let radius = theme.radius;
        let (active_fg, fg) = (palette.selected_text, palette.text);
        let rows: Vec<AnyElement> = games
            .iter()
            .map(|game| {
                let id = game.id();
                let active = current == Route::Game(id.clone());
                let thumb = game
                    .cover_art()
                    .map(str::to_owned)
                    .and_then(|url| art_img(&url, thumb_width, false, cx));
                h_flex()
                    .id(ElementId::Name(format!("recent:{}", id.key()).into()))
                    .keyboard_focusable(cx)
                    .h(rems(2.75))
                    .px_2()
                    .gap_3()
                    .rounded(radius)
                    .cursor_pointer()
                    .text_sm()
                    .text_color(if active { active_fg } else { fg })
                    .when(active, |d| d.bg(hover_bg).font_semibold())
                    .when(!active, |d| {
                        d.hover(move |s| s.bg(hover_bg).text_color(active_fg))
                    })
                    .child(
                        div()
                            .w(rems(1.5))
                            .h(rems(2.))
                            .flex_shrink_0()
                            .rounded_sm()
                            .overflow_hidden()
                            .bg(muted_bg)
                            .when_some(thumb, |d, img| {
                                d.child(
                                    img.image_cache(&self.images)
                                        .size_full()
                                        .rounded_sm()
                                        .object_fit(ObjectFit::Cover),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(game.title.clone()),
                    )
                    .on_click(move |_, _, cx| nav::navigate(Route::Game(id.clone()), cx))
                    .into_any_element()
            })
            .collect();
        v_flex().w_full().gap_0p5().when(!rows.is_empty(), |d| {
            d.child(
                div()
                    .px_3()
                    .pt_5()
                    .pb_2()
                    .child(caption(t("shell.nav.recent"), cx).text_color(palette.caption)),
            )
            .children(rows)
        })
    }
}

/// The way into Big Picture: a quiet button with the controller glyph.
pub(crate) fn big_picture_button(collapsed: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let palette = SidebarPalette::new(cx);
    let hover_border = theme.sidebar_primary;
    let tooltip = t("shell.nav.big_picture_tooltip");
    h_flex()
        .id("big-picture")
        .keyboard_focusable(cx)
        .flex_1()
        .min_w_0()
        .h(rems(2.5))
        .px_3()
        .gap_3()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.sidebar_border)
        .bg(palette.highlight)
        .text_color(palette.selected_text)
        .font_semibold()
        .cursor_pointer()
        .hover(move |s| s.border_color(hover_border))
        .when(collapsed, |d| d.justify_center().px_0())
        .child(Icon::new(IconName::Gamepad2).small())
        .when(!collapsed, |d| {
            d.child(div().min_w_0().truncate().child(t("shell.nav.big_picture")))
        })
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .on_click(|_, _, cx| nav::navigate(Route::BigPicture, cx))
        .into_any_element()
}
