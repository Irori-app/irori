//! Adding and editing apps that are not from a store ("sideloaded" apps):
//! native and Windows programs, and web apps.
//!
//! Compared with Heroic: the platform can be changed when editing, launch
//! arguments are part of the form, the title is not truncated or stripped of
//! characters, addresses are completed only on save (so `http://` survives),
//! and no third-party image lookup runs behind the user's back (art is found
//! on SteamGridDB only when the user asks).
//!
//! "Run installer…" runs a Windows setup program in the app's Wine prefix
//! before its executable is known. A new app is added first (with the
//! installer as its program) so it has settings and a prefix; the prefix
//! path is pinned so renaming the app later keeps the installed files. If
//! the dialog is closed without saving, that provisional entry is removed.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use ember_core::{GameId, Runner, Store as _};
use ember_extras::shortcuts::{self, ShortcutTargets};
use ember_sideload::{SideloadApp, SideloadPlatform};
use ember_ui::i18n::{t, t_args};
use ember_ui::{Route, backend, navigate};
use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::form::{field, v_form};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};

use super::steamgrid::{ArtKind, ArtPicker, ArtPickerEvent};
use super::{dialog_width, optional_text, pick_path, text};

const PLATFORMS: &[SideloadPlatform] = if cfg!(target_os = "macos") {
    &[
        SideloadPlatform::Mac,
        SideloadPlatform::Windows,
        SideloadPlatform::Browser,
    ]
} else if cfg!(windows) {
    &[SideloadPlatform::Windows, SideloadPlatform::Browser]
} else {
    &[
        SideloadPlatform::Windows,
        SideloadPlatform::Linux,
        SideloadPlatform::Browser,
    ]
};

/// Name of a platform choice; operating systems keep their own names.
fn platform_label(platform: SideloadPlatform) -> SharedString {
    match platform {
        SideloadPlatform::Windows => "Windows".into(),
        SideloadPlatform::Linux => "Linux".into(),
        SideloadPlatform::Mac => "macOS".into(),
        SideloadPlatform::Browser => t("library.sideload.platform.web"),
    }
}

/// Title suggested from an executable's file name (`/x/Hollow_Knight.exe` →
/// `Hollow Knight`).
pub fn title_from_executable(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let title = stem.replace(['_', '.'], " ");
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty()).then_some(title)
}

/// The add/edit form.
pub struct SideloadDialog {
    /// Library id of the app being edited; `None` when adding.
    editing: Option<String>,
    title: Entity<InputState>,
    executable: Entity<InputState>,
    arguments: Entity<InputState>,
    install_folder: Entity<InputState>,
    url: Entity<InputState>,
    user_agent: Entity<InputState>,
    cover: Entity<InputState>,
    background: Entity<InputState>,
    platform: SideloadPlatform,
    fullscreen: bool,
    saving: bool,
    /// Saved successfully (a provisional entry is then kept).
    saved: bool,
    /// Library id of an app added only to run its installer; it becomes the
    /// new app on save and is removed if the dialog closes without saving.
    provisional: Option<String>,
    installer: InstallerState,
    error: Option<SharedString>,
    /// The SteamGridDB picker, shown instead of the form while open.
    picker: Option<Entity<ArtPicker>>,
    _picker_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

/// Progress of "Run installer…".
#[derive(Debug, Clone, PartialEq, Eq)]
enum InstallerState {
    Idle,
    Starting,
    /// The installer started; the app's Wine prefix is at this path.
    Started(SharedString),
}

/// How saving the form applies to the sideload store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveMode {
    /// Add a new app.
    Add,
    /// Turn the entry created for an installer into the new app.
    Finish(String),
    /// Change an existing app.
    Edit(String),
}

/// Pick the save mode from the dialog's state.
pub fn save_mode(editing: Option<&str>, provisional: Option<&str>) -> SaveMode {
    match (editing, provisional) {
        (Some(name), _) => SaveMode::Edit(name.to_string()),
        (None, Some(name)) => SaveMode::Finish(name.to_string()),
        (None, None) => SaveMode::Add,
    }
}

