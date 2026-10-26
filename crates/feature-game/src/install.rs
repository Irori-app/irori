//! The install sheet (new installs) and its Modify mode (language, branch,
//! build and DLCs of an installed game), plus the import dialog.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use ember_core::util::format_bytes;
use ember_core::{
    GameId, GameInfo, InstallInfo, InstallInfoQuery, InstallPlatform, InstallRequest, Runner,
    UpdateRequest, WineInstallation,
};
use ember_extras::anticheat::InstallGate;
use ember_ui::i18n::{t, t_args};
use ember_ui::{Route, backend, navigate};
use futures::StreamExt as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::select::{Select, SelectEvent};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _,
    alert::Alert, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, Window, div, relative, rems,
};

use crate::choice::{Choice, ChoiceSelect, choice_select, set_choices};
use crate::hooks::{self, AnticheatStatus, CompatibilityInfo};
use crate::i18n::platform_label;
use crate::load::{Load, language_name, preferred_language};
use crate::ops::{notify_error, pick_path};
use crate::sizes::{
    DiskSpace, DiskVerdict, InstallSize, disk_bar_fractions, disk_space, install_button_label,
    install_size,
};
use crate::state::is_unsupported_third_party;

/// Whether the sheet installs a new game or modifies an installed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    Install,
    Modify,
}

/// State of the install / modify sheet. Owns its inputs and selects; the
/// sheet footer is a separate view observing this one.
pub struct InstallSheet {
    id: GameId,
    game: Arc<GameInfo>,
    mode: InstallMode,
    platforms: Vec<InstallPlatform>,
    platform: InstallPlatform,
    path: Entity<InputState>,
    info: Load<InstallInfo>,
    info_rev: u64,
    dlc_sizes: HashMap<String, InstallSize>,
    selected_dlcs: BTreeSet<String>,
    language: Option<String>,
    language_select: Entity<ChoiceSelect>,
    sdl: BTreeSet<String>,
    branch: Option<String>,
    branch_select: Entity<ChoiceSelect>,
    pin_build: bool,
    build: Option<String>,
    build_select: Entity<ChoiceSelect>,
    password: Entity<InputState>,
    password_busy: bool,
    wines: Load<Vec<WineInstallation>>,
    wine: Option<WineInstallation>,
    wine_select: Entity<ChoiceSelect>,
    prefix: Entity<InputState>,
    disk: Load<DiskSpace>,
    disk_rev: u64,
    /// Defaults for DLCs and components were applied once.
    selection_made: bool,
    compat: Option<CompatibilityInfo>,
    anticheat_ack: bool,
    submitting: bool,
    _subscriptions: Vec<Subscription>,
}

/// Open the install sheet of a game. Defaults to the native Linux build when
/// the store offers one.
pub fn open_install_sheet(id: GameId, window: &mut Window, cx: &mut App) {
    open_sheet(id, InstallMode::Install, window, cx);
}

/// Open the sheet that changes language, branch, build and DLCs of an
/// installed game.
pub fn open_modify_sheet(id: GameId, window: &mut Window, cx: &mut App) {
    open_sheet(id, InstallMode::Modify, window, cx);
}

fn open_sheet(id: GameId, mode: InstallMode, window: &mut Window, cx: &mut App) {
    let Some(game) = backend(cx).game(&id) else {
        notify_error(
            window,
            cx,
            t("game.install.open_failed"),
            &anyhow::anyhow!(t_args(
                "game.install.not_in_library",
                &[("id", &id.app_name)]
            )),
        );
        return;
    };
    let title = match mode {
        InstallMode::Install => t_args("game.install.title", &[("title", &game.title)]),
        InstallMode::Modify => t_args("game.install.modify_title", &[("title", &game.title)]),
    };
    let sheet = cx.new(|cx| InstallSheet::new(game, mode, window, cx));
    let footer = cx.new(|cx| InstallFooter::new(sheet.clone(), cx));
    window.open_sheet(cx, move |s, _, _| {
        s.title(title.clone())
            .size(rems(34.))
            .child(sheet.clone())
            .footer(footer.clone())
    });
}

/// Platforms the user can install on this host, native Linux first.
pub fn installable_platforms(game: &GameInfo) -> Vec<InstallPlatform> {
    let mut out = Vec::new();
    let native = ember_core::HostPlatform::current().native();
    if native != InstallPlatform::Windows
        && (game.platforms.contains(&native)
            || (native == InstallPlatform::Linux && game.is_linux_native)
            || (native == InstallPlatform::Mac && game.is_mac_native))
    {
        out.push(native);
    }
    if game.platforms.is_empty() || game.platforms.contains(&InstallPlatform::Windows) {
        out.push(InstallPlatform::Windows);
    }
    if game.platforms.contains(&InstallPlatform::Browser) {
        out.push(InstallPlatform::Browser);
    }
    if out.is_empty() {
        out.push(InstallPlatform::Windows);
    }
    out
}

