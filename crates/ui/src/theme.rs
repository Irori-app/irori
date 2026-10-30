//! Themes: Irori's own light/dark pair plus a few bundled community themes,
//! applied from the user's [`UiPrefs`].
//!
//! Each bundled file is a family (Catppuccin, Gruvbox...). The user picks one
//! theme; the other colour mode uses its sibling in the same family (Latte ↔
//! Mocha), or Irori's own theme when the family has none, so following the
//! system never drops the user back to an unrelated look.

use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use gpui_kit::{App, Global, Hsla, Rgba, SharedString, Window, WindowAppearance, px};
use irori_core::settings::{ThemePreference, UiPrefs};

use crate::style::{TEXT_CONTRAST, contrast, legible_towards};

pub const DEFAULT_DARK: &str = "Irori Night";
pub const DEFAULT_LIGHT: &str = "Irori Day";

const BUNDLED: &[&str] = &[
    include_str!("../../../assets/themes/irori.json"),
    include_str!("../../../assets/themes/catppuccin.json"),
    include_str!("../../../assets/themes/tokyonight.json"),
    include_str!("../../../assets/themes/gruvbox.json"),
    include_str!("../../../assets/themes/everforest.json"),
    include_str!("../../../assets/themes/solarized.json"),
    include_str!("../../../assets/themes/ayu.json"),
];

/// Irori's typeface (Manrope, SIL Open Font License), used by every theme.
pub const FONT_FAMILY: &str = "Manrope";

const FONTS: &[&[u8]] = &[
    include_bytes!("../../../assets/fonts/Manrope-Regular.ttf"),
    include_bytes!("../../../assets/fonts/Manrope-Medium.ttf"),
    include_bytes!("../../../assets/fonts/Manrope-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/Manrope-Bold.ttf"),
    include_bytes!("../../../assets/fonts/Manrope-ExtraBold.ttf"),
];

/// The bundled themes by family, in file order.
#[derive(Default)]
struct Families(Vec<Vec<(SharedString, bool)>>);

impl Global for Families {}

/// What [`apply`] last applied, so a call that changes nothing costs nothing
/// and one that only resizes the interface skips re-theming.
#[derive(Clone, PartialEq)]
struct Applied {
    light: SharedString,
    dark: SharedString,
    is_dark: bool,
    font_size: u32,
}

impl Global for Applied {}

/// Extra zoom on top of the interface size while a screen asks for it
/// (Big Picture on a TV). Absent means 1.
#[derive(Clone, Copy, PartialEq)]
struct Zoom(f32);

impl Global for Zoom {}

fn zoom(cx: &App) -> f32 {
    cx.try_global::<Zoom>().map_or(1.0, |z| z.0)
}

/// Scale the whole interface by `zoom` on top of the interface size the user
/// picked (1 = none). Big Picture sets its TV size with this while it shows
/// and resets it to 1 when it closes.
pub fn set_zoom(zoom: f32, cx: &mut App) {
    let zoom = zoom.clamp(0.5, 3.0);
    if self::zoom(cx) == zoom {
        return;
    }
    cx.set_global(Zoom(zoom));
    let Some(base) = cx
        .try_global::<Applied>()
        .map(|a| f32::from_bits(a.font_size))
    else {
        return;
    };
    Theme::update(cx, |theme| theme.font_size = px(base * zoom));
    cx.refresh_windows();
}

pub(crate) fn install(cx: &mut App) {
    let fonts = FONTS
        .iter()
        .map(|f| std::borrow::Cow::Borrowed(*f))
        .collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        tracing::warn!("loading Irori's fonts: {e:#}");
    }
    let mut families = Families::default();
    let registry = ThemeRegistry::global_mut(cx);
    for json in BUNDLED {
        if let Err(e) = registry.load_themes_from_str(json) {
            tracing::warn!("loading bundled theme: {e:#}");
        }
        families.0.push(family_members(json));
    }
    cx.set_global(families);
}

/// Names and modes of the themes in one bundled file.
fn family_members(json: &str) -> Vec<(SharedString, bool)> {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    doc["themes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let name = t["name"].as_str()?;
            Some((SharedString::from(name.to_string()), t["mode"] == "dark"))
        })
        .collect()
}

/// The sibling of `name` in the other colour mode: the last theme of that mode
/// in its family (the family's flagship, e.g. Catppuccin Mocha).
fn sibling(name: &str, dark: bool, cx: &App) -> Option<SharedString> {
    let families = cx.try_global::<Families>()?;
    families
        .0
        .iter()
        .find(|f| f.iter().any(|(n, _)| n.as_ref() == name))?
        .iter()
        .rev()
        .find(|(_, d)| *d == dark)
        .map(|(n, _)| n.clone())
}

