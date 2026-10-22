//! Themes: Ember's own light/dark pair plus a few bundled community themes,
//! applied from the user's [`UiPrefs`].

use ember_core::settings::{ThemePreference, UiPrefs};
use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, SharedString, Window, px};

pub const DEFAULT_DARK: &str = "Ember Night";
pub const DEFAULT_LIGHT: &str = "Ember Day";

const BUNDLED: &[&str] = &[
    include_str!("../../../assets/themes/ember.json"),
    include_str!("../../../assets/themes/catppuccin.json"),
    include_str!("../../../assets/themes/tokyonight.json"),
    include_str!("../../../assets/themes/gruvbox.json"),
    include_str!("../../../assets/themes/everforest.json"),
    include_str!("../../../assets/themes/solarized.json"),
    include_str!("../../../assets/themes/ayu.json"),
];

/// Ember's typeface (Manrope, SIL Open Font License), used by every theme.
pub const FONT_FAMILY: &str = "Manrope";

const FONTS: &[&[u8]] = &[
    include_bytes!("../../../assets/fonts/Manrope-Regular.ttf"),
    include_bytes!("../../../assets/fonts/Manrope-Medium.ttf"),
    include_bytes!("../../../assets/fonts/Manrope-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/Manrope-Bold.ttf"),
    include_bytes!("../../../assets/fonts/Manrope-ExtraBold.ttf"),
];

pub(crate) fn install(cx: &mut App) {
    let fonts = FONTS
        .iter()
        .map(|f| std::borrow::Cow::Borrowed(*f))
        .collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        tracing::warn!("loading Ember's fonts: {e:#}");
    }
    let registry = ThemeRegistry::global_mut(cx);
    for json in BUNDLED {
        if let Err(e) = registry.load_themes_from_str(json) {
            tracing::warn!("loading bundled theme: {e:#}");
        }
    }
}

/// Names of all available themes, dark ones first.
pub fn theme_names(cx: &App) -> Vec<(SharedString, bool)> {
    let mut names: Vec<(SharedString, bool)> = ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .map(|t| (t.name.clone(), t.mode.is_dark()))
        .collect();
    names.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    names
}

/// Apply theme, mode and base font size (application zoom) from preferences.
pub fn apply(prefs: &UiPrefs, window: Option<&mut Window>, cx: &mut App) {
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
    let dark = chosen
        .clone()
        .filter(|t| t.mode.is_dark())
        .or_else(|| find(DEFAULT_DARK));
    let light = chosen
        .filter(|t| !t.mode.is_dark())
        .or_else(|| find(DEFAULT_LIGHT));

    let font_size = prefs.font_size.clamp(12.0, 24.0);
    Theme::update(cx, |theme| {
        if let Some(l) = &light {
            theme.apply_config(l);
        }
        if let Some(d) = &dark {
            theme.apply_config(d);
        }
        theme.font_size = px(font_size);
        // The typeface is part of Ember's identity, whatever the colours.
        theme.font_family = FONT_FAMILY.into();
    });

    let mode = match prefs.theme {
        ThemePreference::Dark => Some(ThemeMode::Dark),
        ThemePreference::Light => Some(ThemeMode::Light),
        ThemePreference::System => None,
    };
    match mode {
        Some(m) => Theme::change(m, window, cx),
        None => Theme::sync_system_appearance(window, cx),
    }
}
