//! The "Epic Games" group of Advanced: EOS overlay management and Epic
//! Games Launcher (EGL) sync, driven through `irori_epic::EpicStore`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, PathPromptOptions, Render,
    SharedString, Styled as _, Subscription, Task, Window, div,
};
use irori_core::process::CancellationToken;
use irori_core::{OpContext, Outcome};
use irori_epic::{EosOverlayStatus, EpicStore};
use irori_ui::i18n::{t, t_args};
use irori_ui::{backend, models};

use crate::i18n::LanguageStamp;
use crate::widgets::muted;

/// A running overlay operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayOp {
    Install,
    Update,
    Remove,
}

impl OverlayOp {
    /// Progress line, with the percent when known.
    fn progress_label(self, percent: Option<f32>) -> SharedString {
        let (plain, with_percent) = match self {
            OverlayOp::Install => (
                "settings.eos.progress.installing",
                "settings.eos.progress.installing_percent",
            ),
            OverlayOp::Update => (
                "settings.eos.progress.updating",
                "settings.eos.progress.updating_percent",
            ),
            OverlayOp::Remove => (
                "settings.eos.progress.removing",
                "settings.eos.progress.removing_percent",
            ),
        };
        match percent {
            Some(p) => t_args(with_percent, &[("percent", &format!("{p:.0}"))]),
            None => t(plain),
        }
    }

    fn done_label(self) -> SharedString {
        t(match self {
            OverlayOp::Install => "settings.eos.done.installed",
            OverlayOp::Update => "settings.eos.done.updated",
            OverlayOp::Remove => "settings.eos.done.removed",
        })
    }
}

/// Whether `latest` is a different version than the installed `current`.
/// Unknown versions never count as an update.
pub(crate) fn overlay_update_available(current: Option<&str>, latest: Option<&str>) -> bool {
    match (current, latest) {
        (Some(c), Some(l)) => !c.trim().is_empty() && !l.trim().is_empty() && c.trim() != l.trim(),
        _ => false,
    }
}

/// Status line of the overlay.
pub(crate) fn overlay_status_text(status: &EosOverlayStatus, latest: Option<&str>) -> SharedString {
    if !status.installed {
        return t("settings.eos.status.not_installed");
    }
    let version = status.version.as_deref().filter(|v| !v.is_empty());
    let update = latest.filter(|_| overlay_update_available(status.version.as_deref(), latest));
    match (version, update) {
        (Some(v), Some(l)) => t_args(
            "settings.eos.status.update_available",
            &[("version", v), ("latest", l)],
        ),
        (Some(v), None) => t_args("settings.eos.status.installed_version", &[("version", v)]),
        _ => t("settings.eos.status.installed"),
    }
}

/// Progress percent shared with the backend's progress callback; `NaN`
/// means unknown.
#[derive(Clone)]
struct SharedPercent(Arc<AtomicU32>);

impl SharedPercent {
    fn new() -> Self {
        Self(Arc::new(AtomicU32::new(f32::NAN.to_bits())))
    }

    fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }

    fn get(&self) -> Option<f32> {
        let v = f32::from_bits(self.0.load(Ordering::Relaxed));
        (!v.is_nan()).then_some(v)
    }
}

/// EOS overlay status with install, update, check and remove actions.
pub(crate) struct EosOverlayPanel {
    epic: Arc<EpicStore>,
    /// `None` while the first status read runs.
    status: Option<EosOverlayStatus>,
    latest: Option<String>,
    checking: bool,
    op: Option<OverlayOp>,
    percent: SharedPercent,
    cancel: Option<CancellationToken>,
    _ticker: Option<Task<()>>,
}