/// Names of the themes the user can pick, dark ones first. gpui-kit's own
/// unstyled defaults are left out: they are not Irori themes.
pub fn theme_names(cx: &App) -> Vec<(SharedString, bool)> {
    let mut names: Vec<(SharedString, bool)> = ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .filter(|t| !t.is_default)
        .map(|t| (t.name.clone(), t.mode.is_dark()))
        .collect();
    names.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    names
}

/// Whether the theme called `name` is a dark one, if it exists.
pub fn is_dark_theme(name: &str, cx: &App) -> Option<bool> {
    ThemeRegistry::global(cx)
        .themes()
        .get(&SharedString::from(name.to_string()))
        .map(|t| t.mode.is_dark())
}

/// The light and dark themes `prefs` call for.
fn pair(prefs: &UiPrefs, cx: &App) -> (Option<Rc<ThemeConfig>>, Option<Rc<ThemeConfig>>) {
    let registry = ThemeRegistry::global(cx);
    let find = |name: &str| {
        registry
            .themes()
            .get(&SharedString::from(name.to_string()))
            .cloned()
    };
    let chosen = (!prefs.theme_name.is_empty())
        .then(|| find(&prefs.theme_name))
        .flatten();
    let for_mode = |dark: bool| {
        let fallback = if dark { DEFAULT_DARK } else { DEFAULT_LIGHT };
        match &chosen {
            Some(t) if t.mode.is_dark() == dark => Some(t.clone()),
            Some(t) => sibling(&t.name, dark, cx)
                .and_then(|n| find(&n))
                .or_else(|| find(fallback)),
            None => find(fallback),
        }
    };
    (for_mode(false), for_mode(true))
}

/// Apply theme, mode and base font size (application zoom) from preferences.
///
/// Cheap to call often: it does nothing when the result would not change, and
/// only rescales when just the interface size did.
pub fn apply(prefs: &UiPrefs, window: Option<&mut Window>, cx: &mut App) {
    let is_dark = match prefs.theme {
        ThemePreference::Dark => true,
        ThemePreference::Light => false,
        ThemePreference::System => {
            let appearance = window
                .as_deref()
                .map(Window::appearance)
                .unwrap_or_else(|| cx.window_appearance());
            matches!(
                appearance,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            )
        }
    };
    let font_size = prefs.font_size.clamp(12.0, 24.0);
    let (light, dark) = pair(prefs, cx);
    let name = |t: &Option<Rc<ThemeConfig>>| t.as_ref().map(|t| t.name.clone()).unwrap_or_default();
    let next = Applied {
        light: name(&light),
        dark: name(&dark),
        is_dark,
        font_size: font_size.to_bits(),
    };
    let previous = cx.try_global::<Applied>().cloned();
    if previous.as_ref() == Some(&next) {
        return;
    }
    let only_size = previous.is_some_and(|p| {
        Applied {
            font_size: next.font_size,
            ..p
        } == next
    });
    cx.set_global(next);
    let zoomed = font_size * zoom(cx);
    if only_size {
        Theme::update(cx, |theme| theme.font_size = px(zoomed));
        return;
    }

    Theme::update(cx, |theme| {
        if let Some(l) = &light {
            theme.apply_config(l);
        }
        if let Some(d) = &dark {
            theme.apply_config(d);
        }
        theme.font_size = px(zoomed);
        // The typeface is part of Irori's identity, whatever the colours.
        theme.font_family = FONT_FAMILY.into();
    });
    let mode = if is_dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    Theme::change(mode, window, cx);
    Theme::update(cx, hold_status_colors);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    lift_modal_dim(cx);
}

/// The theme's dim behind dialogs and sheets, kept out of its reach.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
struct ModalDim(Hsla);

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl Global for ModalDim {}

/// gpui-kit paints this dim across the whole window surface, and on Linux that
/// includes the transparent margin client-side decorations keep for their
/// shadow, which shows as a dark band around the window. Clearing its colour
/// is the one hook that reaches every dialog and sheet, so [`modal_dim`] hands
/// the colour to the shell, which paints it inside the visible window.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn lift_modal_dim(cx: &mut App) {
    let dim = Theme::global(cx).overlay;
    if dim.a > 0. {
        cx.set_global(ModalDim(dim));
    }
    Theme::update(cx, |theme| theme.overlay = gpui_kit::transparent_black());
}

