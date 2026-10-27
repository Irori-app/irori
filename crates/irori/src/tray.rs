//! System tray (StatusNotifierItem) with quick access to recent games.

use futures::channel::mpsc::UnboundedSender;
use irori_core::GameId;
use irori_ui::i18n::t;
use ksni::TrayMethods as _;

use crate::ShellCommand;

pub struct IroriTray {
    tx: UnboundedSender<ShellCommand>,
    recent: Vec<(GameId, String)>,
    dark_icon: bool,
}

/// Handle to update the tray menu.
pub type TrayHandle = ksni::Handle<IroriTray>;

impl ksni::Tray for IroriTray {
    fn id(&self) -> String {
        "irori".into()
    }

    fn title(&self) -> String {
        "Irori".into()
    }

    fn icon_name(&self) -> String {
        if self.dark_icon {
            "applications-games-symbolic".into()
        } else {
            "applications-games".into()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.unbounded_send(ShellCommand::Show);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{StandardItem, SubMenu};
        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            StandardItem {
                label: t("shell.tray.show").to_string(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.unbounded_send(ShellCommand::Show);
                }),
                ..Default::default()
            }
            .into(),
        ];
        if !self.recent.is_empty() {
            items.push(
                SubMenu {
                    label: t("shell.tray.recent").to_string(),
                    submenu: self
                        .recent
                        .iter()
                        .map(|(id, title)| {
                            let id = id.clone();
                            StandardItem {
                                label: title.clone(),
                                activate: Box::new(move |t: &mut Self| {
                                    let _ = t.tx.unbounded_send(ShellCommand::Launch(id.clone()));
                                }),
                                ..Default::default()
                            }
                            .into()
                        })
                        .collect(),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.extend([
            StandardItem {
                label: t("shell.nav.big_picture").to_string(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.unbounded_send(ShellCommand::BigPicture);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: t("shell.tray.quit").to_string(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.unbounded_send(ShellCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

/// Start the tray on the backend runtime. Returns `None` when no tray host is
/// available (the app then quits on window close regardless of settings).
pub async fn start(tx: UnboundedSender<ShellCommand>, dark_icon: bool) -> Option<TrayHandle> {
    let tray = IroriTray {
        tx,
        recent: Vec::new(),
        dark_icon,
    };
    match tray.spawn().await {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::info!("system tray unavailable: {e}");
            None
        }
    }
}

/// Replace the recent games shown in the tray menu.
pub async fn set_recent(handle: &TrayHandle, recent: Vec<(GameId, String)>) {
    handle.update(move |t| t.recent = recent).await;
}
