//! The field registry: one static description per editable setting.
//!
//! A field is addressed by its JSON path in the serialized settings
//! (`AppSettings` for application fields, `GameSettings` for launch fields;
//! the two share keys because `AppSettings` flattens its game defaults). A
//! dotted path reaches into an object (`gamescope.fpsLimiter`,
//! `emberUi.fontSize`). The first segment of a game field's path is the key
//! stored in a game's sparse override map.
//!
//! Labels, help, group names, tags, placeholders and option names are
//! translation keys (`settings.field.<id>.label`, …) resolved when rendered,
//! so a language change applies on the next frame. Keywords stay English and
//! the search also matches the English text, so a query works whichever
//! language the interface uses.
//!
//! The registry drives rendering order, grouping and the settings search.

use ember_ui::i18n::{LANGUAGES as UI_LANGUAGES, t};
use gpui_kit::SharedString;
use serde_json::Value;

/// Text shown for a registry item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    /// A translation key.
    Key(&'static str),
    /// Shown as written in every language (brand names, numbers, flags,
    /// language names).
    Verbatim(&'static str),
}

impl Label {
    /// The text in the interface language.
    pub fn text(self) -> SharedString {
        match self {
            Label::Key(key) => t(key),
            Label::Verbatim(s) => SharedString::new_static(s),
        }
    }

    /// The English text.
    pub fn english(self) -> &'static str {
        match self {
            Label::Key(key) => crate::i18n::english(key),
            Label::Verbatim(s) => s,
        }
    }
}

use Label::{Key, Verbatim};

/// Where a field is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldSection {
    /// Application paths, updates, tray, integrations, downloads.
    General,
    /// Theme, interface size and motion.
    Appearance,
    /// Helper binaries, controller timing, diagnostics and maintenance actions.
    Advanced,
    /// Wine/Proton, prefix, sync primitives, translation layers.
    Wine,
    /// GameMode, overlays, frame limits, gamescope.
    Performance,
    /// Arguments, wrappers, environment, scripts.
    Launch,
    /// Cloud save synchronisation.
    CloudSaves,
}

impl FieldSection {
    /// Sections of the shared launch-settings form, in display order.
    pub const GAME: [FieldSection; 4] = [
        FieldSection::Wine,
        FieldSection::Performance,
        FieldSection::Launch,
        FieldSection::CloudSaves,
    ];

    /// Sections holding application-wide fields.
    pub const APP: [FieldSection; 3] = [
        FieldSection::General,
        FieldSection::Appearance,
        FieldSection::Advanced,
    ];

    /// Translation key of the section heading.
    pub fn label_key(self) -> &'static str {
        match self {
            FieldSection::General => "settings.section.general",
            FieldSection::Appearance => "settings.section.appearance",
            FieldSection::Advanced => "settings.section.advanced",
            FieldSection::Wine => "settings.section.wine",
            FieldSection::Performance => "settings.section.performance",
            FieldSection::Launch => "settings.section.launch",
            FieldSection::CloudSaves => "settings.section.cloud_saves",
        }
    }

    /// Section heading in the interface language.
    pub fn label(self) -> SharedString {
        t(self.label_key())
    }

    /// Whether the section belongs to the launch-settings form.
    pub fn is_game(self) -> bool {
        Self::GAME.contains(&self)
    }
}

/// Which settings context shows a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Application-wide settings only.
    App,
    /// Game defaults and per-game settings.
    Shared,
    /// Per-game settings only.
    GameOnly,
}

/// Input conversion of a text field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    /// Free text stored as a string.
    Plain,
    /// A string of at most `max_len` ASCII digits (empty allowed).
    Digits { max_len: usize },
    /// An unsigned number stored as a JSON number.
    Number { min: u32, max: u32 },
    /// A directory path, with a folder picker.
    Directory,
    /// A file path, with a file picker.
    File,
    /// A secret: masked and never shown back in full.
    Secret,
}

/// A value offered by a choice field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceValue {
    Str(&'static str),
    Num(u64),
    /// JSON `false` (Heroic's spelling of "none" for `steamRuntime`).
    False,
}

impl ChoiceValue {
    pub fn to_json(self) -> Value {
        match self {
            ChoiceValue::Str(s) => Value::String(s.to_string()),
            ChoiceValue::Num(n) => Value::from(n),
            ChoiceValue::False => Value::Bool(false),
        }
    }

    /// Whether a stored value selects this option. `null` and `false` both
    /// mean "none".
    pub fn matches(self, value: &Value) -> bool {
        match (self, value) {
            (ChoiceValue::False, Value::Bool(false) | Value::Null) => true,
            (ChoiceValue::Str(s), Value::String(v)) => s == v,
            (ChoiceValue::Num(n), Value::Number(v)) => v.as_u64() == Some(n),
            _ => false,
        }
    }

    /// Stable key used as the select item value.
    pub fn key(self) -> String {
        match self {
            ChoiceValue::Str(s) => format!("s:{s}"),
            ChoiceValue::Num(n) => format!("n:{n}"),
            ChoiceValue::False => "none".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChoiceOption {
    pub value: ChoiceValue,
    pub label: Label,
}

const fn opt(value: ChoiceValue, label: Label) -> ChoiceOption {
    ChoiceOption { value, label }
}

/// Fields rendered by dedicated editors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomKind {
    /// Wine/Proton build picker fed by `ember_runtime::wine_installations`.
    WineVersion,
    /// `KEY=VALUE` environment variable rows.
    EnvVars,
    /// Wrapper command rows.
    Wrappers,
    /// GOG cloud-save locations.
    GogSaves,
    /// Theme picker from the theme registry.
    ThemeName,
    /// Base font size slider (interface zoom).
    FontSize,
    /// Library cover size slider.
    TileSize,
}

/// Rows rendered by a self-contained panel (status plus actions) instead of
/// a single bound value. Their paths start with `panel.`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelKind {
    /// Connected controllers with a refresh button.
    Controllers,
    /// Epic Online Services overlay: status, install, update, remove.
    EosOverlay,
    /// Link legendary with an Epic Games Launcher installation.
    EglSync,
    /// Manual cloud-save synchronisation of one game.
    SaveSync,
}

/// One-shot maintenance commands listed alongside fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    ClearCaches,
    ResetSettings,
    OpenLogsFolder,
    ImportHeroic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Toggle,
    Text(TextKind),
    Choice(&'static [ChoiceOption]),
    Custom(CustomKind),
    Action(ActionKind),
    Panel(PanelKind),
}

/// A field is only editable while the boolean at `path` equals `equals`;
/// otherwise it is shown disabled with the reason at `reason` (a
/// translation key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Condition {
    pub path: &'static str,
    pub equals: bool,
    pub reason: &'static str,
}

const fn when(path: &'static str, equals: bool, reason: &'static str) -> Condition {
    Condition {
        path,
        equals,
        reason,
    }
}

/// Static description of one setting. Text members are translation keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct FieldSpec {
    /// JSON path of the value; unique identifier of the field.
    pub path: &'static str,
    /// Translation key of the label.
    pub label: &'static str,
    /// Translation key of one or two sentences under the label; empty for
    /// none.
    pub help: &'static str,
    /// Extra search terms, in English (they match in every language).
    pub keywords: &'static [&'static str],
    pub section: FieldSection,
    /// Translation key of the sub-heading within the section.
    pub group: &'static str,
    pub kind: FieldKind,
    pub scope: Scope,
    /// Short qualifier shown next to the label ("Epic", "Restart required").
    pub tag: Option<Label>,
    pub placeholder: Option<Label>,
    pub requires: &'static [Condition],
    /// Only shown in the settings of a game installed as a native Linux build.
    pub native_only: bool,
}

