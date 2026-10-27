//! The settings form: renders registry fields bound to one settings target
//! and persists edits through the settings service.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use ember_core::settings::{AppSettings, GameSettings, WineInstallation};
use ember_core::{GameId, InstallPlatform, Paths};
use ember_ui::i18n::{t, t_args};
use ember_ui::{Route, backend, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IndexPath, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, ElementId, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Render,
    SharedString, Styled as _, Subscription, Task, Window, div,
};
use serde_json::Value;

use crate::controllers::ControllersPanel;
use crate::epic::{EglSyncPanel, EosOverlayPanel};
use crate::i18n::LanguageStamp;
use crate::registry::{
    self, ActionKind, ChoiceOption, CustomKind, FieldKind, FieldSection, FieldSpec, PanelKind,
    Scope, TextKind,
};
use crate::saves::SaveSyncPanel;
use crate::value::{
    effective_overrides, get_path, override_key, plan_game_write, remove_row, split_assignment,
    text_to_value, upsert_row, validate_discord_client_id, validate_env_key,
    validate_launcher_args, value_to_text, with_path,
};
use crate::widgets::{
    RowState, field_row, group_heading, group_panel, muted, panel, section_heading,
};

/// What a form edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormTarget {
    /// Application-wide fields (General, Appearance, Advanced).
    App,
    /// Launch settings every game inherits (`AppSettings::game`).
    Defaults,
    /// One game's sparse overrides on top of the defaults.
    Game(GameId),
}

/// Which fields a form shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FormFilter {
    Sections(Vec<FieldSection>),
    Query(String),
}

/// Select item: a stable key and a label.
#[derive(Debug, Clone)]
pub(crate) struct ChoiceItem {
    key: SharedString,
    label: SharedString,
}

impl ChoiceItem {
    pub(crate) fn new(key: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
        }
    }
}

impl SelectItem for ChoiceItem {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &SharedString {
        &self.key
    }
}

type ChoiceState = Entity<SelectState<Vec<ChoiceItem>>>;

/// Two-input row editor for env vars, wrappers and GOG save locations.
struct PairEditor {
    kind: CustomKind,
    first: Entity<InputState>,
    second: Entity<InputState>,
    editing: Option<usize>,
    error: Option<SharedString>,
}

impl PairEditor {
    /// JSON field names of a row.
    fn keys(kind: CustomKind) -> (&'static str, &'static str) {
        match kind {
            CustomKind::EnvVars => ("key", "value"),
            CustomKind::Wrappers => ("exe", "args"),
            _ => ("name", "location"),
        }
    }

    fn placeholders(kind: CustomKind) -> (SharedString, SharedString) {
        match kind {
            CustomKind::EnvVars => (t("settings.pairs.env_name"), t("settings.pairs.env_value")),
            CustomKind::Wrappers => (
                t("settings.pairs.wrapper_command"),
                t("settings.pairs.wrapper_arguments"),
            ),
            _ => (
                t("settings.pairs.save_name"),
                t("settings.pairs.save_folder"),
            ),
        }
    }
}

/// State of the Wine/Proton picker.
struct WinePicker {
    select: ChoiceState,
    builds: Option<Vec<WineInstallation>>,
    loading: bool,
}

/// The shared settings form.
///
/// Owns one retained input or select per field of its target, mirrors the
/// current settings snapshot (re-read whenever the settings model changes)
/// and writes edits through `SettingsService`: toggles and selects on
/// change, text fields on blur or Enter, sliders after a short pause.
/// For a game target, each field that differs from the global default shows
/// an "Overridden" marker with a reset button.
pub struct SettingsForm {
    target: FormTarget,
    fields: Vec<&'static FieldSpec>,
    /// Current values as JSON (`AppSettings` for App/Defaults, effective
    /// `GameSettings` for a game).
    snapshot: Value,
    /// Game target: global defaults as JSON and as settings.
    defaults_json: Value,
    defaults: GameSettings,
    effective: GameSettings,
    overridden: HashSet<String>,
    texts: HashMap<&'static str, Entity<InputState>>,
    choices: HashMap<&'static str, ChoiceState>,
    pairs: HashMap<&'static str, PairEditor>,
    sliders: HashMap<&'static str, Entity<SliderState>>,
    slider_writes: HashMap<&'static str, Task<()>>,
    /// The last value each slider reported. A drag reports every pointer
    /// move, mostly with an unchanged value.
    slider_seen: HashMap<&'static str, f32>,
    wine: Option<WinePicker>,
    errors: HashMap<&'static str, SharedString>,
    filter: FormFilter,
    busy: HashSet<ActionKind>,
    import_summary: Option<SharedString>,
    heroic_detected: bool,
    lutris_detected: bool,
    /// Self-contained panels (controllers, EOS overlay, EGL sync, save sync).
    panels: HashMap<&'static str, AnyView>,
    /// Game target: the game is installed as a native Linux build.
    native: bool,
    /// The shell installed the Epic store handle.
    epic_available: bool,
    /// Language of the retained placeholders and select items.
    language: LanguageStamp,
    _subscriptions: Vec<Subscription>,
}

/// The shared launch-settings form used by the game defaults page and by
/// per-game settings. Build it with [`SettingsForm::new`] and a
/// [`FormTarget::Defaults`] or [`FormTarget::Game`] target.
pub type GameSettingsForm = SettingsForm;

