//! Translucent window chrome ("glass").
//!
//! With translucency on, the window asks the system to blur whatever is
//! behind it (macOS vibrancy, Windows 11 Mica, the compositor on Linux) and
//! Irori paints its title bar and sidebar partly transparent on top, so the
//! desktop shows through them. Pages stay solid unless the user picks
//! [`Translucency::Strong`]. Where nothing blurs, the same surfaces keep more
//! body, because sharp detail behind text is hard to read and looks broken.
//!
//! It costs Irori nothing to draw: the scene has the same primitives, only
//! some colours carry alpha. The blur is the compositor's, and the window
//! goes back to opaque wherever there is nothing worth seeing behind it
//! (Big Picture, fullscreen, gamescope), which also spares the compositor.
//!
//! Views ask [`chrome`] and [`page`] for their surface colours instead of
//! adding alpha themselves, so every surface agrees on one level.

use gpui_kit::component::{ActiveTheme as _, Root};
use gpui_kit::{App, Global, Hsla, Styled as _, Window, WindowBackgroundAppearance};
use irori_core::settings::Translucency;
use once_cell::sync::Lazy;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod linux;

/// How see-through the main window is right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Glass {
    #[default]
    Opaque,
    /// Title bar and sidebar let the desktop through; pages are solid.
    Subtle { blurred: bool },
    /// Clearer chrome, and pages let a little through as well.
    Strong { blurred: bool },
}

impl Global for Glass {}

impl Glass {
    pub fn is_translucent(self) -> bool {
        self != Glass::Opaque
    }

    /// Opacity of the title bar and sidebar. Light themes need more body
    /// than dark ones to stay readable over a busy wallpaper.
    fn chrome_alpha(self, dark: bool) -> f32 {
        match (self, dark) {
            (Glass::Opaque, _) => 1.0,
            (Glass::Subtle { blurred: true }, true) => 0.66,
            (Glass::Subtle { blurred: true }, false) => 0.74,
            // White text still reads at 4.5:1 over a white window behind.
            (Glass::Strong { blurred: true }, true) => 0.58,
            (Glass::Strong { blurred: true }, false) => 0.62,
            (Glass::Subtle { blurred: false }, true) => 0.9,
            (Glass::Subtle { blurred: false }, false) => 0.92,
            (Glass::Strong { blurred: false }, true) => 0.82,
            (Glass::Strong { blurred: false }, false) => 0.86,
        }
    }

    /// Opacity of the page area beside the sidebar.
    fn page_alpha(self, dark: bool) -> f32 {
        match (self, dark) {
            (Glass::Strong { blurred: true }, true) => 0.84,
            (Glass::Strong { blurred: true }, false) => 0.88,
            (Glass::Strong { blurred: false }, true) => 0.94,
            (Glass::Strong { blurred: false }, false) => 0.96,
            _ => 1.0,
        }
    }
}

/// What this machine can put behind a translucent window.
#[derive(Debug, Clone, Copy)]
struct Backdrop {
    appearance: WindowBackgroundAppearance,
    /// The system blurs it.
    blurs: bool,
    /// [`Translucency::Auto`] turns it on. Without blur, text over an
    /// arbitrary wallpaper is hard to read, so it stays opaque.
    automatic: bool,
}

/// Detected once: the OS, its version and the desktop do not change while
/// Irori runs.
static BACKDROP: Lazy<Option<Backdrop>> = Lazy::new(detect_backdrop);

#[cfg(target_os = "macos")]
fn detect_backdrop() -> Option<Backdrop> {
    // An NSVisualEffectView behind the content, blur only (GPUI strips its
    // tint), so the theme's colours decide the look.
    Some(Backdrop {
        appearance: WindowBackgroundAppearance::Blurred,
        blurs: true,
        automatic: true,
    })
}