impl FieldSpec {
    const fn new(
        path: &'static str,
        label: &'static str,
        section: FieldSection,
        group: &'static str,
        kind: FieldKind,
        scope: Scope,
    ) -> Self {
        Self {
            path,
            label,
            help: "",
            keywords: &[],
            section,
            group,
            kind,
            scope,
            tag: None,
            placeholder: None,
            requires: &[],
            native_only: false,
        }
    }

    const fn help(mut self, help: &'static str) -> Self {
        self.help = help;
        self
    }

    const fn keywords(mut self, keywords: &'static [&'static str]) -> Self {
        self.keywords = keywords;
        self
    }

    const fn tag(mut self, tag: Label) -> Self {
        self.tag = Some(tag);
        self
    }

    const fn placeholder(mut self, placeholder: Label) -> Self {
        self.placeholder = Some(placeholder);
        self
    }

    const fn requires(mut self, requires: &'static [Condition]) -> Self {
        self.requires = requires;
        self
    }

    const fn native_only(mut self) -> Self {
        self.native_only = true;
        self
    }

    /// The label in the interface language.
    pub fn label_text(&self) -> SharedString {
        t(self.label)
    }

    /// The help text in the interface language; empty when there is none.
    pub fn help_text(&self) -> SharedString {
        if self.help.is_empty() {
            SharedString::default()
        } else {
            t(self.help)
        }
    }

    /// The group heading in the interface language.
    pub fn group_text(&self) -> SharedString {
        t(self.group)
    }

    /// The placeholder in the interface language; empty when there is none.
    pub fn placeholder_text(&self) -> SharedString {
        self.placeholder.map(Label::text).unwrap_or_default()
    }

    /// Every translation key the field refers to.
    pub fn keys(&self) -> impl Iterator<Item = &'static str> + '_ {
        let labels = [self.tag, self.placeholder]
            .into_iter()
            .flatten()
            .chain(match self.kind {
                FieldKind::Choice(options) => options.iter().map(|o| o.label).collect(),
                _ => Vec::new(),
            })
            .filter_map(|l| match l {
                Label::Key(k) => Some(k),
                Label::Verbatim(_) => None,
            });
        [self.label, self.help, self.group, self.section.label_key()]
            .into_iter()
            .filter(|k| !k.is_empty())
            .chain(self.requires.iter().map(|c| c.reason))
            .chain(labels)
    }

    /// Key stored in a game's override map: the first path segment.
    pub fn override_key(&self) -> &'static str {
        crate::value::override_key(self.path)
    }

    /// Whether the field is shown for application settings, game defaults
    /// (`game == false`) or one game (`game == true`).
    pub fn supported_on_host(&self) -> bool {
        use ember_core::HostPlatform;
        let host = HostPlatform::current();
        let mac_only = matches!(
            self.path,
            "enableMsync"
                | "wineCrossoverBottle"
                | "advertiseAvxForRosetta"
                | "emberRetinaMode"
                | "emberCrossoverBottlesPath"
        );
        if mac_only && host != HostPlatform::Mac {
            return false;
        }
        if host == HostPlatform::Linux {
            return true;
        }
        if self.path.starts_with("gamescope.")
            || matches!(
                self.path,
                "useGameMode"
                    | "showMangohud"
                    | "nvidiaPrime"
                    | "enableFsync"
                    | "enableFSR"
                    | "enableWineWayland"
                    | "enableHDR"
                    | "enableWoW64"
                    | "eacRuntime"
                    | "battlEyeRuntime"
                    | "disableUMU"
                    | "steamRuntime"
                    | "preferSystemLibs"
                    | "autoInstallDxvk"
                    | "autoInstallVkd3d"
                    | "autoInstallDxvkNvapi"
                    | "downloadProtonToSteam"
                    | "showValveProton"
            )
        {
            return false;
        }
        host != HostPlatform::Windows
            || (self.section != FieldSection::Wine
                && !matches!(self.path, "showFps" | "enableDXVKFpsLimit" | "DXVKFpsCap"))
    }

    pub fn applies_to_game(&self, per_game: bool) -> bool {
        match self.scope {
            Scope::App => false,
            Scope::Shared => true,
            Scope::GameOnly => per_game,
        }
    }

    /// Whether every whitespace-separated token of `query` occurs in the
    /// label, help, group, section or tag (in the interface language or in
    /// English), the keywords or the path (case-insensitive).
    pub fn matches(&self, query: &str) -> bool {
        let mut tokens = query.split_whitespace().peekable();
        if tokens.peek().is_none() {
            return false;
        }
        let haystack = self.haystack();
        tokens.all(|t| haystack.contains(&t.to_lowercase()))
    }

    fn haystack(&self) -> String {
        let mut s = String::with_capacity(256);
        let mut push = |part: &str| {
            s.push_str(part);
            s.push('\n');
        };
        let tag = self.tag.map(Label::text).unwrap_or_default();
        for part in [
            self.label_text(),
            self.help_text(),
            self.group_text(),
            self.section.label(),
            tag,
        ] {
            push(&part);
        }
        for key in [self.label, self.help, self.group, self.section.label_key()] {
            if !key.is_empty() {
                push(crate::i18n::english(key));
            }
        }
        if let Some(tag) = self.tag {
            push(tag.english());
        }
        push(self.path);
        for k in self.keywords {
            push(k);
        }
        s.to_lowercase()
    }
}

use FieldKind::{Action, Choice, Custom, Panel, Text, Toggle};
use FieldSection::{Advanced, Appearance, CloudSaves, General, Launch, Performance, Wine};
use Scope::{App, GameOnly, Shared};

/// Interface languages by native name (never translated).
static LANGUAGES: [ChoiceOption; UI_LANGUAGES.len()] = {
    let mut out = [opt(ChoiceValue::Str("en"), Verbatim("English")); UI_LANGUAGES.len()];
    let mut i = 0;
    while i < UI_LANGUAGES.len() {
        let lang = UI_LANGUAGES[i];
        out[i] = opt(ChoiceValue::Str(lang.code), Verbatim(lang.native_name));
        i += 1;
    }
    out
};

const TOP_SECTION: &[ChoiceOption] = &[
    opt(
        ChoiceValue::Str("recently_played"),
        Key("settings.option.top_section.recently_played"),
    ),
    opt(
        ChoiceValue::Str("recently_played_installed"),
        Key("settings.option.top_section.recently_played_installed"),
    ),
    opt(
        ChoiceValue::Str("favourites"),
        Key("settings.option.top_section.favourites"),
    ),
    opt(
        ChoiceValue::Str("disabled"),
        Key("settings.option.top_section.nothing"),
    ),
];

const THEME_MODE: &[ChoiceOption] = &[
    opt(
        ChoiceValue::Str("system"),
        Key("settings.option.theme_mode.system"),
    ),
    opt(
        ChoiceValue::Str("dark"),
        Key("settings.option.theme_mode.dark"),
    ),
    opt(
        ChoiceValue::Str("light"),
        Key("settings.option.theme_mode.light"),
    ),
];

const TRANSLUCENCY: &[ChoiceOption] = &[
    opt(
        ChoiceValue::Str("auto"),
        Key("settings.option.translucency.auto"),
    ),
    opt(
        ChoiceValue::Str("off"),
        Key("settings.option.translucency.off"),
    ),
    opt(
        ChoiceValue::Str("subtle"),
        Key("settings.option.translucency.subtle"),
    ),
    opt(
        ChoiceValue::Str("strong"),
        Key("settings.option.translucency.strong"),
    ),
];

