//! Import, uninstall and move dialogs.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div,
};
use irori_core::{GameId, GameInfo, InstallPlatform, RemoveArgs, Runner};
use irori_ui::i18n::{t, t_args};
use irori_ui::{Route, backend, models, navigate};

use crate::i18n::{platform_label, status_label};
use crate::install::installable_platforms;
use crate::ops::{notify_error, notify_success, pick_path, run_reporting};

/// Form of the import dialog: an existing install folder and its platform.
pub struct ImportForm {
    game: Arc<GameInfo>,
    path: Entity<InputState>,
    platforms: Vec<InstallPlatform>,
    platform: InstallPlatform,
    submitting: bool,
}

/// Register an existing installation of a game (files already on disk).
pub fn open_import_dialog(id: GameId, window: &mut Window, cx: &mut App) {
    let Some(game) = backend(cx).game(&id) else {
        return;
    };
    let title = t_args("game.import.title", &[("title", &game.title)]);
    let form = cx.new(|cx| ImportForm::new(game, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.title(title.clone()).child(form.clone())
    });
}

impl ImportForm {
    fn new(game: Arc<GameInfo>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let platforms = installable_platforms(&game);
        let path =
            cx.new(|cx| InputState::new(window, cx).placeholder(t("game.import.path_placeholder")));
        Self {
            platform: platforms.first().copied().unwrap_or_default(),
            platforms,
            game,
            path,
            submitting: false,
        }
    }

    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = pick_path(window, cx, true, t("game.common.select"));
        cx.spawn_in(window, async move |this, cx| {
            if let Some(path) = pick.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    let value = path.to_string_lossy().into_owned();
                    this.path.update(cx, |s, cx| s.set_value(value, window, cx));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.path.read(cx).value();
        let path = irori_core::paths::expand_home(raw.trim());
        if raw.trim().is_empty() {
            return;
        }
        let b = backend(cx);
        let id = self.game.id();
        let platform = self.platform;
        let title = self.game.title.clone();
        self.submitting = true;
        cx.notify();
        let task = run_reporting(
            window,
            cx,
            t_args("game.import.failed", &[("title", &title)]),
            async move {
                if !tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_dir()) {
                    anyhow::bail!(t_args(
                        "game.import.not_a_folder",
                        &[("path", &path.display().to_string())]
                    ));
                }
                b.import(&id, &path, platform).await
            },
            move |_, window, cx| {
                window.close_dialog(cx);
                notify_success(window, cx, t_args("game.import.done", &[("title", &title)]));
            },
        );
        cx.spawn(async move |this, cx| {
            task.await;
            let _ = this.update(cx, |this, cx| {
                this.submitting = false;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ImportForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.path.read(cx).value().trim().is_empty();
        let platforms = self.platforms.clone();
        v_flex()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t("game.import.description")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.path)))
                    .child(
                        Button::new("import-browse")
                            .outline()
                            .icon(IconName::FolderOpen)
                            .label(t("game.common.browse"))
                            .on_click(cx.listener(|this, _, window, cx| this.browse(window, cx))),
                    ),
            )
            .when(self.platforms.len() > 1, |col| {
                col.child(
                    RadioGroup::horizontal("import-platform")
                        .children(platforms.iter().map(|p| platform_label(*p)))
                        .selected_index(platforms.iter().position(|p| *p == self.platform))
                        .on_change(cx.listener(move |this, ix: &usize, _, cx| {
                            if let Some(p) = platforms.get(*ix) {
                                this.platform = *p;
                                cx.notify();
                            }
                        })),
                )
            })
            .child(
                DialogFooter::new()
                    .child(
                        Button::new("import-cancel")
                            .outline()
                            .label(t("game.common.cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("import-submit")
                            .primary()
                            .label(t("game.import.submit"))
                            .loading(self.submitting)
                            .disabled(empty || self.submitting)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

/// Choices of an uninstall, owned by the open dialog.
pub struct UninstallOptions {
    is_dlc: bool,
    uses_wine: bool,
    prefix: Option<PathBuf>,
    prefix_is_shared: bool,
    remove_prefix: bool,
    delete_files: bool,
    remove_settings: bool,
}

impl UninstallOptions {
    /// Arguments for `Backend::uninstall`.
    pub fn args(&self) -> RemoveArgs {
        RemoveArgs {
            remove_prefix: self.remove_prefix && self.uses_wine && !self.prefix_is_shared,
            delete_files: self.delete_files,
            remove_settings: self.remove_settings && !self.is_dlc,
        }
    }
}

impl Render for UninstallOptions {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let option = |check: Checkbox, help: SharedString| {
            v_flex()
                .gap_1()
                .child(check)
                .child(div().pl_6().text_xs().text_color(muted).child(help))
        };
        v_flex()
            .gap_3()
            .child(option(
                Checkbox::new("uninstall-delete-files")
                    .label(t("game.uninstall.delete_files"))
                    .checked(self.delete_files)
                    .on_change(cx.listener(|this, v: &bool, _, cx| {
                        this.delete_files = *v;
                        cx.notify();
                    })),
                t("game.uninstall.delete_files_help"),
            ))
            .when(self.uses_wine && !self.is_dlc, |col| {
                let path = self
                    .prefix
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let help = if self.prefix_is_shared {
                    t_args("game.uninstall.prefix_shared_help", &[("path", &path)])
                } else {
                    t_args("game.uninstall.prefix_help", &[("path", &path)])
                };
                col.child(option(
                    Checkbox::new("uninstall-remove-prefix")
                        .label(t("game.uninstall.delete_prefix"))
                        .checked(self.remove_prefix && !self.prefix_is_shared)
                        .disabled(self.prefix_is_shared)
                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                            this.remove_prefix = *v;
                            cx.notify();
                        })),
                    help,
                ))
            })
            .when(!self.is_dlc, |col| {
                col.child(option(
                    Checkbox::new("uninstall-remove-settings")
                        .label(t("game.uninstall.remove_settings"))
                        .checked(self.remove_settings)
                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                            this.remove_settings = *v;
                            cx.notify();
                        })),
                    t("game.uninstall.remove_settings_help"),
                ))
            })
    }
}

