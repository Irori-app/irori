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

/// Minimum contrast of text against its background (WCAG AA).
pub const TEXT_CONTRAST: f32 = 4.5;
/// Minimum contrast of icons and other meaningful graphics (WCAG 1.4.11).
pub const ICON_CONTRAST: f32 = 3.0;

/// WCAG contrast ratio of two opaque colours, from 1 to 21.
pub fn contrast(a: Hsla, b: Hsla) -> f32 {
    fn luminance(c: Hsla) -> f32 {
        let c = c.to_rgb();
        let ch = |v: f32| {
            if v <= 0.039_28 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b)
    }
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `fg`, or the least change of it that reads at `min` contrast on every
/// background in `on`: moved towards white in dark themes and black in light
/// ones, which keeps its hue as long as it can. Themes pick their colours by
/// eye, and translucent chrome can sit over anything, so views that know
/// their surfaces hold colours to this rather than trusting them.
pub fn legible(fg: Hsla, on: &[Hsla], min: f32, cx: &App) -> Hsla {
    let towards = if cx.theme().mode.is_dark() {
        gpui_kit::white()
    } else {
        gpui_kit::black()
    };
    legible_towards(fg, on, min, towards)
}

fn legible_towards(fg: Hsla, on: &[Hsla], min: f32, towards: Hsla) -> Hsla {
    let reads = |c: Hsla| on.iter().all(|bg| contrast(c, *bg) >= min);
    let fg = Hsla { a: 1.0, ..fg };
    if reads(fg) {
        return fg;
    }
    let mix = |t: f32| {
        let (a, b) = (fg.to_rgb(), towards.to_rgb());
        Hsla::from(Rgba {
            r: a.r + (b.r - a.r) * t,
            g: a.g + (b.g - a.g) * t,
            b: a.b + (b.b - a.b) * t,
            a: 1.0,
        })
    };
    // Contrast grows monotonically along the way; find where it suffices.
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    if !reads(mix(hi)) {
        return towards;
    }
    for _ in 0..14 {
        let mid = (lo + hi) / 2.0;
        if reads(mix(mid)) { hi = mid } else { lo = mid }
    }
    mix(hi)
}

/// The sidebar's row colours, held readable on what the rows sit on: the
/// sidebar, or the highlight of a selected or hovered row, at the current
/// glass (where the desktop behind could be anything).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SidebarPalette {
    /// Fill of the selected row and of a hovered one.
    pub highlight: Hsla,
    /// Label of an idle row.
    pub text: Hsla,
    /// Icon of an idle row, which stays while it is hovered.
    pub icon: Hsla,
    /// Small headings ("RECENT").
    pub caption: Hsla,
    /// Label of the selected or hovered row.
    pub selected_text: Hsla,
    /// Icon and marker of the selected row.
    pub selected_icon: Hsla,
}

impl SidebarPalette {
    pub fn new(cx: &App) -> Self {
        let theme = cx.theme();
        let idle = crate::glass::chrome_surfaces(&[theme.sidebar], cx);
        let lit = crate::glass::chrome_surfaces(&[theme.sidebar, theme.sidebar_accent], cx);
        let both = [idle[0], idle[1], lit[0], lit[1]];
        Self {
            highlight: crate::glass::chrome(theme.sidebar_accent, cx),
            text: legible(theme.sidebar_foreground, &idle, TEXT_CONTRAST, cx),
            icon: legible(theme.muted_foreground, &both, ICON_CONTRAST, cx),
            caption: legible(theme.muted_foreground, &idle, TEXT_CONTRAST, cx),
            selected_text: legible(theme.sidebar_accent_foreground, &lit, TEXT_CONTRAST, cx),
            selected_icon: legible(theme.sidebar_primary, &lit, ICON_CONTRAST, cx),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(rgb: u32) -> Hsla {
        gpui_kit::rgb(rgb).into()
    }

    #[test]
    fn contrast_matches_wcag() {
        assert!((contrast(gpui_kit::black(), gpui_kit::white()) - 21.0).abs() < 0.01);
        assert!((contrast(hex(0x777777), gpui_kit::white()) - 4.48).abs() < 0.02);
        assert_eq!(contrast(hex(0x336699), hex(0x336699)), 1.0);
    }

    #[test]
    fn readable_colours_are_left_alone() {
        let fg = hex(0xf0f0f0);
        assert_eq!(
            legible_towards(fg, &[hex(0x101010)], 4.5, gpui_kit::white()),
            fg
        );
    }

    #[test]
    fn faint_colours_move_just_far_enough() {
        // Gruvbox Light's yellow on its own highlight reads at 1.65.
        let (fg, bg) = (hex(0xd79921), hex(0xe6d19e));
        let fixed = legible_towards(fg, &[bg], 3.0, gpui_kit::black());
        let ratio = contrast(fixed, bg);
        assert!((3.0..3.2).contains(&ratio), "{ratio}");
    }

    #[test]
    fn every_background_is_satisfied() {
        let fixed = legible_towards(
            hex(0x6c7086),
            &[hex(0x000000), hex(0x7a7a7a)],
            3.0,
            gpui_kit::white(),
        );
        for bg in [hex(0x000000), hex(0x7a7a7a)] {
            assert!(contrast(fixed, bg) >= 3.0);
        }
    }
}