const BIG_PICTURE_SCALE: &[ChoiceOption] = &[
    opt(ChoiceValue::Num(100), Verbatim("100%")),
    opt(ChoiceValue::Num(125), Verbatim("125%")),
    opt(ChoiceValue::Num(150), Verbatim("150%")),
    opt(ChoiceValue::Num(175), Verbatim("175%")),
    opt(ChoiceValue::Num(200), Verbatim("200%")),
];

const LIBRARY_LAYOUT: &[ChoiceOption] = &[
    opt(
        ChoiceValue::Str("grid"),
        Key("settings.option.library_layout.grid"),
    ),
    opt(
        ChoiceValue::Str("list"),
        Key("settings.option.library_layout.list"),
    ),
];

const STEAM_RUNTIME: &[ChoiceOption] = &[
    opt(
        ChoiceValue::False,
        Key("settings.option.steam_runtime.none"),
    ),
    opt(
        ChoiceValue::Str("umu-scout"),
        Verbatim("Steam Linux Runtime 1.0 (scout)"),
    ),
    opt(
        ChoiceValue::Str("umu-soldier"),
        Verbatim("Steam Linux Runtime 2.0 (soldier)"),
    ),
    opt(
        ChoiceValue::Str("umu-sniper"),
        Verbatim("Steam Linux Runtime 3.0 (sniper)"),
    ),
    opt(
        ChoiceValue::Str("umu-steamrt4"),
        Verbatim("Steam Linux Runtime 4.0"),
    ),
];

const SHARPNESS: &[ChoiceOption] = &[
    opt(
        ChoiceValue::Num(1),
        Key("settings.option.sharpness.softest"),
    ),
    opt(ChoiceValue::Num(2), Verbatim("2")),
    opt(ChoiceValue::Num(3), Verbatim("3")),
    opt(ChoiceValue::Num(4), Verbatim("4")),
    opt(
        ChoiceValue::Num(5),
        Key("settings.option.sharpness.sharpest"),
    ),
];

const UPSCALE_METHOD: &[ChoiceOption] = &[
    opt(
        ChoiceValue::Str("fsr"),
        Verbatim("AMD FidelityFX Super Resolution 1.0 (FSR)"),
    ),
    opt(
        ChoiceValue::Str("nis"),
        Verbatim("NVIDIA Image Scaling (NIS)"),
    ),
    opt(
        ChoiceValue::Str("integer"),
        Key("settings.option.upscale_method.integer"),
    ),
    opt(
        ChoiceValue::Str("stretch"),
        Key("settings.option.upscale_method.stretch"),
    ),
];

const WINDOW_TYPE: &[ChoiceOption] = &[
    opt(
        ChoiceValue::Str("fullscreen"),
        Key("settings.option.window_type.fullscreen"),
    ),
    opt(
        ChoiceValue::Str("borderless"),
        Key("settings.option.window_type.borderless"),
    ),
    opt(
        ChoiceValue::Str("windowed"),
        Key("settings.option.window_type.windowed"),
    ),
];

const NEEDS_TRAY: &[Condition] = &[when("noTrayIcon", false, "settings.requires.tray_icon")];
const NEEDS_EXIT_TO_TRAY: &[Condition] = &[
    when("noTrayIcon", false, "settings.requires.tray_icon"),
    when("exitToTray", true, "settings.requires.close_to_tray"),
];
const NEEDS_WAYLAND: &[Condition] = &[when(
    "enableWineWayland",
    true,
    "settings.requires.wine_wayland",
)];
const NEEDS_FSR: &[Condition] = &[when("enableFSR", true, "settings.requires.fsr")];
const NEEDS_DXVK: &[Condition] = &[when("autoInstallDxvk", true, "settings.requires.dxvk")];
const NEEDS_FPS_LIMIT: &[Condition] = &[when(
    "enableDXVKFpsLimit",
    true,
    "settings.requires.fps_limit",
)];
const NEEDS_UPSCALING: &[Condition] = &[when(
    "gamescope.enableUpscaling",
    true,
    "settings.requires.gamescope_upscaling",
)];
const NEEDS_LIMITER: &[Condition] = &[when(
    "gamescope.enableLimiter",
    true,
    "settings.requires.gamescope_limiter",
)];

const NEEDS_CONTROLLER: &[Condition] = &[when(
    "disableController",
    false,
    "settings.requires.controller_navigation",
)];

const fn app(
    path: &'static str,
    label: &'static str,
    section: FieldSection,
    group: &'static str,
    kind: FieldKind,
) -> FieldSpec {
    FieldSpec::new(path, label, section, group, kind, App)
}

const fn game(
    path: &'static str,
    label: &'static str,
    section: FieldSection,
    group: &'static str,
    kind: FieldKind,
) -> FieldSpec {
    FieldSpec::new(path, label, section, group, kind, Shared)
}

const fn game_only(
    path: &'static str,
    label: &'static str,
    section: FieldSection,
    group: &'static str,
    kind: FieldKind,
) -> FieldSpec {
    FieldSpec::new(path, label, section, group, kind, GameOnly)
}