/// The dim the shell should paint over itself right now, if a dialog or sheet
/// is open and gpui-kit is not already painting it.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn modal_dim(window: &mut Window, cx: &mut App) -> Option<Hsla> {
    use gpui_kit::component::WindowExt as _;

    if !window.has_active_dialog(cx) && !window.has_active_sheet(cx) {
        return None;
    }
    cx.try_global::<ModalDim>().map(|dim| dim.0)
}

/// gpui-kit paints the dim itself here: these windows have no shadow margin.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn modal_dim(_window: &mut Window, _cx: &mut App) -> Option<Hsla> {
    None
}

/// What status colours aim for, a little over WCAG's 4.5:1: alerts tint their
/// own background with the colour they write in.
const STATUS_CONTRAST: f32 = 4.8;
/// How much of its colour an alert mixes into its background.
const ALERT_TINT: f32 = 0.04;

/// Themes choose warning, danger, info and success by eye, and many light
/// ones read at 2:1 as text on their own page. Every alert and status line
/// writes in these colours, so hold them here instead of at each. A colour
/// that already reads stays exactly as the theme has it.
fn hold_status_colors(theme: &mut Theme) {
    let towards = if theme.mode.is_dark() {
        gpui_kit::white()
    } else {
        gpui_kit::black()
    };
    let surfaces = [theme.background, theme.popover];
    let fix = |color: Hsla| {
        let tinted = surfaces.map(|s| tint(s, color));
        legible_towards(color, &tinted, STATUS_CONTRAST, towards)
    };
    // What sits on the colour when it fills a button or a badge.
    let on = |foreground: Hsla, fill: Hsla| {
        let best = if contrast(gpui_kit::white(), fill) > contrast(gpui_kit::black(), fill) {
            gpui_kit::white()
        } else {
            gpui_kit::black()
        };
        legible_towards(foreground, &[fill], TEXT_CONTRAST, best)
    };
    let t = &mut *theme;
    (t.warning, t.warning_hover, t.warning_active) =
        (fix(t.warning), fix(t.warning_hover), fix(t.warning_active));
    t.warning_foreground = on(t.warning_foreground, t.warning);
    (t.danger, t.danger_hover, t.danger_active) =
        (fix(t.danger), fix(t.danger_hover), fix(t.danger_active));
    t.danger_foreground = on(t.danger_foreground, t.danger);
    (t.info, t.info_hover, t.info_active) = (fix(t.info), fix(t.info_hover), fix(t.info_active));
    t.info_foreground = on(t.info_foreground, t.info);
    (t.success, t.success_hover, t.success_active) =
        (fix(t.success), fix(t.success_hover), fix(t.success_active));
    t.success_foreground = on(t.success_foreground, t.success);
}

