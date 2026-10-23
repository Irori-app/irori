//! Achievements list with an unlocked/locked filter. Rows are virtualized,
//! so games with hundreds of achievements render only what is visible.

use std::cmp::Ordering;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use ember_core::Achievement;
use ember_core::util::{parse_rfc3339, unix_now};
use ember_ui::components::art_img;
use ember_ui::i18n::{t, t_args};
use gpui_kit::assets::IconName;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, VirtualListScrollHandle, h_flex, v_flex,
    v_virtual_list,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ObjectFit, ParentElement, Pixels,
    SharedString, Size, Styled, StyledImage as _, Window, div, px, rems, size,
};

use super::GamePage;
use crate::load::format_day;

/// Height of one row, in rems.
const ROW_REMS: f32 = 4.5;
/// Rows visible before the list scrolls on its own.
const VISIBLE_ROWS: usize = 8;

/// Which achievements the grid shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AchievementFilter {
    All,
    Unlocked,
    Locked,
}

impl AchievementFilter {
    const ALL: [AchievementFilter; 3] = [
        AchievementFilter::All,
        AchievementFilter::Unlocked,
        AchievementFilter::Locked,
    ];

    fn label(self) -> SharedString {
        t(match self {
            AchievementFilter::All => "game.achievements.filter_all",
            AchievementFilter::Unlocked => "game.achievements.filter_unlocked",
            AchievementFilter::Locked => "game.achievements.filter_locked",
        })
    }
}

