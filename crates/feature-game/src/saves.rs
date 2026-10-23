//! Cloud saves: save location (detected or chosen; one per named location
//! for GOG), manual sync in every direction, and automatic sync around play
//! sessions.

use std::sync::Arc;

use ember_core::{CloudSaveLocation, GameId, GameInfo, InstallPlatform, Runner, SyncDirection};
use ember_ui::i18n::{t, t_args};
use ember_ui::{backend, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::select::{Select, SelectEvent};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, rems,
};
use serde_json::Value;

use crate::choice::{Choice, ChoiceSelect, choice_select};
use crate::ops::{notify_error, open_path, pick_path};

/// Why cloud saves are or aren't available for a game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudSupport {
    Supported,
    NotInstalled,
    StoreUnsupported,
    GameUnsupported,
    /// GOG cloud saves only exist for the Windows build.
    NativeBuild,
}

impl CloudSupport {
    pub fn of(game: &GameInfo, store_supports: bool) -> Self {
        if !store_supports || game.runner() == Runner::Sideload {
            CloudSupport::StoreUnsupported
        } else if !game.cloud_save_enabled {
            CloudSupport::GameUnsupported
        } else if !game.is_installed {
            CloudSupport::NotInstalled
        } else if game.runner() == Runner::Gog
            && game.installed_platform() == Some(InstallPlatform::Linux)
        {
            CloudSupport::NativeBuild
        } else {
            CloudSupport::Supported
        }
    }

    /// Whether the game offers a cloud-saves entry at all. Stores without
    /// cloud saves and games that don't use them have none; the other
    /// unsupported states show it disabled with the explanation.
    pub fn offers_sheet(self) -> bool {
        !matches!(
            self,
            CloudSupport::StoreUnsupported | CloudSupport::GameUnsupported
        )
    }

    pub fn explanation(self, game: &GameInfo) -> Option<String> {
        match self {
            CloudSupport::Supported => None,
            CloudSupport::NotInstalled => Some(t("game.saves.not_installed").to_string()),
            CloudSupport::StoreUnsupported => Some(
                t_args(
                    "game.saves.store_unsupported",
                    &[("store", game.runner().store_name())],
                )
                .to_string(),
            ),
            CloudSupport::GameUnsupported => Some(t("game.saves.game_unsupported").to_string()),
            CloudSupport::NativeBuild => Some(t("game.saves.native_build").to_string()),
        }
    }
}

/// Name GOG uses when a game declares no named locations.
const GOG_DEFAULT_LOCATION: &str = "__default";

/// Save-location names GOG knows for a game, in its order.
pub(crate) fn location_names(game: &GameInfo) -> Vec<String> {
    let names: Vec<String> = game
        .gog_save_locations
        .iter()
        .map(|l| l.name.clone())
        .filter(|n| !n.is_empty())
        .collect();
    if names.is_empty() {
        vec![GOG_DEFAULT_LOCATION.to_string()]
    } else {
        names
    }
}

/// Decode a GOG save path as the store writes it: one bare path, or
/// `name=path` lines. Bare paths take the name of the location at their
/// position; unknown names are not split off (a `=` may be part of a path).
pub(crate) fn decode_locations(encoded: &str, names: &[String]) -> Vec<CloudSaveLocation> {
    let mut out: Vec<CloudSaveLocation> = Vec::new();
    for (i, line) in encoded
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .enumerate()
    {
        let named = line
            .split_once('=')
            .filter(|(n, _)| names.iter().any(|k| k == n) || *n == GOG_DEFAULT_LOCATION);
        let (name, location) = match named {
            Some((n, p)) => (n.to_string(), p.trim().to_string()),
            None => (
                names
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| GOG_DEFAULT_LOCATION.to_string()),
                line.to_string(),
            ),
        };
        if !out.iter().any(|l| l.name == name) {
            out.push(CloudSaveLocation { name, location });
        }
    }
    out
}

/// Known location names that have no row yet (offered by "Add location").
pub(crate) fn missing_names(known: &[String], present: &[String]) -> Vec<String> {
    known
        .iter()
        .filter(|n| !present.contains(n))
        .cloned()
        .collect()
}