/// `color` at [`ALERT_TINT`] over `surface`, in sRGB.
fn tint(surface: Hsla, color: Hsla) -> Hsla {
    let (s, c) = (surface.to_rgb(), color.to_rgb());
    let mix = |a: f32, b: f32| a + (b - a) * ALERT_TINT;
    Rgba {
        r: mix(s.r, c.r),
        g: mix(s.g, c.g),
        b: mix(s.b, c.b),
        a: 1.0,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use gpui_kit::TestAppContext;
    use gpui_kit::component::ActiveTheme as _;

    use super::*;

    fn setup(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install(cx);
        });
    }

    fn prefs(theme: ThemePreference, name: &str) -> UiPrefs {
        UiPrefs {
            theme,
            theme_name: name.to_string(),
            ..UiPrefs::default()
        }
    }

    /// The colour a theme file gives `key`, as GPUI parses it.
    fn file_color(name: &str, key: &str) -> gpui_kit::Hsla {
        for json in BUNDLED {
            let doc: serde_json::Value = serde_json::from_str(json).unwrap();
            for t in doc["themes"].as_array().unwrap() {
                if t["name"] == name {
                    let raw = t["colors"][key].as_str().unwrap();
                    return gpui_kit::component::try_parse_color(raw).unwrap();
                }
            }
        }
        panic!("no theme {name}");
    }

    #[test]
    fn every_bundled_theme_colors_everything_irori_paints() {
        let irori: serde_json::Value = serde_json::from_str(BUNDLED[0]).unwrap();
        let wanted: BTreeSet<&str> = irori["themes"][0]["colors"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for json in BUNDLED {
            let doc: serde_json::Value = serde_json::from_str(json).unwrap();
            for t in doc["themes"].as_array().unwrap() {
                let has: BTreeSet<&str> = t["colors"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                let missing: Vec<_> = wanted.difference(&has).collect();
                assert!(missing.is_empty(), "{} lacks {missing:?}", t["name"]);
            }
        }
    }

    #[gpui_kit::test]
    fn every_theme_shows_when_its_mode_is_current(cx: &mut TestAppContext) {
        setup(cx);
        let names = cx.read(theme_names);
        assert!(names.len() >= 17, "{names:?}");
        for (name, dark) in names {
            let mode = if dark {
                ThemePreference::Dark
            } else {
                ThemePreference::Light
            };
            cx.update(|cx| apply(&prefs(mode, &name), None, cx));
            cx.read(|cx| {
                let theme = cx.theme();
                assert_eq!(theme.theme_name(), &name);
                assert_eq!(theme.mode.is_dark(), dark, "{name}");
                assert_eq!(theme.background, file_color(&name, "background"), "{name}");
                assert_eq!(
                    theme.sidebar,
                    file_color(&name, "sidebar.background"),
                    "{name}"
                );
                assert_eq!(theme.font_family.as_ref(), FONT_FAMILY);
            });
        }
    }

    #[gpui_kit::test]
    fn the_other_mode_uses_the_family_sibling(cx: &mut TestAppContext) {
        setup(cx);
        let shown = |cx: &mut TestAppContext, mode, name: &str| {
            cx.update(|cx| apply(&prefs(mode, name), None, cx));
            cx.read(|cx| cx.theme().theme_name().to_string())
        };
        assert_eq!(
            shown(cx, ThemePreference::Dark, "Catppuccin Latte"),
            "Catppuccin Mocha"
        );
        assert_eq!(
            shown(cx, ThemePreference::Light, "Gruvbox Dark"),
            "Gruvbox Light"
        );
        // Tokyo Night has no light theme: Irori's own stands in.
        assert_eq!(
            shown(cx, ThemePreference::Light, "Tokyo Moon"),
            DEFAULT_LIGHT
        );
        assert_eq!(shown(cx, ThemePreference::Dark, ""), DEFAULT_DARK);
    }

    #[gpui_kit::test]
    fn resizing_keeps_the_theme(cx: &mut TestAppContext) {
        setup(cx);
        let mut p = prefs(ThemePreference::Dark, "Tokyo Storm");
        cx.update(|cx| apply(&p, None, cx));
        p.font_size = 20.0;
        cx.update(|cx| apply(&p, None, cx));
        cx.read(|cx| {
            assert_eq!(cx.theme().theme_name().as_ref(), "Tokyo Storm");
            assert_eq!(cx.theme().font_size, px(20.0));
        });
    }

    #[gpui_kit::test]
    fn zoom_scales_the_interface_size(cx: &mut TestAppContext) {
        setup(cx);
        let mut p = prefs(ThemePreference::Dark, "");
        cx.update(|cx| apply(&p, None, cx));
        cx.update(|cx| set_zoom(1.5, cx));
        cx.read(|cx| assert_eq!(cx.theme().font_size, px(24.0)));
        p.font_size = 20.0;
        cx.update(|cx| apply(&p, None, cx));
        cx.read(|cx| assert_eq!(cx.theme().font_size, px(30.0)));
        cx.update(|cx| set_zoom(1.0, cx));
        cx.read(|cx| assert_eq!(cx.theme().font_size, px(20.0)));
    }

    #[gpui_kit::test]
    fn sidebar_rows_read_in_every_theme_and_glass(cx: &mut TestAppContext) {
        use crate::glass::{Glass, chrome_surfaces};
        use crate::style::{ICON_CONTRAST, SidebarPalette, TEXT_CONTRAST, contrast};

        setup(cx);
        for (name, dark) in cx.read(theme_names) {
            let mode = if dark {
                ThemePreference::Dark
            } else {
                ThemePreference::Light
            };
            cx.update(|cx| apply(&prefs(mode, &name), None, cx));
            for glass in [
                Glass::Opaque,
                Glass::Subtle { blurred: true },
                Glass::Strong { blurred: true },
                Glass::Subtle { blurred: false },
                Glass::Strong { blurred: false },
            ] {
                cx.update(|cx| cx.set_global(glass));
                cx.read(|cx| {
                    let theme = cx.theme();
                    let p = SidebarPalette::new(cx);
                    let idle = chrome_surfaces(&[theme.sidebar], cx);
                    let lit = chrome_surfaces(&[theme.sidebar, theme.sidebar_accent], cx);
                    let check = |what: &str, fg, on: &[gpui_kit::Hsla], min| {
                        for bg in on {
                            let ratio = contrast(fg, *bg);
                            assert!(ratio >= min - 0.01, "{name} {glass:?} {what}: {ratio:.2}");
                        }
                    };
                    check("text", p.text, &idle, TEXT_CONTRAST);
                    check("icon", p.icon, &idle, ICON_CONTRAST);
                    check("hovered icon", p.icon, &lit, ICON_CONTRAST);
                    check("caption", p.caption, &idle, TEXT_CONTRAST);
                    check("selected text", p.selected_text, &lit, TEXT_CONTRAST);
                    check("selected icon", p.selected_icon, &lit, ICON_CONTRAST);
                });
            }
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[gpui_kit::test]
    fn the_modal_dim_never_reaches_gpui_kit(cx: &mut TestAppContext) {
        setup(cx);
        for (name, dark) in cx.read(theme_names) {
            let mode = if dark {
                ThemePreference::Dark
            } else {
                ThemePreference::Light
            };
            cx.update(|cx| apply(&prefs(mode, &name), None, cx));
            // Anything left here dims the shadow margin around the window.
            cx.read(|cx| assert_eq!(cx.theme().overlay.a, 0., "{name}"));
            let dim = cx.read(|cx| cx.global::<ModalDim>().0);
            assert!(dim.a > 0., "{name} lost its dim");
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[gpui_kit::test]
    fn the_modal_dim_lasts_as_long_as_a_dialog_or_sheet(cx: &mut TestAppContext) {
        use gpui_kit::AppContext as _;
        use gpui_kit::component::WindowExt as _;

        setup(cx);
        cx.update(|cx| apply(&prefs(ThemePreference::Dark, ""), None, cx));
        let (handle, _) = cx.update(|cx| {
            gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |_, cx| {
                cx.new(|_| gpui_kit::Empty)
            })
            .expect("open test window")
        });
        let dim = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| modal_dim(window, cx))
                .expect("window is open")
        };
        assert_eq!(dim(cx), None);

        cx.update_window(handle, |_, window, cx| {
            window.open_dialog(cx, |dialog, _, _| dialog);
        })
        .expect("window is open");
        cx.run_until_parked();
        assert!(dim(cx).is_some(), "a dialog is open");
        cx.update_window(handle, |_, window, cx| window.close_dialog(cx))
            .expect("window is open");
        cx.run_until_parked();
        assert_eq!(dim(cx), None);

        cx.update_window(handle, |_, window, cx| {
            window.open_sheet(cx, |sheet, _, _| sheet);
        })
        .expect("window is open");
        cx.run_until_parked();
        assert!(dim(cx).is_some(), "a sheet is open");
    }

    #[gpui_kit::test]
    fn status_colours_read_as_text_on_every_bundled_theme(cx: &mut TestAppContext) {
        setup(cx);
        for json in BUNDLED {
            for (name, dark) in family_members(json) {
                cx.update(|cx| {
                    let theme = if dark {
                        ThemePreference::Dark
                    } else {
                        ThemePreference::Light
                    };
                    apply(&prefs(theme, &name), None, cx);
                    let t = cx.theme();
                    for (what, color, on_it) in [
                        ("warning", t.warning, t.warning_foreground),
                        ("danger", t.danger, t.danger_foreground),
                        ("info", t.info, t.info_foreground),
                        ("success", t.success, t.success_foreground),
                    ] {
                        for surface in [t.background, t.popover] {
                            let alert = contrast(color, tint(surface, color));
                            assert!(alert >= 4.5, "{name} {what}: {alert}");
                        }
                        let filled = contrast(on_it, color);
                        assert!(filled >= 4.5, "{name} {what} filled: {filled}");
                    }
                });
            }
        }
    }

    #[gpui_kit::test]
    fn status_colours_that_read_stay_as_the_theme_has_them(cx: &mut TestAppContext) {
        setup(cx);
        cx.update(|cx| {
            apply(&prefs(ThemePreference::Dark, DEFAULT_DARK), None, cx);
            assert_eq!(
                cx.theme().warning,
                file_color(DEFAULT_DARK, "warning.background")
            );
        });
    }

    #[test]
    fn gpui_kits_unstyled_defaults_are_not_offered() {
        // Covered by `theme_names` filtering `is_default`; the bundled files
        // must not reuse those names either.
        for json in BUNDLED {
            for (name, _) in family_members(json) {
                assert!(!name.starts_with("Default "), "{name}");
            }
        }
    }
}
