//! Emulated games that can't start yet, with the fix one click away: install
//! the missing emulator (then play), go back to the default version when a
//! pinned one is gone, or choose the BIOS folder. Every place that starts a
//! game reaches this through [`crate::save_conflict::resolve_launch_error`].

use ember_core::process::CancellationToken;
use ember_core::{GameId, OpContext};
use ember_engines::{BiosMissing, EngineMissing};
use ember_services::LaunchParams;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{App, ParentElement as _, PathPromptOptions, Styled as _, Window, div};

use crate::i18n::{t, t_args};
use crate::state::{backend, run};

/// Handle an emulator problem of `error`. Returns whether it did.
pub(crate) fn resolve(
    id: &GameId,
    params: &LaunchParams,
    error: &anyhow::Error,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    if let Some(missing) = EngineMissing::of(error).cloned() {
        open_missing_engine(id.clone(), params.clone(), missing, window, cx);
        return true;
    }
    if let Some(missing) = BiosMissing::of(error).cloned() {
        open_missing_bios(id.clone(), params.clone(), missing, window, cx);
        return true;
    }
    false
}

fn title_of(id: &GameId, cx: &App) -> String {
    backend(cx)
        .game(id)
        .map(|g| g.title.clone())
        .unwrap_or_else(|| id.app_name.to_string())
}

/// Start the game again after a fix.
fn relaunch(id: GameId, params: LaunchParams, window: &mut Window, cx: &mut App) {
    let b = backend(cx);
    let title = title_of(&id, cx);
    let task = run(cx, async move { b.launch_with(&id, params).await });
    window
        .spawn(cx, async move |cx| {
            if let Err(e) = task.await {
                let _ = cx.update(|window, cx| {
                    window.push_notification(
                        Notification::error(format!("{e:#}"))
                            .title(t_args("ui.engine.launch_failed", &[("title", &title)])),
                        cx,
                    );
                });
            }
        })
        .detach();
}

fn open_missing_engine(
    id: GameId,
    params: LaunchParams,
    missing: EngineMissing,
    window: &mut Window,
    cx: &mut App,
) {
    let title = title_of(&id, cx);
    let width = window.rem_size() * 32.;
    let (heading, body) = match (&missing.core, &missing.pinned) {
        (Some(core), _) => (
            t_args("ui.engine.core_missing_title", &[("core", core)]),
            t_args(
                "ui.engine.core_missing",
                &[("core", core), ("engine", &missing.name)],
            ),
        ),
        (None, Some(version)) => (
            t_args(
                "ui.engine.version_missing_title",
                &[("engine", &missing.name), ("version", version)],
            ),
            t_args(
                "ui.engine.version_missing",
                &[
                    ("title", &title),
                    ("engine", &missing.name),
                    ("version", version),
                ],
            ),
        ),
        (None, None) => (
            t_args("ui.engine.missing_title", &[("engine", &missing.name)]),
            t_args(
                "ui.engine.missing",
                &[("title", &title), ("engine", &missing.name)],
            ),
        ),
    };
    window.open_dialog(cx, move |dialog, _, cx| {
        let pinned = missing.pinned.is_some() && missing.core.is_none();
        let (id, params, missing) = (id.clone(), params.clone(), missing.clone());
        let primary = if pinned {
            Button::new("engine-use-default")
                .primary()
                .label(t("ui.engine.use_default_version"))
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    let (b, game) = (backend(cx), id.clone());
                    let worker = b.clone();
                    drop(worker.spawn_blocking(move || {
                        b.settings.set_game_value(&game, "emberEngineVersion", None)
                    }));
                    relaunch(id.clone(), params.clone(), window, cx);
                })
        } else {
            Button::new("engine-install")
                .primary()
                .label(t("ui.engine.install_and_play"))
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    install_then_play(id.clone(), params.clone(), missing.clone(), window, cx);
                })
        };
        dialog
            .title(heading.clone())
            .w(width)
            .child(
                v_flex()
                    .gap_3()
                    .child(div().text_sm().child(body.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t("ui.engine.manage_hint")),
                    ),
            )
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("engine-emulators")
                            .ghost()
                            .label(t("ui.engine.open_emulators"))
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                                crate::navigate(crate::Route::Emulators, cx);
                            }),
                    )
                    .child(primary),
            )
    });
}

/// Install the missing emulator or core, then start the game.
fn install_then_play(
    id: GameId,
    params: LaunchParams,
    missing: EngineMissing,
    window: &mut Window,
    cx: &mut App,
) {
    let name = missing.core.clone().unwrap_or_else(|| missing.name.clone());
    window.push_notification(
        Notification::info(t("ui.engine.installing_detail"))
            .title(t_args("ui.engine.installing", &[("engine", &name)])),
        cx,
    );
    let b = backend(cx);
    let task = run(cx, async move {
        let ctx = OpContext {
            cancel: CancellationToken::new(),
            ..OpContext::detached()
        };
        let installed = match &missing.core {
            Some(core) => b.install_core(core, &ctx).await?.is_some(),
            None => b
                .install_engine(&missing.engine, None, true, &ctx)
                .await?
                .is_some(),
        };
        anyhow::ensure!(installed, "the install was stopped");
        Ok(())
    });
    window
        .spawn(cx, async move |cx| {
            let result = task.await;
            let _ = cx.update(|window, cx| match result {
                Ok(()) => relaunch(id.clone(), params.clone(), window, cx),
                Err(e) => window.push_notification(
                    Notification::error(format!("{e:#}"))
                        .title(t_args("ui.engine.install_failed", &[("engine", &name)])),
                    cx,
                ),
            });
        })
        .detach();
}

fn open_missing_bios(
    id: GameId,
    params: LaunchParams,
    missing: BiosMissing,
    window: &mut Window,
    cx: &mut App,
) {
    let width = window.rem_size() * 32.;
    window.open_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let (id, params) = (id.clone(), params.clone());
        dialog
            .title(t_args("ui.engine.bios_title", &[("system", &missing.name)]))
            .w(width)
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .child(t_args("ui.engine.bios", &[("system", &missing.name)])),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(missing.files.join("\n")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t("ui.engine.bios_never")),
                    ),
            )
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("bios-emulators")
                            .ghost()
                            .label(t("ui.engine.open_emulators"))
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                                crate::navigate(crate::Route::Emulators, cx);
                            }),
                    )
                    .child(
                        Button::new("bios-choose")
                            .primary()
                            .label(t("ui.engine.choose_bios"))
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                choose_bios_then_play(id.clone(), params.clone(), window, cx);
                            }),
                    ),
            )
    });
}

fn choose_bios_then_play(id: GameId, params: LaunchParams, window: &mut Window, cx: &mut App) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(t("ui.engine.choose_bios")),
    });
    let b = backend(cx);
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(folder) = paths.into_iter().next() else {
                return;
            };
            let Ok(task) = cx.update(|_, cx| {
                run(
                    cx,
                    async move { b.set_bios_folder(folder).await.map(|_| ()) },
                )
            }) else {
                return;
            };
            let result = task.await;
            let _ = cx.update(|window, cx| match result {
                Ok(()) => relaunch(id.clone(), params.clone(), window, cx),
                Err(e) => window.push_notification(
                    Notification::error(format!("{e:#}")).title(t("ui.engine.bios_failed")),
                    cx,
                ),
            });
        })
        .detach();
}
