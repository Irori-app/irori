//! Epic Online Services overlay on Epic game pages: install, update and
//! remove the overlay (shared by every Epic game), and enable it in this
//! game's Wine prefix.

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, rems,
};
use irori_core::process::CancellationToken;
use irori_core::{
    GameId, GameInfo, InstallPlatform, OpContext, Outcome, Paths, Runner, StatusKind,
};
use irori_epic::{EOS_OVERLAY_APP_NAME, EosOverlayStatus};
use irori_ui::components::ProgressSummary;
use irori_ui::i18n::{t, t_args};
use irori_ui::{backend, models};

use crate::hooks;
use crate::load::Load;
use crate::ops::{notify_error, notify_success};

/// Status-registry id of the overlay's install operation.
pub(crate) fn overlay_id() -> GameId {
    GameId::new(Runner::Legendary, EOS_OVERLAY_APP_NAME)
}

/// Whether the overlay section applies: an installed Epic game that runs
/// through Wine, with the Epic store handle installed.
pub(crate) fn applies(game: &GameInfo, cx: &App) -> bool {
    game.runner() == Runner::Legendary
        && game.is_installed
        && game
            .installed_platform()
            .unwrap_or(InstallPlatform::Windows)
            .needs_wine()
        && hooks::epic_store(cx).is_some()
}

/// What the overlay offers to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverlayAction {
    Install,
    Update { latest: String },
    UpToDate,
}

/// Next action from the installed state and the latest known version.
pub(crate) fn overlay_action(status: &EosOverlayStatus, latest: Option<&str>) -> OverlayAction {
    if !status.installed {
        return OverlayAction::Install;
    }
    match (status.version.as_deref(), latest) {
        (Some(current), Some(latest)) if !latest.is_empty() && current != latest => {
            OverlayAction::Update {
                latest: latest.to_string(),
            }
        }
        (None, Some(latest)) if !latest.is_empty() => OverlayAction::Update {
            latest: latest.to_string(),
        },
        _ => OverlayAction::UpToDate,
    }
}

#[derive(Debug, Clone)]
struct OverlayState {
    status: EosOverlayStatus,
    latest: Option<String>,
    enabled: bool,
}

/// Overlay controls of one Epic game.
pub(crate) struct EosOverlayPanel {
    game: Arc<GameInfo>,
    state: Load<OverlayState>,
    toggling: bool,
    removing: bool,
    cancel: Option<CancellationToken>,
    _subscriptions: Vec<Subscription>,
}

impl EosOverlayPanel {
    pub(crate) fn new(game: Arc<GameInfo>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let statuses = models(cx).statuses.clone();
        let subscriptions = vec![cx.observe(&statuses, |_, _, cx| cx.notify())];
        let mut this = Self {
            game,
            state: Load::Idle,
            toggling: false,
            removing: false,
            cancel: None,
            _subscriptions: subscriptions,
        };
        this.reload(window, cx);
        this
    }

    fn prefix(&self, cx: &App) -> std::path::PathBuf {
        let b = backend(cx);
        irori_runtime::prefix_path(
            &b.settings.app(),
            &b.settings.game(&self.game.id()),
            &self.game,
        )
    }

    fn installing(&self, cx: &App) -> bool {
        self.cancel.is_some() || models(cx).statuses.read(cx).get(&overlay_id()).is_some()
    }