impl SettingsForm {
    /// Create a form for `target`, showing every section that applies to it.
    pub fn new(target: FormTarget, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let fields: Vec<&'static FieldSpec> = registry::FIELDS
            .iter()
            .filter(|f| f.supported_on_host())
            .filter(|f| match &target {
                FormTarget::App => f.scope == Scope::App,
                FormTarget::Defaults => f.applies_to_game(false),
                FormTarget::Game(_) => f.applies_to_game(true),
            })
            .collect();
        let sections = match target {
            FormTarget::App => FieldSection::APP.to_vec(),
            _ => FieldSection::GAME.to_vec(),
        };
        let mut this = Self {
            target,
            fields,
            snapshot: Value::Null,
            defaults_json: Value::Null,
            defaults: GameSettings::default(),
            effective: GameSettings::default(),
            overridden: HashSet::new(),
            texts: HashMap::new(),
            choices: HashMap::new(),
            pairs: HashMap::new(),
            sliders: HashMap::new(),
            slider_writes: HashMap::new(),
            slider_seen: HashMap::new(),
            wine: None,
            errors: HashMap::new(),
            filter: FormFilter::Sections(sections),
            busy: HashSet::new(),
            import_summary: None,
            heroic_detected: ember_ui::hooks::heroic_detected(),
            lutris_detected: ember_ui::hooks::lutris_detected(),
            panels: HashMap::new(),
            native: false,
            epic_available: crate::stores::epic(cx).is_some(),
            language: LanguageStamp::new(),
            _subscriptions: Vec::new(),
        };
        this.read_snapshot(cx);
        this.build_controls(window, cx);
        let settings_model = models(cx).settings.clone();
        this._subscriptions.push(
            cx.observe_in(&settings_model, window, |this, _, window, cx| {
                this.reload(window, cx)
            }),
        );
        if let FormTarget::Game(id) = &this.target {
            // Installing or moving the game can change whether it runs natively.
            let id = id.clone();
            let library = models(cx).library.clone();
            this._subscriptions
                .push(cx.observe(&library, move |this, _, cx| {
                    let native = backend(cx).game(&id).and_then(|g| g.installed_platform())
                        == Some(InstallPlatform::Linux);
                    if native != this.native {
                        this.native = native;
                        cx.notify();
                    }
                }));
        }
        this
    }

    /// The edited target.
    pub fn target(&self) -> &FormTarget {
        &self.target
    }

    /// Show only these sections (in this order).
    pub fn set_sections(&mut self, sections: Vec<FieldSection>, cx: &mut Context<Self>) {
        self.filter = FormFilter::Sections(sections);
        cx.notify();
    }

