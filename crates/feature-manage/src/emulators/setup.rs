//! "Set up emulation": a short guide from a ROM folder to playing. Pick the
//! folders, see what was found, install the recommended emulators in one
//! go, point Irori at the BIOS folder, done. Every step can be skipped and
//! everything it does is also on the Emulators page.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _,
    Subscription, Task, Window, div,
};
use irori_engines::bios::SystemBios;
use irori_engines::{EngineRef, systems};
use irori_services::EngineStatus;
use irori_ui::i18n::{t, t_args};
use irori_ui::{Route, backend, models, navigate};

use super::ops::{EngineOps, core_key, engine_key, engine_ops};
use super::{InstallTarget, bios_line, choice_name, start_install};
use crate::common::notify_error;

const TICK: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Folders,
    Emulators,
    Bios,
    Done,
}

/// Open the guide.
pub fn open_emulation_setup(window: &mut Window, cx: &mut App) {
    crate::i18n::register();
    let guide = cx.new(|cx| SetupGuide::new(window, cx));
    let width = window.rem_size() * 38.;
    window.open_dialog(cx, move |d, _, _| {
        d.title(t("manage.setup.title"))
            .width(width)
            .child(guide.clone())
    });
}

struct SetupGuide {
    step: Step,
    scanning: bool,
    statuses: Option<Vec<EngineStatus>>,
    bios: HashMap<String, SystemBios>,
    ops: Entity<EngineOps>,
    _ticker: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SetupGuide {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ops = engine_ops(cx);
        let m = models(cx).clone();
        let subscriptions = vec![
            cx.observe(&m.library, |_, _, cx| cx.notify()),
            cx.observe(&m.settings, |_, _, cx| cx.notify()),
            cx.observe_in(&ops, window, |this, _, window, cx| this.load(window, cx)),
        ];
        let mut this = Self {
            step: Step::Folders,
            scanning: false,
            statuses: None,
            bios: HashMap::new(),
            ops,
            _ticker: None,
            _subscriptions: subscriptions,
        };
        this.load(window, cx);
        this
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let b = backend(cx);
        let task = irori_ui::run(cx, async move {
            Ok((b.engine_statuses().await, b.bios_statuses().await))
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok((statuses, bios)) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.statuses = Some(statuses);
                this.bios = bios.into_iter().map(|b| (b.system.clone(), b)).collect();
                cx.notify();
            });
        })
        .detach();
        self.ensure_ticker(window, cx);
    }

    /// Repaint while installs report progress.
    fn ensure_ticker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self._ticker.is_some() || self.ops.read(cx).running.is_empty() {
            return;
        }
        self._ticker = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let Ok(busy) = this.update(cx, |this, cx| {
                    cx.notify();
                    !this.ops.read(cx).running.is_empty()
                }) else {
                    return;
                };
                if !busy {
                    let _ = this.update(cx, |this, _| this._ticker = None);
                    return;
                }
            }
        }));
    }

    fn counts(&self, cx: &App) -> BTreeMap<String, usize> {
        backend(cx).emulated_counts()
    }

    fn pick(&mut self, bios: bool, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: !bios,
            prompt: Some(if bios {
                t("manage.emulators.bios.choose")
            } else {
                t("manage.emulators.folders.add")
            }),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let b = backend(cx);
                this.scanning = true;
                cx.notify();
                let task = irori_ui::run(cx, async move {
                    for path in paths {
                        if bios {
                            b.set_bios_folder(path).await?;
                        } else {
                            b.add_rom_folder(path).await?;
                        }
                    }
                    Ok(())
                });
                cx.spawn_in(window, async move |this, cx| {
                    let result = task.await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.scanning = false;
                        if let Err(e) = result {
                            notify_error(t("manage.emulators.folders.scan_failed"), &e, window, cx);
                        }
                        this.load(window, cx);
                    });
                })
                .detach();
            });
        })
        .detach();
    }

    /// The default emulator of each found system.
    fn choices(&self, cx: &App) -> Vec<(String, usize, Option<EngineRef>, bool)> {
        let Some(statuses) = &self.statuses else {
            return Vec::new();
        };
        let b = backend(cx);
        self.counts(cx)
            .into_iter()
            .map(|(system, count)| {
                let choice = b.system_engine(&system, statuses);
                let ready = choice.as_ref().is_some_and(|c| {
                    statuses
                        .iter()
                        .any(|s| s.manifest.id == c.engine && s.plays(c))
                });
                (system, count, choice, ready)
            })
            .collect()
    }

    fn install_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(statuses) = self.statuses.clone() else {
            return;
        };
        let mut engines = Vec::new();
        let mut cores = Vec::new();
        for (_, _, choice, ready) in self.choices(cx) {
            let Some(c) = choice.filter(|_| !ready) else {
                continue;
            };
            let installed = statuses
                .iter()
                .any(|s| s.manifest.id == c.engine && s.is_installed());
            if !installed && !engines.contains(&c.engine) {
                engines.push(c.engine.clone());
            }
            if let Some(core) = c.core
                && !cores.contains(&core)
            {
                cores.push(core);
            }
        }
        for id in engines {
            start_install(
                InstallTarget::Engine {
                    id,
                    release: None,
                    make_default: true,
                },
                cx,
            );
        }
        for id in cores {
            start_install(InstallTarget::Core { id }, cx);
        }
        self.ensure_ticker(window, cx);
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        let needs_bios = self
            .counts(cx)
            .keys()
            .any(|s| self.bios.get(s).is_some_and(|b| !b.files.is_empty()));
        self.step = match self.step {
            Step::Folders => Step::Emulators,
            Step::Emulators if needs_bios => Step::Bios,
            Step::Emulators | Step::Bios | Step::Done => Step::Done,
        };
        cx.notify();
    }

    fn render_folders(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let folders = models(cx).settings.read(cx).app.rom_folders.clone();
        let counts = self.counts(cx);
        let games: usize = counts.values().sum();
        v_flex()
            .gap_3()
            .child(div().text_sm().child(t("manage.setup.folders.intro")))
            .children(folders.into_iter().map(|f| {
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::Folder).small().text_color(muted))
                    .child(div().text_sm().truncate().child(f))
            }))
            .child(
                h_flex().child(
                    Button::new("setup-add-folder")
                        .icon(IconName::FolderPlus)
                        .label(t("manage.emulators.folders.add"))
                        .disabled(self.scanning)
                        .on_click(cx.listener(|this, _, window, cx| this.pick(false, window, cx))),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .when(self.scanning, |d| d.child(Spinner::new().small()))
                    .child(if self.scanning {
                        t("manage.emulators.folders.scanning")
                    } else if games == 0 {
                        t("manage.emulators.folders.none_found")
                    } else if games == 1 {
                        t("manage.emulators.folders.found_one")
                    } else {
                        t_args(
                            "manage.emulators.folders.found",
                            &[
                                ("games", &games.to_string()),
                                ("systems", &counts.len().to_string()),
                            ],
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_emulators(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let running = &self.ops.read(cx).running;
        let rows: Vec<AnyElement> = self
            .choices(cx)
            .into_iter()
            .map(|(system, count, choice, ready)| {
                let name = systems::get(&system).map_or(system.clone(), |s| s.name.clone());
                let busy = choice.as_ref().is_some_and(|c| {
                    running.contains_key(&engine_key(&c.engine))
                        || c.core
                            .as_deref()
                            .is_some_and(|core| running.contains_key(&core_key(core)))
                });
                let state: AnyElement = if busy {
                    Spinner::new().small().into_any_element()
                } else if ready {
                    Tag::success()
                        .xsmall()
                        .child(t("manage.emulators.system.ready"))
                        .into_any_element()
                } else {
                    Tag::secondary()
                        .xsmall()
                        .child(t("manage.setup.emulators.to_install"))
                        .into_any_element()
                };
                h_flex()
                    .id(ElementId::Name(format!("setup-{system}").into()))
                    .gap_3()
                    .py_1()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().truncate().child(name))
                            .child(div().text_xs().text_color(muted).truncate().child({
                                let engine = choice.as_ref().map(choice_name).unwrap_or_default();
                                if count == 1 {
                                    t_args("manage.setup.emulators.row_one", &[("engine", &engine)])
                                } else {
                                    t_args(
                                        "manage.setup.emulators.row_other",
                                        &[("count", &count.to_string()), ("engine", &engine)],
                                    )
                                }
                            })),
                    )
                    .child(state)
                    .into_any_element()
            })
            .collect();
        let all_ready = self.choices(cx).iter().all(|(_, _, _, ready)| *ready);
        v_flex()
            .gap_2()
            .child(div().text_sm().child(t("manage.setup.emulators.intro")))
            .children(rows)
            .child(
                h_flex().pt_2().child(
                    Button::new("install-all")
                        .primary()
                        .icon(IconName::Download)
                        .label(t("manage.setup.emulators.install_all"))
                        .disabled(all_ready || !running.is_empty())
                        .on_click(cx.listener(|this, _, window, cx| this.install_all(window, cx))),
                ),
            )
            .into_any_element()
    }

    fn render_bios(&self, cx: &mut Context<Self>) -> AnyElement {
        let folder = models(cx).settings.read(cx).app.bios_folder.clone();
        let rows: Vec<AnyElement> = self
            .counts(cx)
            .keys()
            .filter_map(|system| {
                let (warn, text) = self.bios.get(system).and_then(bios_line)?;
                let name = systems::get(system).map_or(system.clone(), |s| s.name.clone());
                Some(
                    v_flex()
                        .py_1()
                        .child(div().text_sm().font_medium().child(name))
                        .child(
                            div()
                                .text_xs()
                                .text_color(if warn {
                                    cx.theme().warning
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(text),
                        )
                        .into_any_element(),
                )
            })
            .collect();
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .child(t("manage.emulators.bios.description")),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("setup-bios")
                            .icon(IconName::FolderOpen)
                            .label(t("manage.emulators.bios.choose"))
                            .disabled(self.scanning)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pick(true, window, cx)),
                            ),
                    )
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(
                        if folder.trim().is_empty() {
                            t("manage.emulators.bios.not_chosen")
                        } else {
                            SharedString::from(folder)
                        },
                    )),
            )
            .children(rows)
            .into_any_element()
    }

    fn render_done(&self, cx: &mut Context<Self>) -> AnyElement {
        let games: usize = self.counts(cx).values().sum();
        v_flex()
            .gap_2()
            .items_center()
            .py_4()
            .child(
                Icon::new(IconName::CircleCheck)
                    .large()
                    .text_color(cx.theme().success),
            )
            .child(
                div()
                    .text_lg()
                    .font_semibold()
                    .child(t("manage.setup.done.title")),
            )
            .child(div().text_sm().child(if games == 0 {
                t("manage.setup.done.no_games")
            } else {
                t_args("manage.setup.done.games", &[("count", &games.to_string())])
            }))
            .into_any_element()
    }
}

