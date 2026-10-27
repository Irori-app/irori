//! Screenshots tab: the store's screenshots in a grid, and a viewer that shows one
//! at a time over the page, navigated with the arrow keys.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, KeyDownEvent, ObjectFit,
    ParentElement, StatefulInteractiveElement as _, Styled, StyledImage as _, Window, div, rems,
};
use irori_ui::components::{ArtImage, art_image, art_img, art_loading};
use irori_ui::i18n::{t, t_args};
use irori_ui::style::KeyboardFocusable as _;

use super::GamePage;

/// Pixel width screenshots are decoded at in the grid.
const THUMB_WIDTH: u32 = 640;
/// Largest pixel width screenshots are decoded at in the viewer.
const FULL_WIDTH: u32 = 1920;
/// Narrowest a grid screenshot gets before the grid drops a column, in rem.
const MIN_TILE_REM: f32 = 18.;
/// Gap between grid screenshots (`gap_3`), in rem.
const GAP_REM: f32 = 0.75;
/// Most columns the grid uses, so screenshots stay large on wide windows.
const MAX_COLUMNS: u16 = 5;

/// Columns of screenshots that fit `available` rem, at least one.
fn screenshot_columns(available: f32) -> u16 {
    let fit = ((available + GAP_REM) / (MIN_TILE_REM + GAP_REM)).floor();
    (fit.max(1.) as u16).min(MAX_COLUMNS)
}

impl GamePage {
    fn screenshots(&self) -> Vec<String> {
        self.extra
            .ready()
            .map(|e| e.screenshots.clone())
            .unwrap_or_default()
    }

    pub(super) fn render_screenshots(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shots = self.screenshots();
        // The body is inset by `px_8` on both sides.
        let columns = self
            .page_rem(window)
            .map_or(3, |page| screenshot_columns(page - 4.));
        let tiles: Vec<AnyElement> = shots
            .iter()
            .enumerate()
            .filter_map(|(index, url)| {
                let content = match art_image(url, THUMB_WIDTH, false, cx) {
                    ArtImage::Ready(img) => img
                        .image_cache(&self.image_cache)
                        .size_full()
                        .rounded_md()
                        .object_fit(ObjectFit::Cover)
                        .into_any_element(),
                    ArtImage::Loading => art_loading(),
                    // A screenshot that failed to load is left out, not shown as a hole.
                    ArtImage::Missing => return None,
                };
                Some(
                    div()
                        .id(("screenshot", index))
                        .keyboard_focusable(cx)
                        .w_full()
                        .aspect_ratio(16. / 9.)
                        .rounded_md()
                        .overflow_hidden()
                        .bg(cx.theme().muted)
                        .cursor_pointer()
                        .child(content)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_screenshot(index, window, cx)
                        }))
                        .into_any_element(),
                )
            })
            .collect();
        div()
            .w_full()
            .grid()
            .grid_cols(columns)
            .gap_3()
            .children(tiles)
            .into_any_element()
    }

    fn open_screenshot(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.viewer = Some(index);
        self.viewer_focus.focus(window, cx);
        cx.notify();
    }

    fn close_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.viewer = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Move the viewer by `step` screenshots, wrapping around.
    fn step_viewer(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.screenshots().len() as isize;
        if let (Some(index), true) = (self.viewer, count > 0) {
            self.viewer = Some((index as isize + step).rem_euclid(count) as usize);
            cx.notify();
        }
    }

    /// The open screenshot over the whole page, or nothing.
    pub(super) fn render_viewer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let index = self.viewer?;
        let shots = self.screenshots();
        let Some(url) = shots.get(index) else {
            self.viewer = None;
            return None;
        };
        let width = irori_ui::components::physical_width(rems(80.), window).min(FULL_WIDTH);
        let img = art_img(url, width, false, cx);
        let theme = cx.theme();
        Some(
            v_flex()
                .id("screenshot-viewer")
                .track_focus(&self.viewer_focus)
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .p_6()
                .gap_3()
                .bg(theme.background.opacity(0.95))
                .occlude()
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => this.close_viewer(window, cx),
                        "left" => this.step_viewer(-1, cx),
                        "right" => this.step_viewer(1, cx),
                        _ => return,
                    }
                    cx.stop_propagation();
                }))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(t_args(
                                    "game.screenshots.counter",
                                    &[
                                        ("index", &(index + 1).to_string()),
                                        ("count", &shots.len().to_string()),
                                    ],
                                )),
                        )
                        .child(
                            Button::new("screenshot-close")
                                .ghost()
                                .icon(IconName::X)
                                .accessibility_label(t("game.screenshots.close"))
                                .tooltip(t("game.screenshots.close"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.close_viewer(window, cx)
                                    }),
                                ),
                        ),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .gap_3()
                        .child(
                            Button::new("screenshot-previous")
                                .ghost()
                                .large()
                                .icon(IconName::ChevronLeft)
                                .accessibility_label(t("game.screenshots.previous"))
                                .tooltip(t("game.screenshots.previous"))
                                .on_click(cx.listener(|this, _, _, cx| this.step_viewer(-1, cx))),
                        )
                        .child(div().flex_1().min_w_0().h_full().when_some(img, |d, img| {
                            d.child(
                                img.image_cache(&self.image_cache)
                                    .size_full()
                                    .object_fit(ObjectFit::Contain),
                            )
                        }))
                        .child(
                            Button::new("screenshot-next")
                                .ghost()
                                .large()
                                .icon(IconName::ChevronRight)
                                .accessibility_label(t("game.screenshots.next"))
                                .tooltip(t("game.screenshots.next"))
                                .on_click(cx.listener(|this, _, _, cx| this.step_viewer(1, cx))),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_COLUMNS, screenshot_columns};

    #[test]
    fn columns_fill_the_width() {
        assert_eq!(screenshot_columns(0.), 1);
        assert_eq!(screenshot_columns(18.), 1);
        assert_eq!(screenshot_columns(36.75), 2);
        assert_eq!(screenshot_columns(70.), 3);
        assert_eq!(screenshot_columns(500.), MAX_COLUMNS);
    }
}