impl EosOverlayPanel {
    pub(crate) fn new(epic: Arc<EpicStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            epic,
            status: None,
            latest: None,
            checking: false,
            op: None,
            percent: SharedPercent::new(),
            cancel: None,
            _ticker: None,
        };
        this.load(false, window, cx);
        this
    }

    /// Read the installed state and the latest known version; with
    /// `check`, ask legendary to refresh the latest version first.
    fn load(&mut self, check: bool, window: &mut Window, cx: &mut Context<Self>) {
        if check {
            self.checking = true;
        }
        let epic = self.epic.clone();
        let task = irori_ui::run(cx, async move {
            if check {
                epic.eos_overlay_refresh_info().await?;
            }
            let status = epic.eos_overlay_status().await;
            let latest = if status.installed {
                epic.eos_overlay_latest_version().await.unwrap_or_else(|e| {
                    tracing::warn!("EOS overlay version check: {e:#}");
                    None
                })
            } else {
                None
            };
            Ok((status, latest))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.checking = false;
                match result {
                    Ok((status, latest)) => {
                        this.status = Some(status);
                        this.latest = latest;
                    }
                    Err(e) => {
                        this.status.get_or_insert_with(Default::default);
                        window.push_notification(
                            Notification::error(format!("{e:#}"))
                                .title(t("settings.eos.error.check")),
                            cx,
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn start(&mut self, op: OverlayOp, window: &mut Window, cx: &mut Context<Self>) {
        if self.op.is_some() {
            return;
        }
        self.op = Some(op);
        self.percent = SharedPercent::new();
        let token = CancellationToken::new();
        self.cancel = Some(token.clone());
        let percent = self.percent.clone();
        let ctx = OpContext {
            progress: Arc::new(move |p| {
                if let Some(v) = p.percent {
                    percent.set(v);
                }
            }),
            cancel: token,
            ..OpContext::detached()
        };
        let epic = self.epic.clone();
        let task = irori_ui::run(cx, async move {
            match op {
                OverlayOp::Install => epic.eos_overlay_install(ctx).await,
                OverlayOp::Update => epic.eos_overlay_update(ctx).await,
                OverlayOp::Remove => epic.eos_overlay_remove().await.map(|()| Outcome::Done),
            }
        });
        // Repaint while the operation runs so the progress bar moves.
        self._ticker = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.op.is_some()
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        }));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.op = None;
                this.cancel = None;
                this._ticker = None;
                match result {
                    Ok(Outcome::Done) => {
                        window.push_notification(Notification::success(op.done_label()), cx)
                    }
                    Ok(Outcome::Abort) => {}
                    Ok(Outcome::Error(e)) => window.push_notification(
                        Notification::error(e).title(t("settings.eos.error.operation")),
                        cx,
                    ),
                    Err(e) => window.push_notification(
                        Notification::error(format!("{e:#}"))
                            .title(t("settings.eos.error.operation")),
                        cx,
                    ),
                }
                this.load(false, window, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn confirm_remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let weak = weak.clone();
            alert
                .title(t("settings.eos.remove.title"))
                .description(t("settings.eos.remove.description"))
                .show_cancel(true)
                .cancel_text(t("settings.common.cancel"))
                .ok_text(t("settings.common.remove"))
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    weak.update(cx, |this, cx| this.start(OverlayOp::Remove, window, cx))
                        .ok();
                    true
                })
        });
    }
}

