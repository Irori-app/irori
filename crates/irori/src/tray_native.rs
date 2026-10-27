//! Native macOS menu-bar and Windows notification-area menus. All native
//! objects stay on GPUI's main thread; callbacks only send shell commands.
use crate::ShellCommand;
use futures::channel::mpsc::UnboundedSender;
use irori_core::GameId;
use irori_ui::i18n::t;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

thread_local! {
    static ICON: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
}
#[derive(Clone)]
pub struct TrayHandle {
    actions: Arc<Mutex<HashMap<String, ShellCommand>>>,
}

fn menu(handle: &TrayHandle, recent: Vec<(GameId, String)>) -> anyhow::Result<Menu> {
    let menu = Menu::new();
    let mut actions = HashMap::new();
    for (id, label, command) in [
        ("show", t("shell.tray.show"), ShellCommand::Show),
        (
            "big-picture",
            t("shell.nav.big_picture"),
            ShellCommand::BigPicture,
        ),
    ] {
        menu.append(&MenuItem::with_id(id, label.as_ref(), true, None))?;
        actions.insert(id.into(), command);
    }
    if !recent.is_empty() {
        menu.append(&PredefinedMenuItem::separator())?;
    }
    for (game, title) in recent {
        let id = format!("recent:{}", game.key());
        menu.append(&MenuItem::with_id(id.clone(), title, true, None))?;
        actions.insert(id, ShellCommand::Launch(game));
    }
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id(
        "quit",
        t("shell.tray.quit").as_ref(),
        true,
        None,
    ))?;
    actions.insert("quit".into(), ShellCommand::Quit);
    *handle
        .actions
        .lock()
        .map_err(|_| anyhow::anyhow!("Tray menu is unavailable"))? = actions;
    Ok(menu)
}

fn build(tx: UnboundedSender<ShellCommand>) -> anyhow::Result<TrayHandle> {
    let handle = TrayHandle {
        actions: Arc::new(Mutex::new(HashMap::new())),
    };
    let actions = handle.actions.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let command = actions
            .lock()
            .ok()
            .and_then(|m| m.get(event.id.as_ref()).cloned());
        if let Some(command) = command {
            let _ = tx.unbounded_send(command);
        }
    }));
    let tree = resvg::usvg::Tree::from_data(
        include_bytes!("../../../packaging/irori.svg"),
        &Default::default(),
    )?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(32, 32)
        .ok_or_else(|| anyhow::anyhow!("Could not allocate the tray icon"))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(
            32.0 / tree.size().width(),
            32.0 / tree.size().height(),
        ),
        &mut pixmap.as_mut(),
    );
    let icon = Icon::from_rgba(pixmap.data().to_vec(), 32, 32)?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("Irori")
        .with_icon(icon)
        .with_menu(Box::new(menu(&handle, Vec::new())?))
        .build()?;
    ICON.with(|slot| *slot.borrow_mut() = Some(tray));
    Ok(handle)
}

pub fn start(tx: UnboundedSender<ShellCommand>, _dark_icon: bool) -> Option<TrayHandle> {
    match build(tx) {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::warn!("system tray unavailable: {e:#}");
            None
        }
    }
}

pub fn set_recent(handle: &TrayHandle, recent: Vec<(GameId, String)>) {
    match menu(handle, recent) {
        Ok(menu) => ICON.with(|slot| {
            if let Some(icon) = slot.borrow().as_ref() {
                icon.set_menu(Some(Box::new(menu)));
            }
        }),
        Err(e) => tracing::warn!("updating tray: {e:#}"),
    }
}
