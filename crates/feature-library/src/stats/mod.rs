//! The stats page: how much and how regularly the user plays. A row of
//! figures (this week against last, streaks, all time), play time per week
//! over the last [`model::WEEKS`] weeks, and all-time play time per store and
//! per game.
//!
//! The weekly chart and streaks come from the session history, which starts
//! with the first game exited after it was introduced; store and game totals
//! come from the playtime records, which reach further back.
//!
//! Below them, the storage section shows the installed games by drive.

pub(crate) mod model;
pub(crate) mod storage;

use std::sync::Arc;

use chrono::{Datelike as _, Local, NaiveDate};
use ember_core::{GameInfo, Runner};
use ember_ui::art::LruImageCache;
use ember_ui::components::{EmptyState, GameCover, PageHeader, SectionTitle};
use ember_ui::i18n::{t, t_args};
use ember_ui::style::{KeyboardFocusable as _, caption, panel};
use ember_ui::{Route, backend, models, navigate};
use gpui_kit::assets::IconName;
use gpui_kit::component::chart::BarChart;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Corners, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, px, relative, rems,
};

use model::{GameTotal, Stats, StoreTotal, Week};
use storage::{DriveUsage, InstalledGame};

/// Height of the weekly chart, in rem.
const CHART_REM: f32 = 14.0;
/// Width of the label column of the store and game lists, in rem.
const LABEL_REM: f32 = 12.0;
/// Games listed under each drive, largest first.
const DRIVE_GAMES: usize = 5;

pub struct StatsView {
    /// `None` until the first computation finishes.
    stats: Option<Stats>,
    /// Installed games by drive; `None` until first read.
    storage: Option<Vec<DriveUsage>>,
    /// Library revision and user data the stats were computed from.
    seen: Option<(u64, Arc<ember_services::UserData>)>,
    images: Entity<LruImageCache>,
    _subscriptions: Vec<Subscription>,
}