#[cfg(target_os = "windows")]
fn detect_backdrop() -> Option<Backdrop> {
    // Without DirectComposition GPUI's swap chain ignores alpha.
    if std::env::var("GPUI_DISABLE_DIRECT_COMPOSITION").is_ok_and(|v| v == "true" || v == "1") {
        return None;
    }
    let build = sysinfo::System::kernel_version()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if build >= 22621 {
        // Windows 11 22H2: Mica, which samples the wallpaper once and costs
        // next to nothing.
        Some(Backdrop {
            appearance: WindowBackgroundAppearance::MicaBackdrop,
            blurs: true,
            automatic: true,
        })
    } else if build >= 17763 {
        // Older builds only have acrylic, which lags while the window is
        // dragged; available when asked for, never by default.
        Some(Backdrop {
            appearance: WindowBackgroundAppearance::Blurred,
            blurs: true,
            automatic: false,
        })
    } else {
        None
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn detect_backdrop() -> Option<Backdrop> {
    // Steam's gaming mode: Irori is the only thing on screen.
    if std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktop| desktop == "gamescope") {
        return None;
    }
    // Where the compositor offers no blur the window is plainly see-through,
    // unless it has blur rules of its own (SwayFX, picom, Blur my Shell) that
    // match Irori by its app id.
    let blur = *linux::BLUR;
    let blurs = blur != linux::Blur::None;
    Some(Backdrop {
        appearance: blur.appearance(),
        blurs,
        automatic: blurs,
    })
}

/// The glass a preference asks for on this machine.
pub fn resolve(pref: Translucency) -> Glass {
    let Some(backdrop) = *BACKDROP else {
        return Glass::Opaque;
    };
    let blurred = backdrop.blurs;
    match pref {
        Translucency::Auto if backdrop.automatic => Glass::Subtle { blurred },
        Translucency::Auto | Translucency::Off => Glass::Opaque,
        Translucency::Subtle => Glass::Subtle { blurred },
        Translucency::Strong => Glass::Strong { blurred },
    }
}

/// The window background `glass` needs, for `WindowOptions` and [`apply`].
pub fn window_background(glass: Glass) -> WindowBackgroundAppearance {
    match *BACKDROP {
        Some(backdrop) if glass.is_translucent() => backdrop.appearance,
        _ => WindowBackgroundAppearance::Opaque,
    }
}

/// The glass the main window currently has.
pub fn current(cx: &App) -> Glass {
    cx.try_global::<Glass>().copied().unwrap_or_default()
}

/// `color` as a title bar or sidebar surface at the current glass.
pub fn chrome(color: Hsla, cx: &App) -> Hsla {
    match current(cx) {
        Glass::Opaque => color,
        glass => {
            let level = glass.chrome_alpha(cx.theme().mode.is_dark());
            color.opacity(level.max(legibility_floor(cx)))
        }
    }
}

/// The least opacity at which the theme's sidebar still carries text at
/// 4.5:1 whatever is behind the window. Mid-tone sidebars (Solarized,
/// Tokyo Night) need more body than near-black ones; the glass level is
/// raised to this where it would fall below. Cached per sidebar colour.
fn legibility_floor(cx: &App) -> f32 {
    thread_local! {
        static FLOOR: std::cell::Cell<Option<([u32; 4], bool, f32)>> =
            const { std::cell::Cell::new(None) };
    }
    let theme = cx.theme();
    let surface = theme.sidebar;
    let dark = theme.mode.is_dark();
    let key = [surface.h, surface.s, surface.l, surface.a].map(f32::to_bits);
    if let Some((k, d, floor)) = FLOOR.get()
        && k == key
        && d == dark
    {
        return floor;
    }
    // Dark themes write white over it, with a white window behind at worst;
    // light themes the reverse.
    let (ink, behind) = if dark {
        (gpui_kit::white(), gpui_kit::white())
    } else {
        (gpui_kit::black(), gpui_kit::black())
    };
    let reads =
        |a: f32| crate::style::contrast(ink, over(surface.opacity(a), behind)) >= LEGIBLE_CHROME;
    let floor = if !reads(1.0) {
        1.0
    } else {
        let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
        for _ in 0..12 {
            let mid = (lo + hi) / 2.0;
            if reads(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    };
    FLOOR.set(Some((key, dark, floor)));
    floor
}

/// Contrast the chrome floor keeps for text, a little over WCAG AA so the
/// colours derived from it have room.
const LEGIBLE_CHROME: f32 = 4.7;

/// `color` as the page surface at the current glass.
pub fn page(color: Hsla, cx: &App) -> Hsla {
    match current(cx) {
        Glass::Opaque => color,
        glass => color.opacity(glass.page_alpha(cx.theme().mode.is_dark())),
    }
}

/// What a stack of chrome `layers` (bottom first, each painted through
/// [`chrome`]) can look like on screen. Translucent, the desktop behind is
/// unknown, so the stack is shown over the two extremes, black and white;
/// opaque, both entries are the stack itself. Readability checks hold a
/// colour against both.
pub fn chrome_surfaces(layers: &[Hsla], cx: &App) -> [Hsla; 2] {
    let stack = |backdrop: Hsla| {
        layers
            .iter()
            .fold(backdrop, |under, layer| over(chrome(*layer, cx), under))
    };
    if current(cx).is_translucent() {
        [stack(gpui_kit::black()), stack(gpui_kit::white())]
    } else {
        let solid = stack(cx.theme().background);
        [solid, solid]
    }
}

/// `top` composited over the opaque `under`.
fn over(top: Hsla, under: Hsla) -> Hsla {
    let (t, u) = (top.to_rgb(), under.to_rgb());
    let a = top.a;
    gpui_kit::Rgba {
        r: t.r * a + u.r * (1. - a),
        g: t.g * a + u.g * (1. - a),
        b: t.b * a + u.b * (1. - a),
        a: 1.,
    }
    .into()
}

/// Whether pages are solid, so a page may paint its own opaque background.
/// Under [`Glass::Strong`] the shell paints the one translucent page surface
/// and pages should leave it showing rather than stack another on top.
pub fn solid_pages(cx: &App) -> bool {
    !matches!(current(cx), Glass::Strong { .. })
}

/// Give `window` the glass `glass`: its system backdrop, the root surface
/// behind Irori's views, and the colours [`chrome`] and [`page`] hand out.
/// Callers skip it when the glass did not change; it refreshes the window.
///
/// Returns false, having changed nothing, while the window has no root view
/// yet (during its construction); call it again once it has.
pub fn apply(glass: Glass, window: &mut Window, cx: &mut App) -> bool {
    let Some(root) = window.root::<Root>().flatten() else {
        return false;
    };
    window.set_background_appearance(window_background(glass));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    linux::request(glass.is_translucent(), window, cx);
    // gpui-kit's root paints the theme background under everything; clear
    // it so the backdrop shows. `None` hands it back to the theme.
    root.update(cx, |root, cx| {
        root.style().background = if glass.is_translucent() {
            Some(gpui_kit::transparent_black().into())
        } else {
            None
        };
        cx.notify();
    });
    cx.set_global(glass);
    window.refresh();
    true
}

/// Keep a Linux compositor's blur on the visible window after it resized.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn refit(window: &Window, cx: &mut App) {
    linux::request(current(cx).is_translucent(), window, cx);
}

/// The system's backdrop follows the window by itself here.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn refit(_window: &Window, _cx: &mut App) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_is_always_opaque() {
        assert_eq!(resolve(Translucency::Off), Glass::Opaque);
        assert_eq!(
            window_background(Glass::Opaque),
            WindowBackgroundAppearance::Opaque
        );
    }

    #[test]
    fn explicit_levels_need_only_a_backdrop() {
        let blurred = BACKDROP.is_some_and(|b| b.blurs);
        let expected = |g| if BACKDROP.is_some() { g } else { Glass::Opaque };
        assert_eq!(
            resolve(Translucency::Subtle),
            expected(Glass::Subtle { blurred })
        );
        assert_eq!(
            resolve(Translucency::Strong),
            expected(Glass::Strong { blurred })
        );
    }

    #[test]
    fn auto_needs_a_backdrop_that_turns_on_by_itself() {
        let want = match *BACKDROP {
            Some(b) if b.automatic => Glass::Subtle { blurred: b.blurs },
            _ => Glass::Opaque,
        };
        assert_eq!(resolve(Translucency::Auto), want);
    }

    #[test]
    fn chrome_is_clearer_than_pages_and_light_keeps_more_body() {
        for blurred in [true, false] {
            let (subtle, strong) = (Glass::Subtle { blurred }, Glass::Strong { blurred });
            for glass in [subtle, strong] {
                for dark in [true, false] {
                    assert!(glass.chrome_alpha(dark) < glass.page_alpha(dark));
                }
                assert!(glass.chrome_alpha(false) > glass.chrome_alpha(true));
            }
            assert!(strong.chrome_alpha(true) < subtle.chrome_alpha(true));
            assert_eq!(subtle.page_alpha(true), 1.0);
        }
        assert_eq!(Glass::Opaque.chrome_alpha(true), 1.0);
    }

    #[test]
    fn surfaces_keep_more_body_without_blur() {
        for dark in [true, false] {
            for (clear, plain) in [
                (
                    Glass::Subtle { blurred: true },
                    Glass::Subtle { blurred: false },
                ),
                (
                    Glass::Strong { blurred: true },
                    Glass::Strong { blurred: false },
                ),
            ] {
                assert!(plain.chrome_alpha(dark) > clear.chrome_alpha(dark));
                assert!(plain.page_alpha(dark) >= clear.page_alpha(dark));
            }
        }
    }
}
