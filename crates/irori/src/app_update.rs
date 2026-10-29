//! Notices about Irori's own versions: a newer release is out, or Irori was
//! just updated. Each shows once per session; closing the first dismisses
//! that version for good (Settings › About still shows it).

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, ClipboardItem, Window};
use irori_services::app_update::release_page;
use irori_ui::i18n::{t, t_args};
use irori_ui::models::models;

/// Which notices this session already showed.
#[derive(Default)]
pub(crate) struct Announced {
    available: Option<String>,
    updated: bool,
}

struct AvailableNote;
struct UpdatedNote;

impl Announced {
    pub(crate) fn show(&mut self, window: &mut Window, cx: &mut App) {
        let status = models(cx).app_update.read(cx).status.clone();
        if status.updated && !self.updated {
            self.updated = true;
            let url = release_page(&status.current);
            let note = Notification::success(t_args(
                "shell.app_update.updated",
                &[("version", &status.current)],
            ))
            .id::<UpdatedNote>()
            .action(move |_, _, cx| {
                let url = url.clone();
                Button::new("app-update-notes")
                    .outline()
                    .label(t("shell.app_update.release_notes"))
                    .on_click(cx.listener(move |note, _, window, cx| {
                        cx.open_url(&url);
                        note.dismiss(window, cx);
                    }))
            })
            .autohide(true);
            window.push_notification(note, cx);
        }

        let Some(release) = status.available.filter(|_| status.announce) else {
            return;
        };
        if self.available.as_deref() == Some(release.version.as_str()) {
            return;
        }
        self.available = Some(release.version.clone());
        let title = t_args(
            "shell.app_update.available",
            &[("version", &release.version)],
        );
        let command = status.kind.update_command();
        let message = match command {
            Some(command) => t_args(
                "shell.app_update.available_command",
                &[("current", &status.current), ("command", command)],
            ),
            None => t_args(
                "shell.app_update.available_message",
                &[("current", &status.current)],
            ),
        };
        let url = release.url.clone();
        let version = release.version.clone();
        let note = Notification::info(message.clone())
            .title(title)
            .message(message)
            .id1::<AvailableNote>(release.version.clone())
            .action(move |_, _, cx| {
                let url = url.clone();
                let button = Button::new("app-update-get").primary();
                match command {
                    Some(command) => {
                        button
                            .label(t("shell.app_update.copy_command"))
                            .on_click(cx.listener(move |note, _, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(command.into()));
                                note.dismiss(window, cx);
                            }))
                    }
                    None => button
                        .label(t("shell.app_update.download"))
                        .on_click(cx.listener(move |note, _, window, cx| {
                            cx.open_url(&url);
                            note.dismiss(window, cx);
                        })),
                }
            })
            // Closed by hand or through its button: this version is handled.
            .on_close(move |_, cx| {
                let b = irori_ui::backend(cx);
                let version = version.clone();
                irori_ui::state::run_infallible(cx, async move { b.dismiss_app_update(&version) })
                    .detach();
            });
        window.push_notification(note, cx);
    }
}