/// Display label of a GOG location name.
pub(crate) fn location_label(name: &str) -> String {
    if name == GOG_DEFAULT_LOCATION || name.is_empty() {
        return t("game.saves.default_location").to_string();
    }
    let mut c = name.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// The `gogSaves` value for these rows: rows without a path are left out,
/// and no rows clears the override.
pub(crate) fn gog_saves_value(rows: &[CloudSaveLocation]) -> Option<Value> {
    let kept: Vec<&CloudSaveLocation> = rows
        .iter()
        .filter(|l| !l.location.trim().is_empty())
        .collect();
    if kept.is_empty() {
        return None;
    }
    serde_json::to_value(
        kept.iter()
            .map(|l| CloudSaveLocation {
                name: l.name.clone(),
                location: l.location.trim().to_string(),
            })
            .collect::<Vec<_>>(),
    )
    .ok()
}

fn direction_value(d: SyncDirection) -> &'static str {
    match d {
        SyncDirection::Auto => "auto",
        SyncDirection::Download => "download",
        SyncDirection::Upload => "upload",
        SyncDirection::ForceDownload => "force-download",
        SyncDirection::ForceUpload => "force-upload",
    }
}

fn direction_label(d: SyncDirection) -> SharedString {
    t(match d {
        SyncDirection::Auto => "game.saves.direction.auto",
        SyncDirection::Download => "game.saves.direction.download",
        SyncDirection::Upload => "game.saves.direction.upload",
        SyncDirection::ForceDownload => "game.saves.direction.force_download",
        SyncDirection::ForceUpload => "game.saves.direction.force_upload",
    })
}

