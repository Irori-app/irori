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
    let ctx = Some(SHELL_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("alt-left", GoBack, ctx),
        KeyBinding::new("alt-right", GoForward, ctx),
        KeyBinding::new("ctrl-k", OpenPalette, ctx),
        KeyBinding::new("ctrl-p", OpenPalette, ctx),
        KeyBinding::new("ctrl-1", GoLibrary, ctx),
        KeyBinding::new("ctrl-2", GoDownloads, ctx),
        KeyBinding::new("ctrl-3", GoStores, ctx),
        KeyBinding::new("ctrl-4", GoWine, ctx),
        KeyBinding::new("ctrl-5", GoAccounts, ctx),
        KeyBinding::new("ctrl-6", GoStats, ctx),
        KeyBinding::new("ctrl-,", GoSettings, ctx),
        KeyBinding::new("ctrl-r", RefreshLibrary, ctx),
        KeyBinding::new("f5", RefreshLibrary, ctx),
        KeyBinding::new("ctrl-b", ToggleSidebar, ctx),
        KeyBinding::new("f11", ToggleFullscreen, ctx),
        KeyBinding::new("ctrl-shift-b", EnterBigPicture, ctx),
        KeyBinding::new("ctrl-q", Quit, None),
    ]);
}