impl SideloadDialog {
    pub fn new(editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let existing = editing
            .as_deref()
            .and_then(|name| crate::sideload_store(cx).and_then(|s| s.app(name)))
            .unwrap_or_default();
        let path_text = |p: &Option<PathBuf>| {
            p.as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let mut input = |placeholder: SharedString, value: String| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let title = input(
            t("library.sideload.placeholder.title"),
            existing.title.clone(),
        );
        let executable = input("/path/to/game".into(), path_text(&existing.executable));
        let arguments = input(
            t("library.sideload.placeholder.arguments"),
            existing.launcher_args.clone(),
        );
        let install_folder = input(
            t("library.sideload.placeholder.install_folder"),
            path_text(&existing.install_folder),
        );
        let url = input(
            "https://example.com/game".into(),
            existing.browser_url.clone().unwrap_or_default(),
        );
        let user_agent = input(
            t("library.sideload.placeholder.optional"),
            existing.custom_user_agent.clone().unwrap_or_default(),
        );
        let cover = input(
            t("library.sideload.placeholder.image"),
            existing.cover_art.clone().unwrap_or_default(),
        );
        let background = input(
            t("library.sideload.placeholder.image"),
            existing.background_art.clone().unwrap_or_default(),
        );

        let mut subscriptions = Vec::new();
        for state in [&title, &executable, &url] {
            subscriptions.push(
                cx.subscribe_in(state, window, |this, _, event, window, cx| match event {
                    InputEvent::Change => {
                        this.error = None;
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.save(window, cx),
                    _ => {}
                }),
            );
        }

        Self {
            platform: if editing.is_some() {
                existing.platform
            } else {
                SideloadPlatform::default()
            },
            fullscreen: existing.launch_fullscreen,
            editing,
            title,
            executable,
            arguments,
            install_folder,
            url,
            user_agent,
            cover,
            background,
            saving: false,
            saved: false,
            provisional: None,
            installer: InstallerState::Idle,
            error: None,
            picker: None,
            _picker_subscription: None,
            _subscriptions: subscriptions,
        }
    }

    fn dialog_title(&self, cx: &App) -> SharedString {
        match (&self.picker, &self.editing) {
            (Some(p), _) => p.read(cx).kind().title(),
            (None, Some(_)) => t("library.sideload.title.edit"),
            (None, None) => t("library.sideload.title.add"),
        }
    }

    /// Remove the entry created for an installer when the app was not saved.
    fn discard_provisional(&mut self, cx: &mut App) {
        let Some(name) = self.provisional.take().filter(|_| !self.saved) else {
            return;
        };
        let Some(store) = crate::sideload_store(cx) else {
            return;
        };
        let b = backend(cx);
        ember_ui::state::run_infallible(cx, async move {
            if let Err(e) = store.remove_app(&name, false).await {
                tracing::warn!("removing unsaved app {name}: {e:#}");
            }
            b.settings
                .remove_game(&GameId::new(Runner::Sideload, name.as_str()));
        })
        .detach();
    }

    fn find_art(&mut self, kind: ArtKind, window: &mut Window, cx: &mut Context<Self>) {
        let title = text(&self.title, cx);
        let picker = cx.new(|cx| ArtPicker::new(kind, &title, window, cx));
        picker.update(cx, |p, cx| p.focus(window, cx));
        self._picker_subscription = Some(cx.subscribe_in(
            &picker,
            window,
            move |this, _, event: &ArtPickerEvent, window, cx| {
                if let ArtPickerEvent::Picked(url) = event {
                    let input = if kind == ArtKind::Background {
                        &this.background
                    } else {
                        &this.cover
                    };
                    let url = url.clone();
                    input.update(cx, |s, cx| s.set_value(url, window, cx));
                }
                this.picker = None;
                this._picker_subscription = None;
                this.title.focus_handle(cx).focus(window, cx);
                cx.notify();
            },
        ));
        self.picker = Some(picker);
        cx.notify();
    }

    /// Pick a Windows installer and run it in the app's prefix.
    fn run_installer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.installer == InstallerState::Starting {
            return;
        }
        if text(&self.title, cx).is_empty() {
            self.error = Some(t("library.sideload.error.title_first"));
            cx.notify();
            return;
        }
        let picked = pick_path(false, t("library.sideload.picker.run_installer"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(installer) = picked.await else {
                return;
            };
            this.update_in(cx, |this, _, cx| this.start_installer(installer, cx))
                .ok();
        })
        .detach();
    }

    fn start_installer(&mut self, installer: PathBuf, cx: &mut Context<Self>) {
        let Some(store) = crate::sideload_store(cx) else {
            self.error = Some(t("library.sideload.error.unavailable"));
            cx.notify();
            return;
        };
        let existing = self.editing.clone().or_else(|| self.provisional.clone());
        let mut app = self.collect(cx);
        app.platform = SideloadPlatform::Windows;
        if app.executable.is_none() {
            app.executable = Some(installer.clone());
        }
        self.installer = InstallerState::Starting;
        self.error = None;
        cx.notify();

        let b = backend(cx);
        let task = ember_ui::run(cx, async move {
            let (id, created) = match existing {
                Some(name) => (GameId::new(Runner::Sideload, name.as_str()), None),
                None => {
                    let id = store.add_app(app).await?;
                    (id.clone(), Some(id.app_name.to_string()))
                }
            };
            let game = store.game(&id.app_name).ok_or_else(|| {
                anyhow::anyhow!(t("library.sideload.error.not_found").to_string())
            })?;
            let settings = b.settings.game(&id);
            let prefix = ember_runtime::prefix_path(&b.settings.app(), &settings, &game);
            if created.is_some() && settings.wine_prefix.is_empty() {
                b.settings.set_game_value(
                    &id,
                    "winePrefix",
                    Some(serde_json::Value::String(
                        prefix.to_string_lossy().into_owned(),
                    )),
                );
            }
            let argv = vec![installer.to_string_lossy().into_owned()];
            let run = b.run_in_prefix(&id, argv).await.map(|_| ());
            Ok((created, prefix, run))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((created, prefix, run)) => {
                        if created.is_some() {
                            this.provisional = created;
                        }
                        match run {
                            Ok(()) => {
                                this.installer = InstallerState::Started(
                                    prefix.to_string_lossy().into_owned().into(),
                                );
                            }
                            Err(e) => {
                                this.installer = InstallerState::Idle;
                                this.error = Some(installer_error(&e));
                            }
                        }
                    }
                    Err(e) => {
                        this.installer = InstallerState::Idle;
                        this.error = Some(installer_error(&e));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn collect(&self, cx: &App) -> SideloadApp {
        let path = |input: &Entity<InputState>| optional_text(input, cx).map(PathBuf::from);
        SideloadApp {
            title: text(&self.title, cx),
            executable: path(&self.executable),
            platform: self.platform,
            browser_url: optional_text(&self.url, cx),
            custom_user_agent: optional_text(&self.user_agent, cx),
            launch_fullscreen: self.fullscreen,
            cover_art: optional_text(&self.cover, cx),
            background_art: optional_text(&self.background, cx),
            install_folder: path(&self.install_folder),
            launcher_args: text(&self.arguments, cx),
            description: None,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let app = self.collect(cx);
        if let Err(e) = app.clone().normalized() {
            self.error = Some(format!("{e:#}").into());
            cx.notify();
            return;
        }
        let Some(store) = crate::sideload_store(cx) else {
            self.error = Some(t("library.sideload.error.unavailable"));
            cx.notify();
            return;
        };
        let title = app.title.trim().to_string();
        let mode = save_mode(self.editing.as_deref(), self.provisional.as_deref());
        let settings = backend(cx).settings.app();
        self.saving = true;
        self.error = None;
        cx.notify();

        let task = ember_ui::run(cx, async move {
            let id = match mode {
                SaveMode::Edit(name) => {
                    store.edit_app(&name, app).await?;
                    return Ok(false);
                }
                SaveMode::Finish(name) => {
                    store.edit_app(&name, app).await?;
                    GameId::new(Runner::Sideload, name.as_str())
                }
                SaveMode::Add => store.add_app(app).await?,
            };
            let targets = ShortcutTargets::from_settings(&settings);
            if (targets.desktop || targets.app_menu || targets.steam)
                && let Some(game) = store.game(&id.app_name)
                && let Err(e) = shortcuts::add(&game, &settings, targets).await
            {
                tracing::warn!("adding shortcuts for {id}: {e:#}");
            }
            Ok(true)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(added) => {
                        this.saved = true;
                        window.close_dialog(cx);
                        let message = if added {
                            t_args("library.sideload.notify.added", &[("title", &title)])
                        } else {
                            t_args("library.sideload.notify.saved", &[("title", &title)])
                        };
                        window.push_notification(Notification::success(message), cx);
                    }
                    Err(e) => this.error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn browse(
        &mut self,
        target: fn(&Self) -> &Entity<InputState>,
        directories: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let picked = pick_path(directories, t("library.sideload.picker.select"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = picked.await else { return };
            this.update_in(cx, |this, window, cx| {
                let value = path.to_string_lossy().into_owned();
                let input = target(this).clone();
                input.update(cx, |s, cx| s.set_value(value, window, cx));
                // Suggest a title from the executable when none was entered.
                if std::ptr::eq(target(this), &this.executable)
                    && text(&this.title, cx).is_empty()
                    && let Some(title) = title_from_executable(&path)
                {
                    this.title
                        .update(cx, |s, cx| s.set_value(title, window, cx));
                }
                this.error = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn path_row(
        &self,
        id: &'static str,
        target: fn(&Self) -> &Entity<InputState>,
        directories: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        h_flex()
            .w_full()
            .gap_2()
            .child(div().flex_1().min_w_0().child(Input::new(target(self))))
            .child(
                Button::new(id)
                    .outline()
                    .icon(IconName::FolderOpen)
                    .label(t("library.common.browse"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.browse(target, directories, window, cx)
                    })),
            )
    }

    /// An art field: path or URL, plus "Find online…".
    fn art_row(
        &self,
        id: &'static str,
        target: fn(&Self) -> &Entity<InputState>,
        kind: ArtKind,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let find_id = if kind == ArtKind::Background {
            "find-background"
        } else {
            "find-cover"
        };
        self.path_row(id, target, false, cx).child(
            Button::new(find_id)
                .outline()
                .icon(IconName::Globe)
                .label(t("library.common.find_online"))
                .on_click(cx.listener(move |this, _, window, cx| this.find_art(kind, window, cx))),
        )
    }
}

impl SideloadDialog {
    /// The system a picked file is a game of, when it is a ROM rather than
    /// a program (DOS programs count as programs here).
    fn rom_system(&self, cx: &App) -> Option<&'static ember_engines::System> {
        let path = PathBuf::from(text(&self.executable, cx));
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if matches!(
            ext.as_str(),
            "exe" | "com" | "bat" | "sh" | "appimage" | "msi" | "lnk" | "desktop" | "app"
        ) {
            return None;
        }
        match ember_engines::systems::by_extension(&ext).as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }

    /// Put the picked ROM in the ROM library and open its page.
    fn add_as_rom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = crate::emulation_store(cx) else {
            return;
        };
        let path = PathBuf::from(text(&self.executable, cx));
        let title = optional_text(&self.title, cx);
        self.saving = true;
        cx.notify();
        let task = ember_ui::run(cx, async move { store.add_game(&path, None, title).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(id) => {
                        this.discard_provisional(cx);
                        window.close_dialog(cx);
                        navigate(Route::Game(id), cx);
                    }
                    Err(e) => this.error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for SideloadDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = &self.picker {
            return v_flex().id("sideload-art").child(picker.clone());
        }
        let rom_system = self.rom_system(cx).filter(|_| self.editing.is_none());
        let browser = self.platform == SideloadPlatform::Browser;
        let windows = self.platform == SideloadPlatform::Windows;
        let adding = self.editing.is_none();
        let installer = self.installer.clone();
        let muted = cx.theme().muted_foreground;
        let platform = self.platform;

        v_flex()
            .id("sideload-form")
            .gap_4()
            .when_some(rom_system, |this, system| {
                this.child(
                    Alert::info(
                        "sideload-rom-hint",
                        t_args(
                            "library.sideload.rom_hint.body",
                            &[("system", &system.name)],
                        ),
                    )
                    .title(t("library.sideload.rom_hint.title")),
                )
                .child(
                    h_flex().justify_end().child(
                        Button::new("sideload-add-rom")
                            .primary()
                            .small()
                            .icon(IconName::Joystick)
                            .label(t("library.sideload.rom_hint.add"))
                            .loading(self.saving)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_as_rom(window, cx)),
                            ),
                    ),
                )
            })
            .when(adding && rom_system.is_none(), |this| {
                this.child(
                    Alert::info("sideload-store-hint", t("library.sideload.store_hint.body"))
                        .title(t("library.sideload.store_hint.title")),
                )
                .child(
                    h_flex().justify_end().child(
                        Button::new("sideload-accounts")
                            .small()
                            .ghost()
                            .icon(IconName::LogIn)
                            .label(t("library.sideload.go_to_accounts"))
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                                navigate(Route::Accounts, cx);
                            }),
                    ),
                )
            })
            .child(
                v_form()
                    .child(
                        field()
                            .label(t("library.sideload.field.title"))
                            .child(Input::new(&self.title)),
                    )
                    .child(
                        field().label(t("library.sideload.field.platform")).child(
                            ButtonGroup::new("sideload-platform")
                                .outline()
                                .children(PLATFORMS.iter().enumerate().map(|(ix, p)| {
                                    Button::new(SharedString::from(format!("platform-{ix}")))
                                        .label(platform_label(*p))
                                        .selected(platform == *p)
                                }))
                                .on_click(cx.listener(|this, clicked: &Vec<usize>, _, cx| {
                                    if let Some(&ix) = clicked.first() {
                                        this.platform = PLATFORMS[ix];
                                        this.error = None;
                                        cx.notify();
                                    }
                                })),
                        ),
                    )
                    .when(!browser, |form| {
                        form.child(
                            field()
                                .label(t("library.sideload.field.executable"))
                                .description(match platform {
                                    SideloadPlatform::Windows => {
                                        t("library.sideload.executable_hint.windows")
                                    }
                                    _ => t("library.sideload.executable_hint.native"),
                                })
                                .child(
                                    self.path_row(
                                        "browse-executable",
                                        |this| &this.executable,
                                        false,
                                        cx,
                                    )
                                    .when(windows, |row| {
                                        row.child(
                                            Button::new("run-installer")
                                                .outline()
                                                .icon(IconName::PackageOpen)
                                                .label(t("library.sideload.run_installer"))
                                                .tooltip(t(
                                                    "library.sideload.run_installer_tooltip",
                                                ))
                                                .loading(installer == InstallerState::Starting)
                                                .disabled(installer == InstallerState::Starting)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.run_installer(window, cx)
                                                })),
                                        )
                                    }),
                                ),
                        )
                        .when_some(
                            match &installer {
                                InstallerState::Started(prefix) if windows => Some(prefix.clone()),
                                _ => None,
                            },
                            |form, prefix| {
                                form.child(
                                    field().label_indent(false).child(
                                        Alert::info(
                                            "sideload-installer-started",
                                            t_args(
                                                "library.sideload.installer_started.body",
                                                &[("prefix", &prefix)],
                                            ),
                                        )
                                        .title(t("library.sideload.installer_started.title")),
                                    ),
                                )
                            },
                        )
                        .child(
                            field()
                                .label(t("library.sideload.field.arguments"))
                                .child(Input::new(&self.arguments)),
                        )
                        .child(
                            field()
                                .label(t("library.sideload.field.install_folder"))
                                .description(t("library.sideload.install_folder_hint"))
                                .child(self.path_row(
                                    "browse-folder",
                                    |this| &this.install_folder,
                                    true,
                                    cx,
                                )),
                        )
                    })
                    .when(browser, |form| {
                        form.child(
                            field()
                                .label(t("library.sideload.field.address"))
                                .child(Input::new(&self.url)),
                        )
                        .child(
                            field()
                                .label(t("library.sideload.field.user_agent"))
                                .description(t("library.sideload.user_agent_hint"))
                                .child(Input::new(&self.user_agent)),
                        )
                        .child(
                            field().label_indent(false).child(
                                Switch::new("sideload-fullscreen")
                                    .label(t("library.sideload.fullscreen"))
                                    .checked(self.fullscreen)
                                    .on_change(cx.listener(|this, v: &bool, _, cx| {
                                        this.fullscreen = *v;
                                        cx.notify();
                                    })),
                            ),
                        )
                    })
                    .child(
                        field()
                            .label(t("library.common.cover_art"))
                            .child(self.art_row(
                                "browse-cover",
                                |this| &this.cover,
                                ArtKind::Cover,
                                cx,
                            )),
                    )
                    .child(field().label(t("library.sideload.field.background")).child(
                        self.art_row(
                            "browse-background",
                            |this| &this.background,
                            ArtKind::Background,
                            cx,
                        ),
                    )),
            )
            .when_some(self.error.clone(), |this, e| {
                this.child(Alert::error("sideload-error", e))
            })
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .min_w_0()
                            .child(t("library.sideload.art_hint")),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("sideload-cancel")
                                    .ghost()
                                    .label(t("library.common.cancel"))
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("sideload-save")
                                    .primary()
                                    .label(if adding {
                                        t("library.sideload.add")
                                    } else {
                                        t("library.common.save")
                                    })
                                    .loading(self.saving)
                                    .disabled(self.saving)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.save(window, cx)),
                                    ),
                            ),
                    ),
            )
    }
}

/// Error line for an installer that could not be started.
fn installer_error(error: &anyhow::Error) -> SharedString {
    t_args(
        "library.sideload.error.installer",
        &[("error", &format!("{error:#}"))],
    )
}

fn open(editing: Option<String>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        cx.on_release(|this: &mut SideloadDialog, cx| this.discard_provisional(cx))
            .detach();
        SideloadDialog::new(editing, window, cx)
    });
    let width = dialog_width(window, 40.);
    let focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if focus.replace(false) {
            let input = view.read(cx).title.clone();
            input.focus_handle(cx).focus(window, cx);
        }
        let title = view.read(cx).dialog_title(cx);
        dialog.title(title).w(width).child(view.clone())
    });
}

/// Open the "Add game" dialog for an app that is not from a store.
pub fn open_add_game_dialog(window: &mut Window, cx: &mut App) {
    open(None, window, cx);
}

/// Open the edit dialog of a manually added app.
pub(crate) fn open_edit_app_dialog(app_name: String, window: &mut Window, cx: &mut App) {
    open(Some(app_name), window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_modes() {
        assert_eq!(save_mode(None, None), SaveMode::Add);
        assert_eq!(save_mode(None, Some("p")), SaveMode::Finish("p".into()));
        assert_eq!(save_mode(Some("e"), Some("p")), SaveMode::Edit("e".into()));
    }

    #[test]
    fn suggests_titles() {
        assert_eq!(
            title_from_executable(Path::new("/games/Hollow_Knight.exe")).as_deref(),
            Some("Hollow Knight")
        );
        assert_eq!(
            title_from_executable(Path::new("/opt/celeste/Celeste.bin.x86_64")).as_deref(),
            Some("Celeste bin")
        );
        assert_eq!(title_from_executable(Path::new("/")), None);
    }
}
