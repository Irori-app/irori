//! The Emulators page ([`Route::Emulators`](ember_ui::Route::Emulators)):
//! every emulator Ember knows with the installs found on this computer,
//! one-click installs and updates of the versions Ember manages, RetroArch
//! cores, and each system's default emulator.

mod configure;
mod ops;
mod setup;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use ember_engines::bios::{FileState, SystemBios, SystemState};
use ember_engines::releases::{self, Release};
use ember_engines::{EngineKind, EngineRef, Host, Installation, Origin, systems};
use ember_services::EngineStatus;
use ember_ui::components::{EmptyState, PageHeader, SectionTitle};
use ember_ui::i18n::{t, t_args};
use ember_ui::{backend, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::IndexPath;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::link::Link;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _,
    Subscription, Task, Window, div, rems,
};

use crate::common::{card, notify_error};
use crate::format;
pub use configure::open_engine_settings;
use ops::{EngineOps, OpLive, cancel_install, core_key, core_name, engine_key, engine_ops};
pub use ops::{InstallTarget, start_install};
pub use setup::open_emulation_setup;

/// Translation keys of the built-in emulators' options.
#[cfg(test)]
pub(crate) fn option_keys() -> Vec<String> {
    configure::required_keys()
}

/// Repaint interval while installs report progress.
const PROGRESS_TICK: Duration = Duration::from_millis(300);

/// One engine choice in a system's default picker.
#[derive(Debug, Clone, PartialEq)]
struct EngineChoice {
    value: SharedString,
    title: SharedString,
}

impl SelectItem for EngineChoice {
    type Value = SharedString;
    fn title(&self) -> SharedString {
        self.title.clone()
    }
    fn value(&self) -> &SharedString {
        &self.value
    }
}

/// The display name of an engine choice: the engine, or "RetroArch · Core".
pub fn choice_name(choice: &EngineRef) -> String {
    let Some(m) = ember_engines::engine(&choice.engine) else {
        return choice.to_string();
    };
    match choice.core.as_deref() {
        Some(core) => format!(
            "{} · {}",
            m.name,
            m.core(core).map_or(core, |c| c.name.as_str())
        ),
        None => m.name.clone(),
    }
}