    /// Show the fields matching a search query, grouped by section.
    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.filter = FormFilter::Query(query.to_string());
        cx.notify();
    }

    /// Number of fields the current filter shows.
    pub fn visible_count(&self) -> usize {
        self.visible_fields().count()
    }

    /// Number of fields that differ from the global defaults (game target).
    pub fn overridden_count(&self) -> usize {
        self.overridden.len()
    }

    /// Load the Wine/Proton builds for the version picker (once).
    pub fn load_wine_versions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(wine) = &mut self.wine else { return };
        if wine.loading || wine.builds.is_some() {
            return;
        }
        wine.loading = true;
        let app = backend(cx).settings.app();
        let task = ember_ui::run(cx, async move {
            Ok(ember_runtime::wine_installations(&app).await)
        });
        cx.spawn_in(window, async move |this, cx| {
            let builds = task.await.unwrap_or_default();
            this.update_in(cx, |this, window, cx| {
                if let Some(wine) = &mut this.wine {
                    wine.loading = false;
                    wine.builds = Some(builds);
                }
                this.sync_wine_select(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Reset every override of the game target.
    pub fn reset_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let FormTarget::Game(id) = &self.target else {
            return;
        };
        // Replacing the whole map also drops stale entries equal to the defaults.
        backend(cx)
            .settings
            .replace_overrides(id, Default::default());
        self.reload(window, cx);
    }

    fn visible_fields(&self) -> impl Iterator<Item = &'static FieldSpec> + '_ {
        self.fields.iter().copied().filter(move |f| {
            if matches!(f.kind, FieldKind::Action(ActionKind::ImportHeroic))
                && !self.heroic_detected
            {
                return false;
            }
            if matches!(f.kind, FieldKind::Action(ActionKind::ImportLutris))
                && !self.lutris_detected
            {
                return false;
            }
            if matches!(
                f.kind,
                FieldKind::Panel(PanelKind::EosOverlay | PanelKind::EglSync)
            ) && !self.epic_available
            {
                return false;
            }
            if f.native_only && !self.native {
                return false;
            }
            match &self.filter {
                FormFilter::Sections(s) => s.contains(&f.section),
                FormFilter::Query(q) => f.matches(q),
            }
        })
    }

    // ---- snapshot ----

    fn read_snapshot(&mut self, cx: &mut Context<Self>) {
        let b = backend(cx);
        let app: Arc<AppSettings> = b.settings.app();
        match &self.target {
            FormTarget::App | FormTarget::Defaults => {
                self.snapshot = serde_json::to_value(&*app).unwrap_or_default();
            }
            FormTarget::Game(id) => {
                self.native =
                    b.game(id).and_then(|g| g.installed_platform()) == Some(InstallPlatform::Linux);
                let overrides = b.settings.overrides(id);
                self.defaults = app.game.clone();
                self.defaults_json = serde_json::to_value(&self.defaults).unwrap_or_default();
                self.effective = overrides.apply(&self.defaults);
                self.snapshot = serde_json::to_value(&self.effective).unwrap_or_default();
                self.overridden = effective_overrides(&self.defaults, &overrides)
                    .into_iter()
                    .collect();
            }
        }
    }

    fn value(&self, path: &str) -> Option<&Value> {
        get_path(&self.snapshot, path)
    }

    fn bool_value(&self, path: &str) -> bool {
        self.value(path).and_then(Value::as_bool).unwrap_or(false)
    }

    /// Whether this field differs from the global default (game target).
    fn is_overridden(&self, spec: &FieldSpec) -> bool {
        matches!(self.target, FormTarget::Game(_))
            && self.overridden.contains(spec.override_key())
            && get_path(&self.defaults_json, spec.path) != self.value(spec.path)
    }

    fn disabled_reason(&self, spec: &FieldSpec) -> Option<SharedString> {
        spec.requires
            .iter()
            .find(|c| self.bool_value(c.path) != c.equals)
            .map(|c| t(c.reason))
    }

    fn build_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for spec in self.fields.clone() {
            match spec.kind {
                FieldKind::Text(kind) => {
                    let text = if kind == TextKind::Secret {
                        String::new()
                    } else {
                        value_to_text(self.value(spec.path))
                    };
                    let placeholder = self.placeholder(spec, cx);
                    let state = cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(placeholder)
                            .masked(kind == TextKind::Secret)
                            .default_value(text)
                    });
                    let path = spec.path;
                    self._subscriptions.push(cx.subscribe_in(
                        &state,
                        window,
                        move |this, _, event: &InputEvent, window, cx| match event {
                            InputEvent::Blur | InputEvent::PressEnter { .. } => {
                                this.commit_text(path, window, cx)
                            }
                            InputEvent::Change => {
                                if this.errors.remove(path).is_some() {
                                    cx.notify();
                                }
                            }
                            InputEvent::Focus => {}
                        },
                    ));
                    self.texts.insert(spec.path, state);
                }
                FieldKind::Choice(options) => {
                    let items = choice_items(options);
                    let selected = self.choice_index(options, spec.path);
                    let state = cx.new(|cx| SelectState::new(items, selected, window, cx));
                    let path = spec.path;
                    self._subscriptions.push(cx.subscribe_in(
                        &state,
                        window,
                        move |this, _, event: &SelectEvent<Vec<ChoiceItem>>, window, cx| {
                            let SelectEvent::Confirm(Some(key)) = event else {
                                return;
                            };
                            let Some(FieldKind::Choice(options)) =
                                registry::find(path).map(|f| f.kind)
                            else {
                                return;
                            };
                            if let Some(o) = options.iter().find(|o| o.value.key() == key.as_ref())
                            {
                                this.write(path, o.value.to_json(), window, cx);
                            }
                        },
                    ));
                    self.choices.insert(spec.path, state);
                }
                FieldKind::Custom(kind) => self.build_custom(spec, kind, window, cx),
                FieldKind::Panel(kind) => self.build_panel(spec, kind, window, cx),
                FieldKind::Toggle | FieldKind::Action(_) => {}
            }
        }
    }

    fn build_panel(
        &mut self,
        spec: &'static FieldSpec,
        kind: PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view: Option<AnyView> = match kind {
            PanelKind::Controllers => Some(cx.new(ControllersPanel::new).into()),
            PanelKind::EosOverlay => crate::stores::epic(cx)
                .map(|epic| cx.new(|cx| EosOverlayPanel::new(epic, window, cx)).into()),
            PanelKind::EglSync => crate::stores::epic(cx)
                .map(|epic| cx.new(|cx| EglSyncPanel::new(epic, window, cx)).into()),
            PanelKind::SaveSync => match &self.target {
                FormTarget::Game(id) => {
                    let id = id.clone();
                    Some(cx.new(|cx| SaveSyncPanel::new(id, window, cx)).into())
                }
                _ => None,
            },
        };
        if let Some(view) = view {
            self.panels.insert(spec.path, view);
        }
    }

    fn placeholder(&self, spec: &FieldSpec, cx: &App) -> SharedString {
        match spec.kind {
            FieldKind::Text(TextKind::Secret)
                if !value_to_text(self.value(spec.path)).is_empty() =>
            {
                t("settings.field.secret_saved_placeholder")
            }
            FieldKind::Text(_) if spec.path == "winePrefix" => match &self.target {
                FormTarget::Game(id) => {
                    let b = backend(cx);
                    match b.game(id) {
                        Some(game) => ember_runtime::default_prefix(&b.settings.app(), &game)
                            .to_string_lossy()
                            .to_string()
                            .into(),
                        None => spec.placeholder_text(),
                    }
                }
                _ => spec.placeholder_text(),
            },
            _ => spec.placeholder_text(),
        }
    }

    fn choice_index(&self, options: &[ChoiceOption], path: &str) -> Option<IndexPath> {
        let v = self.value(path).cloned().unwrap_or(Value::Null);
        options
            .iter()
            .position(|o| o.value.matches(&v))
            .map(|ix| IndexPath::default().row(ix))
    }

    fn build_custom(
        &mut self,
        spec: &'static FieldSpec,
        kind: CustomKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match kind {
            CustomKind::WineVersion => {
                let select = cx.new(|cx| {
                    SelectState::new(Vec::<ChoiceItem>::new(), None, window, cx).searchable(true)
                });
                self._subscriptions.push(cx.subscribe_in(
                    &select,
                    window,
                    |this, _, event: &SelectEvent<Vec<ChoiceItem>>, window, cx| {
                        let SelectEvent::Confirm(Some(bin)) = event else {
                            return;
                        };
                        let build = this
                            .wine
                            .as_ref()
                            .and_then(|w| w.builds.as_ref())
                            .and_then(|b| b.iter().find(|w| w.bin == bin.as_ref()))
                            .cloned();
                        if let Some(build) = build
                            && let Ok(v) = serde_json::to_value(build)
                        {
                            this.write("wineVersion", v, window, cx);
                        }
                    },
                ));
                self.wine = Some(WinePicker {
                    select,
                    builds: None,
                    loading: false,
                });
                self.sync_wine_select(window, cx);
            }
            CustomKind::EnvVars | CustomKind::Wrappers | CustomKind::GogSaves => {
                let (p1, p2) = PairEditor::placeholders(kind);
                let first = cx.new(|cx| InputState::new(window, cx).placeholder(p1));
                let second = cx.new(|cx| InputState::new(window, cx).placeholder(p2));
                let path = spec.path;
                for input in [&first, &second] {
                    self._subscriptions.push(cx.subscribe_in(
                        input,
                        window,
                        move |this, _, event: &InputEvent, window, cx| {
                            if let InputEvent::PressEnter { .. } = event {
                                this.commit_pair(path, window, cx);
                            }
                        },
                    ));
                }
                self.pairs.insert(
                    spec.path,
                    PairEditor {
                        kind,
                        first,
                        second,
                        editing: None,
                        error: None,
                    },
                );
            }
            CustomKind::ThemeName => {
                let items = theme_items(cx);
                let current = value_to_text(self.value(spec.path));
                let selected = items
                    .iter()
                    .position(|i| i.key.as_ref() == current)
                    .map(|ix| IndexPath::default().row(ix));
                let state =
                    cx.new(|cx| SelectState::new(items, selected, window, cx).searchable(true));
                self._subscriptions.push(cx.subscribe_in(
                    &state,
                    window,
                    |this, _, event: &SelectEvent<Vec<ChoiceItem>>, window, cx| {
                        if let SelectEvent::Confirm(Some(name)) = event {
                            this.pick_theme(name, window, cx);
                        }
                    },
                ));
                self.choices.insert(spec.path, state);
            }
            CustomKind::FontSize | CustomKind::TileSize => {
                let (min, max, step) = if kind == CustomKind::FontSize {
                    (12., 24., 1.)
                } else {
                    (8., 16., 0.5)
                };
                let current = self
                    .value(spec.path)
                    .and_then(Value::as_f64)
                    .unwrap_or(min as f64) as f32;
                let state = cx.new(|_| {
                    SliderState::new()
                        .min(min)
                        .max(max)
                        .step(step)
                        .default_value(current)
                });
                let path = spec.path;
                self._subscriptions.push(cx.subscribe_in(
                    &state,
                    window,
                    move |this, _, event: &SliderEvent, window, cx| {
                        let (v, released) = match event {
                            SliderEvent::Change(v) => (v.start(), false),
                            SliderEvent::Release(v) => (v.start(), true),
                        };
                        this.slider_changed(path, v, released, window, cx);
                    },
                ));
                self.sliders.insert(spec.path, state);
            }
        }
    }

    /// Re-read settings and bring controls in line, leaving focused inputs alone.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.read_snapshot(cx);
        for (path, input) in &self.texts {
            let Some(spec) = registry::find(path) else {
                continue;
            };
            if spec.kind == FieldKind::Text(TextKind::Secret) {
                let placeholder = self.placeholder(spec, cx);
                input.update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
                continue;
            }
            if input.read(cx).focus_handle(cx).is_focused(window) {
                continue;
            }
            let text = value_to_text(get_path(&self.snapshot, path));
            if input.read(cx).value().as_ref() != text {
                input.update(cx, |s, cx| s.set_value(text, window, cx));
            }
        }
        for (path, state) in &self.choices {
            let Some(spec) = registry::find(path) else {
                continue;
            };
            let selected = match spec.kind {
                FieldKind::Choice(options) => self.choice_index(options, path),
                _ => {
                    let current = value_to_text(get_path(&self.snapshot, path));
                    state.update(cx, |s, cx| {
                        s.set_selected_value(&current.into(), window, cx)
                    });
                    continue;
                }
            };
            if state.read(cx).selected_index(cx) != selected {
                state.update(cx, |s, cx| s.set_selected_index(selected, window, cx));
            }
        }
        for (path, slider) in &self.sliders {
            if self.slider_writes.contains_key(path) {
                continue;
            }
            if let Some(v) = get_path(&self.snapshot, path).and_then(Value::as_f64) {
                let v = v as f32;
                if (slider.read(cx).value().start() - v).abs() > f32::EPSILON {
                    slider.update(cx, |s, cx| s.set_value(v, window, cx));
                }
            }
        }
        self.sync_wine_select(window, cx);
        cx.notify();
    }

    /// Rebuild the retained text (placeholders, select items) in the
    /// current interface language.
    fn retranslate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (path, input) in &self.texts {
            if let Some(spec) = registry::find(path) {
                let placeholder = self.placeholder(spec, cx);
                input.update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
            }
        }
        for editor in self.pairs.values() {
            let (first, second) = PairEditor::placeholders(editor.kind);
            editor
                .first
                .update(cx, |s, cx| s.set_placeholder(first, window, cx));
            editor
                .second
                .update(cx, |s, cx| s.set_placeholder(second, window, cx));
        }
        for (path, state) in &self.choices {
            let items = match registry::find(path).map(|f| f.kind) {
                Some(FieldKind::Choice(options)) => choice_items(options),
                Some(FieldKind::Custom(CustomKind::ThemeName)) => theme_items(cx),
                _ => continue,
            };
            state.update(cx, |s, cx| s.set_items(items, window, cx));
        }
        self.sync_wine_select(window, cx);
        cx.notify();
    }

    fn sync_wine_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current: WineInstallation = self
            .value("wineVersion")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let Some(wine) = &mut self.wine else { return };
        let mut items: Vec<ChoiceItem> = wine
            .builds
            .iter()
            .flatten()
            .map(|w| ChoiceItem {
                key: w.bin.clone().into(),
                label: w.name.clone().into(),
            })
            .collect();
        if current.is_set() && !items.iter().any(|i| i.key.as_ref() == current.bin) {
            let label = if wine.builds.is_some() {
                t_args("settings.wine.build_missing", &[("name", &current.name)])
            } else {
                current.name.clone().into()
            };
            items.insert(
                0,
                ChoiceItem {
                    key: current.bin.clone().into(),
                    label,
                },
            );
            if let Some(builds) = &mut wine.builds {
                builds.insert(0, current.clone());
            }
        }
        let key: SharedString = current.bin.into();
        wine.select.update(cx, |s, cx| {
            s.set_items(items, window, cx);
            s.set_selected_value(&key, window, cx);
        });
    }

    // ---- writes ----

    /// Persist a value at `path` for this form's target.
    fn write(
        &mut self,
        path: &'static str,
        value: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let b = backend(cx);
        let result = match &self.target {
            FormTarget::App | FormTarget::Defaults => {
                let mut result = Ok(());
                b.settings.update_app(|s| match with_path(s, path, value) {
                    Ok(next) => *s = next,
                    Err(e) => result = Err(e),
                });
                if result.is_ok() && path.starts_with("emberUi.") {
                    ember_ui::theme::apply(&b.settings.app().ui, Some(window), cx);
                }
                result
            }
            FormTarget::Game(id) => plan_game_write(&self.defaults, &self.effective, path, value)
                .map(|plan| b.settings.set_game_value(id, override_key(path), plan)),
        };
        if let Err(e) = result {
            window.push_notification(
                Notification::error(format!("{e:#}")).title(t("settings.error.save_setting")),
                cx,
            );
        }
        self.reload(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn write_for_test(
        &mut self,
        path: &'static str,
        value: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.write(path, value, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn reset_for_test(
        &mut self,
        path: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reset_field(path, window, cx);
    }

    /// Return a field to the global default (game target).
    fn reset_field(&mut self, path: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let FormTarget::Game(id) = &self.target else {
            return;
        };
        let key = override_key(path);
        if key == path {
            backend(cx).settings.set_game_value(id, key, None);
            self.errors.remove(path);
            self.reload(window, cx);
        } else if let Some(default) = get_path(&self.defaults_json, path).cloned() {
            self.write(path, default, window, cx);
        }
    }

    fn commit_text(&mut self, path: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(spec), Some(input)) = (registry::find(path), self.texts.get(path)) else {
            return;
        };
        let FieldKind::Text(kind) = spec.kind else {
            return;
        };
        let text = input.read(cx).value().to_string();
        if kind == TextKind::Secret && text.trim().is_empty() {
            return;
        }
        let checked = text_to_value(&text, kind).and_then(|v| {
            let s = v.as_str().unwrap_or_default();
            if path == "launcherArgs" {
                validate_launcher_args(s)?;
            }
            if path == "discordClientId" {
                validate_discord_client_id(s)?;
            }
            Ok(v)
        });
        match checked {
            Err(e) => {
                self.errors.insert(path, e.into());
                cx.notify();
            }
            Ok(v) => {
                self.errors.remove(path);
                if kind == TextKind::Secret {
                    input.update(cx, |s, cx| s.set_value("", window, cx));
                }
                if self.value(path) != Some(&v) {
                    self.write(path, v, window, cx);
                } else {
                    cx.notify();
                }
            }
        }
    }

    fn pick_path(
        &mut self,
        path: &'static str,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: !directory,
            directories: directory,
            multiple: false,
            prompt: Some(if directory {
                t("settings.picker.select_folder")
            } else {
                t("settings.picker.select_file")
            }),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = match rx.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(e)) => {
                    this.update_in(cx, |_, window, cx| {
                        window.push_notification(
                            Notification::error(format!("{e:#}"))
                                .title(t("settings.error.file_picker")),
                            cx,
                        );
                    })
                    .ok();
                    None
                }
                _ => None,
            };
            if let Some(p) = picked {
                this.update_in(cx, |this, window, cx| {
                    if let Some(input) = this.texts.get(path) {
                        input.update(cx, |s, cx| {
                            s.set_value(p.to_string_lossy().to_string(), window, cx)
                        });
                    }
                    this.commit_text(path, window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// A slider moved (`released` once the drag ends, or for a reset).
    fn slider_changed(
        &mut self,
        path: &'static str,
        v: f32,
        released: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !released && self.slider_seen.get(path) == Some(&v) {
            return;
        }
        self.slider_seen.insert(path, v);
        if path == "emberUi.fontSize" {
            // The interface size rescales this page, slider included: applied
            // mid-drag, the track would move under the pointer and fight it.
            // The percentage follows the drag; the size applies on release.
            if released {
                self.slider_writes.remove(path);
                self.write(path, Value::from(v as f64), window, cx);
            } else {
                // Keeps `reload` from snapping the slider back meanwhile.
                self.slider_writes.insert(path, Task::ready(()));
                cx.notify();
            }
            return;
        }
        let task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            this.update_in(cx, |this, window, cx| {
                this.slider_writes.remove(path);
                this.write(path, Value::from(v as f64), window, cx);
            })
            .ok();
        });
        self.slider_writes.insert(path, task);
        cx.notify();
    }

    /// Use the theme called `name`. A theme for the other colour mode than
    /// the one showing switches the mode too, so picking it always shows it.
    fn pick_theme(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dark) = ember_ui::theme::is_dark_theme(name, cx)
            && dark != cx.theme().mode.is_dark()
        {
            let mode = if dark { "dark" } else { "light" };
            self.write("emberUi.theme", Value::from(mode), window, cx);
        }
        self.write(
            "emberUi.themeName",
            Value::String(name.to_string()),
            window,
            cx,
        );
    }

    fn rows(&self, path: &str) -> Vec<(String, String)> {
        let kind = self
            .pairs
            .get(path)
            .map(|p| p.kind)
            .unwrap_or(CustomKind::EnvVars);
        let (k1, k2) = PairEditor::keys(kind);
        self.value(path)
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|r| (value_to_text(r.get(k1)), value_to_text(r.get(k2))))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn commit_pair(&mut self, path: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.rows(path);
        let Some(editor) = self.pairs.get_mut(path) else {
            return;
        };
        let (first, second) = split_assignment(
            &editor.first.read(cx).value(),
            &editor.second.read(cx).value(),
        );
        let existing: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
        let checked = match editor.kind {
            CustomKind::EnvVars => validate_env_key(&first, &existing, editor.editing),
            CustomKind::Wrappers if first.is_empty() => {
                Err(t("settings.validate.wrapper_empty").to_string())
            }
            CustomKind::GogSaves if first.is_empty() || second.is_empty() => {
                Err(t("settings.validate.save_location_empty").to_string())
            }
            _ => Ok(first),
        };
        match checked {
            Err(e) => {
                editor.error = Some(e.into());
                cx.notify();
            }
            Ok(first) => {
                let (k1, k2) = PairEditor::keys(editor.kind);
                let row = serde_json::json!({ k1: first, k2: second });
                let current = self
                    .value(path)
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new()));
                let editor = self.pairs.get_mut(path).expect("editor exists");
                let next = upsert_row(&current, editor.editing, row);
                editor.editing = None;
                editor.error = None;
                editor.first.update(cx, |s, cx| s.set_value("", window, cx));
                editor
                    .second
                    .update(cx, |s, cx| s.set_value("", window, cx));
                self.write(path, next, window, cx);
            }
        }
    }

    fn edit_row(
        &mut self,
        path: &'static str,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.rows(path);
        let Some((a, b)) = rows.get(ix).cloned() else {
            return;
        };
        let Some(editor) = self.pairs.get_mut(path) else {
            return;
        };
        editor.editing = Some(ix);
        editor.error = None;
        editor.first.update(cx, |s, cx| s.set_value(a, window, cx));
        editor.second.update(cx, |s, cx| s.set_value(b, window, cx));
        editor.first.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn cancel_edit(&mut self, path: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.pairs.get_mut(path) else {
            return;
        };
        editor.editing = None;
        editor.error = None;
        editor.first.update(cx, |s, cx| s.set_value("", window, cx));
        editor
            .second
            .update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    fn remove_row(
        &mut self,
        path: &'static str,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .value(path)
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        if let Some(editor) = self.pairs.get_mut(path)
            && editor.editing == Some(ix)
        {
            editor.editing = None;
        }
        self.write(path, remove_row(&current, ix), window, cx);
    }

    // ---- actions ----

    fn run_action(&mut self, action: ActionKind, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            ActionKind::OpenLogsFolder => {
                let dir = Paths::get().logs_dir();
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    window.push_notification(
                        Notification::error(format!("{e}")).title(t("settings.error.logs_folder")),
                        cx,
                    );
                    return;
                }
                cx.open_with_system(&dir);
            }
            ActionKind::ClearCaches => {
                let weak = cx.entity().downgrade();
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let weak = weak.clone();
                    alert
                        .title(t("settings.clear_caches.title"))
                        .description(t("settings.clear_caches.description"))
                        .show_cancel(true)
                        .cancel_text(t("settings.common.cancel"))
                        .ok_text(t("settings.clear_caches.confirm"))
                        .on_ok(move |_, window, cx| {
                            weak.update(cx, |this, cx| this.clear_caches(window, cx))
                                .ok();
                            true
                        })
                });
            }
            ActionKind::ResetSettings => {
                let weak = cx.entity().downgrade();
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let weak = weak.clone();
                    alert
                        .title(t("settings.reset_settings.title"))
                        .description(t("settings.reset_settings.description"))
                        .show_cancel(true)
                        .cancel_text(t("settings.common.cancel"))
                        .ok_text(t("settings.reset_settings.confirm"))
                        .ok_variant(ButtonVariant::Danger)
                        .on_ok(move |_, window, cx| {
                            weak.update(cx, |this, cx| this.reset_settings(window, cx))
                                .ok();
                            true
                        })
                });
            }
            ActionKind::ImportHeroic => self.import_heroic(window, cx),
            ActionKind::ImportLutris => self.import_lutris(window, cx),
        }
    }

    fn clear_caches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.busy.insert(ActionKind::ClearCaches);
        let task = ember_ui::run(cx, async move {
            let paths = Paths::get();
            for dir in [paths.images_dir(), paths.cache.join("stores")] {
                match tokio::fs::remove_dir_all(&dir).await {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        return Err(e).with_context(|| format!("removing {}", dir.display()));
                    }
                    _ => {}
                }
            }
            Ok(())
        });
        self.finish_action(
            ActionKind::ClearCaches,
            task,
            "settings.clear_caches.done",
            window,
            cx,
        );
    }

    fn reset_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.busy.insert(ActionKind::ResetSettings);
        let b = backend(cx);
        let task = ember_ui::run(cx, async move {
            // Both calls write files synchronously.
            tokio::task::spawn_blocking(move || {
                b.settings.replace_app(AppSettings::default());
                b.settings.reset_all_games();
            })
            .await
            .context("resetting settings")
        });
        self.finish_action(
            ActionKind::ResetSettings,
            task,
            "settings.reset_settings.done",
            window,
            cx,
        );
    }

    fn import_lutris(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.busy.insert(ActionKind::ImportLutris);
        let task = ember_ui::hooks::import_from_lutris(cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.busy.remove(&ActionKind::ImportLutris);
                match result {
                    Ok(summary) => {
                        models::refresh_library(false, cx);
                        ember_ui::lutris_summary::open_lutris_summary(summary, window, cx);
                    }
                    Err(e) => window.push_notification(
                        Notification::error(format!("{e:#}"))
                            .title(t("settings.error.import_lutris")),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn import_heroic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.busy.insert(ActionKind::ImportHeroic);
        self.import_summary = None;
        let task = ember_ui::hooks::import_from_heroic(cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.busy.remove(&ActionKind::ImportHeroic);
                match result {
                    Ok(s) => {
                        let stores = if s.stores.is_empty() {
                            t("settings.import.no_accounts").to_string()
                        } else {
                            s.stores.join(", ")
                        };
                        this.import_summary = Some(t_args(
                            "settings.import.summary",
                            &[
                                ("stores", &stores),
                                ("games", &s.games_with_settings.to_string()),
                                ("favourites", &s.favourites.to_string()),
                                ("categories", &s.categories.to_string()),
                            ],
                        ));
                        models::refresh_library(false, cx);
                    }
                    Err(e) => window.push_notification(
                        Notification::error(format!("{e:#}"))
                            .title(t("settings.error.import_heroic")),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn finish_action(
        &mut self,
        action: ActionKind,
        task: Task<anyhow::Result<()>>,
        success_key: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.busy.remove(&action);
                match result {
                    Ok(()) => {
                        window.push_notification(Notification::success(t(success_key)), cx);
                        if action == ActionKind::ResetSettings {
                            ember_ui::theme::apply(
                                &backend(cx).settings.app().ui,
                                Some(window),
                                cx,
                            );
                            this.reload(window, cx);
                        }
                        if action == ActionKind::ClearCaches {
                            models::refresh_library(true, cx);
                        }
                    }
                    Err(e) => window.push_notification(
                        Notification::error(format!("{e:#}")).title(t("settings.error.generic")),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // ---- rendering ----

    fn render_field(
        &self,
        spec: &'static FieldSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let disabled_reason = self.disabled_reason(spec);
        let disabled = disabled_reason.is_some();
        let id = |part: &str| ElementId::Name(format!("{part}-{}", spec.path).into());
        let overridden = self.is_overridden(spec);
        let path = spec.path;
        let reset = overridden.then(|| {
            Button::new(id("reset"))
                .ghost()
                .xsmall()
                .icon(IconName::RotateCcw)
                .tooltip(t("settings.field.use_default"))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.reset_field(path, window, cx)),
                )
                .into_any_element()
        });
        let label = spec.label_text();
        let mut row = RowState {
            label: label.clone(),
            help: spec.help_text(),
            tag: spec.tag.map(|tag| tag.text()),
            reset,
            disabled_reason,
            error: self.errors.get(spec.path).cloned(),
            stacked: false,
        };
        let control: AnyElement = match spec.kind {
            FieldKind::Toggle => Switch::new(id("field"))
                .checked(self.bool_value(spec.path))
                .disabled(disabled)
                .accessibility_label(label.clone())
                .on_change(cx.listener(move |this, on: &bool, window, cx| {
                    this.write(path, Value::Bool(*on), window, cx)
                }))
                .into_any_element(),
            FieldKind::Text(kind) => {
                let Some(state) = self.texts.get(spec.path) else {
                    return div().into_any_element();
                };
                let picker = matches!(kind, TextKind::Directory | TextKind::File).then(|| {
                    Button::new(id("pick"))
                        .ghost()
                        .xsmall()
                        .icon(IconName::FolderOpen)
                        .tooltip(if kind == TextKind::Directory {
                            t("settings.picker.choose_folder")
                        } else {
                            t("settings.picker.choose_file")
                        })
                        .disabled(disabled)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.pick_path(path, kind == TextKind::Directory, window, cx)
                        }))
                });
                let wide = matches!(
                    kind,
                    TextKind::Plain | TextKind::Directory | TextKind::File | TextKind::Secret
                );
                Input::new(state)
                    .id(id("field"))
                    .aria_label(label.clone())
                    .disabled(disabled)
                    .when(wide, |i| i.w_80())
                    .when(!wide, |i| i.w_24())
                    .when_some(picker, |i, p| i.suffix(p))
                    .into_any_element()
            }
            FieldKind::Choice(_) => match self.choices.get(spec.path) {
                Some(state) => Select::new(state)
                    .id(id("field"))
                    .accessibility_label(label.clone())
                    .disabled(disabled)
                    .w_64()
                    .into_any_element(),
                None => div().into_any_element(),
            },
            FieldKind::Custom(kind) => {
                if matches!(
                    kind,
                    CustomKind::EnvVars | CustomKind::Wrappers | CustomKind::GogSaves
                ) {
                    row.stacked = true;
                }
                self.render_custom(spec, kind, &mut row, window, cx)
            }
            FieldKind::Panel(_) => {
                row.stacked = true;
                match self.panels.get(spec.path) {
                    Some(view) => view.clone().into_any_element(),
                    None => div().into_any_element(),
                }
            }
            FieldKind::Action(action) => {
                if action == ActionKind::ImportHeroic
                    && let Some(summary) = &self.import_summary
                {
                    row.help = summary.clone();
                }
                let (key, danger) = match action {
                    ActionKind::ClearCaches => ("settings.action.clear_caches", false),
                    ActionKind::ResetSettings => ("settings.action.reset_settings", true),
                    ActionKind::OpenLogsFolder => ("settings.action.open_folder", false),
                    ActionKind::ImportHeroic | ActionKind::ImportLutris => {
                        ("settings.action.import", false)
                    }
                };
                Button::new(id("action"))
                    .label(t(key))
                    .when(danger, |b| b.danger())
                    .loading(self.busy.contains(&action))
                    .disabled(self.busy.contains(&action))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.run_action(action, window, cx)),
                    )
                    .into_any_element()
            }
        };
        field_row(row, control, cx).id(id("row")).into_any_element()
    }

    fn render_custom(
        &self,
        spec: &'static FieldSpec,
        kind: CustomKind,
        row: &mut RowState,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = spec.path;
        match kind {
            CustomKind::WineVersion => {
                let Some(wine) = &self.wine else {
                    return div().into_any_element();
                };
                let empty = wine.builds.as_ref().is_some_and(|b| b.is_empty())
                    && !self
                        .value(path)
                        .and_then(|v| v.get("bin"))
                        .and_then(Value::as_str)
                        .is_some_and(|b| !b.is_empty());
                if wine.loading {
                    row.help = t("settings.wine.searching");
                }
                if empty {
                    row.help = t("settings.wine.none_found");
                    return Button::new("open-wine-manager")
                        .label(t("settings.wine.open_manager"))
                        .on_click(|_, _, cx| ember_ui::navigate(Route::WineManager, cx))
                        .into_any_element();
                }
                if let Some(bin) = self
                    .value(path)
                    .and_then(|v| v.get("bin"))
                    .and_then(Value::as_str)
                    && !bin.is_empty()
                    && row.help.is_empty()
                {
                    row.help = bin.to_string().into();
                }
                Select::new(&wine.select)
                    .id("field-wineVersion")
                    .accessibility_label(spec.label_text())
                    .placeholder(if wine.loading {
                        t("settings.common.loading")
                    } else {
                        t("settings.wine.choose_build")
                    })
                    .w_64()
                    .into_any_element()
            }
            CustomKind::ThemeName => match self.choices.get(path) {
                Some(state) => Select::new(state)
                    .id("field-themeName")
                    .accessibility_label(spec.label_text())
                    .w_64()
                    .into_any_element(),
                None => div().into_any_element(),
            },
            CustomKind::FontSize | CustomKind::TileSize => {
                let Some(state) = self.sliders.get(path) else {
                    return div().into_any_element();
                };
                let v = state.read(cx).value().start();
                let label = if kind == CustomKind::FontSize {
                    format!("{:.0}%", v / 16. * 100.)
                } else {
                    format!("{v:.1} rem")
                };
                let reset_value = if kind == CustomKind::FontSize {
                    16.
                } else {
                    11.
                };
                h_flex()
                    .gap_3()
                    .w_80()
                    .child(
                        Slider::new(state)
                            .accessibility_label(spec.label_text())
                            .flex_1(),
                    )
                    .child(
                        div()
                            .w_16()
                            .flex_shrink_0()
                            .whitespace_nowrap()
                            .text_sm()
                            .text_right()
                            .child(label),
                    )
                    .child(
                        Button::new(ElementId::Name(format!("default-{path}").into()))
                            .ghost()
                            .xsmall()
                            .icon(IconName::RotateCcw)
                            .tooltip(t("settings.field.default_size"))
                            .disabled((v - reset_value).abs() < f32::EPSILON)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(s) = this.sliders.get(path) {
                                    s.update(cx, |s, cx| s.set_value(reset_value, window, cx));
                                }
                                this.slider_changed(path, reset_value, true, window, cx);
                            })),
                    )
                    .into_any_element()
            }
            CustomKind::EnvVars | CustomKind::Wrappers | CustomKind::GogSaves => {
                self.render_pairs(spec, cx).into_any_element()
            }
        }
    }

    fn render_pairs(
        &self,
        spec: &'static FieldSpec,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let path = spec.path;
        let Some(editor) = self.pairs.get(path) else {
            return v_flex();
        };
        let rows = self.rows(path);
        let sep = match editor.kind {
            CustomKind::EnvVars => "=",
            _ => " ",
        };
        let theme = cx.theme();
        let (border, muted_fg, danger) = (theme.border, theme.muted_foreground, theme.danger);
        let list = panel(cx).when(rows.is_empty(), |p| {
            p.child(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(muted_fg)
                    .child(t("settings.pairs.empty")),
            )
        });
        let list = rows
            .into_iter()
            .enumerate()
            .fold(list, |list, (ix, (a, b))| {
                let editing = editor.editing == Some(ix);
                list.child(
                    h_flex()
                        .id(ElementId::Name(format!("{path}-row-{ix}").into()))
                        .gap_2()
                        .px_3()
                        .py_1()
                        .when(ix > 0, |r| r.border_t_1().border_color(border))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .font_family(cx.theme().mono_font_family.clone())
                                .when(editing, |d| d.text_color(muted_fg))
                                .child(format!("{a}{sep}{b}")),
                        )
                        .child(
                            Button::new(ElementId::Name(format!("{path}-edit-{ix}").into()))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Pencil)
                                .tooltip(t("settings.common.edit"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.edit_row(path, ix, window, cx)
                                })),
                        )
                        .child(
                            Button::new(ElementId::Name(format!("{path}-remove-{ix}").into()))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Trash)
                                .tooltip(t("settings.common.remove"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.remove_row(path, ix, window, cx)
                                })),
                        ),
                )
            });
        let editing = editor.editing.is_some();
        let add = h_flex()
            .gap_2()
            .child(
                Input::new(&editor.first)
                    .id(ElementId::Name(format!("{path}-first").into()))
                    .flex_1(),
            )
            .child(
                Input::new(&editor.second)
                    .id(ElementId::Name(format!("{path}-second").into()))
                    .flex_1(),
            )
            .child(
                Button::new(ElementId::Name(format!("{path}-add").into()))
                    .icon(if editing {
                        IconName::Check
                    } else {
                        IconName::Plus
                    })
                    .label(if editing {
                        t("settings.common.save")
                    } else {
                        t("settings.common.add")
                    })
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.commit_pair(path, window, cx)),
                    ),
            )
            .when(editing, |r| {
                r.child(
                    Button::new(ElementId::Name(format!("{path}-cancel").into()))
                        .ghost()
                        .label(t("settings.common.cancel"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.cancel_edit(path, window, cx)
                        })),
                )
            });
        let hint = (editor.kind == CustomKind::Wrappers)
            .then(|| muted(t("settings.pairs.wrapper_hint"), cx).text_xs());
        v_flex()
            .gap_2()
            .w_full()
            .child(list)
            .child(add)
            .children(hint)
            .when_some(editor.error.clone(), |v, e| {
                v.child(div().text_xs().text_color(danger).child(e))
            })
    }
}

