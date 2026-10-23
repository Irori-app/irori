//! Cross-feature requests: a feature asks for an action owned by another
//! feature (the library asks the game feature to install), and the shell
//! routes it.

use ember_game::{GamePage, GamePageEvent};
use ember_library::LibraryEvent;
use gpui_kit::{App, Entity, Window};

pub fn library_event(event: &LibraryEvent, window: &mut Window, cx: &mut App) {
    match event {
        LibraryEvent::RequestInstall(id) => ember_game::open_install_sheet(id.clone(), window, cx),
        LibraryEvent::RequestUninstall(id) => {
            ember_game::open_uninstall_dialog(id.clone(), window, cx)
        }
        LibraryEvent::RequestLogs(id) => ember_game::open_log_sheet(id.clone(), window, cx),
        LibraryEvent::RequestImportInstallers => {
            ember_game::open_import_installers_dialog(window, cx)
        }
    }
}

pub fn game_event(
    page: &Entity<GamePage>,
    event: &GamePageEvent,
    window: &mut Window,
    cx: &mut App,
) {
    match event {
        GamePageEvent::TitleChanged(_) => {}
        GamePageEvent::RequestEdit(id) => ember_library::open_edit_game_dialog(id, window, cx),
        GamePageEvent::RequestCategories => {
            let id = page.read(cx).game_id().clone();
            ember_library::open_categories_dialog(Some(&id), window, cx);
        }
    }
}