/// Confirm and uninstall a game (or DLC). Refuses while the game runs or
/// another operation is in progress.
pub fn open_uninstall_dialog(id: GameId, window: &mut Window, cx: &mut App) {
    let b = backend(cx);
    let Some(game) = b.game(&id) else { return };
    let title = game.title.clone();
    let status = models(cx).statuses.read(cx).kind(&id);
    if let Some(kind) = status.filter(|k| k.is_busy()) {
        let text = if kind == irori_core::StatusKind::Playing {
            t_args("game.uninstall.running", &[("title", &title)])
        } else {
            t_args(
                "game.uninstall.busy",
                &[("title", &title), ("status", &status_label(kind))],
            )
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            alert
                .title(t("game.uninstall.blocked_title"))
                .description(text.clone())
        });
        return;
    }
    let app = b.settings.app();
    let settings = b.settings.game(&id);
    let uses_wine = game
        .installed_platform()
        .unwrap_or(InstallPlatform::Windows)
        .needs_wine();
    let prefix = uses_wine.then(|| irori_runtime::prefix_path(&app, &settings, &game));
    let prefix_is_shared = prefix
        .as_ref()
        .is_some_and(|p| irori_runtime::prefix_is_shared(&app, p));
    let is_dlc = game.is_dlc;
    let options = cx.new(|_| UninstallOptions {
        is_dlc,
        uses_wine,
        prefix,
        prefix_is_shared,
        remove_prefix: false,
        delete_files: true,
        remove_settings: false,
    });
    let heading = t_args("game.uninstall.title", &[("title", &title)]);
    let sideload = id.runner == Runner::Sideload;
    window.open_alert_dialog(cx, move |alert, _, _| {
        let options = options.clone();
        let id = id.clone();
        let title = title.clone();
        alert
            .title(heading.clone())
            .child(options.clone())
            .confirm()
            .ok_text(t("game.uninstall.confirm"))
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                let args = options.read(cx).args();
                let b = backend(cx);
                let id = id.clone();
                run_reporting(
                    window,
                    cx,
                    t_args("game.uninstall.failed", &[("title", &title)]),
                    async move { b.uninstall(&id, args).await },
                    move |_, _, cx| {
                        if sideload {
                            navigate(Route::Library, cx);
                        }
                    },
                )
                .detach();
                true
            })
    });
}

