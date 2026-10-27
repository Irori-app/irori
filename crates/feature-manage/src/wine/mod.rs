//! The Wine manager ([`Route::WineManager`](irori_ui::Route::WineManager)):
//! downloadable Proton/Wine builds per source, and every build found on this
//! system with the user's custom Wine paths.

mod installed;
mod ops;

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::link::Link;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
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
use irori_core::{Event, Paths, WineInstallation};
use irori_runtime::manager::{self, Release, ReleaseKind};
use irori_ui::components::{EmptyState, PageHeader, SectionTitle};
use irori_ui::i18n::{t, t_args};
use irori_ui::{backend, models};

use crate::common::{backend_op, notify_error};
use crate::format;
use installed::{WineOrigin, group, kind_label};
use ops::{OpLive, WineOps, cancel_install, release_key, start_install, wine_ops};

/// Repaint interval while installs report progress.
const PROGRESS_TICK: Duration = Duration::from_millis(300);

fn source_label(kind: ReleaseKind) -> &'static str {
    kind.type_str()
}

fn source_description(kind: ReleaseKind) -> SharedString {
    match kind {
        ReleaseKind::GeProton => t("manage.wine.source.ge_proton"),
        ReleaseKind::ProtonCachyOs => t("manage.wine.source.proton_cachyos"),
        ReleaseKind::WineGe => t("manage.wine.source.wine_ge"),
        ReleaseKind::WineKron4ek => t("manage.wine.source.wine_kron4ek"),
        ReleaseKind::WineCrossover
        | ReleaseKind::WineStagingMac
        | ReleaseKind::GamePortingToolkit => kind.type_str().into(),
    }
}

#[derive(Clone)]
enum ListState {
    Loading,
    Loaded(Vec<Release>),
    Failed(SharedString),
}

/// What the "On this system" tab shows.
#[derive(Clone)]
enum SystemState {
    Loading,
    Loaded(Vec<(WineOrigin, Vec<WineInstallation>)>),
}

