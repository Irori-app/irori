//! Ember's visual vocabulary on top of the theme: captions, stats and panels.
//! Feature crates use these instead of restyling text and surfaces by hand, so
//! screens share one voice.

use gpui_kit::component::{ActiveTheme as _, StyledExt as _, v_flex};
use gpui_kit::{
    App, BoxShadow, Div, Hsla, InteractiveElement, IntoElement, ParentElement as _, Rgba,
    SharedString, Styled as _, div, point, px,
};

use crate::models;

/// A game's art colour made fit for the chrome around it: its hue kept, its
/// saturation and lightness pulled into a band that reads on the theme's
/// background without competing with the content.
pub fn art_tint(rgb: [u8; 3], cx: &App) -> Hsla {
    let [r, g, b] = rgb.map(|c| c as f32 / 255.0);
    let color: Hsla = Rgba { r, g, b, a: 1.0 }.into();
    let (min_l, max_l) = if cx.theme().mode.is_dark() {
        (0.45, 0.62)
    } else {
        (0.38, 0.52)
    };
    Hsla {
        h: color.h,
        s: color.s.min(0.7),
        l: color.l.clamp(min_l, max_l),
        a: 1.0,
    }
}

/// Whether the user asked for less motion (Settings → Appearance → Motion).
/// Animations and pulsing placeholders hold still when this is set.
pub fn reduce_motion(cx: &App) -> bool {
    let app = &models(cx).settings.read(cx).app;
    app.ui.reduce_motion || app.disable_animations
}

/// A small uppercase label above a value or a group ("PLAY TIME", "LIBRARY").
pub fn caption(text: impl Into<SharedString>, cx: &App) -> Div {
    let text: SharedString = text.into();
    div()
        .text_xs()
        .font_bold()
        .text_color(cx.theme().muted_foreground)
        .child(SharedString::from(text.to_uppercase()))
}

/// A caption above its value, as in a game's play bar.
pub fn stat(label: impl Into<SharedString>, value: impl IntoElement, cx: &App) -> Div {
    v_flex()
        .gap_0p5()
        .min_w_0()
        .child(caption(label, cx))
        .child(div().text_sm().font_semibold().truncate().child(value))
}

/// A raised surface grouping related content.
pub fn panel(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .p_4()
        .gap_3()
        .rounded(theme.radius_lg)
        .bg(theme.secondary)
        .border_1()
        .border_color(theme.border)
}

/// Keyboard reach for Ember's own clickable surfaces (sidebar rows, cards,
/// screenshot tiles), which gpui-kit's controls already have.
pub trait KeyboardFocusable: InteractiveElement {
    /// Makes the element a Tab and gamepad stop: Enter and Space trigger its
    /// `on_click`, and a ring in the theme's focus colour shows while it is
    /// focused from the keyboard. The element needs an `id`. The ring is drawn
    /// just outside the element, so a clipping parent must leave it room.
    fn keyboard_focusable(self, cx: &App) -> Self {
        let ring = cx.theme().ring;
        self.tab_index(0).focus_visible(move |style| {
            style.shadow(vec![BoxShadow {
                color: ring,
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(2.),
                inset: false,
            }])
        })
    }
}

impl<E: InteractiveElement> KeyboardFocusable for E {}
