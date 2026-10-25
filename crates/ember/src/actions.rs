//! Window-level commands and their key bindings.

use gpui_kit::{App, KeyBinding, actions};

actions!(
    ember,
    [
        GoBack,
        GoForward,
        OpenPalette,
        GoLibrary,
        GoDownloads,
        GoStores,
        GoStats,
        GoWine,
        GoAccounts,
        GoSettings,
        RefreshLibrary,
        ToggleSidebar,
        ToggleFullscreen,
        EnterBigPicture,
        Quit
    ]
);

/// Context name of the main window's root element.
pub const SHELL_CONTEXT: &str = "EmberShell";

pub fn bind_keys(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    let ctx = Some(SHELL_CONTEXT);
    let primary = |key: &str| {
        if cfg!(target_os = "macos") {
            key.replacen("ctrl-", "cmd-", 1)
        } else {
            key.to_owned()
        }
    };
    cx.bind_keys([
        KeyBinding::new("alt-left", GoBack, ctx),
        KeyBinding::new("alt-right", GoForward, ctx),
        KeyBinding::new(&primary("ctrl-k"), OpenPalette, ctx),
        KeyBinding::new(&primary("ctrl-p"), OpenPalette, ctx),
        KeyBinding::new(&primary("ctrl-1"), GoLibrary, ctx),
        KeyBinding::new(&primary("ctrl-2"), GoDownloads, ctx),
        KeyBinding::new(&primary("ctrl-3"), GoStores, ctx),
        KeyBinding::new(&primary("ctrl-4"), GoWine, ctx),
        KeyBinding::new(&primary("ctrl-5"), GoAccounts, ctx),
        KeyBinding::new(&primary("ctrl-6"), GoStats, ctx),
        KeyBinding::new(&primary("ctrl-,"), GoSettings, ctx),
        KeyBinding::new(&primary("ctrl-r"), RefreshLibrary, ctx),
        KeyBinding::new("f5", RefreshLibrary, ctx),
        KeyBinding::new(&primary("ctrl-b"), ToggleSidebar, ctx),
        KeyBinding::new("f11", ToggleFullscreen, ctx),
        KeyBinding::new(&primary("ctrl-shift-b"), EnterBigPicture, ctx),
        KeyBinding::new(&primary("ctrl-q"), Quit, None),
    ]);
    #[cfg(target_os = "macos")]
    {
        use ember_ui::i18n::t;
        use gpui_kit::{Menu, MenuItem};
        cx.bind_keys([
            KeyBinding::new("cmd-[", GoBack, ctx),
            KeyBinding::new("cmd-]", GoForward, ctx),
            KeyBinding::new("ctrl-cmd-f", ToggleFullscreen, ctx),
        ]);
        cx.set_menus([Menu::new("Ember").items([
            MenuItem::action(t("shell.nav.settings"), GoSettings),
            MenuItem::separator(),
            MenuItem::action(t("shell.tray.quit"), Quit),
        ])]);
    }
}