/// Downloadable builds per source plus the builds found on this system.
pub struct WineManagerView {
    tab_ix: usize,
    lists: HashMap<ReleaseKind, ListState>,
    refreshing: bool,
    search: Entity<InputState>,
    system: Option<SystemState>,
    ops: Entity<WineOps>,
    seen_revision: u64,
    ticking: bool,
    /// Language the search placeholder was set in.
    placeholder_language: String,
    _ticker: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl WineManagerView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::i18n::register();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t("manage.wine.search")));
        let ops = wine_ops(cx);
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
                    this.reload_after_change(window, cx);
                }
                this.ensure_ticker(window, cx);
                cx.notify();
            }),
            cx.observe_in(&settings, window, |this, _, window, cx| {
                if this.system.is_some() {
                    this.load_system(window, cx);
                }
            }),
        ];
        let seen_revision = ops.read(cx).revision;
        let mut this = Self {
            tab_ix: 0,
            lists: HashMap::new(),
            refreshing: false,
            search,
            system: None,
            ops,
            seen_revision,
            ticking: false,
            placeholder_language: irori_ui::i18n::language(),
            _ticker: None,
            _subscriptions: subscriptions,
        };
        this.select_tab(0, window, cx);
        this.ensure_ticker(window, cx);
        this
    }

    fn tab_kind(&self) -> Option<ReleaseKind> {
        ReleaseKind::available().get(self.tab_ix).copied()
    }

    fn select_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_ix = ix;
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        match self.tab_kind() {
            Some(kind) if !self.lists.contains_key(&kind) => {
                self.load_list(kind, false, window, cx)
            }
            Some(_) => {}
            None if self.system.is_none() => self.load_system(window, cx),
            None => {}
        }
        cx.notify();
    }

    fn reload_after_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let kinds: Vec<ReleaseKind> = self.lists.keys().copied().collect();
        for kind in kinds {
            self.load_list(kind, false, window, cx);
        }
        if self.system.is_some() {
            self.load_system(window, cx);
        }
    }

    fn load_list(
        &mut self,
        kind: ReleaseKind,
        fetch: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.lists.entry(kind).or_insert(ListState::Loading);
        if fetch {
            self.refreshing = true;
        }
        cx.notify();
        let task = irori_ui::run(cx, async move {
            if fetch {
                manager::update_releases(true, &[kind]).await?;
            }
            manager::available_releases(kind).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if fetch {
                    this.refreshing = false;
                }
                match result {
                    Ok(list) => {
                        this.lists.insert(kind, ListState::Loaded(list));
                    }
                    Err(e) => {
                        let message =
                            t_args("manage.wine.fetch_failed", &[("error", &format!("{e:#}"))]);
                        match this.lists.get(&kind) {
                            Some(ListState::Loaded(_)) => notify_error(
                                t_args(
                                    "manage.wine.refresh_failed",
                                    &[("source", source_label(kind))],
                                ),
                                &e,
                                window,
                                cx,
                            ),
                            _ => {
                                this.lists.insert(kind, ListState::Failed(message));
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_system(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.system.is_none() {
            self.system = Some(SystemState::Loading);
        }
        let app = backend(cx).settings.app();
        let custom = app.custom_wine_paths.clone();
        let task = irori_ui::run(cx, async move {
            Ok(irori_runtime::wine_installations(&app).await)
        });
        cx.spawn_in(window, async move |this, cx| {
            let list = task.await.unwrap_or_default();
            let own = vec![Paths::get().tools_dir()];
            let grouped = group(&list, &custom, &own);
            let _ = this.update_in(cx, |this, _, cx| {
                this.system = Some(SystemState::Loaded(grouped));
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tab_kind() {
            Some(kind) => self.load_list(kind, true, window, cx),
            None => self.load_system(window, cx),
        }
    }

    /// Repaint while installs run so progress moves; stops when none is left.
    fn ensure_ticker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ticking || self.ops.read(cx).running.is_empty() {
            return;
        }
        self.ticking = true;
        self._ticker = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_TICK).await;
                let keep = this.update_in(cx, |this, _, cx| {
                    cx.notify();
                    let running = !this.ops.read(cx).running.is_empty();
                    if !running {
                        this.ticking = false;
                    }
                    running
                });
                if !matches!(keep, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn confirm_remove(&mut self, release: Release, window: &mut Window, cx: &mut Context<Self>) {
        let default_bin = models(cx)
            .settings
            .read(cx)
            .app
            .game
            .wine_version
            .bin
            .clone();
        let is_default = release.install_dir.as_deref().is_some_and(|d| {
            !d.is_empty() && !default_bin.is_empty() && default_bin.starts_with(d)
        });
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            let release = release.clone();
            let description = if is_default {
                t("manage.wine.remove_confirm.default_description")
            } else {
                t("manage.wine.remove_confirm.description")
            };
            alert
                .confirm()
                .title(t_args(
                    "manage.wine.remove_confirm.title",
                    &[("version", &release.version)],
                ))
                .description(description)
                .ok_text(t("manage.wine.remove_confirm.ok"))
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let release = release.clone();
                    let _ = view.update(cx, |this, cx| this.remove(release, window, cx));
                    true
                })
        });
    }

    fn remove(&mut self, release: Release, window: &mut Window, cx: &mut Context<Self>) {
        let b = backend(cx);
        let version = release.version.clone();
        let task = irori_ui::run(cx, async move {
            manager::remove(&release).await?;
            b.bus.emit(Event::WineVersionsChanged);
            Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(e) = result {
                    notify_error(
                        t_args("manage.wine.remove_failed", &[("version", &version)]),
                        &e,
                        window,
                        cx,
                    );
                }
                this.reload_after_change(window, cx);
            });
        })
        .detach();
    }

    fn add_custom_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t("manage.wine.custom.prompt")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let path = path.to_string_lossy().into_owned();
            let _ = this.update_in(cx, |_, _, cx| {
                backend_op(cx, move |b| {
                    b.settings.update_app(|s| {
                        if !s.custom_wine_paths.contains(&path) {
                            s.custom_wine_paths.push(path);
                        }
                    });
                    b.bus.emit(Event::WineVersionsChanged);
                });
            });
        })
        .detach();
    }

    fn render_release(&self, release: &Release, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let key = release_key(release);
        let op = self.ops.read(cx).running.get(&key).map(|op| op.snapshot());
        let installed = release.installed();
        let has_update = installed && release.has_update == Some(true);
        let size = if installed && release.disksize > 0 {
            release.disksize
        } else {
            release.downsize
        };
        let meta = [
            (!release.date.is_empty())
                .then(|| t_args("manage.wine.released", &[("date", &release.date)]).to_string()),
            format::size(size),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");

        let actions: AnyElement = match (&op, installed) {
            (Some(_), _) => {
                let cancel_key = key.clone();
                Button::new("cancel")
                    .label(t("manage.wine.cancel"))
                    .on_click(move |_, _, cx| cancel_install(&cancel_key, cx))
                    .into_any_element()
            }
            (None, true) => {
                let (dir, upd, rem) = (
                    release.install_dir.clone().unwrap_or_default(),
                    release.clone(),
                    release.clone(),
                );
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("open-folder")
                            .ghost()
                            .small()
                            .icon(IconName::FolderOpen)
                            .tooltip(t("manage.wine.open_folder"))
                            .on_click(move |_, _, cx| cx.open_with_system(Path::new(&dir))),
                    )
                    .when(has_update, |this| {
                        this.child(
                            Button::new("update")
                                .small()
                                .icon(IconName::RefreshCw)
                                .label(t("manage.wine.update"))
                                .on_click(move |_, _, cx| start_install(upd.clone(), cx)),
                        )
                    })
                    .child(
                        Button::new("remove")
                            .ghost()
                            .small()
                            .label(t("manage.wine.remove"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_remove(rem.clone(), window, cx)
                            })),
                    )
                    .into_any_element()
            }
            (None, false) => {
                let r = release.clone();
                Button::new("install")
                    .small()
                    .icon(IconName::Download)
                    .label(t("manage.wine.install"))
                    .on_click(move |_, _, cx| start_install(r.clone(), cx))
                    .into_any_element()
            }
        };

        h_flex()
            .id(ElementId::Name(format!("release-{key}").into()))
            .gap_4()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .font_medium()
                                    .truncate()
                                    .child(release.version.clone()),
                            )
                            .when(installed, |this| {
                                this.child(
                                    Tag::success()
                                        .xsmall()
                                        .child(t("manage.wine.tag.installed")),
                                )
                            })
                            .when(has_update, |this| {
                                this.child(
                                    Tag::warning()
                                        .xsmall()
                                        .child(t("manage.wine.tag.update_available")),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(meta)
                            .when(!release.release_notes_link.is_empty(), |this| {
                                this.child(
                                    Link::new("notes")
                                        .href(release.release_notes_link.clone())
                                        .child(t("manage.wine.release_notes")),
                                )
                            }),
                    ),
            )
            .when_some(op, |this, live| this.child(render_op(&live, cx)))
            .child(div().flex_shrink_0().child(actions))
            .into_any_element()
    }

    fn render_source(&self, kind: ReleaseKind, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let query = self.search.read(cx).value().trim().to_lowercase();
        let body: AnyElement = match self.lists.get(&kind) {
            None | Some(ListState::Loading) => h_flex()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Spinner::new().small())
                .child(t("manage.wine.loading"))
                .into_any_element(),
            Some(ListState::Failed(message)) => {
                Alert::error("wine-list-error", message.clone()).into_any_element()
            }
            Some(ListState::Loaded(list)) if list.is_empty() => {
                EmptyState::new(IconName::Wine, t("manage.wine.empty.title"))
                    .description(t("manage.wine.empty.description"))
                    .into_any_element()
            }
            Some(ListState::Loaded(list)) => {
                let rows: Vec<AnyElement> = list
                    .iter()
                    .filter(|r| !r.download.is_empty() || r.installed())
                    .filter(|r| query.is_empty() || r.version.to_lowercase().contains(&query))
                    .map(|r| self.render_release(r, cx))
                    .collect();
                if rows.is_empty() {
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(t("manage.wine.no_match"))
                        .into_any_element()
                } else {
                    v_flex().gap_1().children(rows).into_any_element()
                }
            }
        };
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(muted)
                            .child(source_description(kind)),
                    )
                    .child(
                        div()
                            .w(rems(16.))
                            .flex_shrink_0()
                            .child(Input::new(&self.search).small()),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    fn render_system(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let custom_paths = models(cx).settings.read(cx).app.custom_wine_paths.clone();
        let builds: AnyElement = match &self.system {
            None | Some(SystemState::Loading) => h_flex()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Spinner::new().small())
                .child(t("manage.wine.system.loading"))
                .into_any_element(),
            Some(SystemState::Loaded(groups)) if groups.is_empty() => {
                EmptyState::new(IconName::Wine, t("manage.wine.system.empty.title"))
                    .description(t("manage.wine.system.empty.description"))
                    .into_any_element()
            }
            Some(SystemState::Loaded(groups)) => v_flex()
                .gap_6()
                .children(groups.iter().map(|(origin, list)| {
                    v_flex()
                        .gap_1()
                        .child(SectionTitle::new(origin.label()))
                        .children(list.iter().map(|w| {
                            h_flex()
                                .gap_3()
                                .px_3()
                                .py_1p5()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().font_medium().truncate().child(w.name.clone()))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(muted)
                                                .truncate()
                                                .child(w.bin.clone()),
                                        ),
                                )
                                .child(Tag::secondary().xsmall().child(kind_label(w.kind)))
                        }))
                }))
                .into_any_element(),
        };
        let paths = v_flex()
            .gap_1()
            .child(
                SectionTitle::new(t("manage.wine.custom.title")).trailing(
                    Button::new("add-path")
                        .small()
                        .icon(IconName::FolderPlus)
                        .label(t("manage.wine.custom.add"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.add_custom_path(window, cx)),
                        ),
                ),
            )
            .when(custom_paths.is_empty(), |this| {
                this.child(
                    div()
                        .px_3()
                        .text_sm()
                        .text_color(muted)
                        .child(t("manage.wine.custom.hint")),
                )
            })
            .children(custom_paths.into_iter().map(|p| {
                let remove = p.clone();
                h_flex()
                    .id(ElementId::Name(format!("custom-{p}").into()))
                    .gap_3()
                    .px_3()
                    .py_1()
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(p))
                    .child(
                        Button::new("remove-path")
                            .ghost()
                            .small()
                            .label(t("manage.wine.custom.remove"))
                            .on_click(move |_, _, cx| {
                                let remove = remove.clone();
                                backend_op(cx, move |b| {
                                    b.settings.update_app(|s| {
                                        s.custom_wine_paths.retain(|x| x != &remove)
                                    });
                                    b.bus.emit(Event::WineVersionsChanged);
                                });
                            }),
                    )
            }));
        v_flex()
            .gap_6()
            .child(paths)
            .child(builds)
            .into_any_element()
    }
}