impl StatsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sessions = models(cx).sessions.clone();
        let library = models(cx).library.clone();
        let subscriptions = vec![
            // A finished session changes every number on the page.
            cx.observe_in(&sessions, window, |this, _, _, cx| this.reload(cx)),
            // So do a library refresh (which games count) and playtime
            // synced from the stores; the model notifies for much else too.
            cx.observe_in(&library, window, |this, library, _, cx| {
                let l = library.read(cx);
                let same = this.seen.as_ref().is_some_and(|(revision, user)| {
                    *revision == l.snapshot.revision && Arc::ptr_eq(user, &l.user)
                });
                if !same {
                    this.reload(cx);
                }
            }),
        ];
        let mut view = Self {
            seen: None,
            stats: None,
            storage: None,
            images: LruImageCache::new(32, cx),
            _subscriptions: subscriptions,
        };
        view.reload(cx);
        view
    }

    /// Read the history (off the UI thread) and recompute.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let b = backend(cx);
        let library = models(cx).library.read(cx);
        let (snapshot, user) = (library.snapshot.clone(), library.user.clone());
        let playtime = user.playtime.clone();
        self.seen = Some((snapshot.revision, user));
        let task = ember_ui::state::run_infallible(cx, async move {
            let sessions: Vec<_> = b
                .sessions()
                .into_iter()
                .filter_map(|s| Some((model::local_day(&s)?, s)))
                .collect();
            model::compute(
                &sessions,
                &playtime,
                |id| snapshot.get(id).is_some(),
                Local::now().date_naive(),
            )
        });
        cx.spawn(async move |this, cx| {
            if let Some(stats) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.stats = Some(stats);
                    cx.notify();
                });
            }
        })
        .detach();
        self.reload_storage(cx);
    }

    /// Read the drives and where the installed games are (off the UI thread).
    fn reload_storage(&mut self, cx: &mut Context<Self>) {
        let b = backend(cx);
        let installed: Vec<(ember_core::GameId, std::path::PathBuf, u64)> = models(cx)
            .library
            .read(cx)
            .snapshot
            .games
            .iter()
            .filter(|g| g.is_installed)
            .filter_map(|g| {
                let bytes = g.install.as_ref().map_or(0, |i| i.install_size_bytes);
                Some((g.id(), g.install_path()?, bytes))
            })
            .collect();
        let task = ember_ui::state::run_infallible(cx, async move {
            b.spawn_blocking(move || {
                let games: Vec<InstalledGame> = installed
                    .into_iter()
                    .map(|(id, path, bytes)| InstalledGame {
                        id,
                        path: storage::resolve(&path),
                        bytes,
                    })
                    .collect();
                storage::group(&games, &storage::read_drives())
            })
            .await
            .ok()
        });
        cx.spawn(async move |this, cx| {
            if let Some(Some(drives)) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.storage = Some(drives);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn render_figures(&self, stats: &Stats, cx: &Context<Self>) -> AnyElement {
        let (this_week, last_week) = stats.this_and_last_week();
        let change = this_week as i64 - last_week as i64;
        let delta = match change.signum() {
            0 => t("library.stats.same_as_last_week"),
            1 => t_args(
                "library.stats.more_than_last_week",
                &[("duration", &duration(change.unsigned_abs()))],
            ),
            _ => t_args(
                "library.stats.less_than_last_week",
                &[("duration", &duration(change.unsigned_abs()))],
            ),
        };
        h_flex()
            .w_full()
            .flex_wrap()
            // Tiles share one height whether or not they carry a note.
            .items_stretch()
            .gap_4()
            .child(figure(
                t("library.stats.this_week"),
                duration(this_week),
                Some(delta),
                cx,
            ))
            .child(figure(
                t("library.stats.current_streak"),
                days(stats.current_streak),
                None,
                cx,
            ))
            .child(figure(
                t("library.stats.longest_streak"),
                days(stats.longest_streak),
                None,
                cx,
            ))
            .child(figure(
                t("library.stats.all_time"),
                duration(stats.total_minutes),
                None,
                cx,
            ))
            .into_any_element()
    }

    fn render_weeks(&self, stats: &Stats, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let fill = theme.chart_1;
        let since = stats.since.map(|d| {
            t_args(
                "library.stats.recorded_since",
                &[("date", short_date(d).as_ref())],
            )
        });
        let chart = BarChart::new(stats.weeks.clone())
            .id("stats-weeks")
            .band(|w: &Week| short_date(w.start))
            .value(|w: &Week| w.minutes as f64 / 60.0)
            .fill(move |_: &Week, _, _, _| fill)
            .corner_radii(Corners {
                top_left: px(4.),
                top_right: px(4.),
                bottom_left: px(0.),
                bottom_right: px(0.),
            })
            .max_band_width(px(24.))
            .grid_dashed(false)
            .value_axis(true)
            .value_tick_count(4)
            .value_tick_format(hours_tick)
            .name(t("library.stats.weekly"))
            .tooltip_title(|w: &Week| {
                t_args(
                    "library.stats.week_of",
                    &[("date", short_date(w.start).as_ref())],
                )
            })
            .tooltip_value(|w: &Week, _| duration(w.minutes));
        panel(cx)
            .flex_1()
            .min_w_0()
            .child(SectionTitle::new(t("library.stats.weekly")))
            .child(if stats.weeks.iter().any(|w| w.minutes > 0) {
                div().h(rems(CHART_REM)).w_full().child(chart)
            } else {
                // An all-zero chart is only axes; say why it is empty instead.
                div()
                    .h(rems(CHART_REM))
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(t("library.stats.no_recent_sessions"))
            })
            .children(since.map(|s| div().text_xs().text_color(theme.muted_foreground).child(s)))
            .into_any_element()
    }

    fn render_stores(&self, stores: &[StoreTotal], cx: &Context<Self>) -> AnyElement {
        let max = stores.first().map_or(1, |s| s.minutes.max(1));
        panel(cx)
            .flex_1()
            .min_w(rems(20.))
            .child(SectionTitle::new(t("library.stats.by_store")))
            .children(stores.iter().map(|s| {
                bar_row(
                    div()
                        .truncate()
                        .child(store_name(s.runner))
                        .into_any_element(),
                    s.minutes,
                    max,
                    cx,
                )
            }))
            .into_any_element()
    }

    fn render_games(&self, games: &[GameTotal], cx: &mut Context<Self>) -> AnyElement {
        let max = games.first().map_or(1, |g| g.minutes.max(1));
        let library = models(cx).library.read(cx).snapshot.clone();
        let (radius, hover_bg) = (cx.theme().radius, cx.theme().list_hover);
        let rows: Vec<AnyElement> = games
            .iter()
            .map(|g| {
                let info: Option<Arc<GameInfo>> = library.get(&g.id).cloned();
                let title = info
                    .as_ref()
                    .map(|i| i.title.clone())
                    .unwrap_or_else(|| g.id.app_name.to_string());
                let label = h_flex()
                    .gap_2()
                    .min_w_0()
                    .children(info.map(|i| GameCover::new(i).width(rems(1.25)).radius(px(3.))))
                    .child(div().min_w_0().truncate().child(title))
                    .into_any_element();
                let id = g.id.clone();
                div()
                    .id(ElementId::Name(format!("stats-game:{}", g.id.key()).into()))
                    .keyboard_focusable(cx)
                    .rounded(radius)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .on_click(move |_, _, cx| navigate(Route::Game(id.clone()), cx))
                    .child(bar_row(label, g.minutes, max, cx))
                    .into_any_element()
            })
            .collect();
        panel(cx)
            .flex_1()
            .min_w(rems(20.))
            .image_cache(self.images.clone())
            .child(SectionTitle::new(t("library.stats.most_played")))
            .children(rows)
            .into_any_element()
    }
}

impl StatsView {
    fn render_storage(&self, drives: &[DriveUsage], cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<AnyElement> = drives
            .iter()
            .enumerate()
            .map(|(ix, usage)| self.render_drive(ix, usage, cx))
            .collect();
        let theme = cx.theme();
        let legend = |color, label: SharedString| {
            h_flex()
                .gap_1p5()
                .child(div().size_2p5().rounded_full().bg(color))
                .child(label)
        };
        panel(cx)
            .gap_4()
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_between()
                    .gap_2()
                    .child(SectionTitle::new(t("library.stats.storage")))
                    .child(
                        h_flex()
                            .gap_4()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(legend(
                                theme.chart_1,
                                t("library.stats.storage_legend_games"),
                            ))
                            .child(legend(
                                theme.chart_3,
                                t("library.stats.storage_legend_other"),
                            )),
                    ),
            )
            .children(rows)
            .into_any_element()
    }

    fn render_drive(&self, ix: usize, usage: &DriveUsage, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted_fg, track, games_color, other_color) = (
            theme.muted_foreground,
            theme.muted,
            theme.chart_1,
            theme.chart_3,
        );
        let (radius, hover_bg) = (theme.radius, theme.list_hover);
        let size = |bytes: u64| ember_core::util::format_bytes(bytes);
        let (label, summary) = match &usage.drive {
            Some(d) => (
                SharedString::from(d.label()),
                t_args(
                    "library.stats.storage_summary",
                    &[
                        ("games", &size(usage.used)),
                        ("free", &size(d.free)),
                        ("total", &size(d.total)),
                    ],
                ),
            ),
            None => (
                t("library.stats.storage_unknown"),
                t_args(
                    "library.stats.storage_games",
                    &[("games", &size(usage.used))],
                ),
            ),
        };
        let bar = usage.drive.as_ref().filter(|d| d.total > 0).map(|d| {
            let total = d.total as f32;
            let games = (usage.used as f32 / total).clamp(0.0, 1.0);
            let taken = d.total.saturating_sub(d.free) as f32 / total;
            let other = (taken - games).clamp(0.0, 1.0 - games);
            h_flex()
                .w_full()
                .h(px(10.))
                .rounded(px(5.))
                .overflow_hidden()
                .bg(track)
                .child(div().h_full().w(relative(games)).bg(games_color))
                .child(div().h_full().w(relative(other)).bg(other_color))
        });
        let library = models(cx).library.read(cx).snapshot.clone();
        let games: Vec<AnyElement> = usage
            .games
            .iter()
            .take(DRIVE_GAMES)
            .map(|(id, bytes)| {
                let title = library
                    .get(id)
                    .map(|g| g.title.clone())
                    .unwrap_or_else(|| id.app_name.to_string());
                let id = id.clone();
                h_flex()
                    .id(ElementId::Name(format!("storage-game:{}", id.key()).into()))
                    .keyboard_focusable(cx)
                    .w_full()
                    .gap_3()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .rounded(radius)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(muted_fg)
                            .child(if *bytes > 0 {
                                SharedString::from(size(*bytes))
                            } else {
                                t("library.stats.storage_size_unknown")
                            }),
                    )
                    .on_click(move |_, _, cx| navigate(Route::Game(id.clone()), cx))
                    .into_any_element()
            })
            .collect();
        let more = usage.games.len().saturating_sub(DRIVE_GAMES);
        v_flex()
            .id(ElementId::Name(format!("storage-drive:{ix}").into()))
            .gap_2()
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_between()
                    .gap_x_4()
                    .child(div().min_w_0().font_semibold().truncate().child(label))
                    .child(div().text_sm().text_color(muted_fg).child(summary)),
            )
            .children(bar)
            .child(v_flex().children(games))
            .when(more > 0, |d| {
                d.child(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(muted_fg)
                        .child(if more == 1 {
                            t("library.stats.storage_more_one")
                        } else {
                            t_args(
                                "library.stats.storage_more_other",
                                &[("count", &more.to_string())],
                            )
                        }),
                )
            })
            .into_any_element()
    }
}

