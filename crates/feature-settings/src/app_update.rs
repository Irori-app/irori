//! Settings → About: whether a newer Irori is out, who updates this install,
//! and "Check now".

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, h_flex};
use gpui_kit::{
    ClipboardItem, Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, div,
};
use irori_services::{AppUpdateStatus, InstallKind};
use irori_ui::i18n::{t, t_args};
use irori_ui::models::models;

use crate::widgets::panel;

pub struct AppUpdatePanel {
    /// Why the last "Check now" failed.
    error: Option<String>,
    copied: bool,
    _subscriptions: Vec<Subscription>,
}

impl AppUpdatePanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = models(cx).app_update.clone();
        Self {
            error: None,
            copied: false,
            _subscriptions: vec![cx.observe(&model, |_, _, cx| cx.notify())],
        }
    }

    fn check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        let b = irori_ui::backend(cx);
        let task = irori_ui::run(cx, async move { b.check_app_update().await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result.err().map(|e| format!("{e:#}"));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn copy(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(command.to_string()));
        self.copied = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                this.copied = false;
                cx.notify();
            });
        })
        .detach();
    }
}

/// The sentence about this install's updates.
fn status_text(status: &AppUpdateStatus, error: Option<&str>) -> SharedString {
    match &status.kind {
        InstallKind::Flatpak => return t("settings.about.update.flatpak"),
        InstallKind::SystemPackage | InstallKind::Nix => {
            return t("settings.about.update.system");
        }
        InstallKind::Packaged(text) => return text.clone().into(),
        InstallKind::Source => return t("settings.about.update.source"),
        _ => {}
    }
    if status.checking {
        return t("settings.about.update.checking");
    }
    if let Some(error) = error {
        return t_args("settings.about.update.failed", &[("error", error)]);
    }
    match (&status.available, status.kind.update_command()) {
        (Some(r), Some(command)) => t_args(
            "settings.about.update.available_command",
            &[("version", &r.version), ("command", command)],
        ),
        (Some(r), None) => t_args(
            "settings.about.update.available",
            &[("version", &r.version)],
        ),
        (None, _) if status.checked_at.is_some() => t("settings.about.update.up_to_date"),
        (None, _) => t("settings.about.update.not_checked"),
    }
}

impl Render for AppUpdatePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = models(cx).app_update.read(cx).status.clone();
        let text = status_text(&status, self.error.as_deref());
        let mut actions = h_flex().gap_2().flex_shrink_0();
        if let Some(release) = &status.available
            && status.kind.checks()
        {
            actions = match status.kind.update_command() {
                Some(command) => actions.child(
                    Button::new("app-update-copy")
                        .primary()
                        .icon(if self.copied {
                            IconName::Check
                        } else {
                            IconName::Copy
                        })
                        .label(if self.copied {
                            t("settings.common.copied")
                        } else {
                            t("settings.about.update.copy_command")
                        })
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.copy(command, window, cx)),
                        ),
                ),
                None => {
                    let url = release.url.clone();
                    actions.child(
                        Button::new("app-update-download")
                            .primary()
                            .icon(IconName::ExternalLink)
                            .label(t("settings.about.update.download"))
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                }
            };
        }
        if status.kind.checks() {
            actions = actions.child(
                Button::new("app-update-check")
                    .outline()
                    .icon(IconName::RefreshCw)
                    .label(t("settings.about.update.check_now"))
                    .loading(status.checking)
                    .disabled(status.checking)
                    .on_click(cx.listener(|this, _, window, cx| this.check(window, cx))),
            );
        }
        panel(cx).child(
            h_flex()
                .flex_wrap()
                .gap_3()
                .px_4()
                .py_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(if self.error.is_some() {
                            cx.theme().danger
                        } else {
                            cx.theme().foreground
                        })
                        .child(text),
                )
                .child(actions),
        )
    }
}