/// Pick a new parent folder and move the game's files there.
pub fn open_move_dialog(id: GameId, window: &mut Window, cx: &mut App) {
    let Some(game) = backend(cx).game(&id) else {
        return;
    };
    let Some(current) = game.install_path() else {
        notify_error(
            window,
            cx,
            t("game.move.failed_generic"),
            &anyhow::anyhow!(t_args("game.move.no_location", &[("title", &game.title)])),
        );
        return;
    };
    let pick = pick_path(window, cx, true, t("game.move.pick"));
    window
        .spawn(cx, async move |cx| {
            let Some(target) = pick.await else { return };
            let _ = cx.update(|window, cx| confirm_move(game, current, target, window, cx));
        })
        .detach();
}

fn confirm_move(
    game: Arc<GameInfo>,
    current: PathBuf,
    target: PathBuf,
    window: &mut Window,
    cx: &mut App,
) {
    if target.starts_with(&current) {
        notify_error(
            window,
            cx,
            t("game.move.failed_generic"),
            &anyhow::anyhow!(t("game.move.inside_current")),
        );
        return;
    }
    let heading = t_args("game.move.title", &[("title", &game.title)]);
    let description = t_args(
        "game.move.description",
        &[
            ("from", &current.display().to_string()),
            ("to", &target.display().to_string()),
        ],
    );
    window.open_alert_dialog(cx, move |alert, _, _| {
        let game = game.clone();
        let target = target.clone();
        alert
            .title(heading.clone())
            .description(description.clone())
            .confirm()
            .ok_text(t("game.move.confirm"))
            .on_ok(move |_, window, cx| {
                let b = backend(cx);
                let id = game.id();
                let target = target.clone();
                run_reporting(
                    window,
                    cx,
                    t_args("game.move.failed", &[("title", &game.title)]),
                    async move { b.move_install(&id, target).await },
                    |_, _, _| {},
                )
                .detach();
                true
            })
    });
}

/// Ask how to stop a running install: keep the partial download (resumable)
/// or delete it.
pub fn open_cancel_download_dialog(id: GameId, window: &mut Window, cx: &mut App) {
    let title = backend(cx)
        .game(&id)
        .map(|g| g.title.clone())
        .unwrap_or_else(|| id.app_name.to_string());
    let heading = t_args("game.cancel_download.title", &[("title", &title)]);
    window.open_dialog(cx, move |dialog, _, _| {
        let keep_id = id.clone();
        let delete_id = id.clone();
        dialog.title(heading.clone()).child(
            v_flex()
                .gap_4()
                .child(div().text_sm().child(t("game.cancel_download.description")))
                .child(
                    DialogFooter::new()
                        .flex_wrap()
                        .child(
                            Button::new("cancel-dl-back")
                                .ghost()
                                .label(t("game.cancel_download.keep_installing"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("cancel-dl-keep")
                                .outline()
                                .label(t("game.cancel_download.stop_keep"))
                                .on_click(move |_, window, cx| {
                                    backend(cx).cancel_download(&keep_id, true);
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("cancel-dl-delete")
                                .danger()
                                .label(t("game.cancel_download.stop_delete"))
                                .on_click(move |_, window, cx| {
                                    backend(cx).cancel_download(&delete_id, false);
                                    window.close_dialog(cx);
                                }),
                        ),
                ),
        )
    });
}