/// One GOG save location being edited.
struct LocationRow {
    name: String,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// Cloud-save controls of one game. Embeddable; also shown as a sheet by
/// [`open_cloud_saves_sheet`]. Epic and others edit one save folder
/// (`savesPath`); GOG edits one folder per named location (`gogSaves`).
pub struct CloudSavesPanel {
    id: GameId,
    game: Arc<GameInfo>,
    support: CloudSupport,
    path: Entity<InputState>,
    /// GOG rows; `None` for single-folder stores.
    locations: Option<Vec<LocationRow>>,
    direction: SyncDirection,
    direction_select: Entity<ChoiceSelect>,
    detecting: bool,
    syncing: bool,
    output: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

/// Open the cloud-save sheet of a game.
pub fn open_cloud_saves_sheet(id: GameId, window: &mut Window, cx: &mut App) {
    let Some(game) = backend(cx).game(&id) else {
        return;
    };
    let title = t_args("game.saves.sheet_title", &[("title", &game.title)]);
    let panel = cx.new(|cx| CloudSavesPanel::new(game, window, cx));
    window.open_sheet(cx, move |sheet, _, _| {
        sheet
            .title(title.clone())
            .size(rems(30.))
            .child(panel.clone())
    });
}

impl CloudSavesPanel {
    pub fn new(game: Arc<GameInfo>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let b = backend(cx);
        let id = game.id();
        let store_supports = b.store(id.runner).is_some_and(|s| s.supports_cloud_saves());
        let support = CloudSupport::of(&game, store_supports);
        let settings = b.settings.game(&id);
        let saved = settings.saves_path.clone();
        let path = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t("game.saves.save_folder"))
                .default_value(saved.clone())
        });
        let settings_model = models(cx).settings.clone();
        let items = SyncDirection::ALL
            .iter()
            .map(|d| Choice::new(direction_value(*d), direction_label(*d)))
            .collect();
        let direction_select = choice_select(items, Some("auto"), window, cx);
        let subscriptions = vec![
            cx.subscribe_in(&path, window, |this, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    this.persist_path(cx);
                }
            }),
            cx.subscribe_in(
                &direction_select,
                window,
                |this, _, e: &SelectEvent<Vec<Choice>>, _, cx| {
                    let SelectEvent::Confirm(Some(v)) = e else {
                        return;
                    };
                    if let Some(d) = SyncDirection::ALL
                        .iter()
                        .find(|d| direction_value(**d) == v.as_ref())
                    {
                        this.direction = *d;
                        cx.notify();
                    }
                },
            ),
            cx.observe(&settings_model, |_, _, cx| cx.notify()),
        ];
        let is_gog = id.runner == Runner::Gog;
        let mut this = Self {
            id,
            game,
            support,
            path,
            locations: is_gog.then(Vec::new),
            direction: SyncDirection::Auto,
            direction_select,
            detecting: false,
            syncing: false,
            output: None,
            _subscriptions: subscriptions,
        };
        let initial = if !is_gog {
            None
        } else if !settings.gog_saves.is_empty() {
            Some(settings.gog_saves.clone())
        } else if !saved.trim().is_empty() {
            Some(decode_locations(&saved, &location_names(&this.game)))
        } else {
            None
        };
        if let Some(rows) = initial {
            this.set_rows(rows, window, cx);
        }
        if support == CloudSupport::Supported && !this.has_any_path(cx) {
            this.detect(window, cx);
        }
        this
    }

    fn has_any_path(&self, cx: &App) -> bool {
        match &self.locations {
            Some(rows) => rows
                .iter()
                .any(|r| !r.input.read(cx).value().trim().is_empty()),
            None => !self.path.read(cx).value().trim().is_empty(),
        }
    }

    /// Every location has a folder (sync and auto-sync need all of them).
    fn is_complete(&self, cx: &App) -> bool {
        match &self.locations {
            Some(rows) => {
                !rows.is_empty()
                    && rows
                        .iter()
                        .all(|r| !r.input.read(cx).value().trim().is_empty())
            }
            None => !self.path.read(cx).value().trim().is_empty(),
        }
    }

    fn row_values(&self, cx: &App) -> Vec<CloudSaveLocation> {
        self.locations
            .iter()
            .flatten()
            .map(|r| CloudSaveLocation {
                name: r.name.clone(),
                location: r.input.read(cx).value().trim().to_string(),
            })
            .collect()
    }

    fn new_row(
        name: String,
        location: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> LocationRow {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t("game.saves.location_placeholder"))
                .default_value(location)
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                this.persist_path(cx);
            }
        });
        LocationRow {
            name,
            input,
            _subscription: subscription,
        }
    }

    fn set_rows(
        &mut self,
        rows: Vec<CloudSaveLocation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = rows
            .into_iter()
            .map(|l| Self::new_row(l.name, l.location, window, cx))
            .collect();
        self.locations = Some(rows);
        cx.notify();
    }

    fn add_row(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let row = Self::new_row(name, String::new(), window, cx);
        if let Some(rows) = self.locations.as_mut() {
            rows.push(row);
        }
        cx.notify();
    }

    fn remove_row(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(rows) = self.locations.as_mut() {
            rows.retain(|r| r.name != name);
        }
        self.persist_path(cx);
        cx.notify();
    }

    fn persist_path(&self, cx: &mut Context<Self>) {
        let b = backend(cx);
        if self.locations.is_some() {
            let value = gog_saves_value(&self.row_values(cx));
            let current = serde_json::to_value(&b.settings.game(&self.id).gog_saves).ok();
            let unchanged = match (&value, &current) {
                (None, Some(Value::Array(a))) => a.is_empty(),
                (v, c) => v == c,
            };
            if !unchanged {
                b.settings.set_game_value(&self.id, "gogSaves", value);
            }
            return;
        }
        let value = self.path.read(cx).value().trim().to_string();
        if b.settings.game(&self.id).saves_path == value {
            return;
        }
        b.settings.set_game_value(
            &self.id,
            "savesPath",
            (!value.is_empty()).then_some(Value::String(value)),
        );
    }

    /// Forget the chosen folders and detect the store's defaults again.
    pub fn reset_to_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let b = backend(cx);
        b.settings.set_game_value(&self.id, "gogSaves", None);
        b.settings.set_game_value(&self.id, "savesPath", None);
        self.path.update(cx, |s, cx| s.set_value("", window, cx));
        if self.locations.is_some() {
            self.locations = Some(Vec::new());
        }
        self.detect(window, cx);
    }

    /// Ask the store for the default save folder(s) (inside the prefix).
    pub fn detect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.detecting = true;
        cx.notify();
        let b = backend(cx);
        let id = self.id.clone();
        let game = self.game.clone();
        let task = ember_ui::run(cx, async move {
            let store = b
                .store(id.runner)
                .ok_or_else(|| crate::i18n::store_unavailable(id.runner))?;
            let prefix =
                ember_runtime::prefix_path(&b.settings.app(), &b.settings.game(&id), &game);
            let path = store.default_save_path(&id.app_name, Some(&prefix)).await?;
            // Detection may have taught the store the location names.
            let names = location_names(&b.game(&id).unwrap_or(game));
            Ok((path, names))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.detecting = false;
                match result {
                    Ok((path, names)) if !path.trim().is_empty() => {
                        if this.locations.is_some() {
                            let mut rows = decode_locations(&path, &names);
                            for name in missing_names(
                                &names,
                                &rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
                            ) {
                                rows.push(CloudSaveLocation {
                                    name,
                                    location: String::new(),
                                });
                            }
                            this.set_rows(rows, window, cx);
                        } else {
                            this.path.update(cx, |s, cx| s.set_value(path, window, cx));
                        }
                        this.persist_path(cx);
                    }
                    Ok((_, names)) => {
                        if let Some(rows) = this.locations.as_ref()
                            && rows.is_empty()
                        {
                            let blank = names
                                .into_iter()
                                .map(|name| CloudSaveLocation {
                                    name,
                                    location: String::new(),
                                })
                                .collect();
                            this.set_rows(blank, window, cx);
                        }
                        notify_error(
                            window,
                            cx,
                            t("game.saves.not_found_title"),
                            &anyhow::anyhow!(t("game.saves.not_found_hint")),
                        )
                    }
                    Err(e) => notify_error(window, cx, t("game.saves.detect_failed"), &e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Choose the folder of the single save path (`row == None`) or of the
    /// named GOG location.
    fn browse(&mut self, row: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let pick = pick_path(window, cx, true, t("game.common.select"));
        cx.spawn_in(window, async move |this, cx| {
            if let Some(path) = pick.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    let value = path.to_string_lossy().into_owned();
                    let input = match &row {
                        None => Some(this.path.clone()),
                        Some(name) => this
                            .locations
                            .iter()
                            .flatten()
                            .find(|r| &r.name == name)
                            .map(|r| r.input.clone()),
                    };
                    if let Some(input) = input {
                        input.update(cx, |s, cx| s.set_value(value, window, cx));
                    }
                    this.persist_path(cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Sync now in the chosen direction. Forced directions overwrite one side
    /// and ask first.
    pub fn request_sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let direction = self.direction;
        let warning = match direction {
            SyncDirection::ForceDownload => Some((
                t("game.saves.replace_local_title"),
                t("game.saves.replace_local_description"),
                t("game.saves.replace_local"),
            )),
            SyncDirection::ForceUpload => Some((
                t("game.saves.replace_cloud_title"),
                t("game.saves.replace_cloud_description"),
                t("game.saves.replace_cloud"),
            )),
            _ => None,
        };
        match warning {
            None => self.sync(window, cx),
            Some((title, description, ok)) => {
                let this = cx.entity();
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let this = this.clone();
                    alert
                        .title(title.clone())
                        .description(description.clone())
                        .confirm()
                        .ok_text(ok.clone())
                        .ok_variant(ButtonVariant::Danger)
                        .on_ok(move |_, window, cx| {
                            this.update(cx, |p, cx| p.sync(window, cx));
                            true
                        })
                });
            }
        }
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.persist_path(cx);
        self.syncing = true;
        self.output = None;
        cx.notify();
        let b = backend(cx);
        let id = self.id.clone();
        let direction = self.direction;
        let task = ember_ui::run(cx, async move {
            if !b.is_online() {
                anyhow::bail!(t("game.saves.offline"));
            }
            b.sync_saves(&id, direction).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.syncing = false;
                match result {
                    Ok(out) => {
                        let out = out.trim();
                        this.output = Some(if out.is_empty() {
                            t("game.saves.in_sync")
                        } else {
                            out.to_string().into()
                        });
                    }
                    Err(e) => notify_error(window, cx, t("game.saves.sync_failed"), &e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_auto_sync(&mut self, on: bool, cx: &mut Context<Self>) {
        backend(cx)
            .settings
            .set_game_value(&self.id, "autoSyncSaves", Some(Value::Bool(on)));
        cx.notify();
    }
}

impl CloudSavesPanel {
    fn section_title(title: SharedString, cx: &App) -> gpui_kit::AnyElement {
        div()
            .text_sm()
            .font_semibold()
            .text_color(cx.theme().muted_foreground)
            .child(title)
            .into_any_element()
    }

    /// The single save folder of Epic and other stores.
    fn render_single(&self, busy: bool, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let has_path = !self.path.read(cx).value().trim().is_empty();
        let path_value = self.path.read(cx).value();
        v_flex()
            .gap_2()
            .child(Self::section_title(t("game.saves.save_folder"), cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.path).disabled(busy)),
                    )
                    .child(
                        Button::new("saves-browse")
                            .outline()
                            .icon(IconName::FolderOpen)
                            .tooltip(t("game.common.choose_folder"))
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.browse(None, window, cx)),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("saves-detect")
                            .small()
                            .ghost()
                            .icon(IconName::Search)
                            .label(t("game.saves.detect"))
                            .loading(self.detecting)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.detect(window, cx))),
                    )
                    .child(
                        Button::new("saves-open")
                            .small()
                            .ghost()
                            .icon(IconName::Folder)
                            .label(t("game.common.open_folder"))
                            .disabled(!has_path)
                            .on_click(move |_, window, cx| {
                                open_path(
                                    ember_core::paths::expand_home(path_value.trim()),
                                    window,
                                    cx,
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    /// One folder per GOG save location, with add, remove and reset.
    fn render_locations(&self, busy: bool, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let rows = self.locations.as_deref().unwrap_or_default();
        let present: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
        let addable = missing_names(&location_names(&self.game), &present);
        let muted = cx.theme().muted_foreground;
        let mut col = v_flex()
            .gap_3()
            .child(Self::section_title(t("game.saves.save_folders"), cx));
        if rows.is_empty() && !self.detecting {
            col = col.child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(t("game.saves.no_folders")),
            );
        }
        for (ix, row) in rows.iter().enumerate() {
            let value = row.input.read(cx).value();
            let has_path = !value.trim().is_empty();
            let browse_name = row.name.clone();
            let remove_name = row.name.clone();
            col = col.child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(location_label(&row.name)),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&row.input).disabled(busy)),
                            )
                            .child(
                                Button::new(("saves-row-browse", ix))
                                    .outline()
                                    .icon(IconName::FolderOpen)
                                    .tooltip(t("game.common.choose_folder"))
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.browse(Some(browse_name.clone()), window, cx)
                                    })),
                            )
                            .child(
                                Button::new(("saves-row-open", ix))
                                    .ghost()
                                    .icon(IconName::Folder)
                                    .tooltip(t("game.common.open_folder"))
                                    .disabled(!has_path)
                                    .on_click(move |_, window, cx| {
                                        open_path(
                                            ember_core::paths::expand_home(value.trim()),
                                            window,
                                            cx,
                                        )
                                    }),
                            )
                            .child(
                                Button::new(("saves-row-remove", ix))
                                    .ghost()
                                    .icon(IconName::X)
                                    .tooltip(t("game.saves.remove_location"))
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_row(&remove_name, cx)
                                    })),
                            ),
                    ),
            );
        }
        let this = cx.entity();
        col.child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .when(!addable.is_empty(), |row| {
                    row.child(
                        Button::new("saves-add")
                            .small()
                            .ghost()
                            .icon(IconName::Plus)
                            .label(t("game.saves.add_location"))
                            .disabled(busy)
                            .dropdown_menu(move |mut menu, _, _| {
                                for name in &addable {
                                    let this = this.clone();
                                    let n = name.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(location_label(name)).on_click(
                                            move |_, window, cx| {
                                                this.update(cx, |p, cx| {
                                                    p.add_row(n.clone(), window, cx)
                                                })
                                            },
                                        ),
                                    );
                                }
                                menu
                            }),
                    )
                })
                .child(
                    Button::new("saves-reset")
                        .small()
                        .ghost()
                        .icon(IconName::RotateCcw)
                        .label(t("game.saves.reset"))
                        .loading(self.detecting)
                        .disabled(busy)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.reset_to_defaults(window, cx)),
                        ),
                ),
        )
        .into_any_element()
    }
}