/// Filter and order achievements: unlocked first (newest unlock first), then
/// locked ones from the most common (the likeliest next unlock). Returns
/// indices into `list`.
pub fn arrange(list: &[Achievement], filter: AchievementFilter) -> Vec<usize> {
    let mut out: Vec<usize> = list
        .iter()
        .enumerate()
        .filter(|(_, a)| match filter {
            AchievementFilter::All => true,
            AchievementFilter::Unlocked => a.unlocked_at.is_some(),
            AchievementFilter::Locked => a.unlocked_at.is_none(),
        })
        .map(|(ix, _)| ix)
        .collect();
    out.sort_by(|&a, &b| {
        let (a, b) = (&list[a], &list[b]);
        match (&a.unlocked_at, &b.unlocked_at) {
            (Some(x), Some(y)) => parse_rfc3339(y).cmp(&parse_rfc3339(x)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => b.rarity.partial_cmp(&a.rarity).unwrap_or(Ordering::Equal),
        }
    });
    out
}

/// Arranged rows and their measurements, rebuilt only when the list, the
/// filter or the rem size change.
pub(crate) struct RowsCache {
    key: Option<(AchievementFilter, usize, Pixels)>,
    rows: Rc<Vec<usize>>,
    sizes: Rc<Vec<Size<Pixels>>>,
    scroll: VirtualListScrollHandle,
}

impl Default for RowsCache {
    fn default() -> Self {
        Self {
            key: None,
            rows: Rc::default(),
            sizes: Rc::default(),
            scroll: VirtualListScrollHandle::new(),
        }
    }
}

impl RowsCache {
    fn sync(&mut self, list: &Arc<Vec<Achievement>>, filter: AchievementFilter, row_h: Pixels) {
        let key = (filter, Arc::as_ptr(list) as usize, row_h);
        if self.key == Some(key) {
            return;
        }
        let filter_changed = self.key.is_some_and(|k| k.0 != filter);
        let rows = arrange(list, filter);
        self.sizes = Rc::new(vec![size(px(0.), row_h); rows.len()]);
        self.rows = Rc::new(rows);
        self.key = Some(key);
        if filter_changed {
            self.scroll.scroll_to_item(0, gpui_kit::ScrollStrategy::Top);
        }
    }
}

impl GamePage {
    pub(super) fn render_achievements(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(list) = self.achievements.ready().cloned() else {
            return div().into_any_element();
        };
        let row_h = window.rem_size() * ROW_REMS;
        self.achievement_view
            .sync(&list, self.achievement_filter, row_h);
        let total = self.achievement_view.rows.len();
        let filter_ix = AchievementFilter::ALL
            .iter()
            .position(|f| *f == self.achievement_filter)
            .unwrap_or(0);
        let unlocked = list.iter().filter(|a| a.unlocked_at.is_some()).count();
        let rows = v_virtual_list(
            cx.entity(),
            "achievement-rows",
            self.achievement_view.sizes.clone(),
            |this, range, window, cx| this.render_achievement_rows(range, window, cx),
        )
        .track_scroll(&self.achievement_view.scroll)
        .size_full();
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_4()
                    .child(
                        TabBar::new("achievement-filter")
                            .segmented()
                            .small()
                            .selected_index(filter_ix)
                            .children(
                                AchievementFilter::ALL
                                    .iter()
                                    .map(|f| Tab::new().label(f.label())),
                            )
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                if let Some(f) = AchievementFilter::ALL.get(*ix) {
                                    this.achievement_filter = *f;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(t_args(
                                "game.achievements.progress",
                                &[
                                    ("unlocked", &unlocked.to_string()),
                                    ("total", &list.len().to_string()),
                                ],
                            )),
                    ),
            )
            .when(total == 0, |col| {
                col.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(t("game.achievements.empty")),
                )
            })
            .when(total > 0, |col| {
                col.child(
                    div()
                        .id("achievement-list")
                        .w_full()
                        .h(rems(ROW_REMS * total.min(VISIBLE_ROWS) as f32))
                        .rounded(cx.theme().radius_lg)
                        .border_1()
                        .border_color(cx.theme().border)
                        .overflow_hidden()
                        .child(rows),
                )
            })
            .into_any_element()
    }

    fn render_achievement_rows(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(list) = self.achievements.ready().cloned() else {
            return Vec::new();
        };
        let rows = self.achievement_view.rows.clone();
        let icon_px = (window.rem_size() * 3.0 * window.scale_factor()).ceil();
        let icon_px: f32 = icon_px.into();
        let now = unix_now() as i64;
        range
            .filter_map(|row| {
                let a = list.get(*rows.get(row)?)?;
                Some(self.render_row(row, a, icon_px as u32, now, cx))
            })
            .collect()
    }

    fn render_row(
        &self,
        row: usize,
        a: &Achievement,
        icon_px: u32,
        now: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let unlocked = a.unlocked_at.is_some();
        let hidden = !unlocked && !a.visible;
        let url = if unlocked {
            &a.image_unlocked
        } else {
            &a.image_locked
        };
        let image = (!url.is_empty())
            .then(|| art_img(url, icon_px, true, cx))
            .flatten();
        let theme = cx.theme();
        let (name, description) = if hidden {
            (
                t("game.achievements.hidden_name").to_string(),
                t("game.achievements.hidden_description").to_string(),
            )
        } else {
            (a.name.clone(), a.description.clone())
        };
        let rarity = match (a.rarity_label.is_empty(), a.rarity > 0.0) {
            (false, true) => Some(format!("{} · {:.1}%", a.rarity_label, a.rarity)),
            (false, false) => Some(a.rarity_label.clone()),
            (true, true) => Some(
                t_args(
                    "game.achievements.rarity_players",
                    &[("percent", &format!("{:.1}", a.rarity))],
                )
                .to_string(),
            ),
            (true, false) => None,
        };
        let when = a.unlocked_at.as_ref().map(|w| {
            t_args(
                "game.achievements.unlocked_at",
                &[("date", &format_day(w, now))],
            )
        });
        let icon_slot = div()
            .size(rems(3.))
            .flex_shrink_0()
            .rounded(theme.radius)
            .overflow_hidden()
            .bg(theme.muted)
            .map(|d| match image {
                Some(img) => d.child(
                    img.image_cache(&self.image_cache)
                        .size_full()
                        .rounded(theme.radius)
                        .object_fit(ObjectFit::Cover)
                        .when(!unlocked, |i| i.grayscale(true)),
                ),
                None => d.flex().items_center().justify_center().child(
                    Icon::new(if unlocked {
                        IconName::Trophy
                    } else {
                        IconName::Lock
                    })
                    .text_color(theme.muted_foreground),
                ),
            });
        h_flex()
            .id(("achievement", row))
            .w_full()
            .h(rems(ROW_REMS))
            .gap_3()
            .px_3()
            .items_center()
            .when(row > 0, |r| r.border_t_1().border_color(theme.border))
            .child(icon_slot)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .truncate()
                            .when(!unlocked, |d| d.text_color(theme.muted_foreground))
                            .child(name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(description),
                    ),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .max_w(rems(12.))
                    .items_end()
                    .gap_0p5()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .children(rarity.map(|r| div().truncate().child(r)))
                    .children(when.map(|w| div().truncate().child(w))),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(id: &str, rarity: f32, unlocked: Option<&str>) -> Achievement {
        Achievement {
            id: id.into(),
            rarity,
            unlocked_at: unlocked.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn order_and_filter() {
        let list = vec![
            a("rare", 1.0, None),
            a("old", 50.0, Some("2020-01-01T00:00:00Z")),
            a("common", 80.0, None),
            a("new", 10.0, Some("2024-01-01T00:00:00Z")),
        ];
        let ids = |f| {
            arrange(&list, f)
                .into_iter()
                .map(|ix| list[ix].id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(AchievementFilter::All),
            ["new", "old", "common", "rare"]
        );
        assert_eq!(ids(AchievementFilter::Unlocked), ["new", "old"]);
        assert_eq!(ids(AchievementFilter::Locked), ["common", "rare"]);
    }
}