impl Render for SetupGuide {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let steps = [Step::Folders, Step::Emulators, Step::Bios, Step::Done];
        let ix = steps.iter().position(|s| *s == self.step).unwrap_or(0);
        let body = match self.step {
            Step::Folders => self.render_folders(cx),
            Step::Emulators => self.render_emulators(cx),
            Step::Bios => self.render_bios(cx),
            Step::Done => self.render_done(cx),
        };
        let has_games = !self.counts(cx).is_empty();
        let _ = window;
        v_flex()
            .gap_4()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t_args(
                        "manage.setup.step",
                        &[
                            ("step", &(ix + 1).to_string()),
                            ("total", &steps.len().to_string()),
                        ],
                    )),
            )
            .child(body)
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .when(self.step != Step::Done, |d| {
                        d.child(
                            Button::new("setup-skip")
                                .ghost()
                                .label(t("manage.setup.skip"))
                                .on_click(cx.listener(|this, _, _, cx| this.next(cx))),
                        )
                        .child(
                            Button::new("setup-next")
                                .primary()
                                .label(t("manage.setup.next"))
                                .disabled(
                                    self.scanning || (self.step == Step::Folders && !has_games),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.next(cx))),
                        )
                    })
                    .when(self.step == Step::Done, |d| {
                        d.child(
                            Button::new("setup-play")
                                .primary()
                                .icon(IconName::Play)
                                .label(t("manage.setup.done.open_library"))
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                    navigate(Route::Library, cx);
                                }),
                        )
                    }),
            )
    }
}