impl Render for StatsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = match self.stats.clone() {
            None => div()
                .flex()
                .justify_center()
                .py_12()
                .child(Spinner::new())
                .into_any_element(),
            Some(stats) if stats.is_empty() => {
                EmptyState::new(IconName::ChartColumn, t("library.stats.empty.title"))
                    .description(t("library.stats.empty.description"))
                    .into_any_element()
            }
            Some(stats) => v_flex()
                .gap_6()
                .child(self.render_figures(&stats, cx))
                .child(self.render_weeks(&stats, cx))
                .child(
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .items_start()
                        .gap_6()
                        .child(self.render_stores(&stats.stores, cx))
                        .child(self.render_games(&stats.games, cx)),
                )
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                div().px_6().pt_6().pb_4().child(
                    PageHeader::new(t("library.stats.title"))
                        .description(t("library.stats.description")),
                ),
            )
            .child(
                div()
                    .id("stats-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex().px_6().pb_6().gap_6().child(body).children(
                            self.storage
                                .clone()
                                .filter(|d| !d.is_empty())
                                .map(|d| self.render_storage(&d, cx)),
                        ),
                    ),
            )
    }
}

/// A figure: a caption over one large value, with an optional note below.
fn figure(
    label: SharedString,
    value: SharedString,
    note: Option<SharedString>,
    cx: &Context<StatsView>,
) -> AnyElement {
    panel(cx)
        .flex_1()
        .min_w(rems(11.))
        .gap_1()
        .child(caption(label, cx))
        .child(div().text_2xl().font_bold().child(value))
        .children(note.map(|n| {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(n)
        }))
        .into_any_element()
}