/// Every field, in display order.
pub static FIELDS: &[FieldSpec] = &[
    // ---- General ----
    app(
        "uiLanguage",
        "settings.field.ui_language.label",
        General,
        "settings.group.language",
        Choice(&LANGUAGES),
    )
    .help("settings.field.ui_language.help")
    .keywords(&["language", "locale", "translation", "interface"]),
    app(
        "defaultInstallPath",
        "settings.field.default_install_path.label",
        General,
        "settings.group.folders",
        Text(TextKind::Directory),
    )
    .help("settings.field.default_install_path.help")
    .keywords(&["path", "directory", "games"]),
    app(
        "defaultWinePrefixDir",
        "settings.field.default_wine_prefix_dir.label",
        General,
        "settings.group.folders",
        Text(TextKind::Directory),
    )
    .help("settings.field.default_wine_prefix_dir.help")
    .keywords(&["prefix", "path", "wineprefix"]),
    app(
        "defaultSteamPath",
        "settings.field.default_steam_path.label",
        General,
        "settings.group.folders",
        Text(TextKind::Directory),
    )
    .help("settings.field.default_steam_path.help")
    .keywords(&["steam", "proton", "path"]),
    app(
        "emberCrossoverBottlesPath",
        "settings.field.crossover_bottles_path.label",
        General,
        "settings.group.folders",
        Text(TextKind::Directory),
    )
    .tag(Verbatim("CrossOver"))
    .help("settings.field.crossover_bottles_path.help")
    .placeholder(Verbatim("~/Library/Application Support/CrossOver/Bottles"))
    .keywords(&["crossover", "bottle", "bottles", "path", "mac"]),
    app(
        "checkForUpdatesOnStartup",
        "settings.field.check_for_updates_on_startup.label",
        General,
        "settings.group.updates",
        Toggle,
    )
    .keywords(&["update", "startup"]),
    app(
        "checkUpdatesInterval",
        "settings.field.check_updates_interval.label",
        General,
        "settings.group.updates",
        Text(TextKind::Number { min: 5, max: 1440 }),
    )
    .help("settings.field.check_updates_interval.help")
    .keywords(&["minutes", "update", "frequency"]),
    app(
        "autoUpdateGames",
        "settings.field.auto_update_games.label",
        General,
        "settings.group.updates",
        Toggle,
    )
    .help("settings.field.auto_update_games.help")
    .keywords(&["update", "automatic"]),
    app(
        "noTrayIcon",
        "settings.field.no_tray_icon.label",
        General,
        "settings.group.tray_and_window",
        Toggle,
    )
    .tag(Key("settings.tag.restart_required"))
    .keywords(&["tray", "systray", "icon"]),
    app(
        "exitToTray",
        "settings.field.exit_to_tray.label",
        General,
        "settings.group.tray_and_window",
        Toggle,
    )
    .help("settings.field.exit_to_tray.help")
    .keywords(&["tray", "exit", "close", "background"])
    .requires(NEEDS_TRAY),
    app(
        "startInTray",
        "settings.field.start_in_tray.label",
        General,
        "settings.group.tray_and_window",
        Toggle,
    )
    .help("settings.field.start_in_tray.help")
    .keywords(&["tray", "minimized", "startup"])
    .requires(NEEDS_EXIT_TO_TRAY),
    app(
        "minimizeOnLaunch",
        "settings.field.minimize_on_launch.label",
        General,
        "settings.group.tray_and_window",
        Toggle,
    )
    .help("settings.field.minimize_on_launch.help")
    .keywords(&["minimize", "hide", "launch"])
    .requires(NEEDS_TRAY),
    app(
        "darkTrayIcon",
        "settings.field.dark_tray_icon.label",
        General,
        "settings.group.tray_and_window",
        Toggle,
    )
    .tag(Key("settings.tag.restart_required"))
    .keywords(&["tray", "icon", "dark"])
    .requires(NEEDS_TRAY),
    app(
        "hideWindowOnProtocolLaunch",
        "settings.field.hide_window_on_protocol_launch.label",
        General,
        "settings.group.tray_and_window",
        Toggle,
    )
    .help("settings.field.hide_window_on_protocol_launch.help")
    .keywords(&["protocol", "url", "heroic://", "link", "shortcut"]),
    app(
        "startInConsoleMode",
        "settings.field.start_in_console_mode.label",
        General,
        "settings.group.tray_and_window",
        Toggle,
    )
    .keywords(&["console", "gamepad", "fullscreen", "big picture"]),
    app(
        "emberUi.bigPictureInGameMode",
        "settings.field.big_picture_in_game_mode.label",
        General,
        "settings.group.tray_and_window",
        Toggle,
    )
    .help("settings.field.big_picture_in_game_mode.help")
    .keywords(&[
        "steam deck",
        "game mode",
        "gamescope",
        "console",
        "big picture",
    ]),
    app(
        "addDesktopShortcuts",
        "settings.field.add_desktop_shortcuts.label",
        General,
        "settings.group.new_installs",
        Toggle,
    )
    .help("settings.field.add_desktop_shortcuts.help")
    .keywords(&["shortcut", "desktop"]),
    app(
        "addStartMenuShortcuts",
        "settings.field.add_start_menu_shortcuts.label",
        General,
        "settings.group.new_installs",
        Toggle,
    )
    .keywords(&["shortcut", "start menu", "applications"]),
    app(
        "addSteamShortcuts",
        "settings.field.add_steam_shortcuts.label",
        General,
        "settings.group.new_installs",
        Toggle,
    )
    .help("settings.field.add_steam_shortcuts.help")
    .keywords(&["steam", "shortcut", "deck"]),
    app(
        "libraryTopSection",
        "settings.field.library_top_section.label",
        General,
        "settings.group.library",
        Choice(TOP_SECTION),
    )
    .keywords(&["recent", "favourites", "top"]),
    app(
        "emberUi.stores.epic",
        "settings.field.show_store_epic.label",
        General,
        "settings.group.stores",
        Toggle,
    )
    .help("settings.field.show_stores.help")
    .keywords(&["store", "hide", "epic", "legendary"]),
    app(
        "emberUi.stores.gog",
        "settings.field.show_store_gog.label",
        General,
        "settings.group.stores",
        Toggle,
    )
    .help("settings.field.show_stores.help")
    .keywords(&["store", "hide", "gog"]),
    app(
        "emberUi.stores.amazon",
        "settings.field.show_store_amazon.label",
        General,
        "settings.group.stores",
        Toggle,
    )
    .help("settings.field.show_stores.help")
    .keywords(&["store", "hide", "amazon", "prime", "nile"]),
    app(
        "maxRecentGames",
        "settings.field.max_recent_games.label",
        General,
        "settings.group.library",
        Text(TextKind::Number { min: 1, max: 20 }),
    )
    .keywords(&["recent", "tray"]),
    app(
        "discordRPC",
        "settings.field.discord_rpc.label",
        General,
        "settings.group.integrations",
        Toggle,
    )
    .help("settings.field.discord_rpc.help")
    .keywords(&["discord", "presence", "rpc"]),
    app(
        "discordClientId",
        "settings.field.discord_client_id.label",
        General,
        "settings.group.integrations",
        Text(TextKind::Digits { max_len: 20 }),
    )
    .help("settings.field.discord_client_id.help")
    .placeholder(Key("settings.field.discord_client_id.placeholder"))
    .keywords(&[
        "discord",
        "presence",
        "rpc",
        "client id",
        "application",
        "developers",
    ]),
    app(
        "disablePlaytimeSync",
        "settings.field.disable_playtime_sync.label",
        General,
        "settings.group.integrations",
        Toggle,
    )
    .tag(Verbatim("GOG · Epic"))
    .keywords(&["playtime", "sync", "gog", "epic"]),
    app(
        "disableGOGPresence",
        "settings.field.disable_gog_presence.label",
        General,
        "settings.group.integrations",
        Toggle,
    )
    .tag(Verbatim("GOG"))
    .keywords(&["presence", "gog", "galaxy", "online"]),
    app(
        "cometSupport",
        "settings.field.comet_support.label",
        General,
        "settings.group.integrations",
        Toggle,
    )
    .tag(Verbatim("GOG"))
    .help("settings.field.comet_support.help")
    .keywords(&["comet", "galaxy", "achievements", "gog"]),
    app(
        "zoomPlatform",
        "settings.field.zoom_platform.label",
        General,
        "settings.group.integrations",
        Toggle,
    )
    .keywords(&["zoom", "store"]),
    app(
        "emberItchIo",
        "settings.field.itch_io.label",
        General,
        "settings.group.integrations",
        Toggle,
    )
    .help("settings.field.itch_io.help")
    .keywords(&["itch", "itch.io", "store", "indie"]),
    app(
        "maxWorkers",
        "settings.field.max_workers.label",
        General,
        "settings.group.downloads",
        Text(TextKind::Number { min: 0, max: 64 }),
    )
    .help("settings.field.max_workers.help")
    .keywords(&["workers", "threads", "parallel", "download"]),
    app(
        "maxDownloadSpeed",
        "settings.field.max_download_speed.label",
        General,
        "settings.group.downloads",
        Text(TextKind::Number {
            min: 0,
            max: 10_000_000,
        }),
    )
    .help("settings.field.max_download_speed.help")
    .keywords(&["bandwidth", "speed", "limit", "throttle", "download"]),
    app(
        "downloadNoHttps",
        "settings.field.download_no_https.label",
        General,
        "settings.group.downloads",
        Toggle,
    )
    .tag(Verbatim("Epic"))
    .help("settings.field.download_no_https.help")
    .keywords(&["https", "lancache", "cdn", "cache"]),
    app(
        "panel.controllers",
        "settings.field.panel_controllers.label",
        General,
        "settings.group.controller",
        Panel(PanelKind::Controllers),
    )
    .keywords(&[
        "gamepad",
        "controller",
        "joystick",
        "xbox",
        "playstation",
        "dualsense",
        "steam deck",
        "battery",
    ]),
    app(
        "disableController",
        "settings.field.disable_controller.label",
        General,
        "settings.group.controller",
        Toggle,
    )
    .help("settings.field.disable_controller.help")
    .keywords(&["gamepad", "controller", "joystick"]),
    app(
        "gamepadInitialRepeatDelay",
        "settings.field.gamepad_initial_repeat_delay.label",
        General,
        "settings.group.controller",
        Text(TextKind::Number { min: 50, max: 500 }),
    )
    .help("settings.field.gamepad_initial_repeat_delay.help")
    .keywords(&["gamepad", "controller", "repeat", "delay"])
    .requires(NEEDS_CONTROLLER),
    app(
        "gamepadRepeatDelay",
        "settings.field.gamepad_repeat_delay.label",
        General,
        "settings.group.controller",
        Text(TextKind::Number { min: 5, max: 150 }),
    )
    .help("settings.field.gamepad_repeat_delay.help")
    .keywords(&["gamepad", "controller", "repeat", "frequency"])
    .requires(NEEDS_CONTROLLER),
    // ---- Appearance ----
    app(
        "emberUi.theme",
        "settings.field.ui_theme.label",
        Appearance,
        "settings.group.theme",
        Choice(THEME_MODE),
    )
    .keywords(&["dark", "light", "system", "mode"]),
    app(
        "emberUi.themeName",
        "settings.field.ui_theme_name.label",
        Appearance,
        "settings.group.theme",
        Custom(CustomKind::ThemeName),
    )
    .keywords(&["colors", "palette", "catppuccin", "gruvbox"]),
    app(
        "emberUi.translucency",
        "settings.field.ui_translucency.label",
        Appearance,
        "settings.group.theme",
        Choice(TRANSLUCENCY),
    )
    .help("settings.field.ui_translucency.help")
    .keywords(&[
        "translucency",
        "transparency",
        "blur",
        "glass",
        "vibrancy",
        "mica",
        "acrylic",
    ]),
    app(
        "emberUi.fontSize",
        "settings.field.ui_font_size.label",
        Appearance,
        "settings.group.size",
        Custom(CustomKind::FontSize),
    )
    .help("settings.field.ui_font_size.help")
    .keywords(&["zoom", "font", "scale", "text size"]),
    app(
        "emberUi.tileSize",
        "settings.field.ui_tile_size.label",
        Appearance,
        "settings.group.size",
        Custom(CustomKind::TileSize),
    )
    .help("settings.field.ui_tile_size.help")
    .keywords(&["tile", "grid", "cover", "library"]),
    app(
        "emberUi.bigPictureScale",
        "settings.field.ui_big_picture_scale.label",
        Appearance,
        "settings.group.size",
        Choice(BIG_PICTURE_SCALE),
    )
    .help("settings.field.ui_big_picture_scale.help")
    .keywords(&[
        "big picture",
        "tv",
        "console",
        "zoom",
        "scale",
        "steam deck",
    ]),
    app(
        "emberUi.libraryLayout",
        "settings.field.ui_library_layout.label",
        Appearance,
        "settings.group.library",
        Choice(LIBRARY_LAYOUT),
    )
    .keywords(&["grid", "list", "view"]),
    app(
        "emberUi.showStoreBadges",
        "settings.field.ui_show_store_badges.label",
        Appearance,
        "settings.group.library",
        Toggle,
    )
    .keywords(&["badge", "store", "label"]),
    app(
        "emberUi.reduceMotion",
        "settings.field.ui_reduce_motion.label",
        Appearance,
        "settings.group.motion",
        Toggle,
    )
    .help("settings.field.ui_reduce_motion.help")
    .keywords(&["animation", "accessibility", "motion"]),
    app(
        "disableAnimations",
        "settings.field.disable_animations.label",
        Appearance,
        "settings.group.motion",
        Toggle,
    )
    .keywords(&["animation", "accessibility"]),
    // ---- Advanced ----
    app(
        "steamGridDbApiKey",
        "settings.field.steam_grid_db_api_key.label",
        Advanced,
        "settings.group.artwork",
        Text(TextKind::Secret),
    )
    .help("settings.field.steam_grid_db_api_key.help")
    .placeholder(Key("settings.field.steam_grid_db_api_key.placeholder"))
    .keywords(&["steamgriddb", "artwork", "cover", "key"]),
    app(
        "panel.eos-overlay",
        "settings.field.panel_eos_overlay.label",
        Advanced,
        "settings.group.epic_games",
        Panel(PanelKind::EosOverlay),
    )
    .tag(Verbatim("Epic"))
    .help("settings.field.panel_eos_overlay.help")
    .keywords(&[
        "eos",
        "overlay",
        "epic online services",
        "epic",
        "legendary",
    ]),
    app(
        "panel.egl-sync",
        "settings.field.panel_egl_sync.label",
        Advanced,
        "settings.group.epic_games",
        Panel(PanelKind::EglSync),
    )
    .tag(Verbatim("Epic"))
    .help("settings.field.panel_egl_sync.help")
    .keywords(&[
        "egl",
        "egs",
        "epic games launcher",
        "sync",
        "epic",
        "legendary",
        "link",
    ]),
    app(
        "showValveProton",
        "settings.field.show_valve_proton.label",
        Advanced,
        "settings.group.wine",
        Toggle,
    )
    .help("settings.field.show_valve_proton.help")
    .keywords(&["proton", "valve", "steam"]),
    app(
        "downloadProtonToSteam",
        "settings.field.download_proton_to_steam.label",
        Advanced,
        "settings.group.wine",
        Toggle,
    )
    .keywords(&["proton", "steam", "compatibilitytools"]),
    app(
        "allowInstallationBrokenAnticheat",
        "settings.field.allow_installation_broken_anticheat.label",
        Advanced,
        "settings.group.installs",
        Toggle,
    )
    .help("settings.field.allow_installation_broken_anticheat.help")
    .keywords(&["anticheat", "eac", "battleye", "denied"]),
    app(
        "disableLogs",
        "settings.field.disable_logs.label",
        Advanced,
        "settings.group.diagnostics",
        Toggle,
    )
    .help("settings.field.disable_logs.help")
    .keywords(&["log", "debug"]),
    app(
        "action.open-logs",
        "settings.field.action_open_logs.label",
        Advanced,
        "settings.group.diagnostics",
        Action(ActionKind::OpenLogsFolder),
    )
    .keywords(&["log", "folder", "open"]),
    app(
        "action.import-heroic",
        "settings.field.action_import_heroic.label",
        Advanced,
        "settings.group.maintenance",
        Action(ActionKind::ImportHeroic),
    )
    .help("settings.field.action_import_heroic.help")
    .keywords(&["heroic", "import", "migrate"]),
    app(
        "action.clear-caches",
        "settings.field.action_clear_caches.label",
        Advanced,
        "settings.group.maintenance",
        Action(ActionKind::ClearCaches),
    )
    .help("settings.field.action_clear_caches.help")
    .keywords(&["cache", "clear", "artwork", "refresh"]),
    app(
        "action.reset-settings",
        "settings.field.action_reset_settings.label",
        Advanced,
        "settings.group.maintenance",
        Action(ActionKind::ResetSettings),
    )
    .help("settings.field.action_reset_settings.help")
    .keywords(&["reset", "defaults", "factory"]),
    // ---- Wine & Proton ----
    game(
        "wineVersion",
        "settings.field.wine_version.label",
        Wine,
        "settings.group.compatibility_layer",
        Custom(CustomKind::WineVersion),
    )
    .keywords(&["wine", "proton", "ge", "version", "runner"]),
    game(
        "winePrefix",
        "settings.field.wine_prefix.label",
        Wine,
        "settings.group.compatibility_layer",
        Text(TextKind::Directory),
    )
    .help("settings.field.wine_prefix.help")
    .placeholder(Key("settings.field.wine_prefix.placeholder"))
    .keywords(&["prefix", "wineprefix", "path", "bottle"]),
    game(
        "wineCrossoverBottle",
        "settings.field.wine_crossover_bottle.label",
        Wine,
        "settings.group.compatibility_layer",
        Text(TextKind::Plain),
    )
    .tag(Verbatim("CrossOver"))
    .help("settings.field.wine_crossover_bottle.help")
    .keywords(&["crossover", "bottle", "prefix", "mac"]),
    game(
        "disableUMU",
        "settings.field.disable_umu.label",
        Wine,
        "settings.group.compatibility_layer",
        Toggle,
    )
    .tag(Verbatim("Proton"))
    .help("settings.field.disable_umu.help")
    .keywords(&["umu", "proton"]),
    game(
        "enableEsync",
        "settings.field.enable_esync.label",
        Wine,
        "settings.group.wine_sync",
        Toggle,
    )
    .help("settings.field.enable_esync.help")
    .keywords(&["esync", "sync", "performance"]),
    game(
        "enableFsync",
        "settings.field.enable_fsync.label",
        Wine,
        "settings.group.wine_sync",
        Toggle,
    )
    .help("settings.field.enable_fsync.help")
    .keywords(&["fsync", "futex", "sync", "performance"]),
    game(
        "enableMsync",
        "settings.field.enable_msync.label",
        Wine,
        "settings.group.wine_sync",
        Toggle,
    )
    .tag(Verbatim("macOS"))
    .keywords(&["msync", "sync", "mac"]),
    game(
        "emberRetinaMode",
        "settings.field.retina_mode.label",
        Wine,
        "settings.group.display",
        Toggle,
    )
    .tag(Verbatim("macOS"))
    .help("settings.field.retina_mode.help")
    .keywords(&["retina", "hidpi", "resolution", "scaling", "mac"]),
    game(
        "enableWineWayland",
        "settings.field.enable_wine_wayland.label",
        Wine,
        "settings.group.display",
        Toggle,
    )
    .tag(Key("settings.tag.experimental"))
    .help("settings.field.enable_wine_wayland.help")
    .keywords(&["wayland", "xwayland", "display"]),
    game(
        "enableHDR",
        "settings.field.enable_hdr.label",
        Wine,
        "settings.group.display",
        Toggle,
    )
    .keywords(&["hdr", "high dynamic range", "wayland"])
    .requires(NEEDS_WAYLAND),
    game(
        "enableFSR",
        "settings.field.enable_fsr.label",
        Wine,
        "settings.group.display",
        Toggle,
    )
    .keywords(&["fsr", "upscale", "amd", "fidelityfx"]),
    game(
        "maxSharpness",
        "settings.field.max_sharpness.label",
        Wine,
        "settings.group.display",
        Choice(SHARPNESS),
    )
    .keywords(&["fsr", "sharpness", "strength"])
    .requires(NEEDS_FSR),
    game(
        "enableWoW64",
        "settings.field.enable_wow64.label",
        Wine,
        "settings.group.display",
        Toggle,
    )
    .tag(Key("settings.tag.experimental"))
    .help("settings.field.enable_wow64.help")
    .keywords(&["wow64", "32-bit", "multilib"]),
    game(
        "autoInstallDxvk",
        "settings.field.auto_install_dxvk.label",
        Wine,
        "settings.group.graphics_translation",
        Toggle,
    )
    .help("settings.field.auto_install_dxvk.help")
    .keywords(&["dxvk", "directx", "d3d11", "vulkan"]),
    game(
        "autoInstallVkd3d",
        "settings.field.auto_install_vkd3d.label",
        Wine,
        "settings.group.graphics_translation",
        Toggle,
    )
    .help("settings.field.auto_install_vkd3d.help")
    .keywords(&["vkd3d", "directx 12", "d3d12", "vulkan"])
    .requires(NEEDS_DXVK),
    game(
        "autoInstallDxvkNvapi",
        "settings.field.auto_install_dxvk_nvapi.label",
        Wine,
        "settings.group.graphics_translation",
        Toggle,
    )
    .help("settings.field.auto_install_dxvk_nvapi.help")
    .keywords(&["nvapi", "dlss", "nvidia"])
    .requires(NEEDS_DXVK),
    game(
        "preferSystemLibs",
        "settings.field.prefer_system_libs.label",
        Wine,
        "settings.group.graphics_translation",
        Toggle,
    )
    .help("settings.field.prefer_system_libs.help")
    .keywords(&["libraries", "ld_library_path", "system"]),
    game(
        "eacRuntime",
        "settings.field.eac_runtime.label",
        Wine,
        "settings.group.anti_cheat",
        Toggle,
    )
    .keywords(&["eac", "easyanticheat", "anticheat", "runtime"]),
    game(
        "battlEyeRuntime",
        "settings.field.battleye_runtime.label",
        Wine,
        "settings.group.anti_cheat",
        Toggle,
    )
    .keywords(&["battleye", "anticheat", "runtime"]),
    // ---- Performance ----
    game(
        "useGameMode",
        "settings.field.use_game_mode.label",
        Performance,
        "settings.group.system",
        Toggle,
    )
    .help("settings.field.use_game_mode.help")
    .keywords(&["gamemode", "feral", "performance", "cpu governor"]),
    game(
        "showMangohud",
        "settings.field.show_mangohud.label",
        Performance,
        "settings.group.system",
        Toggle,
    )
    .help("settings.field.show_mangohud.help")
    .keywords(&["mangohud", "overlay", "fps", "hud"]),
    game(
        "showFps",
        "settings.field.show_fps.label",
        Performance,
        "settings.group.system",
        Toggle,
    )
    .keywords(&["fps", "hud", "dxvk", "counter"]),
    game(
        "nvidiaPrime",
        "settings.field.nvidia_prime.label",
        Performance,
        "settings.group.system",
        Toggle,
    )
    .help("settings.field.nvidia_prime.help")
    .keywords(&["prime", "optimus", "dgpu", "nvidia", "hybrid"]),
    game(
        "enableDXVKFpsLimit",
        "settings.field.enable_dxvk_fps_limit.label",
        Performance,
        "settings.group.frame_rate",
        Toggle,
    )
    .help("settings.field.enable_dxvk_fps_limit.help")
    .keywords(&["fps", "limit", "cap", "dxvk", "frame rate"]),
    game(
        "DXVKFpsCap",
        "settings.field.dxvk_fps_cap.label",
        Performance,
        "settings.group.frame_rate",
        Text(TextKind::Digits { max_len: 4 }),
    )
    .placeholder(Verbatim("60"))
    .keywords(&["fps", "limit", "cap"])
    .requires(NEEDS_FPS_LIMIT),
    game(
        "gamescope.enableUpscaling",
        "settings.field.gamescope_enable_upscaling.label",
        Performance,
        "settings.group.gamescope",
        Toggle,
    )
    .help("settings.field.gamescope_enable_upscaling.help")
    .keywords(&["gamescope", "upscale", "resolution"]),
    game(
        "gamescope.upscaleMethod",
        "settings.field.gamescope_upscale_method.label",
        Performance,
        "settings.group.gamescope",
        Choice(UPSCALE_METHOD),
    )
    .keywords(&["gamescope", "fsr", "nis", "integer"])
    .requires(NEEDS_UPSCALING),
    game(
        "gamescope.windowType",
        "settings.field.gamescope_window_type.label",
        Performance,
        "settings.group.gamescope",
        Choice(WINDOW_TYPE),
    )
    .keywords(&["gamescope", "fullscreen", "borderless", "windowed"])
    .requires(NEEDS_UPSCALING),
    game(
        "gamescope.gameWidth",
        "settings.field.gamescope_game_width.label",
        Performance,
        "settings.group.gamescope",
        Text(TextKind::Digits { max_len: 5 }),
    )
    .placeholder(Verbatim("1280"))
    .keywords(&["gamescope", "resolution", "width"])
    .requires(NEEDS_UPSCALING),
    game(
        "gamescope.gameHeight",
        "settings.field.gamescope_game_height.label",
        Performance,
        "settings.group.gamescope",
        Text(TextKind::Digits { max_len: 5 }),
    )
    .placeholder(Verbatim("720"))
    .keywords(&["gamescope", "resolution", "height"])
    .requires(NEEDS_UPSCALING),
    game(
        "gamescope.upscaleWidth",
        "settings.field.gamescope_upscale_width.label",
        Performance,
        "settings.group.gamescope",
        Text(TextKind::Digits { max_len: 5 }),
    )
    .placeholder(Verbatim("2560"))
    .keywords(&["gamescope", "resolution", "width"])
    .requires(NEEDS_UPSCALING),
    game(
        "gamescope.upscaleHeight",
        "settings.field.gamescope_upscale_height.label",
        Performance,
        "settings.group.gamescope",
        Text(TextKind::Digits { max_len: 5 }),
    )
    .placeholder(Verbatim("1440"))
    .keywords(&["gamescope", "resolution", "height"])
    .requires(NEEDS_UPSCALING),
    game(
        "gamescope.enableLimiter",
        "settings.field.gamescope_enable_limiter.label",
        Performance,
        "settings.group.gamescope",
        Toggle,
    )
    .keywords(&["gamescope", "fps", "limit"]),
    game(
        "gamescope.fpsLimiter",
        "settings.field.gamescope_fps_limiter.label",
        Performance,
        "settings.group.gamescope",
        Text(TextKind::Digits { max_len: 3 }),
    )
    .placeholder(Verbatim("60"))
    .keywords(&["gamescope", "fps", "limit"])
    .requires(NEEDS_LIMITER),
    game(
        "gamescope.fpsLimiterNoFocus",
        "settings.field.gamescope_fps_limiter_no_focus.label",
        Performance,
        "settings.group.gamescope",
        Text(TextKind::Digits { max_len: 3 }),
    )
    .placeholder(Verbatim("30"))
    .keywords(&["gamescope", "fps", "limit", "focus", "background"])
    .requires(NEEDS_LIMITER),
    game(
        "gamescope.enableForceGrabCursor",
        "settings.field.gamescope_enable_force_grab_cursor.label",
        Performance,
        "settings.group.gamescope",
        Toggle,
    )
    .keywords(&["gamescope", "cursor", "mouse", "grab"]),
    game(
        "gamescope.additionalOptions",
        "settings.field.gamescope_additional_options.label",
        Performance,
        "settings.group.gamescope",
        Text(TextKind::Plain),
    )
    .placeholder(Verbatim("--hdr-enabled"))
    .keywords(&["gamescope", "options", "arguments"]),
    // ---- Launch ----
    game(
        "launcherArgs",
        "settings.field.launcher_args.label",
        Launch,
        "settings.group.command",
        Text(TextKind::Plain),
    )
    .help("settings.field.launcher_args.help")
    .placeholder(Verbatim("-nolauncher"))
    .keywords(&["arguments", "args", "parameters", "command line"]),
    game_only(
        "targetExe",
        "settings.field.target_exe.label",
        Launch,
        "settings.group.command",
        Text(TextKind::File),
    )
    .help("settings.field.target_exe.help")
    .placeholder(Key("settings.field.target_exe.placeholder"))
    .keywords(&["exe", "executable", "override", "alternative"]),
    game(
        "language",
        "settings.field.language.label",
        Launch,
        "settings.group.command",
        Text(TextKind::Plain),
    )
    .help("settings.field.language.help")
    .placeholder(Key("settings.field.language.placeholder"))
    .keywords(&["language", "locale", "translation"]),
    game_only(
        "steamRuntime",
        "settings.field.steam_runtime.label",
        Launch,
        "settings.group.native_linux",
        Choice(STEAM_RUNTIME),
    )
    .help("settings.field.steam_runtime.help")
    .keywords(&[
        "runtime",
        "scout",
        "soldier",
        "sniper",
        "native",
        "container",
        "steam",
    ])
    .native_only(),
    game(
        "wrapperOptions",
        "settings.field.wrapper_options.label",
        Launch,
        "settings.group.wrappers_and_environment",
        Custom(CustomKind::Wrappers),
    )
    .help("settings.field.wrapper_options.help")
    .keywords(&["wrapper", "prefix command", "strace", "obs-gamecapture"]),
    game(
        "enviromentOptions",
        "settings.field.environment_options.label",
        Launch,
        "settings.group.wrappers_and_environment",
        Custom(CustomKind::EnvVars),
    )
    .keywords(&["environment", "env", "variables", "dll overrides"]),
    game(
        "beforeLaunchScriptPath",
        "settings.field.before_launch_script_path.label",
        Launch,
        "settings.group.scripts",
        Text(TextKind::File),
    )
    .keywords(&["script", "before", "pre-launch"]),
    game(
        "afterLaunchScriptPath",
        "settings.field.after_launch_script_path.label",
        Launch,
        "settings.group.scripts",
        Text(TextKind::File),
    )
    .keywords(&["script", "after", "post-exit"]),
    game_only(
        "offlineMode",
        "settings.field.offline_mode.label",
        Launch,
        "settings.group.online",
        Toggle,
    )
    .tag(Verbatim("Epic"))
    .help("settings.field.offline_mode.help")
    .keywords(&["offline", "network", "internet"]),
    game_only(
        "ignoreGameUpdates",
        "settings.field.ignore_game_updates.label",
        Launch,
        "settings.group.online",
        Toggle,
    )
    .help("settings.field.ignore_game_updates.help")
    .keywords(&["update", "ignore", "pin", "version"]),
    game(
        "verboseLogs",
        "settings.field.verbose_logs.label",
        Launch,
        "settings.group.logging",
        Toggle,
    )
    .help("settings.field.verbose_logs.help")
    .keywords(&["log", "verbose", "debug"]),
    // ---- Cloud saves ----
    game(
        "autoSyncSaves",
        "settings.field.auto_sync_saves.label",
        CloudSaves,
        "settings.group.save_sync",
        Toggle,
    )
    .help("settings.field.auto_sync_saves.help")
    .keywords(&["cloud", "save", "sync"]),
    game_only(
        "savesPath",
        "settings.field.saves_path.label",
        CloudSaves,
        "settings.group.locations",
        Text(TextKind::Directory),
    )
    .tag(Verbatim("Epic"))
    .placeholder(Key("settings.field.saves_path.placeholder"))
    .keywords(&["save", "cloud", "path"]),
    game_only(
        "gogSaves",
        "settings.field.gog_saves.label",
        CloudSaves,
        "settings.group.locations",
        Custom(CustomKind::GogSaves),
    )
    .tag(Verbatim("GOG"))
    .keywords(&["save", "cloud", "path", "gog"]),
    game_only(
        "panel.sync-saves",
        "settings.field.panel_sync_saves.label",
        CloudSaves,
        "settings.group.sync_now",
        Panel(PanelKind::SaveSync),
    )
    .help("settings.field.panel_sync_saves.help")
    .keywords(&["cloud", "save", "sync", "upload", "download", "force"]),
];