/// Every emulator with its installs, and each system's default emulator.
pub struct EmulatorsView {
    tab_ix: usize,
    statuses: Option<Vec<EngineStatus>>,
    /// BIOS status by system id.
    bios: HashMap<String, SystemBios>,
    /// Emulated games in the library by system id.
    counts: BTreeMap<String, usize>,
    /// A ROM or BIOS scan started here is running.
    scanning: bool,
    /// Downloadable versions per engine (the cached list first).
    releases: HashMap<String, Result<Vec<Release>, SharedString>>,
    refreshing: bool,
    /// RetroArch's core list is open.
    show_cores: bool,
    search: Entity<InputState>,
    system_selects: HashMap<String, Entity<SelectState<Vec<EngineChoice>>>>,
    ops: Entity<EngineOps>,
    seen_revision: u64,
    ticking: bool,
    placeholder_language: String,
    _ticker: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EmulatorsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::i18n::register();
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(t("manage.emulators.search")));
        let ops = engine_ops(cx);
        let settings = models(cx).settings.clone();
        let subscriptions = vec![
            cx.subscribe(&search, |_, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.observe_in(&ops, window, |this, ops, window, cx| {
                let revision = ops.read(cx).revision;
                if revision != this.seen_revision {
                    this.seen_revision = revision;
                    this.load(window, cx);
                }
                this.ensure_ticker(window, cx);
                cx.notify();
            }),
            // Engine changes (installs elsewhere, the default per system)
            // arrive as settings notifications.
            cx.observe_in(&settings, window, |this, _, window, cx| {
                this.load(window, cx)
            }),
            cx.observe(&models(cx).library.clone(), |this, _, cx| {
                this.counts = backend(cx).emulated_counts();
                cx.notify();
            }),
        ];
        let seen_revision = ops.read(cx).revision;
        let mut this = Self {
            tab_ix: 0,
            statuses: None,
            bios: HashMap::new(),
            counts: BTreeMap::new(),
            scanning: false,
            releases: HashMap::new(),
            refreshing: false,
            show_cores: false,
            search,
            system_selects: HashMap::new(),
            ops,
            seen_revision,
            ticking: false,
            placeholder_language: ember_ui::i18n::language(),
            _ticker: None,
            _subscriptions: subscriptions,
        };
        this.load(window, cx);
        this.load_releases(false, window, cx);
        this.ensure_ticker(window, cx);
        this
    }

    /// Re-read installs from disk.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let b = backend(cx);
        self.counts = b.emulated_counts();
        let task = ember_ui::run(cx, async move {
            Ok((b.engine_statuses().await, b.bios_statuses().await))
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok((statuses, bios)) = task.await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.statuses = Some(statuses);
                this.bios = bios.into_iter().map(|b| (b.system.clone(), b)).collect();
                this.sync_system_selects(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Version lists of every downloadable engine; `refresh` asks the
    /// projects even when the saved lists are fresh.
    fn load_releases(&mut self, refresh: bool, window: &mut Window, cx: &mut Context<Self>) {
        let host = Host::current();
        let ids: Vec<String> = ember_engines::engines()
            .iter()
            .filter(|m| m.downloadable_on(host))
            .map(|m| m.id.clone())
            .collect();
        if refresh {
            self.refreshing = true;
        }
        let b = backend(cx);
        let task = ember_ui::run(cx, async move {
            let mut out = Vec::new();
            for id in ids {
                let r = b.engine_releases(&id, refresh).await;
                out.push((id, r.map_err(|e| SharedString::from(format!("{e:#}")))));
            }
            Ok(out)
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(lists) = task.await else { return };
            let _ = this.update_in(cx, |this, _, cx| {
                this.refreshing = false;
                for (id, list) in lists {
                    this.releases.insert(id, list);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn ensure_ticker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ticking || self.ops.read(cx).running.is_empty() {
            return;
        }
        self.ticking = true;
        self._ticker = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_TICK).await;
                let Ok(running) = this.update(cx, |this, cx| {
                    cx.notify();
                    !this.ops.read(cx).running.is_empty()
                }) else {
                    return;
                };
                if !running {
                    let _ = this.update(cx, |this, _| this.ticking = false);
                    return;
                }
            }
        }));
    }

    /// One select per system with the emulators that play it.
    fn sync_system_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(statuses) = self.statuses.clone() else {
            return;
        };
        let b = backend(cx);
        for system in systems::all() {
            let choices = ember_engines::choices_for(&system.id);
            let current = b.system_engine(&system.id, &statuses);
            let recommended = choices.first().cloned();
            let items: Vec<EngineChoice> = choices
                .iter()
                .map(|c| {
                    let name = choice_name(c);
                    let title = if Some(c) == recommended.as_ref() {
                        t_args("manage.emulators.system.recommended", &[("engine", &name)])
                    } else {
                        name.into()
                    };
                    EngineChoice {
                        value: c.to_string().into(),
                        title,
                    }
                })
                .collect();
            let ix = current
                .as_ref()
                .and_then(|c| choices.iter().position(|x| x == c))
                .map(|row| IndexPath::default().row(row));
            match self.system_selects.get(&system.id) {
                Some(select) => select.update(cx, |s, cx| {
                    s.set_items(items, window, cx);
                    s.set_selected_index(ix, window, cx);
                }),
                None => {
                    let select = cx.new(|cx| SelectState::new(items, ix, window, cx));
                    let id = system.id.clone();
                    self._subscriptions.push(cx.subscribe_in(
                        &select,
                        window,
                        move |_, _, e: &SelectEvent<Vec<EngineChoice>>, _, cx| {
                            let SelectEvent::Confirm(Some(value)) = e else {
                                return;
                            };
                            let choice = EngineRef::parse(value);
                            let (b, id) = (backend(cx), id.clone());
                            drop(
                                b.clone().spawn_blocking(move || {
                                    b.set_system_engine(&id, choice.as_ref())
                                }),
                            );
                        },
                    ));
                    self.system_selects.insert(system.id.clone(), select);
                }
            }
        }
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
    }

    fn pick_program(&mut self, engine: String, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: cfg!(target_os = "macos"),
            multiple: false,
            prompt: Some(t("manage.emulators.use_program")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.set_program(engine, Some(path), window, cx)
            });
        })
        .detach();
    }

    fn set_program(
        &mut self,
        engine: String,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let b = backend(cx);
        let id = engine.clone();
        let task = ember_ui::run(cx, async move {
            b.set_engine_program(&id, path.clone()).await?;
            if path.is_some() {
                b.set_engine_default(&id, "custom").await?;
            }
            Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(e) = result {
                    notify_error(t("manage.emulators.program_failed"), &e, window, cx);
                }
                this.load(window, cx);
            });
        })
        .detach();
    }

    fn make_default(
        &mut self,
        engine: String,
        installation: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let b = backend(cx);
        let task = ember_ui::run(cx, async move {
            b.set_engine_default(&engine, &installation).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(e) = result {
                    notify_error(t("manage.emulators.default_failed"), &e, window, cx);
                }
                this.load(window, cx);
            });
        })
        .detach();
    }

    fn confirm_remove(
        &mut self,
        engine: String,
        name: String,
        version: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (view, engine, name, version) =
                (view.clone(), engine.clone(), name.clone(), version.clone());
            alert
                .confirm()
                .title(t_args(
                    "manage.emulators.remove_title",
                    &[("engine", &name), ("version", &version)],
                ))
                .description(t("manage.emulators.remove_message"))
                .ok_text(t("manage.emulators.remove_confirm"))
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let (engine, name, version) = (engine.clone(), name.clone(), version.clone());
                    let _ = view.update(cx, |this, cx| {
                        this.remove(engine, name, version, window, cx)
                    });
                    true
                })
        });
    }

    fn remove(
        &mut self,
        engine: String,
        name: String,
        version: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let b = backend(cx);
        let v = version.clone();
        let task = ember_ui::run(
            cx,
            async move { b.remove_engine_version(&engine, &v).await },
        );
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(e) = result {
                    notify_error(
                        t_args(
                            "manage.emulators.remove_failed",
                            &[("engine", &name), ("version", &version)],
                        ),
                        &e,
                        window,
                        cx,
                    );
                }
                this.load(window, cx);
            });
        })
        .detach();
    }

    fn remove_core(&mut self, core: String, window: &mut Window, cx: &mut Context<Self>) {
        let b = backend(cx);
        let name = core_name(&core);
        let task = ember_ui::run(cx, async move { b.remove_core(&core).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(e) = result {
                    notify_error(
                        t_args("manage.emulators.core.remove_failed", &[("core", &name)]),
                        &e,
                        window,
                        cx,
                    );
                }
                this.load(window, cx);
            });
        })
        .detach();
    }

    fn pick_folder(&mut self, bios: bool, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
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
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let b = backend(cx);
                this.run_scan(window, cx, async move {
                    if bios {
                        b.set_bios_folder(path).await.map(|_| ())
                    } else {
                        b.add_rom_folder(path).await
                    }
                })
            });
        })
        .detach();
    }

    /// Run a folder change or rescan, showing it as scanning.
    fn run_scan(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        work: impl std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    ) {
        self.scanning = true;
        cx.notify();
        let task = ember_ui::run(cx, work);
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
    }

    fn rescan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let b = backend(cx);
        let bios = b.settings.app().bios_folder.clone();
        self.run_scan(window, cx, async move {
            b.refresh_store(ember_core::Runner::Emulation, false)
                .await?;
            if !bios.trim().is_empty() {
                b.rescan_bios(ember_core::paths::expand_home(&bios)).await?;
            }
            Ok(())
        });
    }

    fn render_folders(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let app = models(cx).settings.read(cx).app.clone();
        let folders: Vec<AnyElement> = app
            .rom_folders
            .iter()
            .enumerate()
            .map(|(ix, folder)| {
                let remove = folder.clone();
                h_flex()
                    .id(ElementId::Name(format!("rom-folder-{ix}").into()))
                    .gap_3()
                    .child(Icon::new(IconName::Folder).small().text_color(muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(folder.clone()),
                    )
                    .child(
                        Button::new("remove-folder")
                            .ghost()
                            .small()
                            .label(t("manage.emulators.folders.remove"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let (b, folder) = (backend(cx), remove.clone());
                                this.run_scan(window, cx, async move {
                                    b.remove_rom_folder(&folder).await
                                });
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let games: usize = self.counts.values().sum();
        let summary = match (games, self.counts.len()) {
            (0, _) => t("manage.emulators.folders.none_found"),
            (1, _) => t("manage.emulators.folders.found_one"),
            (n, systems) => t_args(
                "manage.emulators.folders.found",
                &[("games", &n.to_string()), ("systems", &systems.to_string())],
            ),
        };
        let bios = app.bios_folder.trim().to_string();
        h_flex()
            .gap_4()
            .flex_wrap()
            .items_start()
            .child(
                card(cx)
                    .w(rems(24.))
                    .flex_grow(1.)
                    .child(
                        SectionTitle::new(t("manage.emulators.folders.title")).trailing(
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new("rescan")
                                        .ghost()
                                        .small()
                                        .icon(IconName::RefreshCw)
                                        .label(t("manage.emulators.folders.rescan"))
                                        .loading(self.scanning)
                                        .disabled(self.scanning)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.rescan(window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("add-folder")
                                        .small()
                                        .icon(IconName::FolderPlus)
                                        .label(t("manage.emulators.folders.add"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.pick_folder(false, window, cx)
                                        })),
                                ),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t("manage.emulators.folders.description")),
                    )
                    .children(folders)
                    .child(div().text_sm().child(if self.scanning {
                        t("manage.emulators.folders.scanning")
                    } else {
                        summary
                    })),
            )
            .child(
                card(cx)
                    .w(rems(24.))
                    .flex_grow(1.)
                    .child(
                        SectionTitle::new(t("manage.emulators.bios.title")).trailing(
                            Button::new("choose-bios")
                                .small()
                                .icon(IconName::FolderOpen)
                                .label(t("manage.emulators.bios.choose"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.pick_folder(true, window, cx)
                                })),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t("manage.emulators.bios.description")),
                    )
                    .child(div().text_sm().truncate().child(if bios.is_empty() {
                        t("manage.emulators.bios.not_chosen")
                    } else {
                        bios.into()
                    })),
            )
            .into_any_element()
    }

    fn render_engine(&self, status: &EngineStatus, cx: &mut Context<Self>) -> AnyElement {
        let m = &status.manifest;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let host = Host::current();
        let key = engine_key(&m.id);
        let op = self.ops.read(cx).running.get(&key).map(|o| o.snapshot());
        let newest = match self.releases.get(&m.id) {
            Some(Ok(list)) => list.first().cloned(),
            _ => None,
        };
        let update = newest.clone().filter(|r| {
            status.newest_managed().is_some_and(|have| {
                releases::compare_versions(&r.version, have) == std::cmp::Ordering::Greater
            })
        });
        let systems_text = m
            .systems
            .iter()
            .filter_map(|s| systems::get(s).map(|s| s.short.clone()))
            .collect::<Vec<_>>()
            .join(", ");

        let primary: AnyElement = if op.is_some() {
            let k = key.clone();
            Button::new("cancel")
                .label(t("manage.emulators.cancel"))
                .on_click(move |_, _, cx| cancel_install(&k, cx))
                .into_any_element()
        } else if let Some(release) = update.clone() {
            let (id, label) = (
                m.id.clone(),
                t_args("manage.emulators.update", &[("version", &release.version)]),
            );
            Button::new("update")
                .primary()
                .small()
                .icon(IconName::RefreshCw)
                .label(label)
                .on_click(move |_, _, cx| {
                    start_install(
                        InstallTarget::Engine {
                            id: id.clone(),
                            release: Some(release.clone()),
                            make_default: true,
                        },
                        cx,
                    )
                })
                .into_any_element()
        } else if !status.is_installed() && m.downloadable_on(host) {
            let id = m.id.clone();
            let label = match newest.as_ref().and_then(|r| r.size).and_then(format::size) {
                Some(size) => t_args("manage.emulators.install_size", &[("size", &size)]),
                None => t("manage.emulators.install"),
            };
            Button::new("install")
                .primary()
                .small()
                .icon(IconName::Download)
                .label(label)
                .on_click(move |_, _, cx| {
                    start_install(
                        InstallTarget::Engine {
                            id: id.clone(),
                            release: None,
                            make_default: true,
                        },
                        cx,
                    )
                })
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let installs: Vec<AnyElement> = status
            .installations
            .iter()
            .enumerate()
            .map(|(ix, i)| self.render_installation(ix, status, i, cx))
            .collect();
        let release_error = match self.releases.get(&m.id) {
            Some(Err(e)) if !status.is_installed() => Some(e.clone()),
            _ => None,
        };

        card(cx)
            .id(ElementId::Name(format!("engine-{}", m.id).into()))
            .child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .flex_wrap()
                                    .child(div().text_lg().font_semibold().child(m.name.clone()))
                                    .child(if status.is_installed() {
                                        Tag::success()
                                            .xsmall()
                                            .child(t("manage.emulators.tag.installed"))
                                    } else {
                                        Tag::secondary()
                                            .xsmall()
                                            .child(t("manage.emulators.tag.not_installed"))
                                    })
                                    .when(update.is_some(), |d| {
                                        d.child(
                                            Tag::warning()
                                                .xsmall()
                                                .child(t("manage.emulators.tag.update")),
                                        )
                                    })
                                    .when(m.experimental, |d| {
                                        d.child(
                                            Tag::warning()
                                                .xsmall()
                                                .child(t("manage.emulators.tag.experimental")),
                                        )
                                    }),
                            )
                            .child(div().text_sm().text_color(muted).child(t_args(
                                "manage.emulators.plays",
                                &[("systems", &systems_text)],
                            ))),
                    )
                    .when_some(op, |d, live| d.child(render_op(&live, cx)))
                    .child(div().flex_shrink_0().child(primary)),
            )
            .when_some(release_error, |d, e| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(t_args("manage.emulators.releases_failed", &[("error", &e)])),
                )
            })
            .when(!status.is_installed() && !m.downloadable_on(host), |d| {
                d.child(div().text_sm().text_color(muted).child(t_args(
                    "manage.emulators.not_downloadable",
                    &[("engine", &m.name)],
                )))
            })
            .when(!installs.is_empty(), |d| {
                d.child(v_flex().gap_1().children(installs))
            })
            .when(m.kind == EngineKind::Retroarch, |d| {
                d.child(self.render_cores(status, cx))
            })
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .items_center()
                    .child(
                        div().flex_1().min_w_0().text_xs().text_color(muted).child(
                            [
                                t_args("manage.emulators.exit_hotkey", &[("keys", &m.exit_hotkey)])
                                    .to_string(),
                                t_args("manage.emulators.license", &[("license", &m.license)])
                                    .to_string(),
                            ]
                            .join(" · "),
                        ),
                    )
                    .when(!m.homepage.is_empty(), |d| {
                        d.child(
                            Link::new("homepage")
                                .href(m.homepage.clone())
                                .child(t("manage.emulators.homepage")),
                        )
                    })
                    .child({
                        let id = m.id.clone();
                        Button::new("use-program")
                            .ghost()
                            .small()
                            .label(t("manage.emulators.use_program"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.pick_program(id.clone(), window, cx)
                            }))
                    })
                    .when(!m.options.is_empty(), |d| {
                        let id = m.id.clone();
                        d.child(
                            Button::new("configure")
                                .small()
                                .icon(IconName::SlidersHorizontal)
                                .label(t("manage.emulators.configure"))
                                .on_click(move |_, window, cx| {
                                    open_engine_settings(&id, window, cx)
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_installation(
        &self,
        ix: usize,
        status: &EngineStatus,
        i: &Installation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let m = &status.manifest;
        let is_default = status.default_installation().map(Installation::id) == Some(i.id());
        let (title, detail): (SharedString, String) = match i.origin {
            Origin::Managed => (
                t_args(
                    "manage.emulators.origin.managed",
                    &[("version", i.version.as_deref().unwrap_or("?"))],
                ),
                match format::size(i.size) {
                    Some(size) => {
                        t_args("manage.emulators.origin.managed_detail", &[("size", &size)])
                            .to_string()
                    }
                    None => t("manage.emulators.origin.managed_plain").to_string(),
                },
            ),
            Origin::Flatpak => (
                t("manage.emulators.origin.flatpak"),
                i.flatpak.clone().unwrap_or_default(),
            ),
            Origin::System | Origin::Windows => (
                t("manage.emulators.origin.system"),
                i.program.display().to_string(),
            ),
            Origin::App => (
                t("manage.emulators.origin.app"),
                i.program.display().to_string(),
            ),
            Origin::Custom => (
                t("manage.emulators.origin.custom"),
                i.program.display().to_string(),
            ),
        };
        let (engine, install_id) = (m.id.clone(), i.id());
        let folder = i
            .dir
            .clone()
            .or_else(|| (i.origin != Origin::Flatpak).then(|| i.program.clone()));
        h_flex()
            .id(ElementId::Name(format!("install-{}-{ix}", m.id).into()))
            .gap_3()
            .px_3()
            .py_1p5()
            .rounded(cx.theme().radius)
            .bg(cx.theme().background)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().font_medium().truncate().child(title))
                            .when(is_default, |d| {
                                d.child(
                                    Tag::info()
                                        .xsmall()
                                        .child(t("manage.emulators.tag.default")),
                                )
                            }),
                    )
                    .child(div().text_xs().text_color(muted).truncate().child(detail)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .flex_shrink_0()
                    .when(!is_default && status.installations.len() > 1, |d| {
                        let (engine, install_id) = (engine.clone(), install_id.clone());
                        d.child(
                            Button::new("make-default")
                                .ghost()
                                .small()
                                .label(t("manage.emulators.make_default"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.make_default(
                                        engine.clone(),
                                        install_id.clone(),
                                        window,
                                        cx,
                                    )
                                })),
                        )
                    })
                    .when_some(folder, |d, folder| {
                        d.child(
                            Button::new("show")
                                .ghost()
                                .small()
                                .icon(IconName::FolderOpen)
                                .tooltip(t("manage.emulators.open_folder"))
                                .on_click(move |_, _, cx| cx.reveal_path(&folder)),
                        )
                    })
                    .when(i.origin == Origin::Managed, |d| {
                        let (engine, name, version) = (
                            engine.clone(),
                            m.name.clone(),
                            i.version.clone().unwrap_or_default(),
                        );
                        d.child(
                            Button::new("remove")
                                .ghost()
                                .small()
                                .label(t("manage.emulators.remove"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_remove(
                                        engine.clone(),
                                        name.clone(),
                                        version.clone(),
                                        window,
                                        cx,
                                    )
                                })),
                        )
                    })
                    .when(i.origin == Origin::Custom, |d| {
                        let engine = engine.clone();
                        d.child(
                            Button::new("stop-using")
                                .ghost()
                                .small()
                                .label(t("manage.emulators.stop_using"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_program(engine.clone(), None, window, cx)
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_cores(&self, status: &EngineStatus, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let m = &status.manifest;
        let running = self.ops.read(cx);
        let installed_cores = m.cores.iter().filter(|c| status.has_core(&c.id)).count();
        let busy = m
            .cores
            .iter()
            .any(|c| running.running.contains_key(&core_key(&c.id)));
        let open = self.show_cores || busy;
        let rows: Vec<AnyElement> = m
            .cores
            .iter()
            .filter(|_| open)
            .map(|core| {
                let key = core_key(&core.id);
                let op = running.running.get(&key).map(|o| o.snapshot());
                let installed = status.has_core(&core.id);
                let systems_text = core
                    .systems
                    .iter()
                    .filter_map(|s| systems::get(s).map(|s| s.short.clone()))
                    .collect::<Vec<_>>()
                    .join(", ");
                let action: AnyElement = match (&op, installed) {
                    (Some(_), _) => {
                        let k = key.clone();
                        Button::new("cancel-core")
                            .ghost()
                            .small()
                            .label(t("manage.emulators.cancel"))
                            .on_click(move |_, _, cx| cancel_install(&k, cx))
                            .into_any_element()
                    }
                    (None, true) => {
                        let id = core.id.clone();
                        Button::new("remove-core")
                            .ghost()
                            .small()
                            .label(t("manage.emulators.core.remove"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.remove_core(id.clone(), window, cx)
                            }))
                            .into_any_element()
                    }
                    (None, false) => {
                        let id = core.id.clone();
                        Button::new("install-core")
                            .small()
                            .icon(IconName::Download)
                            .label(t("manage.emulators.core.install"))
                            .on_click(move |_, _, cx| {
                                start_install(InstallTarget::Core { id: id.clone() }, cx)
                            })
                            .into_any_element()
                    }
                };
                h_flex()
                    .id(ElementId::Name(format!("core-{}", core.id).into()))
                    .gap_3()
                    .px_3()
                    .py_1()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().text_sm().font_medium().child(core.name.clone()))
                                    .when(installed, |d| {
                                        d.child(
                                            Tag::success()
                                                .xsmall()
                                                .child(t("manage.emulators.tag.installed")),
                                        )
                                    }),
                            )
                            .child(div().text_xs().text_color(muted).truncate().child(t_args(
                                "manage.emulators.core.plays",
                                &[("systems", &systems_text), ("license", &core.license)],
                            ))),
                    )
                    .when_some(op, |d, live| d.child(render_op(&live, cx)))
                    .child(div().flex_shrink_0().child(action))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_1()
            .child(
                SectionTitle::new(t_args(
                    "manage.emulators.cores.title",
                    &[
                        ("installed", &installed_cores.to_string()),
                        ("total", &m.cores.len().to_string()),
                    ],
                ))
                .trailing(
                    Button::new("toggle-cores")
                        .ghost()
                        .small()
                        .label(if open {
                            t("manage.emulators.cores.hide")
                        } else {
                            t("manage.emulators.cores.show")
                        })
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_cores = !this.show_cores;
                            cx.notify();
                        })),
                ),
            )
            .child(
                div()
                    .px_3()
                    .text_xs()
                    .text_color(muted)
                    .child(t("manage.emulators.cores.description")),
            )
            .children(rows)
            .into_any_element()
    }

    fn render_engines(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let Some(statuses) = &self.statuses else {
            return h_flex()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Spinner::new().small())
                .child(t("manage.emulators.loading"))
                .into_any_element();
        };
        let query = self.query(cx);
        let cards: Vec<AnyElement> = statuses
            .iter()
            .filter(|s| {
                query.is_empty()
                    || s.manifest.name.to_lowercase().contains(&query)
                    || s.manifest.systems.iter().any(|id| {
                        systems::get(id).is_some_and(|sys| {
                            sys.name.to_lowercase().contains(&query)
                                || sys.short.to_lowercase().contains(&query)
                        })
                    })
            })
            .map(|s| self.render_engine(s, cx))
            .collect();
        if cards.is_empty() {
            return div()
                .text_sm()
                .text_color(muted)
                .child(t("manage.emulators.no_match"))
                .into_any_element();
        }
        v_flex().gap_4().children(cards).into_any_element()
    }

    fn render_systems(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let Some(statuses) = &self.statuses else {
            return Spinner::new().small().into_any_element();
        };
        let b = backend(cx);
        let query = self.query(cx);
        // Systems with games first, each group in catalog order.
        let mut ordered: Vec<&ember_engines::System> = systems::all().iter().collect();
        ordered.sort_by_key(|s| !self.counts.contains_key(&s.id));
        let cards: Vec<AnyElement> = ordered
            .into_iter()
            .filter(|s| {
                query.is_empty()
                    || s.name.to_lowercase().contains(&query)
                    || s.short.to_lowercase().contains(&query)
                    || s.maker.to_lowercase().contains(&query)
            })
            .map(|system| {
                let choice = b.system_engine(&system.id, statuses);
                let ready = choice.as_ref().is_some_and(|c| {
                    statuses
                        .iter()
                        .any(|s| s.manifest.id == c.engine && s.plays(c))
                });
                let engine_installed = choice.as_ref().is_some_and(|c| {
                    statuses
                        .iter()
                        .any(|s| s.manifest.id == c.engine && s.is_installed())
                });
                let state: AnyElement = if ready {
                    Tag::success()
                        .xsmall()
                        .child(t("manage.emulators.system.ready"))
                        .into_any_element()
                } else {
                    Tag::warning()
                        .xsmall()
                        .child(t("manage.emulators.system.no_engine"))
                        .into_any_element()
                };
                let fix: Option<AnyElement> = (!ready).then(|| {
                    let choice = choice.clone();
                    let name = choice.as_ref().map(choice_name).unwrap_or_default();
                    Button::new("install-for-system")
                        .small()
                        .icon(IconName::Download)
                        .label(t_args(
                            "manage.emulators.system.install_engine",
                            &[("engine", &name)],
                        ))
                        .on_click(move |_, _, cx| {
                            let Some(c) = choice.clone() else { return };
                            if !engine_installed {
                                start_install(
                                    InstallTarget::Engine {
                                        id: c.engine.clone(),
                                        release: None,
                                        make_default: true,
                                    },
                                    cx,
                                );
                            }
                            if let Some(core) = c.core {
                                start_install(InstallTarget::Core { id: core }, cx);
                            }
                        })
                        .into_any_element()
                });
                let count = self.counts.get(&system.id).copied().unwrap_or(0);
                let bios_line = self.bios.get(&system.id).and_then(bios_line);
                let count_text = match count {
                    0 => t("manage.emulators.system.no_games"),
                    1 => t("manage.emulators.system.games_one"),
                    n => t_args(
                        "manage.emulators.system.games_other",
                        &[("count", &n.to_string())],
                    ),
                };
                card(cx)
                    .id(ElementId::Name(format!("system-{}", system.id).into()))
                    .w(rems(20.))
                    .flex_grow(1.)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div().font_semibold().truncate().child(system.name.clone()),
                                    )
                                    .child(div().text_xs().text_color(muted).child(format!(
                                        "{} · {} · {count_text}",
                                        system.maker, system.year
                                    ))),
                            )
                            .child(state),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(t("manage.emulators.system.default")),
                            )
                            .when_some(self.system_selects.get(&system.id), |d, select| {
                                d.child(Select::new(select).small())
                            }),
                    )
                    .when_some(bios_line, |d, (warn, text)| {
                        d.child(
                            h_flex()
                                .gap_2()
                                .items_start()
                                .text_xs()
                                .child(
                                    Icon::new(if warn {
                                        IconName::TriangleAlert
                                    } else {
                                        IconName::CircleCheck
                                    })
                                    .xsmall()
                                    .text_color(if warn {
                                        cx.theme().warning
                                    } else {
                                        cx.theme().success
                                    }),
                                )
                                .child(div().flex_1().min_w_0().child(text)),
                        )
                    })
                    .when_some(fix, |d, fix| d.child(fix))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(t("manage.emulators.systems.description")),
            )
            .child(self.render_folders(cx))
            .child(h_flex().flex_wrap().gap_4().items_start().children(cards))
            .into_any_element()
    }
}

