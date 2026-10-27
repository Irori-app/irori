//! Cross-feature requests: a feature asks for an action owned by another
//! feature (the library asks the game feature to install), and the shell
//! routes it.

use gpui_kit::{App, Entity, Window};
use irori_game::{GamePage, GamePageEvent};
use irori_library::LibraryEvent;

pub fn library_event(event: &LibraryEvent, window: &mut Window, cx: &mut App) {
    match event {
        LibraryEvent::RequestInstall(id) => irori_game::open_install_sheet(id.clone(), window, cx),
        LibraryEvent::RequestUninstall(id) => {
            irori_game::open_uninstall_dialog(id.clone(), window, cx)
        }
        LibraryEvent::RequestLogs(id) => irori_game::open_log_sheet(id.clone(), window, cx),
        LibraryEvent::RequestImportInstallers => {
            irori_game::open_import_installers_dialog(window, cx)
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
        GamePageEvent::RequestEdit(id) => irori_library::open_edit_game_dialog(id, window, cx),
        GamePageEvent::RequestCategories => {
            let id = page.read(cx).game_id().clone();
            irori_library::open_categories_dialog(Some(&id), window, cx);
        }
    }
}