/// The field with this path.
pub fn find(path: &str) -> Option<&'static FieldSpec> {
    FIELDS.iter().find(|f| f.path == path)
}

/// Fields of one section, in display order.
pub fn section_fields(section: FieldSection) -> impl Iterator<Item = &'static FieldSpec> {
    FIELDS
        .iter()
        .filter(move |f| f.supported_on_host() && f.section == section)
}

/// Fields matching a search query, in display order. Empty for a blank query.
pub fn search(query: &str) -> impl Iterator<Item = &'static FieldSpec> + '_ {
    FIELDS
        .iter()
        .filter(move |f| f.supported_on_host() && f.matches(query))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::get_path;
    use ember_core::{AppSettings, GameSettings};
    use std::collections::HashSet;

    #[test]
    fn paths_are_unique() {
        let mut seen = HashSet::new();
        for f in FIELDS {
            assert!(seen.insert(f.path), "duplicate field {}", f.path);
        }
    }

    #[test]
    fn every_value_field_exists_in_the_schema() {
        let app = serde_json::to_value(AppSettings::default()).unwrap();
        let game = serde_json::to_value(GameSettings::default()).unwrap();
        for f in FIELDS {
            if matches!(f.kind, FieldKind::Action(_)) {
                assert!(f.path.starts_with("action."), "{}", f.path);
                continue;
            }
            if matches!(f.kind, FieldKind::Panel(_)) {
                assert!(f.path.starts_with("panel."), "{}", f.path);
                continue;
            }
            assert!(!f.path.starts_with("panel.") && !f.path.starts_with("action."));
            let root = if f.scope == Scope::App { &app } else { &game };
            assert!(
                get_path(root, f.path).is_some(),
                "{} is not a settings key",
                f.path
            );
            // Game fields must also resolve in the flattened app settings
            // (game defaults are edited there).
            if f.scope != Scope::App {
                assert!(get_path(&app, f.path).is_some(), "{}", f.path);
            }
        }
    }

    #[test]
    fn conditions_point_at_booleans() {
        let app = serde_json::to_value(AppSettings::default()).unwrap();
        for f in FIELDS {
            for c in f.requires {
                assert!(
                    matches!(get_path(&app, c.path), Some(Value::Bool(_))),
                    "{} requires non-boolean {}",
                    f.path,
                    c.path
                );
            }
        }
    }

    #[test]
    fn game_sections_hold_only_game_fields() {
        for f in FIELDS {
            assert_eq!(f.section.is_game(), f.scope != Scope::App, "{}", f.path);
        }
    }

    #[test]
    fn search_matches_labels_keywords_and_tags() {
        crate::i18n::register();
        let paths = |q: &str| search(q).map(|f| f.path).collect::<Vec<_>>();
        assert_eq!(
            paths("fsync"),
            if cfg!(target_os = "linux") {
                vec!["enableFsync"]
            } else {
                vec![]
            }
        );
        assert!(paths("TRAY").contains(&"exitToTray"));
        assert!(paths("epic").contains(&"downloadNoHttps"));
        assert!(paths("epic").contains(&"offlineMode"));
        // Every token must match.
        let fps = paths("gamescope fps");
        assert_eq!(
            fps.contains(&"gamescope.fpsLimiter"),
            cfg!(target_os = "linux")
        );
        assert!(!fps.contains(&"enableDXVKFpsLimit"));
        assert!(paths("   ").is_empty());
        assert!(paths("no such setting").is_empty());
    }

    #[test]
    fn search_finds_actions() {
        crate::i18n::register();
        assert!(search("heroic").any(|f| f.path == "action.import-heroic"));
        assert!(search("cache").any(|f| f.path == "action.clear-caches"));
    }

    #[test]
    fn scope_filters_per_game_items() {
        let target = find("targetExe").unwrap();
        assert!(target.applies_to_game(true));
        assert!(!target.applies_to_game(false));
        let esync = find("enableEsync").unwrap();
        assert!(esync.applies_to_game(false) && esync.applies_to_game(true));
        assert!(!find("exitToTray").unwrap().applies_to_game(true));
        assert_eq!(
            find("gamescope.fpsLimiter").unwrap().override_key(),
            "gamescope"
        );
    }

    #[test]
    fn mac_options_show_on_macos_only() {
        let mac = cfg!(target_os = "macos");
        for path in [
            "emberRetinaMode",
            "wineCrossoverBottle",
            "emberCrossoverBottlesPath",
        ] {
            assert_eq!(find(path).unwrap().supported_on_host(), mac, "{path}");
        }
        assert!(
            !find("emberCrossoverBottlesPath")
                .unwrap()
                .applies_to_game(true)
        );
        assert!(find("emberRetinaMode").unwrap().applies_to_game(true));
    }

    #[test]
    fn steam_runtime_is_a_native_per_game_choice() {
        let natives: Vec<_> = FIELDS.iter().filter(|f| f.native_only).collect();
        assert_eq!(natives.len(), 1);
        let rt = natives[0];
        assert_eq!(rt.path, "steamRuntime");
        assert!(rt.applies_to_game(true));
        assert!(!rt.applies_to_game(false));
        assert_ne!(rt.section, FieldSection::Wine);
    }

    #[test]
    fn controller_and_epic_rows_are_findable() {
        crate::i18n::register();
        let paths = |q: &str| search(q).map(|f| f.path).collect::<Vec<_>>();
        let pads = paths("controller");
        for p in [
            "panel.controllers",
            "disableController",
            "gamepadInitialRepeatDelay",
            "gamepadRepeatDelay",
        ] {
            assert!(pads.contains(&p), "{p}");
            assert_eq!(find(p).unwrap().section, FieldSection::General);
        }
        assert!(paths("egl").contains(&"panel.egl-sync"));
        assert!(paths("eos overlay").contains(&"panel.eos-overlay"));
        assert!(paths("discord").contains(&"discordClientId"));
        assert!(paths("sync saves").contains(&"panel.sync-saves"));
        assert_eq!(find("panel.sync-saves").unwrap().scope, Scope::GameOnly);
    }

    #[test]
    fn language_choice_lists_every_interface_language_by_native_name() {
        let FieldKind::Choice(options) = find("uiLanguage").unwrap().kind else {
            panic!("uiLanguage is not a choice");
        };
        assert_eq!(options.len(), UI_LANGUAGES.len());
        for (o, lang) in options.iter().zip(UI_LANGUAGES) {
            assert_eq!(o.value, ChoiceValue::Str(lang.code));
            assert_eq!(o.label, Label::Verbatim(lang.native_name));
        }
        assert_eq!(find("uiLanguage").unwrap().section, FieldSection::General);
    }

    #[test]
    fn search_matches_english_text() {
        crate::i18n::register();
        let paths = |q: &str| search(q).map(|f| f.path).collect::<Vec<_>>();
        assert!(paths("default install folder").contains(&"defaultInstallPath"));
        assert!(paths("language").contains(&"uiLanguage"));
        assert!(paths("restart required").contains(&"noTrayIcon"));
    }

    #[test]
    fn choice_values_match_stored_json() {
        assert!(ChoiceValue::False.matches(&Value::Bool(false)));
        assert!(ChoiceValue::False.matches(&Value::Null));
        assert!(ChoiceValue::Str("fsr").matches(&Value::from("fsr")));
        assert!(ChoiceValue::Num(3).matches(&Value::from(3)));
        assert!(!ChoiceValue::Num(3).matches(&Value::from("3")));
    }
}