impl Render for CloudSavesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(text) = self.support.explanation(&self.game) {
            return v_flex()
                .py_2()
                .child(Alert::info("cloud-unsupported", text))
                .into_any_element();
        }
        let b = backend(cx);
        let auto_sync = b.settings.game(&self.id).auto_sync_saves;
        let complete = self.is_complete(cx);
        let busy = self.syncing || self.detecting;
        let muted = cx.theme().muted_foreground;
        let folders = if self.locations.is_some() {
            self.render_locations(busy, cx)
        } else {
            self.render_single(busy, cx)
        };
        v_flex()
            .id("cloud-saves")
            .w_full()
            .gap_6()
            .py_2()
            .child(folders)
            .child(
                v_flex()
                    .gap_2()
                    .child(Self::section_title(t("game.saves.sync_now"), cx))
                    .child(Select::new(&self.direction_select).disabled(busy))
                    .child(
                        h_flex().child(
                            Button::new("saves-sync")
                                .outline()
                                .icon(IconName::RefreshCw)
                                .label(if self.syncing {
                                    t("game.saves.syncing")
                                } else {
                                    t("game.saves.sync")
                                })
                                .loading(self.syncing)
                                .disabled(busy || !complete)
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.request_sync(window, cx)
                                    }),
                                ),
                        ),
                    )
                    .when_some(self.output.clone(), |col, out| {
                        col.child(
                            div()
                                .p_2()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().muted)
                                .text_xs()
                                .font_family(cx.theme().mono_font_family.clone())
                                .child(out),
                        )
                    }),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Switch::new("saves-auto")
                            .label(t("game.saves.auto_sync"))
                            .checked(auto_sync)
                            .disabled(!complete)
                            .on_change(
                                cx.listener(|this, v: &bool, _, cx| this.set_auto_sync(*v, cx)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t("game.saves.auto_sync_help")),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_core::InstalledInfo;

    #[test]
    fn support_reasons() {
        let mut g = GameInfo {
            runner: Some(Runner::Gog),
            cloud_save_enabled: true,
            is_installed: true,
            install: Some(InstalledInfo {
                platform: InstallPlatform::Windows,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(CloudSupport::of(&g, true), CloudSupport::Supported);
        assert_eq!(CloudSupport::of(&g, false), CloudSupport::StoreUnsupported);
        if let Some(i) = g.install.as_mut() {
            i.platform = InstallPlatform::Linux;
        }
        assert_eq!(CloudSupport::of(&g, true), CloudSupport::NativeBuild);
        g.is_installed = false;
        assert_eq!(CloudSupport::of(&g, true), CloudSupport::NotInstalled);
        g.cloud_save_enabled = false;
        assert_eq!(CloudSupport::of(&g, true), CloudSupport::GameUnsupported);
        assert!(CloudSupport::GameUnsupported.explanation(&g).is_some());
    }

    #[test]
    fn sheet_entry_visibility() {
        assert!(CloudSupport::Supported.offers_sheet());
        assert!(CloudSupport::NotInstalled.offers_sheet());
        assert!(CloudSupport::NativeBuild.offers_sheet());
        assert!(!CloudSupport::StoreUnsupported.offers_sheet());
        assert!(!CloudSupport::GameUnsupported.offers_sheet());
        let sideload = GameInfo {
            runner: Some(Runner::Sideload),
            cloud_save_enabled: true,
            is_installed: true,
            ..Default::default()
        };
        assert!(!CloudSupport::of(&sideload, true).offers_sheet());
    }

    fn loc(name: &str, location: &str) -> CloudSaveLocation {
        CloudSaveLocation {
            name: name.into(),
            location: location.into(),
        }
    }

    #[test]
    fn gog_location_decoding() {
        let names = vec!["saves".to_string(), "settings".to_string()];
        assert_eq!(
            decode_locations("/a\n/b", &names),
            vec![loc("saves", "/a"), loc("settings", "/b")]
        );
        assert_eq!(
            decode_locations("settings=/s\nsaves=/x=y", &names),
            vec![loc("settings", "/s"), loc("saves", "/x=y")]
        );
        // Unknown names stay part of the path.
        assert_eq!(
            decode_locations("/p/a=b", &names),
            vec![loc("saves", "/p/a=b")]
        );
        assert_eq!(
            decode_locations("/only", &[]),
            vec![loc(GOG_DEFAULT_LOCATION, "/only")]
        );
        assert!(decode_locations("  \n", &names).is_empty());
    }

    #[test]
    fn gog_location_names_and_values() {
        crate::i18n::register();
        let mut g = GameInfo::default();
        assert_eq!(location_names(&g), vec![GOG_DEFAULT_LOCATION.to_string()]);
        g.gog_save_locations = vec![loc("saves", "<?X?>"), loc("settings", "<?Y?>")];
        assert_eq!(location_names(&g), vec!["saves", "settings"]);
        assert_eq!(
            missing_names(&location_names(&g), &["saves".into()]),
            vec!["settings"]
        );
        assert_eq!(location_label(GOG_DEFAULT_LOCATION), "Saves");
        assert_eq!(location_label("settings"), "Settings");
        assert_eq!(gog_saves_value(&[loc("saves", "  ")]), None);
        assert_eq!(
            gog_saves_value(&[loc("saves", " /a "), loc("settings", "")]),
            Some(serde_json::json!([{ "name": "saves", "location": "/a" }]))
        );
    }
}
