//! Emulated games that can't start yet, with the fix one click away: install
//! the missing emulator (then play), go back to the default version when a
//! pinned one is gone, or choose the BIOS folder. An emulator that can't run
//! on this computer at all is explained instead. Every place that starts a
//! game reaches this through [`crate::save_conflict::resolve_launch_error`].

use std::collections::HashMap;

use ember_core::process::CancellationToken;
use ember_core::{GameId, OpContext};
use ember_engines::{BiosMissing, EngineMissing, EngineUnavailable, Unavailable};
use ember_services::LaunchParams;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Entity, Global, ParentElement as _, PathPromptOptions, Styled as _,
    WeakEntity, Window, div,
};

use crate::i18n::{t, t_args};
use crate::state::{backend, run};

/// A game whose fix is under way: its dialog is open, or the install chosen
/// there is running. The dialog and then the install hold the entity; the fix
/// is over once both have dropped it.
struct PendingFix {
    /// The emulator or core being installed, once the user chose to.
    installing: Option<String>,
}

/// Pending fixes by [`GameId::key`]. Pressing Play again while one is under
/// way must not stack a second dialog or start the install twice.
#[derive(Default)]
struct PendingFixes(HashMap<String, WeakEntity<PendingFix>>);

impl Global for PendingFixes {}

/// Handle an emulator problem of `error`. Returns whether it did.
pub(crate) fn resolve(
    id: &GameId,
    params: &LaunchParams,
    error: &anyhow::Error,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let engine = EngineMissing::of(error).cloned();
    let bios = BiosMissing::of(error).cloned();
    let unavailable = EngineUnavailable::of(error).cloned();
    if engine.is_none() && bios.is_none() && unavailable.is_none() {
        return false;
    }
    let pending = cx
        .default_global::<PendingFixes>()
        .0
        .get(&id.key())
        .and_then(WeakEntity::upgrade);
    if let Some(pending) = pending {
        // Its dialog is already on screen; an install says it's still going.
        if let Some(name) = pending.read(cx).installing.clone() {
            notify_installing(&name, window, cx);
        }
        return true;
    }
    let fix = cx.new(|_| PendingFix { installing: None });
    let fixes = &mut cx.default_global::<PendingFixes>().0;
    fixes.retain(|_, fix| fix.upgrade().is_some());
    fixes.insert(id.key(), fix.downgrade());
    if let Some(missing) = engine {
        open_missing_engine(id.clone(), params.clone(), missing, fix, window, cx);
    } else if let Some(missing) = bios {
        open_missing_bios(id.clone(), params.clone(), missing, fix, window, cx);
    } else if let Some(unavailable) = unavailable {
        open_unavailable(id, unavailable, fix, window, cx);
    }
    true
}

/// Explain why the game's emulator can't run here. There is nothing to
/// install, so the dialog only points to the Emulators page.
fn open_unavailable(
    id: &GameId,
    unavailable: EngineUnavailable,
    fix: Entity<PendingFix>,
    window: &mut Window,
    cx: &mut App,
) {
    let title = title_of(id, cx);
    let engine = unavailable.name.as_str();
    let (heading, body) = match &unavailable.reason {
        Unavailable::NoBuild { hosts } if hosts.is_empty() => (
            t_args("ui.engine.unavailable_title", &[("engine", engine)]),
            t_args(
                "ui.engine.no_download",
                &[("title", &title), ("engine", engine)],
            ),
        ),
        Unavailable::NoBuild { hosts } => {
            let hosts: Vec<String> = hosts
                .iter()
                .map(|h| ember_engines::host::label(h))
                .collect();
            (
                t_args("ui.engine.unavailable_title", &[("engine", engine)]),
                t_args(
                    "ui.engine.no_build",
                    &[
                        ("title", &title),
                        ("engine", engine),
                        ("hosts", &hosts.join(", ")),
                    ],
                ),
            )
        }
        Unavailable::Processor { missing } => (
            t_args("ui.engine.processor_title", &[("engine", engine)]),
            t_args(
                "ui.engine.processor",
                &[
                    ("engine", engine),
                    ("features", &missing.join(", ").to_uppercase()),
                ],
            ),
        ),
    };
    let width = window.rem_size() * 32.;
    window.open_dialog(cx, move |dialog, _, _| {
        // Held while the dialog is open, so Play doesn't open another.
        let _fix = &fix;
        dialog
            .title(heading.clone())
            .w(width)
            .child(div().text_sm().child(body.clone()))
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("engine-unavailable-emulators")
                            .ghost()
                            .label(t("ui.engine.open_emulators"))
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                                crate::navigate(crate::Route::Emulators, cx);
                            }),
                    )
                    .child(
                        Button::new("engine-unavailable-close")
                            .primary()
                            .label(t("ui.engine.close"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
    });
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
    fix: Entity<PendingFix>,
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
        let (id, params, missing, fix) = (id.clone(), params.clone(), missing.clone(), fix.clone());
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
                    install_then_play(
                        id.clone(),
                        params.clone(),
                        missing.clone(),
                        fix.clone(),
                        window,
                        cx,
                    );
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

fn notify_installing(name: &str, window: &mut Window, cx: &mut App) {
    window.push_notification(
        Notification::info(t("ui.engine.installing_detail"))
            .title(t_args("ui.engine.installing", &[("engine", name)])),
        cx,
    );
}

/// Install the missing emulator or core, then start the game.
fn install_then_play(
    id: GameId,
    params: LaunchParams,
    missing: EngineMissing,
    fix: Entity<PendingFix>,
    window: &mut Window,
    cx: &mut App,
) {
    let name = missing.core.clone().unwrap_or_else(|| missing.name.clone());
    fix.update(cx, |fix, _| fix.installing = Some(name.clone()));
    notify_installing(&name, window, cx);
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
            // Held until the game starts again or the install fails.
            let _fix = fix;
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
    fix: Entity<PendingFix>,
    window: &mut Window,
    cx: &mut App,
) {
    let width = window.rem_size() * 32.;
    window.open_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let (id, params, fix) = (id.clone(), params.clone(), fix.clone());
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
                                choose_bios_then_play(
                                    id.clone(),
                                    params.clone(),
                                    fix.clone(),
                                    window,
                                    cx,
                                );
                            }),
                    ),
            )
    });
}

fn choose_bios_then_play(
    id: GameId,
    params: LaunchParams,
    fix: Entity<PendingFix>,
    window: &mut Window,
    cx: &mut App,
) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(t("ui.engine.choose_bios")),
    });
    let b = backend(cx);
    window
        .spawn(cx, async move |cx| {
            // Held until the game starts again or the user gives up.
            let _fix = fix;
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