impl InstallSheet {
    pub fn new(
        game: Arc<GameInfo>,
        mode: InstallMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let b = backend(cx);
        let id = game.id();
        let app = b.settings.app();
        let settings = b.settings.game(&id);
        let platforms = match (mode, game.installed_platform()) {
            (InstallMode::Modify, Some(p)) => vec![p],
            _ => installable_platforms(&game),
        };
        let platform = platforms.first().copied().unwrap_or_default();
        let install = game.install.as_ref();

        let default_path = app.default_install_path.clone();
        let path = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t("game.install.path_placeholder"))
                .default_value(default_path)
        });
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t("game.install.password_placeholder"))
                .masked(true)
        });
        let prefix_default = if settings.wine_prefix.is_empty() {
            ember_runtime::default_prefix(&app, &game)
                .to_string_lossy()
                .into_owned()
        } else {
            settings.wine_prefix.clone()
        };
        let prefix = cx.new(|cx| InputState::new(window, cx).default_value(prefix_default));

        let language_select = choice_select(Vec::new(), None, window, cx);
        let branch_select = choice_select(Vec::new(), None, window, cx);
        let build_select = choice_select(Vec::new(), None, window, cx);
        let wine_select = choice_select(Vec::new(), None, window, cx);

        let subscriptions = vec![
            cx.subscribe_in(&path, window, |this, _, e: &InputEvent, window, cx| {
                if matches!(e, InputEvent::Change) {
                    this.refresh_disk(window, cx);
                }
            }),
            cx.subscribe_in(
                &language_select,
                window,
                |this, _, e: &SelectEvent<Vec<Choice>>, _, cx| {
                    let SelectEvent::Confirm(v) = e;
                    this.language = v.as_ref().map(|v| v.to_string());
                    cx.notify();
                },
            ),
            cx.subscribe_in(
                &branch_select,
                window,
                |this, _, e: &SelectEvent<Vec<Choice>>, window, cx| {
                    let SelectEvent::Confirm(v) = e;
                    let next = v.as_ref().map(|v| v.to_string()).filter(|v| !v.is_empty());
                    if next != this.branch {
                        this.branch = next;
                        this.build = None;
                        this.fetch_info(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &build_select,
                window,
                |this, _, e: &SelectEvent<Vec<Choice>>, window, cx| {
                    let SelectEvent::Confirm(v) = e;
                    let next = v.as_ref().map(|v| v.to_string());
                    if next != this.build {
                        this.build = next;
                        // Sizes and languages differ between builds.
                        this.fetch_info(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &wine_select,
                window,
                |this, _, e: &SelectEvent<Vec<Choice>>, _, cx| {
                    let SelectEvent::Confirm(v) = e;
                    if let (Some(bin), Some(list)) = (v, this.wines.ready()) {
                        this.wine = list.iter().find(|w| w.bin == bin.as_ref()).cloned();
                    }
                    cx.notify();
                },
            ),
        ];

        let mut this = Self {
            id,
            mode,
            platforms,
            platform,
            path,
            info: Load::Loading,
            info_rev: 0,
            dlc_sizes: HashMap::new(),
            selected_dlcs: BTreeSet::new(),
            language: install.and_then(|i| i.language.clone()),
            language_select,
            sdl: BTreeSet::new(),
            branch: install.and_then(|i| i.branch.clone()),
            branch_select,
            pin_build: install.is_some_and(|i| i.pinned_version),
            build: install.and_then(|i| i.build_id.clone()),
            build_select,
            password,
            password_busy: false,
            wines: Load::Idle,
            wine: settings
                .wine_version
                .is_set()
                .then(|| settings.wine_version.clone()),
            wine_select,
            prefix,
            disk: Load::Idle,
            disk_rev: 0,
            selection_made: false,
            compat: None,
            anticheat_ack: false,
            submitting: false,
            game,
            _subscriptions: subscriptions,
        };
        this.fetch_info(window, cx);
        this.fetch_wines(window, cx);
        this.fetch_compat(window, cx);
        this.refresh_disk(window, cx);
        this
    }

    fn is_modify(&self) -> bool {
        self.mode == InstallMode::Modify
    }

    fn set_platform(
        &mut self,
        platform: InstallPlatform,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if platform != self.platform {
            self.platform = platform;
            self.branch = None;
            self.build = None;
            self.fetch_info(window, cx);
        }
    }

    fn fetch_info(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.info_rev += 1;
        let rev = self.info_rev;
        self.info = Load::Loading;
        self.dlc_sizes.clear();
        cx.notify();
        let b = backend(cx);
        let id = self.id.clone();
        let platform = self.platform;
        let query = InstallInfoQuery {
            branch: self.branch.clone(),
            build: self.build.clone().filter(|_| self.pin_build),
            language: None,
        };
        let task = ember_ui::run(cx, async move {
            let store = b
                .store(id.runner)
                .ok_or_else(|| crate::i18n::store_unavailable(id.runner))?;
            store.install_info(&id.app_name, platform, &query).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.info_rev == rev {
                    this.apply_info(result, window, cx);
                }
            });
        })
        .detach();
    }

    fn apply_info(
        &mut self,
        result: anyhow::Result<InstallInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let info = match result {
            Ok(info) => info,
            Err(e) => {
                self.info = Load::Failed(format!("{e:#}").into());
                cx.notify();
                return;
            }
        };
        let ui_language = backend(cx).settings.app().ui_language.clone();
        self.language = preferred_language(&info.languages, self.language.as_deref(), &ui_language);
        let mut languages = info.languages.clone();
        languages.sort();
        let items = languages
            .iter()
            .map(|l| Choice::new(l.clone(), language_name(l)))
            .collect();
        set_choices(
            &self.language_select,
            items,
            self.language.as_deref(),
            window,
            cx,
        );

        let branches = std::iter::once(Choice::new("", t("game.install.branch_default")))
            .chain(
                info.branches
                    .iter()
                    .filter(|b| !b.is_empty())
                    .map(|b| Choice::new(b.clone(), b.clone())),
            )
            .collect();
        set_choices(
            &self.branch_select,
            branches,
            Some(self.branch.as_deref().unwrap_or("")),
            window,
            cx,
        );

        let builds: Vec<Choice> = info
            .builds
            .iter()
            .filter(|b| b.branch == self.branch)
            .map(|b| {
                let date = b.date.chars().take(10).collect::<String>();
                Choice::new(b.build_id.clone(), format!("{} · {date}", b.version_name))
            })
            .collect();
        if self.build.is_none()
            || !builds
                .iter()
                .any(|c| Some(c.value_str()) == self.build.as_deref())
        {
            self.build = builds.first().map(|c| c.value_str().to_string());
        }
        set_choices(
            &self.build_select,
            builds,
            self.build.as_deref(),
            window,
            cx,
        );

        let owned: BTreeSet<String> = info.dlcs.iter().map(|d| d.app_name.clone()).collect();
        if self.selection_made {
            // A refetch (branch, build, password) keeps the user's choices.
            let sdl_ids: BTreeSet<String> = info.sdl.iter().map(|o| o.id.clone()).collect();
            self.sdl.retain(|id| sdl_ids.contains(id));
            self.selected_dlcs.retain(|d| owned.contains(d));
        } else {
            self.selection_made = true;
            self.sdl = info
                .sdl
                .iter()
                .filter(|o| o.mandatory || o.default_selected)
                .map(|o| o.id.clone())
                .collect();
            self.selected_dlcs = self.default_dlcs(&owned);
        }
        let dlc_ids: Vec<String> = info.dlcs.iter().map(|d| d.app_name.clone()).collect();
        self.info = Load::Ready(info);
        self.fetch_dlc_sizes(dlc_ids, window, cx);
        cx.notify();
    }

    /// Owned DLCs preselected for a new install; installed ones when modifying.
    fn default_dlcs(&self, owned: &BTreeSet<String>) -> BTreeSet<String> {
        match self.mode {
            InstallMode::Install => owned.clone(),
            InstallMode::Modify => self
                .game
                .install
                .as_ref()
                .map(|i| {
                    i.installed_dlcs
                        .iter()
                        .filter(|d| owned.contains(*d))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn fetch_dlc_sizes(&mut self, dlcs: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if dlcs.is_empty() {
            return;
        }
        let rev = self.info_rev;
        let b = backend(cx);
        let runner = self.id.runner;
        let platform = self.platform;
        let task = ember_ui::run(cx, async move {
            let store = b
                .store(runner)
                .ok_or_else(|| crate::i18n::store_unavailable(runner))?;
            let sizes: Vec<(String, InstallSize)> = futures::stream::iter(dlcs)
                .map(|dlc| {
                    let store = store.clone();
                    async move {
                        let info = store
                            .install_info(&dlc, platform, &InstallInfoQuery::default())
                            .await
                            .ok()?;
                        Some((dlc, InstallSize::new(info.download_size, info.disk_size)))
                    }
                })
                .buffer_unordered(4)
                .filter_map(|x| async move { x })
                .collect()
                .await;
            Ok(sizes)
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(sizes) = task.await {
                let _ = this.update(cx, |this, cx| {
                    if this.info_rev == rev {
                        this.dlc_sizes
                            .extend(sizes.into_iter().filter(|(_, s)| s.is_known()));
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    fn fetch_wines(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_modify() {
            return;
        }
        self.wines = Load::Loading;
        let app = backend(cx).settings.app();
        let task = ember_ui::run(cx, async move {
            Ok(ember_runtime::wine_installations(&app).await)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let mut list = result.unwrap_or_default();
                if let Some(current) = &this.wine
                    && !list.iter().any(|w| w.bin == current.bin)
                {
                    list.insert(0, current.clone());
                }
                if this.wine.is_none() {
                    this.wine = list.first().cloned();
                }
                let items = list
                    .iter()
                    .map(|w| Choice::new(w.bin.clone(), w.name.clone()))
                    .collect();
                let selected = this.wine.as_ref().map(|w| w.bin.clone());
                set_choices(&this.wine_select, items, selected.as_deref(), window, cx);
                this.wines = Load::Ready(list);
                cx.notify();
            });
        })
        .detach();
    }

    fn fetch_compat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = hooks::fetch_compatibility(self.game.clone(), cx) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.compat = result.ok();
                cx.notify();
            });
        })
        .detach();
    }

    fn install_dir(&self, cx: &App) -> PathBuf {
        let raw = self.path.read(cx).value();
        ember_core::paths::expand_home(raw.trim())
    }

    fn refresh_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_modify() {
            return;
        }
        self.disk_rev += 1;
        let rev = self.disk_rev;
        let dir = self.install_dir(cx);
        if dir.as_os_str().is_empty() {
            self.disk = Load::Idle;
            cx.notify();
            return;
        }
        let task = cx.background_spawn(async move { disk_space(&dir) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.disk_rev == rev {
                    this.disk = Load::from_result(result);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn browse_install_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = pick_path(window, cx, true, t("game.common.select"));
        cx.spawn_in(window, async move |this, cx| {
            if let Some(path) = pick.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    let value = path.to_string_lossy().into_owned();
                    this.path.update(cx, |s, cx| s.set_value(value, window, cx));
                    this.refresh_disk(window, cx);
                });
            }
        })
        .detach();
    }

    fn browse_prefix(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = pick_path(window, cx, true, t("game.common.select"));
        cx.spawn_in(window, async move |this, cx| {
            if let Some(path) = pick.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    let value = path.to_string_lossy().into_owned();
                    this.prefix
                        .update(cx, |s, cx| s.set_value(value, window, cx));
                });
            }
        })
        .detach();
    }

    fn apply_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let password = self.password.read(cx).value();
        self.password_busy = true;
        cx.notify();
        let task = hooks::set_branch_password(self.id.clone(), password, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.password_busy = false;
                match result {
                    Ok(()) => this.fetch_info(window, cx),
                    Err(e) => notify_error(window, cx, t("game.install.password_failed"), &e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Bytes this choice downloads and occupies.
    pub fn size(&self) -> InstallSize {
        self.info
            .ready()
            .map(|info| {
                install_size(
                    info,
                    self.language.as_deref(),
                    &self.selected_dlcs,
                    &self.dlc_sizes,
                )
            })
            .unwrap_or_default()
    }

    fn third_party(&self) -> Option<String> {
        self.info
            .ready()
            .and_then(|i| i.third_party_managed_app.clone())
            .or_else(|| self.game.third_party_managed_app.clone())
    }

    fn anticheat_gate(&self, cx: &App) -> InstallGate {
        hooks::install_gate(
            self.compat.as_ref().and_then(|c| c.anticheat.as_ref()),
            &backend(cx).settings.app(),
        )
    }

    fn not_installable(&self) -> bool {
        self.info.ready().is_some_and(|i| {
            i.download_size == 0 && i.disk_size == 0 && i.language_sizes.is_empty()
        })
    }

    /// Why the primary button is disabled, if it is (a `game.install.blocker`
    /// key).
    fn blocker(&self, cx: &App) -> Option<&'static str> {
        if self.submitting {
            return Some("game.install.blocker.working");
        }
        if !matches!(self.info, Load::Ready(_)) {
            return Some("game.install.blocker.loading");
        }
        if self.not_installable() {
            return Some("game.install.blocker.not_installable");
        }
        if is_unsupported_third_party(&self.game) {
            return Some("game.install.blocker.unsupported_launcher");
        }
        match self.anticheat_gate(cx) {
            InstallGate::Blocked => return Some("game.install.blocker.anticheat"),
            InstallGate::Confirm if !self.anticheat_ack => {
                return Some("game.install.blocker.anticheat");
            }
            _ => {}
        }
        if self.is_modify() {
            return (!self.has_changes()).then_some("game.install.blocker.no_changes");
        }
        if self.path.read(cx).value().trim().is_empty() {
            return Some("game.install.blocker.choose_folder");
        }
        if matches!(
            self.disk,
            Load::Ready(DiskSpace {
                writable: false,
                ..
            })
        ) {
            return Some("game.install.blocker.not_writable");
        }
        None
    }

    fn has_changes(&self) -> bool {
        let Some(install) = self.game.install.as_ref() else {
            return false;
        };
        let installed: BTreeSet<String> = install.installed_dlcs.iter().cloned().collect();
        installed != self.selected_dlcs
            || (self.language.is_some() && install.language != self.language)
            || install.branch != self.branch
            || self.pin_build != install.pinned_version
            || (self.pin_build && install.build_id != self.build)
    }

    /// Primary button label: `Install · 12.3 GiB` or `Apply changes`.
    pub fn primary_label(&self) -> String {
        match self.mode {
            InstallMode::Install => {
                let resumable = self.info.ready().is_some_and(|i| i.resumable);
                let verb = if resumable {
                    t("game.install.continue")
                } else {
                    t("game.install.install")
                };
                install_button_label(&verb, self.size().download)
            }
            InstallMode::Modify => t("game.install.apply").to_string(),
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocker(cx).is_some() {
            return;
        }
        match self.mode {
            InstallMode::Install => self.submit_install(window, cx),
            InstallMode::Modify => self.submit_modify(window, cx),
        }
    }

    fn selected_sdl_tags(&self, info: &InstallInfo) -> Vec<String> {
        info.sdl
            .iter()
            .filter(|o| o.mandatory || self.sdl.contains(&o.id))
            .flat_map(|o| o.tags.iter().cloned())
            .collect()
    }

    fn submit_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = self.info.ready().cloned() else {
            return;
        };
        let b = backend(cx);
        let id = self.id.clone();
        if self.platform.needs_wine() {
            self.persist_wine(cx);
        }
        let size = self.size();
        let request = InstallRequest {
            path: self.install_dir(cx),
            platform: self.platform,
            dlcs: info
                .dlcs
                .iter()
                .filter(|d| self.selected_dlcs.contains(&d.app_name))
                .map(|d| d.app_name.clone())
                .collect(),
            sdl: self.selected_sdl_tags(&info),
            language: self.language.clone(),
            branch: self.branch.clone(),
            build: self.build.clone().filter(|_| self.pin_build),
            dependencies: info.dependencies.clone(),
            installer: None,
        };
        let title = self.game.title.clone();
        self.submitting = true;
        cx.notify();
        let dir = request.path.clone();
        let task = ember_ui::run(cx, async move {
            tokio::fs::create_dir_all(&dir).await.map_err(|e| {
                anyhow::anyhow!(t_args(
                    "game.install.create_dir_failed",
                    &[
                        ("path", &dir.display().to_string()),
                        ("error", &e.to_string())
                    ]
                ))
            })?;
            b.enqueue_install(id, title, request, size.download);
            Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.submitting = false;
                match result {
                    Ok(()) => window.close_sheet(cx),
                    Err(e) => notify_error(window, cx, t("game.install.start_failed"), &e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Store the chosen Wine build and prefix as per-game overrides when they
    /// differ from what the game would use anyway.
    fn persist_wine(&self, cx: &App) {
        let b = backend(cx);
        let effective = b.settings.game(&self.id);
        if let Some(wine) = &self.wine
            && *wine != effective.wine_version
            && let Ok(v) = serde_json::to_value(wine)
        {
            b.settings.set_game_value(&self.id, "wineVersion", Some(v));
        }
        let prefix = self.prefix.read(cx).value().trim().to_string();
        let current = ember_runtime::prefix_path(&b.settings.app(), &effective, &self.game);
        if !prefix.is_empty() && ember_core::paths::expand_home(&prefix) != current {
            b.settings.set_game_value(
                &self.id,
                "winePrefix",
                Some(serde_json::Value::String(prefix)),
            );
        }
    }

    fn submit_modify(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(install) = self.game.install.clone() else {
            return;
        };
        let b = backend(cx);
        let installed: BTreeSet<String> = install.installed_dlcs.iter().cloned().collect();
        // Epic DLCs are separate installs; queue each change on its own.
        if self.id.runner == Runner::Legendary
            && let Some(info) = self.info.ready()
        {
            for dlc in &info.dlcs {
                let want = self.selected_dlcs.contains(&dlc.app_name);
                if want != installed.contains(&dlc.app_name) {
                    crate::dlc::toggle_dlc(&self.game, dlc, want, window, cx);
                }
            }
        }
        let dlcs_changed = installed != self.selected_dlcs && self.id.runner != Runner::Legendary;
        let request = UpdateRequest {
            dlcs: dlcs_changed.then(|| self.selected_dlcs.iter().cloned().collect()),
            language: self
                .language
                .clone()
                .filter(|l| install.language.as_ref() != Some(l)),
            branch: (install.branch != self.branch)
                .then(|| self.branch.clone().unwrap_or_default()),
            build: (self.pin_build && install.build_id != self.build)
                .then(|| self.build.clone())
                .flatten(),
            dependencies: None,
        };
        let needs_update = request.dlcs.is_some()
            || request.language.is_some()
            || request.branch.is_some()
            || request.build.is_some();
        if needs_update {
            b.enqueue_update(self.id.clone(), self.game.title.clone(), request);
        }
        if self.pin_build != install.pinned_version {
            let id = self.id.clone();
            let pinned = self.pin_build;
            crate::ops::run_reporting(
                window,
                cx,
                t("game.install.pin_failed"),
                async move {
                    let store = b
                        .store(id.runner)
                        .ok_or_else(|| crate::i18n::store_unavailable(id.runner))?;
                    store.set_version_pinned(&id.app_name, pinned).await
                },
                |_, _, _| {},
            )
            .detach();
        }
        window.close_sheet(cx);
    }

    fn render_section(title: SharedString, cx: &App) -> AnyElement {
        div()
            .text_sm()
            .font_semibold()
            .text_color(cx.theme().muted_foreground)
            .child(title)
            .into_any_element()
    }

    fn render_notices(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        if let Load::Failed(e) = &self.info {
            out.push(
                v_flex()
                    .gap_2()
                    .child(Alert::error(
                        "install-info-error",
                        t_args("game.install.info_failed", &[("error", e.as_ref())]),
                    ))
                    .child(
                        Button::new("retry-install-info")
                            .small()
                            .outline()
                            .icon(IconName::RefreshCw)
                            .label(t("game.common.retry"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.fetch_info(window, cx)),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if self.not_installable() {
            out.push(
                Alert::warning("not-installable", t("game.install.nothing_to_install"))
                    .into_any_element(),
            );
        }
        if let Some(app) = self.third_party() {
            if is_unsupported_third_party(&self.game) {
                out.push(
                    Alert::error(
                        "third-party",
                        t_args("game.install.third_party_unsupported", &[("app", &app)]),
                    )
                    .into_any_element(),
                );
            } else {
                out.push(
                    Alert::info(
                        "third-party",
                        t_args("game.install.third_party", &[("app", &app)]),
                    )
                    .into_any_element(),
                );
            }
        }
        let gate = self.anticheat_gate(cx);
        if let Some(report) = self.compat.as_ref().and_then(|c| c.anticheat.as_ref())
            && gate != InstallGate::Allowed
        {
            let allowed = gate == InstallGate::Confirm;
            let mut col = v_flex().gap_2().child(Alert::warning(
                "anticheat",
                anticheat_warning(report.status, &report.anticheats),
            ));
            if allowed {
                col = col.child(
                    Checkbox::new("anticheat-ack")
                        .label(t("game.install.install_anyway"))
                        .checked(self.anticheat_ack)
                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                            this.anticheat_ack = *v;
                            cx.notify();
                        })),
                );
            } else {
                col = col
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(t("game.install.anticheat_disabled")),
                    )
                    .child(
                        Button::new("open-advanced")
                            .small()
                            .outline()
                            .label(t("game.install.open_settings"))
                            .on_click(|_, window, cx| {
                                window.close_sheet(cx);
                                navigate(Route::Settings("advanced".into()), cx);
                            }),
                    );
            }
            out.push(col.into_any_element());
        }
        out
    }

    fn render_platform(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.platforms.len() < 2 || self.is_modify() {
            return None;
        }
        let selected = self.platforms.iter().position(|p| *p == self.platform);
        let platforms = self.platforms.clone();
        Some(
            v_flex()
                .gap_2()
                .child(Self::render_section(t("game.common.platform"), cx))
                .child(
                    RadioGroup::horizontal("install-platform")
                        .children(platforms.iter().map(|p| {
                            if *p == InstallPlatform::Linux {
                                t_args("game.install.native", &[("platform", &platform_label(*p))])
                            } else {
                                platform_label(*p)
                            }
                        }))
                        .selected_index(selected)
                        .on_change(cx.listener(move |this, ix: &usize, window, cx| {
                            if let Some(p) = platforms.get(*ix) {
                                this.set_platform(*p, window, cx);
                            }
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_location(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.is_modify() {
            return None;
        }
        let size = self.size();
        let space = self.disk.ready().copied();
        let verdict = DiskVerdict::evaluate(space, size.disk);
        let theme = cx.theme();
        let bar = space.map(|s| {
            let (used, install) = disk_bar_fractions(s, size.disk);
            let install_color = match verdict {
                DiskVerdict::TooSmall { .. } => theme.danger,
                _ => theme.primary,
            };
            h_flex()
                .id("disk-bar")
                .w_full()
                .h_2()
                .rounded(theme.radius)
                .overflow_hidden()
                .bg(theme.muted)
                .child(
                    div()
                        .h_full()
                        .w(relative(used))
                        .bg(theme.muted_foreground.opacity(0.5)),
                )
                .child(div().h_full().w(relative(install)).bg(install_color))
        });
        let message_color = match verdict {
            DiskVerdict::TooSmall { .. } | DiskVerdict::NotWritable => theme.danger,
            _ => theme.muted_foreground,
        };
        let message = match &self.disk {
            Load::Failed(e) => e.to_string(),
            _ => verdict.message(space),
        };
        Some(
            v_flex()
                .gap_2()
                .child(Self::render_section(t("game.common.location"), cx))
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(Input::new(&self.path)))
                        .child(
                            Button::new("browse-install-dir")
                                .outline()
                                .icon(IconName::FolderOpen)
                                .label(t("game.common.browse"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.browse_install_dir(window, cx)
                                })),
                        ),
                )
                .when_some(bar, |this, bar| this.child(bar))
                .child(div().text_xs().text_color(message_color).child(message))
                .into_any_element(),
        )
    }

    fn render_sizes(&self, cx: &App) -> AnyElement {
        let size = self.size();
        let row = |label: SharedString, value: String| {
            h_flex()
                .justify_between()
                .text_sm()
                .child(div().text_color(cx.theme().muted_foreground).child(label))
                .child(div().child(value))
        };
        let value = |bytes: u64| {
            if self.info.is_loading() {
                t("game.install.calculating").to_string()
            } else if bytes == 0 {
                t("game.install.unknown").to_string()
            } else {
                format_bytes(bytes)
            }
        };
        v_flex()
            .gap_1()
            .child(row(t("game.install.download"), value(size.download)))
            .child(row(t("game.install.size_on_disk"), value(size.disk)))
            .into_any_element()
    }

    fn render_options(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(info) = self.info.ready() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if info.languages.len() > 1 {
            out.push(
                v_flex()
                    .gap_2()
                    .child(Self::render_section(t("game.common.language"), cx))
                    .child(
                        Select::new(&self.language_select).placeholder(t("game.common.language")),
                    )
                    .into_any_element(),
            );
        }
        let has_branches = !info.branches.is_empty();
        let builds = info
            .builds
            .iter()
            .filter(|b| b.branch == self.branch)
            .count();
        let password = hooks::has_branch_password(cx) && self.id.runner == Runner::Gog;
        if has_branches || builds > 0 || password {
            let mut col = v_flex()
                .gap_2()
                .child(Self::render_section(t("game.common.version"), cx));
            if has_branches {
                col = col
                    .child(Select::new(&self.branch_select).placeholder(t("game.install.branch")));
            }
            if builds > 0 {
                col = col
                    .child(
                        Checkbox::new("pin-build")
                            .label(t("game.install.pin"))
                            .checked(self.pin_build)
                            .on_change(cx.listener(|this, v: &bool, window, cx| {
                                this.pin_build = *v;
                                this.fetch_info(window, cx);
                            })),
                    )
                    .when(self.pin_build, |c| {
                        c.child(
                            Select::new(&self.build_select)
                                .placeholder(t("game.common.version"))
                                .disabled(builds < 2),
                        )
                    });
            }
            if password {
                col = col.child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&self.password).mask_toggle()),
                        )
                        .child(
                            Button::new("apply-branch-password")
                                .outline()
                                .label(t("game.install.unlock"))
                                .loading(self.password_busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.apply_password(window, cx)
                                })),
                        ),
                );
            }
            out.push(col.into_any_element());
        } else if self.is_modify() && self.id.runner == Runner::Legendary {
            // Epic offers only its latest build, but an installed game can
            // stay at the version it has.
            out.push(
                v_flex()
                    .gap_2()
                    .child(Self::render_section(t("game.common.version"), cx))
                    .child(
                        Checkbox::new("skip-updates")
                            .label(t("game.install.skip_updates"))
                            .checked(self.pin_build)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                this.pin_build = *v;
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        // Optional components are chosen at install time only.
        if !info.sdl.is_empty() && !self.is_modify() {
            let mut col = v_flex().gap_2().child(Self::render_section(
                t("game.install.optional_components"),
                cx,
            ));
            for opt in &info.sdl {
                let id = opt.id.clone();
                col = col.child(
                    Checkbox::new(SharedString::from(format!("sdl-{}", opt.id)))
                        .label(opt.name.clone())
                        .checked(opt.mandatory || self.sdl.contains(&opt.id))
                        .disabled(opt.mandatory)
                        .on_change(cx.listener(move |this, v: &bool, _, cx| {
                            if *v {
                                this.sdl.insert(id.clone());
                            } else {
                                this.sdl.remove(&id);
                            }
                            cx.notify();
                        })),
                );
            }
            out.push(col.into_any_element());
        }
        if !info.dlcs.is_empty() {
            out.push(self.render_dlcs(info, cx));
        }
        out
    }

    fn render_dlcs(&self, info: &InstallInfo, cx: &mut Context<Self>) -> AnyElement {
        let all: BTreeSet<String> = info.dlcs.iter().map(|d| d.app_name.clone()).collect();
        let all_selected = all.is_subset(&self.selected_dlcs);
        let mut col = v_flex().gap_2().child(
            h_flex()
                .justify_between()
                .child(Self::render_section(t("game.install.dlc"), cx))
                .child(
                    Checkbox::new("dlc-all")
                        .label(t("game.install.dlc_all"))
                        .checked(all_selected)
                        .on_change(cx.listener(move |this, v: &bool, _, cx| {
                            if *v {
                                this.selected_dlcs.extend(all.iter().cloned());
                            } else {
                                this.selected_dlcs.clear();
                            }
                            cx.notify();
                        })),
                ),
        );
        for dlc in &info.dlcs {
            let app = dlc.app_name.clone();
            let size = self
                .dlc_sizes
                .get(&dlc.app_name)
                .map(|s| format_bytes(s.download))
                .unwrap_or_default();
            col = col.child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        div().flex_1().min_w_0().child(
                            Checkbox::new(SharedString::from(format!("dlc-{}", dlc.app_name)))
                                .label(dlc.title.clone())
                                .checked(self.selected_dlcs.contains(&dlc.app_name))
                                .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                    if *v {
                                        this.selected_dlcs.insert(app.clone());
                                    } else {
                                        this.selected_dlcs.remove(&app);
                                    }
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(size),
                    ),
            );
        }
        col.into_any_element()
    }

    fn render_wine(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.is_modify() || !self.platform.needs_wine() {
            return None;
        }
        let empty = self.wines.ready().is_some_and(|w| w.is_empty());
        Some(
            v_flex()
                .gap_2()
                .child(Self::render_section("Wine".into(), cx))
                .map(|col| {
                    if self.wines.is_loading() {
                        col.child(
                            h_flex()
                                .gap_2()
                                .text_sm()
                                .child(Spinner::new().small())
                                .child(t("game.install.looking_for_wine")),
                        )
                    } else if empty {
                        col.child(
                            v_flex()
                                .gap_2()
                                .child(Alert::warning("no-wine", t("game.install.no_wine")))
                                .child(
                                    Button::new("open-wine-manager")
                                        .small()
                                        .outline()
                                        .label(t("game.install.open_wine_manager"))
                                        .on_click(|_, window, cx| {
                                            window.close_sheet(cx);
                                            navigate(Route::WineManager, cx);
                                        }),
                                ),
                        )
                    } else {
                        col.child(
                            Select::new(&self.wine_select)
                                .placeholder(t("game.install.wine_version")),
                        )
                    }
                })
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t("game.common.prefix")),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(Input::new(&self.prefix)))
                        .child(
                            Button::new("browse-prefix")
                                .outline()
                                .icon(IconName::FolderOpen)
                                .label(t("game.common.browse"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.browse_prefix(window, cx)
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// Install warning for a gated anticheat, naming the products when known.
fn anticheat_warning(status: AnticheatStatus, products: &[String]) -> SharedString {
    let key = match (status, products.is_empty()) {
        (AnticheatStatus::Broken, false) => "game.install.anticheat.broken",
        (AnticheatStatus::Broken, true) => "game.install.anticheat.broken_unnamed",
        (AnticheatStatus::Denied, false) => "game.install.anticheat.denied",
        (AnticheatStatus::Denied, true) => "game.install.anticheat.denied_unnamed",
        (_, false) => "game.install.anticheat.unknown",
        (_, true) => "game.install.anticheat.unknown_unnamed",
    };
    t_args(key, &[("products", &products.join(", "))])
}

impl Choice {
    fn value_str(&self) -> &str {
        use gpui_kit::component::select::SelectItem as _;
        self.value().as_ref()
    }
}

impl Render for InstallSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let notices = self.render_notices(cx);
        let platform = self.render_platform(cx);
        let location = self.render_location(cx);
        let sizes = self.render_sizes(cx);
        let options = self.render_options(cx);
        let wine = self.render_wine(cx);
        v_flex()
            .id("install-sheet")
            .w_full()
            .gap_6()
            .py_2()
            .children(notices)
            .children(platform)
            .children(location)
            .child(sizes)
            .children(options)
            .children(wine)
    }
}

/// Sheet footer: secondary actions and the primary `Install · <size>` button.
pub struct InstallFooter {
    sheet: Entity<InstallSheet>,
    _observe: Subscription,
}

impl InstallFooter {
    fn new(sheet: Entity<InstallSheet>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&sheet, |_, _, cx| cx.notify());
        Self {
            sheet,
            _observe: observe,
        }
    }
}

impl Render for InstallFooter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.sheet.read(cx);
        let blocker = state.blocker(cx);
        let label = state.primary_label();
        let modify = state.is_modify();
        let submitting = state.submitting;
        let id = state.id.clone();
        let sheet = self.sheet.clone();
        h_flex()
            .w_full()
            .gap_2()
            .flex_wrap()
            .justify_between()
            .child(h_flex().gap_2().when(!modify, |row| {
                row.child(
                    Button::new("import-instead")
                        .ghost()
                        .label(t("game.install.import_existing"))
                        .on_click(move |_, window, cx| {
                            window.close_sheet(cx);
                            crate::manage::open_import_dialog(id.clone(), window, cx);
                        }),
                )
            }))
            .child(
                h_flex()
                    .min_w_0()
                    .gap_2()
                    .when_some(blocker.filter(|_| !submitting), |row, reason| {
                        row.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t(reason)),
                        )
                    })
                    .child(
                        Button::new("install-submit")
                            .primary()
                            .when(!modify, |b| b.icon(Icon::new(IconName::Download)))
                            .label(label)
                            .loading(submitting)
                            .disabled(blocker.is_some())
                            .on_click(move |_, window, cx| {
                                sheet.update(cx, |s, cx| s.submit(window, cx));
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_linux_is_the_default_platform() {
        let game = GameInfo {
            is_linux_native: true,
            platforms: vec![InstallPlatform::Windows, InstallPlatform::Linux],
            ..Default::default()
        };
        assert_eq!(
            installable_platforms(&game),
            if cfg!(target_os = "linux") {
                vec![InstallPlatform::Linux, InstallPlatform::Windows]
            } else {
                vec![InstallPlatform::Windows]
            }
        );
        let windows_only = GameInfo {
            platforms: vec![InstallPlatform::Windows, InstallPlatform::Mac],
            ..Default::default()
        };
        assert_eq!(
            installable_platforms(&windows_only),
            if cfg!(target_os = "macos") {
                vec![InstallPlatform::Mac, InstallPlatform::Windows]
            } else {
                vec![InstallPlatform::Windows]
            }
        );
        assert_eq!(
            installable_platforms(&GameInfo::default()),
            vec![InstallPlatform::Windows]
        );
    }
}