/// The BIOS line of a system card: whether it is a warning, and its text.
/// `None` for systems that need no BIOS.
pub(crate) fn bios_line(status: &SystemBios) -> Option<(bool, SharedString)> {
    if status.files.is_empty() {
        return None;
    }
    let names = |state: FileState| {
        status
            .files
            .iter()
            .filter(|f| f.state == state)
            .map(|f| match &f.region {
                Some(r) => format!("{} ({r})", f.name),
                None => f.name.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    Some(match status.state {
        SystemState::Ready => (false, t("manage.emulators.bios.ready")),
        SystemState::Unrecognized => (
            true,
            t_args(
                "manage.emulators.bios.unrecognized",
                &[("files", &names(FileState::Unrecognized))],
            ),
        ),
        SystemState::Missing => (
            true,
            t_args(
                "manage.emulators.bios.missing",
                &[("files", &names(FileState::Missing))],
            ),
        ),
        SystemState::Optional => (
            false,
            t_args(
                "manage.emulators.bios.optional",
                &[("files", &names(FileState::Missing))],
            ),
        ),
    })
}

/// Inline progress of a running install.
fn render_op(live: &OpLive, cx: &App) -> impl IntoElement {
    let progress = live.progress.unwrap_or_default();
    let unpacking = live.step.as_deref() == Some("Unpacking");
    let detail = if unpacking {
        t("manage.emulators.progress.unpacking").to_string()
    } else {
        [
            progress.percent.map(|p| {
                t_args(
                    "manage.emulators.progress.percent",
                    &[("percent", &format!("{p:.0}"))],
                )
                .to_string()
            }),
            Some(format::rate(progress.download_speed)).filter(|s| s != "—"),
            progress.eta_secs.filter(|e| *e > 0).map(|e| {
                t_args(
                    "manage.emulators.progress.time_left",
                    &[("time", &format::eta(Some(e)))],
                )
                .to_string()
            }),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
    };
    v_flex()
        .w(rems(12.))
        .flex_shrink_0()
        .gap_1()
        .child(
            Progress::new("engine-progress")
                .value(progress.percent.unwrap_or(0.0))
                .loading(unpacking || progress.percent.is_none()),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .truncate()
                .child(if detail.is_empty() {
                    t("manage.emulators.progress.starting").to_string()
                } else {
                    detail
                }),
        )
}

impl Render for EmulatorsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let language = ember_ui::i18n::language();
        if language != self.placeholder_language {
            self.placeholder_language = language;
            self.search.update(cx, |s, cx| {
                s.set_placeholder(t("manage.emulators.search"), window, cx)
            });
        }
        let content = match self.tab_ix {
            0 => self.render_engines(cx),
            _ => self.render_systems(cx),
        };
        let empty = self.statuses.as_ref().is_some_and(|s| s.is_empty());
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .px_6()
                    .pt_6()
                    .gap_4()
                    .child(
                        PageHeader::new(t("manage.emulators.title"))
                            .description(t("manage.emulators.description"))
                            .action(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("setup-emulation")
                                            .icon(IconName::WandSparkles)
                                            .label(t("manage.setup.open"))
                                            .on_click(|_, window, cx| {
                                                open_emulation_setup(window, cx)
                                            }),
                                    )
                                    .child(
                                        Button::new("refresh-emulators")
                                            .icon(IconName::RefreshCw)
                                            .label(t("manage.emulators.refresh"))
                                            .loading(self.refreshing)
                                            .disabled(self.refreshing)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.load_releases(true, window, cx);
                                                this.load(window, cx);
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_4()
                            .items_end()
                            .child(
                                div().flex_1().min_w_0().child(
                                    TabBar::new("emulator-tabs")
                                        .underline()
                                        .selected_index(self.tab_ix)
                                        .child(Tab::new().label(t("manage.emulators.tab.engines")))
                                        .child(Tab::new().label(t("manage.emulators.tab.systems")))
                                        .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                            this.tab_ix = *ix;
                                            cx.notify();
                                        })),
                                ),
                            )
                            .child(
                                div()
                                    .w(rems(18.))
                                    .flex_shrink_0()
                                    .pb_1()
                                    .child(Input::new(&self.search).small()),
                            ),
                    ),
            )
            .child(
                div()
                    .id(ElementId::Name(
                        format!("emulators-body-{}", self.tab_ix).into(),
                    ))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(div().px_6().py_4().child(if empty {
                        EmptyState::new(IconName::Joystick, t("manage.emulators.title"))
                            .into_any_element()
                    } else {
                        content
                    })),
            )
    }
}