impl Render for EosOverlayPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(status) = &self.status else {
            return v_flex().child(muted(t("settings.eos.status.checking"), cx));
        };
        if let Some(op) = self.op {
            let percent = self.percent.get();
            let label = op.progress_label(percent);
            return v_flex().gap_2().w_full().child(
                h_flex()
                    .gap_3()
                    .child(
                        Progress::new("eos-overlay-progress")
                            .accessibility_label(t("settings.eos.progress.label"))
                            .loading(percent.is_none())
                            .value(percent.unwrap_or(0.))
                            .flex_1(),
                    )
                    .child(div().text_sm().flex_shrink_0().child(label))
                    .when(op != OverlayOp::Remove, |r| {
                        r.child(
                            Button::new("eos-overlay-cancel")
                                .ghost()
                                .small()
                                .label(t("settings.common.cancel"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(t) = &this.cancel {
                                        t.cancel();
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }
        let update = overlay_update_available(status.version.as_deref(), self.latest.as_deref());
        let installed = status.installed;
        let actions = if installed {
            h_flex()
                .gap_2()
                .when(update, |r| {
                    r.child(
                        Button::new("eos-overlay-update")
                            .small()
                            .label(t("settings.eos.update"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start(OverlayOp::Update, window, cx)
                            })),
                    )
                })
                .child(
                    Button::new("eos-overlay-check")
                        .small()
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label(t("settings.eos.check_updates"))
                        .loading(self.checking)
                        .disabled(self.checking)
                        .on_click(cx.listener(|this, _, window, cx| this.load(true, window, cx))),
                )
                .child(
                    Button::new("eos-overlay-remove")
                        .small()
                        .ghost()
                        .danger()
                        .label(t("settings.eos.remove.action"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.confirm_remove(window, cx)),
                        ),
                )
        } else {
            h_flex().gap_2().child(
                Button::new("eos-overlay-install")
                    .small()
                    .icon(IconName::Download)
                    .label(t("settings.common.install"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.start(OverlayOp::Install, window, cx)
                    })),
            )
        };
        v_flex()
            .gap_2()
            .w_full()
            .child(
                div()
                    .text_sm()
                    .child(overlay_status_text(status, self.latest.as_deref())),
            )
            .child(actions)
    }
}

fn egl_path_hint() -> SharedString {
    if cfg!(windows) {
        r"C:\ProgramData\Epic\EpicGamesLauncher\Data".into()
    } else {
        t("settings.egl.prefix_placeholder")
    }
}

/// Validate the prefix typed for EGL sync: the expanded path, or why it
/// can't be used.
pub(crate) fn egl_prefix_from_text(text: &str) -> Result<PathBuf, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(t("settings.egl.prefix_required").into());
    }
    Ok(irori_core::paths::expand_home(text))
}

/// Link legendary with an Epic Games Launcher in a Wine prefix, or unlink.
pub(crate) struct EglSyncPanel {
    epic: Arc<EpicStore>,
    input: Entity<InputState>,
    busy: bool,
    error: Option<SharedString>,
    language: LanguageStamp,
    _subscriptions: Vec<Subscription>,
}

impl EglSyncPanel {
    pub(crate) fn new(epic: Arc<EpicStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let linked = backend(cx).settings.app().egs_linked_path.clone();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(egl_path_hint())
                .default_value(linked)
        });
        let settings = models(cx).settings.clone();
        let subscriptions =
            vec![
                cx.observe(&settings, |_, _, cx| cx.notify()),
                cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::PressEnter { .. } => this.sync(true, window, cx),
                        InputEvent::Change if this.error.take().is_some() => cx.notify(),
                        _ => {}
                    }
                }),
            ];
        Self {
            epic,
            input,
            busy: false,
            error: None,
            language: LanguageStamp::new(),
            _subscriptions: subscriptions,
        }
    }

    fn pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t("settings.egl.select_prefix")),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next()
            {
                this.update_in(cx, |this, window, cx| {
                    this.input.update(cx, |s, cx| {
                        s.set_value(p.to_string_lossy().to_string(), window, cx)
                    });
                    this.error = None;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Link (`link == true`) with the typed prefix, or unlink.
    fn sync(&mut self, link: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let prefix = if link {
            match egl_prefix_from_text(&self.input.read(cx).value()) {
                Ok(p) => Some(p),
                Err(e) => {
                    self.error = Some(e.into());
                    cx.notify();
                    return;
                }
            }
        } else {
            None
        };
        self.busy = true;
        self.error = None;
        let epic = self.epic.clone();
        let target = prefix.clone();
        let not_folder = prefix.as_ref().map(|p| {
            t_args(
                "settings.egl.not_a_folder",
                &[("path", &p.display().to_string())],
            )
        });
        let task = irori_ui::run(cx, async move {
            if let Some(p) = &target {
                let is_dir = tokio::fs::metadata(p).await.is_ok_and(|m| m.is_dir());
                if !is_dir {
                    anyhow::bail!("{}", not_folder.unwrap_or_default());
                }
            }
            epic.egl_sync(target.as_deref()).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(_) => {
                        let stored = prefix
                            .as_ref()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        backend(cx)
                            .settings
                            .update_app(|s| s.egs_linked_path = stored.clone());
                        if prefix.is_none() {
                            this.input.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                        window.push_notification(
                            Notification::success(if prefix.is_some() {
                                t("settings.egl.synced")
                            } else {
                                t("settings.egl.unlinked")
                            }),
                            cx,
                        );
                        models::refresh_library(true, cx);
                    }
                    Err(e) => {
                        // The previous link stays in place (Heroic clears it on failure).
                        this.error = Some(format!("{e:#}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for EglSyncPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language.changed() {
            cx.defer_in(window, |this, window, cx| {
                let placeholder = egl_path_hint();
                this.input
                    .update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
            });
        }
        let linked = backend(cx).settings.app().egs_linked_path.clone();
        let danger = cx.theme().danger;
        let busy = self.busy;
        v_flex()
            .gap_2()
            .w_full()
            .child(muted(
                if linked.is_empty() {
                    t("settings.egl.not_linked")
                } else {
                    t_args("settings.egl.linked", &[("path", &linked)])
                },
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Input::new(&self.input)
                            .id("egl-sync-prefix")
                            .aria_label(t("settings.egl.prefix_label"))
                            .disabled(busy)
                            .flex_1()
                            .suffix(
                                Button::new("egl-sync-pick")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::FolderOpen)
                                    .tooltip(t("settings.picker.choose_folder"))
                                    .disabled(busy)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.pick(window, cx)),
                                    ),
                            ),
                    )
                    .child(
                        Button::new("egl-sync-link")
                            .label(if linked.is_empty() {
                                t("settings.egl.sync")
                            } else {
                                t("settings.egl.sync_again")
                            })
                            .loading(busy)
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.sync(true, window, cx)),
                            ),
                    )
                    .when(!linked.is_empty(), |r| {
                        r.child(
                            Button::new("egl-sync-unlink")
                                .ghost()
                                .label(t("settings.egl.unlink"))
                                .disabled(busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.sync(false, window, cx)),
                                ),
                        )
                    }),
            )
            .when_some(self.error.clone(), |v, e| {
                v.child(div().text_xs().text_color(danger).child(e))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_updates_need_two_known_versions() {
        assert!(overlay_update_available(Some("1.0"), Some("1.1")));
        assert!(!overlay_update_available(Some("1.1"), Some(" 1.1 ")));
        assert!(!overlay_update_available(None, Some("1.1")));
        assert!(!overlay_update_available(Some("1.0"), None));
        assert!(!overlay_update_available(Some(""), Some("1.1")));
    }

    #[test]
    fn overlay_status_lines() {
        crate::i18n::register();
        let missing = EosOverlayStatus::default();
        assert_eq!(overlay_status_text(&missing, None), "Not installed");
        let installed = EosOverlayStatus {
            installed: true,
            version: Some("1.0".into()),
            install_path: None,
        };
        assert_eq!(
            overlay_status_text(&installed, Some("1.0")),
            "Installed · version 1.0"
        );
        assert_eq!(
            overlay_status_text(&installed, Some("1.2")),
            "Installed · version 1.0 · version 1.2 is available"
        );
    }

    #[test]
    fn egl_prefix_needs_a_path() {
        assert!(egl_prefix_from_text("  ").is_err());
        assert_eq!(
            egl_prefix_from_text(" /games/egl ").unwrap(),
            PathBuf::from("/games/egl")
        );
    }
}