/// Inline progress of a running install.
fn render_op(live: &OpLive, cx: &App) -> impl IntoElement {
    let progress = live.progress.unwrap_or_default();
    let extracting = live
        .step
        .as_deref()
        .is_some_and(|s| s.starts_with("Extracting"));
    let detail = if extracting {
        t("manage.wine.progress.extracting").to_string()
    } else {
        [
            progress.percent.map(|p| {
                t_args(
                    "manage.wine.progress.percent",
                    &[("percent", &format!("{p:.0}"))],
                )
                .to_string()
            }),
            Some(format::rate(progress.download_speed)).filter(|s| s != "—"),
            progress.eta_secs.filter(|e| *e > 0).map(|e| {
                t_args(
                    "manage.wine.progress.time_left",
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
        .w(rems(14.))
        .flex_shrink_0()
        .gap_1()
        .child(
            Progress::new("wine-progress")
                .value(progress.percent.unwrap_or(0.0))
                .loading(extracting || progress.percent.is_none()),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .truncate()
                .child(if detail.is_empty() {
                    t("manage.wine.progress.starting").to_string()
                } else {
                    detail
                }),
        )
}

impl Render for WineManagerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let language = irori_ui::i18n::language();
        if language != self.placeholder_language {
            self.placeholder_language = language;
            self.search.update(cx, |s, cx| {
                s.set_placeholder(t("manage.wine.search"), window, cx)
            });
        }
        let running = self.ops.read(cx).running.len();
        let tabs = ReleaseKind::available()
            .iter()
            .map(|k| Tab::new().label(source_label(*k)))
            .chain(std::iter::once(
                Tab::new().label(t("manage.wine.tab.system")),
            ));
        let content = match self.tab_kind() {
            Some(kind) => self.render_source(kind, cx),
            None => self.render_system(cx),
        };
        let description = if cfg!(target_os = "macos") {
            t("manage.wine.description_macos")
        } else {
            match running {
                0 => t("manage.wine.description"),
                1 => t("manage.wine.description_running_one"),
                n => t_args(
                    "manage.wine.description_running_other",
                    &[("count", &n.to_string())],
                ),
            }
        };
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .px_6()
                    .pt_6()
                    .gap_4()
                    .child(
                        PageHeader::new(t("manage.wine.title"))
                            .description(description)
                            .action(
                                Button::new("refresh-wine")
                                    .icon(IconName::RefreshCw)
                                    .label(t("manage.wine.refresh"))
                                    .loading(self.refreshing)
                                    .disabled(self.refreshing)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.refresh(window, cx)),
                                    ),
                            ),
                    )
                    .child(
                        TabBar::new("wine-tabs")
                            .underline()
                            .selected_index(self.tab_ix)
                            .children(tabs)
                            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                                this.select_tab(*ix, window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .id("wine-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(div().px_6().py_4().child(content)),
            )
    }
}