/// A labelled horizontal bar: its length is `minutes` against the list's
/// largest value, its value written after it.
fn bar_row(label: AnyElement, minutes: u64, max: u64, cx: &Context<StatsView>) -> AnyElement {
    let theme = cx.theme();
    let share = (minutes as f32 / max as f32).clamp(0.0, 1.0);
    h_flex()
        .w_full()
        .gap_3()
        .py_1()
        .text_sm()
        .child(
            div()
                .w(rems(LABEL_REM))
                .flex_shrink_0()
                .min_w_0()
                .child(label),
        )
        .child(
            div().flex_1().min_w_0().h(px(10.)).child(
                div()
                    .h_full()
                    .w(relative(share))
                    .min_w(px(4.))
                    .bg(theme.chart_1)
                    .rounded_r(px(4.)),
            ),
        )
        .child(
            div()
                .w(rems(6.))
                .flex_shrink_0()
                .text_right()
                .text_color(theme.muted_foreground)
                .child(duration(minutes)),
        )
        .into_any_element()
}

/// "45 min", "2 h", "2 h 5 min"; "0 min" for nothing.
fn duration(minutes: u64) -> SharedString {
    crate::i18n::playtime(minutes).into()
}

/// "1 day", "12 days".
fn days(count: u32) -> SharedString {
    let key = if count == 1 {
        "library.stats.days_one"
    } else {
        "library.stats.days_other"
    };
    t_args(key, &[("count", &count.to_string())])
}

/// A day and month in the interface language's order ("10/19", "19.10.").
fn short_date(date: NaiveDate) -> SharedString {
    t_args(
        "library.stats.short_date",
        &[
            ("day", &date.day().to_string()),
            ("month", &date.month().to_string()),
        ],
    )
}

/// Value axis ticks in hours: "2 h", "1.5 h".
fn hours_tick(hours: f64) -> SharedString {
    let value = if hours.fract() == 0.0 {
        format!("{hours:.0}")
    } else {
        format!("{hours:.1}")
    };
    t_args("library.playtime.hours", &[("hours", &value)])
}

fn store_name(runner: Runner) -> SharedString {
    SharedString::from(runner.store_name())
}