impl Render for SettingsForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language.changed() {
            cx.defer_in(window, |this, window, cx| this.retranslate(window, cx));
        }
        let multi_section = match &self.filter {
            FormFilter::Sections(s) => s.len() > 1,
            FormFilter::Query(_) => true,
        };
        let fields: Vec<&'static FieldSpec> = self.visible_fields().collect();
        let border = cx.theme().border;
        let mut body = v_flex().w_full();
        // Rows of the current group, gathered into one panel.
        let mut rows: Option<gpui_kit::Div> = None;
        let mut section = None;
        let mut group = "";
        for spec in fields {
            if section != Some(spec.section) {
                body = body.children(rows.take());
                if multi_section {
                    body = body.child(section_heading(spec.section.label()));
                }
                section = Some(spec.section);
                group = "";
            }
            if group != spec.group {
                body = body.children(rows.take());
                body = body.child(group_heading(spec.group_text(), cx));
                group = spec.group;
            }
            let first = rows.is_none();
            let row = div()
                .when(!first, |d| d.border_t_1().border_color(border))
                .child(self.render_field(spec, window, cx));
            rows = Some(rows.unwrap_or_else(|| group_panel(cx)).child(row));
        }
        body.children(rows)
    }
}

/// Select items of a choice field, labelled in the interface language.
fn choice_items(options: &[ChoiceOption]) -> Vec<ChoiceItem> {
    options
        .iter()
        .map(|o| ChoiceItem::new(o.value.key(), o.label.text()))
        .collect()
}

/// The built-in theme followed by every theme of the theme registry.
fn theme_items(cx: &App) -> Vec<ChoiceItem> {
    let mut items = vec![ChoiceItem::new("", t("settings.theme.default"))];
    for (name, dark) in ember_ui::theme::theme_names(cx) {
        let key = if dark {
            "settings.theme.dark_variant"
        } else {
            "settings.theme.light_variant"
        };
        let label = t_args(key, &[("name", &name)]);
        items.push(ChoiceItem::new(name, label));
    }
    items
}

/// Inline notice explaining why a whole group does not apply.
pub(crate) fn info_alert(id: &'static str, text: impl Into<SharedString>) -> Alert {
    Alert::info(id, text.into())
}