    /// Re-read the installed state, the latest version and this prefix's
    /// enable state.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(epic) = hooks::epic_store(cx) else {
            return;
        };
        if !matches!(self.state, Load::Ready(_)) {
            self.state = Load::Loading;
        }
        let prefix = self.prefix(cx);
        let task = irori_ui::run(cx, async move {
            let status = epic.eos_overlay_status().await;
            let latest = if status.installed {
                epic.eos_overlay_latest_version().await.unwrap_or(None)
            } else {
                None
            };
            let enabled = status.installed && epic.eos_overlay_is_enabled(&prefix).await?;
            Ok(OverlayState {
                status,
                latest,
                enabled,
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.state = Load::from_result(result);
                cx.notify();
            });
        })
        .detach();
    }

    /// Install or update the overlay, with progress in the status registry.
    fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(epic) = hooks::epic_store(cx) else {
            return;
        };
        if self.installing(cx) {
            return;
        }
        let b = backend(cx);
        let cancel = CancellationToken::new();
        self.cancel = Some(cancel.clone());
        cx.notify();
        let task = irori_ui::run(cx, async move {
            if !b.is_online() {
                anyhow::bail!(t("game.overlay.offline"));
            }
            let id = overlay_id();
            let _guard = b.statuses.guard(&id, StatusKind::Installing);
            let statuses = b.statuses.clone();
            let progress_id = id.clone();
            let step_statuses = b.statuses.clone();
            let step_id = id.clone();
            let ctx = OpContext {
                progress: Arc::new(move |p| statuses.progress(&progress_id, p)),
                cancel,
                step: Arc::new(move |s| step_statuses.context(&step_id, s)),
                log_file: (!b.settings.app().disable_logs)
                    .then(|| Paths::get().operation_log_file(&id)),
                settings: None,
            };
            epic.eos_overlay_install(ctx).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.cancel = None;
                match result {
                    Ok(Outcome::Done) => notify_success(window, cx, t("game.overlay.installed")),
                    Ok(Outcome::Abort) => {}
                    Ok(Outcome::Error(e)) => notify_error(
                        window,
                        cx,
                        t("game.overlay.install_failed"),
                        &anyhow::anyhow!(e),
                    ),
                    Err(e) => notify_error(window, cx, t("game.overlay.install_failed"), &e),
                }
                this.reload(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel_install(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.cancel.take() {
            c.cancel();
        }
        cx.notify();
    }

    fn set_enabled(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(epic) = hooks::epic_store(cx) else {
            return;
        };
        self.toggling = true;
        cx.notify();
        let prefix = self.prefix(cx);
        let task = irori_ui::run(cx, async move {
            if on {
                epic.eos_overlay_enable(&prefix).await
            } else {
                epic.eos_overlay_disable(&prefix).await
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.toggling = false;
                if let Err(e) = result {
                    let title = if on {
                        t("game.overlay.enable_failed")
                    } else {
                        t("game.overlay.disable_failed")
                    };
                    notify_error(window, cx, title, &e);
                }
                this.reload(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn request_remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .title(t("game.overlay.remove_title"))
                .description(t("game.overlay.remove_description"))
                .confirm()
                .ok_text(t("game.overlay.remove"))
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |p, cx| p.remove(window, cx));
                    true
                })
        });
    }

    fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(epic) = hooks::epic_store(cx) else {
            return;
        };
        self.removing = true;
        cx.notify();
        let task = irori_ui::run(cx, async move { epic.eos_overlay_remove().await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.removing = false;
                match result {
                    Ok(()) => notify_success(window, cx, t("game.overlay.removed")),
                    Err(e) => notify_error(window, cx, t("game.overlay.remove_failed"), &e),
                }
                this.reload(window, cx);
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for EosOverlayPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let title = div()
            .text_sm()
            .font_semibold()
            .text_color(muted)
            .child(t("game.overlay.title"));
        let mut col = v_flex().gap_2().child(title);
        let installing = self.installing(cx);
        let progress = models(cx).statuses.read(cx).get(&overlay_id()).cloned();
        match &self.state {
            Load::Idle | Load::Loading => {
                return col
                    .child(Skeleton::new().h_4().w(rems(12.)))
                    .into_any_element();
            }
            Load::Failed(e) => {
                return col
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(t_args("game.overlay.read_failed", &[("error", e.as_ref())])),
                    )
                    .child(
                        h_flex().child(
                            Button::new("overlay-retry")
                                .small()
                                .outline()
                                .icon(IconName::RefreshCw)
                                .label(t("game.common.retry"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.reload(window, cx)),
                                ),
                        ),
                    )
                    .into_any_element();
            }
            Load::Ready(_) => {}
        }
        let Some(state) = self.state.ready().cloned() else {
            return col.into_any_element();
        };
        let action = overlay_action(&state.status, state.latest.as_deref());
        let busy = installing || self.toggling || self.removing;
        col = col.child(
            div()
                .text_xs()
                .text_color(muted)
                .child(t("game.overlay.description")),
        );
        if installing {
            col = col
                .when_some(progress, |c, status| {
                    c.child(ProgressSummary::new("overlay-progress", status))
                })
                .child(
                    h_flex().child(
                        Button::new("overlay-cancel")
                            .small()
                            .ghost()
                            .icon(IconName::X)
                            .label(t("game.common.cancel"))
                            .disabled(self.cancel.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_install(cx))),
                    ),
                );
            return col.into_any_element();
        }
        if !state.status.installed {
            return col
                .child(
                    h_flex().child(
                        Button::new("overlay-install")
                            .small()
                            .outline()
                            .icon(IconName::Download)
                            .label(t("game.overlay.install"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.install(window, cx))),
                    ),
                )
                .into_any_element();
        }
        let version = match &state.status.version {
            Some(v) => t_args("game.overlay.version", &[("version", v)]),
            None => t("game.overlay.version_unknown"),
        };
        col = col
            .child(
                Switch::new("overlay-enabled")
                    .label(t("game.overlay.enable"))
                    .checked(state.enabled)
                    .disabled(busy)
                    .on_change(
                        cx.listener(|this, on: &bool, window, cx| {
                            this.set_enabled(*on, window, cx)
                        }),
                    ),
            )
            .child(div().text_xs().text_color(muted).child(version));
        let mut buttons = h_flex().gap_2().flex_wrap();
        if let OverlayAction::Update { latest } = action {
            buttons = buttons.child(
                Button::new("overlay-update")
                    .small()
                    .outline()
                    .icon(IconName::CloudDownload)
                    .label(t_args("game.overlay.update_to", &[("version", &latest)]))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.install(window, cx))),
            );
        }
        buttons = buttons.child(
            Button::new("overlay-remove")
                .small()
                .ghost()
                .icon(IconName::Trash)
                .label(t("game.overlay.remove_ellipsis"))
                .loading(self.removing)
                .disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| this.request_remove(window, cx))),
        );
        col.child(buttons).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(installed: bool, version: Option<&str>) -> EosOverlayStatus {
        EosOverlayStatus {
            installed,
            version: version.map(String::from),
            install_path: None,
        }
    }

    #[test]
    fn next_overlay_action() {
        assert_eq!(
            overlay_action(&status(false, None), Some("1.2")),
            OverlayAction::Install
        );
        assert_eq!(
            overlay_action(&status(true, Some("1.1")), Some("1.2")),
            OverlayAction::Update {
                latest: "1.2".into()
            }
        );
        assert_eq!(
            overlay_action(&status(true, Some("1.2")), Some("1.2")),
            OverlayAction::UpToDate
        );
        assert_eq!(
            overlay_action(&status(true, Some("1.2")), None),
            OverlayAction::UpToDate
        );
        assert_eq!(
            overlay_action(&status(true, None), Some("")),
            OverlayAction::UpToDate
        );
    }
}
