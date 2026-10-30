//! Settings schema. Field names follow Heroic's JSON keys (including its
//! `enviromentOptions` spelling) so Heroic's `config.json` and `GamesConfig/*.json`
//! deserialize directly during import.
//!
//! Global defaults live in [`AppSettings::game`]. Per-game settings are stored as
//! a sparse override map ([`GameSettingsOverrides`]) layered on top of the
//! defaults, so changing a global default reaches every game that has not
//! overridden that field.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::Runner;
use crate::game::{CloudSaveLocation, LaunchOption};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WineKind {
    #[default]
    Wine,
    Proton,
    Crossover,
    Toolkit,
}

/// A Wine or Proton build usable to run Windows games.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct WineInstallation {
    /// `bin/wine` for Wine builds, the `proton` script for Proton.
    pub bin: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: WineKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lib: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lib32: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wineserver: Option<String>,
}

impl WineInstallation {
    pub fn is_proton(&self) -> bool {
        self.kind == WineKind::Proton
    }

    pub fn is_set(&self) -> bool {
        !self.bin.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Wrapper {
    pub exe: String,
    pub args: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GamescopeSettings {
    /// The on/off switch. `None` in settings saved before it existed:
    /// gamescope then runs when one of its options asks for it, as it did.
    /// Irori-only.
    #[serde(rename = "iroriEnabled")]
    pub enabled: Option<bool>,
    pub enable_upscaling: bool,
    pub enable_limiter: bool,
    pub enable_force_grab_cursor: bool,
    /// "fullscreen" | "borderless" | "windowed"
    pub window_type: String,
    pub game_width: String,
    pub game_height: String,
    pub upscale_width: String,
    pub upscale_height: String,
    /// "fsr" | "nis" | "integer" | "stretch" | ""
    pub upscale_method: String,
    pub fps_limiter: String,
    pub fps_limiter_no_focus: String,
    pub additional_options: String,
}

impl Default for GamescopeSettings {
    fn default() -> Self {
        Self {
            enabled: None,
            enable_upscaling: false,
            enable_limiter: false,
            enable_force_grab_cursor: false,
            window_type: "fullscreen".into(),
            game_width: String::new(),
            game_height: String::new(),
            upscale_width: String::new(),
            upscale_height: String::new(),
            upscale_method: "fsr".into(),
            fps_limiter: String::new(),
            fps_limiter_no_focus: String::new(),
            additional_options: String::new(),
        }
    }
}

impl GamescopeSettings {
    /// Whether games run inside gamescope.
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(
            self.enable_limiter
                || self.enable_upscaling
                || self.enable_force_grab_cursor
                || !self.additional_options.trim().is_empty(),
        )
    }

    /// Whether the window type and resolutions apply. Without the switch
    /// they only ever applied together with upscaling.
    pub fn sets_window(&self) -> bool {
        self.enabled == Some(true) || self.enable_upscaling
    }
}

/// Settings that apply to launching a game. Global defaults and per-game
/// effective values share this type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GameSettings {
    pub auto_install_dxvk: bool,
    pub auto_install_vkd3d: bool,
    pub auto_install_dxvk_nvapi: bool,
    pub auto_sync_saves: bool,
    pub battl_eye_runtime: bool,
    #[serde(rename = "DXVKFpsCap")]
    pub dxvk_fps_cap: String,
    pub eac_runtime: bool,
    #[serde(rename = "enableDXVKFpsLimit")]
    pub enable_dxvk_fps_limit: bool,
    pub enable_esync: bool,
    #[serde(rename = "enableFSR")]
    pub enable_fsr: bool,
    pub enable_msync: bool,
    pub enable_fsync: bool,
    pub enable_wine_wayland: bool,
    #[serde(rename = "enableHDR")]
    pub enable_hdr: bool,
    #[serde(rename = "enableWoW64")]
    pub enable_wow64: bool,
    pub gamescope: GamescopeSettings,
    #[serde(rename = "enviromentOptions")]
    pub environment_options: Vec<EnvVar>,
    pub ignore_game_updates: bool,
    pub language: String,
    pub launcher_args: String,
    pub last_used_launch_option: Option<LaunchOption>,
    pub max_sharpness: Option<u8>,
    pub nvidia_prime: bool,
    pub offline_mode: bool,
    pub prefer_system_libs: bool,
    pub show_fps: bool,
    pub show_mangohud: bool,
    pub target_exe: String,
    pub use_game_mode: bool,
    /// Steam runtime name (`"soldier"`, `"sniper"`, …) or none. Heroic writes
    /// `false` for none.
    #[serde(
        serialize_with = "ser_false_or_string",
        deserialize_with = "de_false_or_string"
    )]
    pub steam_runtime: Option<String>,
    pub wine_crossover_bottle: String,
    pub wine_prefix: String,
    pub wine_version: WineInstallation,
    pub wrapper_options: Vec<Wrapper>,
    pub saves_path: String,
    pub gog_saves: Vec<CloudSaveLocation>,
    pub before_launch_script_path: String,
    pub after_launch_script_path: String,
    #[serde(rename = "disableUMU")]
    pub disable_umu: bool,
    pub verbose_logs: bool,
    pub advertise_avx_for_rosetta: bool,
    pub enable_quick_saves_menu: bool,
    /// macOS: render at the display's full resolution (Wine's Mac driver
    /// `RetinaMode`). Irori-only.
    #[serde(rename = "iroriRetinaMode", alias = "emberRetinaMode")]
    pub retina_mode: bool,
    /// The emulator that plays this game (`duckstation`,
    /// `retroarch:<core>`); empty for the system's default. Irori-only.
    #[serde(rename = "iroriEngine", alias = "emberEngine")]
    pub engine: String,
    /// The installation of that emulator this game is pinned to
    /// (`managed:<version>`, `flatpak`, `system`, …); empty for the
    /// emulator's default. Irori-only.
    #[serde(rename = "iroriEngineVersion", alias = "emberEngineVersion")]
    pub engine_version: String,
    /// Emulator option values by emulator id. Per game, only the options
    /// set for that game; [`GameSettingsOverrides::apply`] merges them over
    /// the defaults option by option. Irori-only.
    #[serde(rename = "iroriEngineOptions", alias = "emberEngineOptions")]
    pub engine_options: BTreeMap<String, Map<String, Value>>,
    /// Leave the emulator's own configuration alone instead of the settings
    /// Irori manages. Irori-only.
    #[serde(rename = "iroriEngineOwnSettings", alias = "emberEngineOwnSettings")]
    pub engine_own_settings: bool,
    /// DXVK release tag for plain Wine builds; empty for the newest.
    /// Irori-only.
    #[serde(rename = "iroriDxvkVersion")]
    pub dxvk_version: String,
    /// VKD3D-Proton release tag for plain Wine builds; empty for the newest.
    /// Irori-only.
    #[serde(rename = "iroriVkd3dVersion")]
    pub vkd3d_version: String,
    /// Turn on VKD3D-Proton's descriptor heap. Irori-only.
    #[serde(rename = "iroriDescriptorHeap")]
    pub descriptor_heap: bool,
    /// `PROTON_DLSS_UPGRADE`. Irori-only.
    #[serde(rename = "iroriDlssUpgrade")]
    pub dlss_upgrade: bool,
    /// `PROTON_DLSS_INDICATOR`. Irori-only.
    #[serde(rename = "iroriDlssIndicator")]
    pub dlss_indicator: bool,
    /// `PROTON_FSR4_UPGRADE`. Irori-only.
    #[serde(rename = "iroriFsr4Upgrade")]
    pub fsr4_upgrade: bool,
    /// `PROTON_XESS_UPGRADE`. Irori-only.
    #[serde(rename = "iroriXessUpgrade")]
    pub xess_upgrade: bool,
    /// The Lutris install script that installed this game, so it can be
    /// reinstalled or updated later. Per game only. Irori-only.
    #[serde(
        rename = "iroriLutrisScript",
        alias = "emberLutrisScript",
        skip_serializing_if = "Option::is_none"
    )]
    pub lutris_script: Option<LutrisScriptRecord>,
}

/// Which Lutris install script installed a game (`iroriLutrisScript`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LutrisScriptRecord {
    /// The game's slug on lutris.net.
    pub slug: String,
    /// The installer's slug on lutris.net, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installer_slug: Option<String>,
    /// The installer's name ("GOG", "Setup"…).
    pub version: String,
    /// lutris.net's revision (or installer id) of the script.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Where the script came from: its lutris.net page or the local file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            auto_install_dxvk: true,
            auto_install_vkd3d: true,
            auto_install_dxvk_nvapi: true,
            auto_sync_saves: false,
            battl_eye_runtime: true,
            dxvk_fps_cap: String::new(),
            eac_runtime: true,
            enable_dxvk_fps_limit: false,
            enable_esync: true,
            enable_fsr: false,
            enable_msync: false,
            enable_fsync: true,
            enable_wine_wayland: false,
            enable_hdr: false,
            enable_wow64: false,
            gamescope: GamescopeSettings::default(),
            environment_options: Vec::new(),
            ignore_game_updates: false,
            language: String::new(),
            launcher_args: String::new(),
            last_used_launch_option: None,
            max_sharpness: None,
            nvidia_prime: false,
            offline_mode: false,
            prefer_system_libs: false,
            show_fps: false,
            show_mangohud: false,
            target_exe: String::new(),
            use_game_mode: false,
            steam_runtime: None,
            wine_crossover_bottle: "Heroic".into(),
            wine_prefix: String::new(),
            wine_version: WineInstallation::default(),
            wrapper_options: Vec::new(),
            saves_path: String::new(),
            gog_saves: Vec::new(),
            before_launch_script_path: String::new(),
            after_launch_script_path: String::new(),
            disable_umu: false,
            verbose_logs: true,
            advertise_avx_for_rosetta: false,
            dxvk_version: String::new(),
            vkd3d_version: String::new(),
            descriptor_heap: false,
            dlss_upgrade: false,
            dlss_indicator: false,
            fsr4_upgrade: false,
            xess_upgrade: false,
            enable_quick_saves_menu: false,
            retina_mode: false,
            engine: String::new(),
            engine_version: String::new(),
            engine_options: BTreeMap::new(),
            engine_own_settings: false,
            lutris_script: None,
        }
    }
}

fn ser_false_or_string<S: Serializer>(v: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(name) => s.serialize_str(name),
        None => s.serialize_bool(false),
    }
}

fn de_false_or_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) if !s.is_empty() => Some(s),
        _ => None,
    })
}

/// Sparse per-game settings: only the keys a user changed for that game.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "Map<String, Value>", into = "Map<String, Value>")]
pub struct GameSettingsOverrides(pub Map<String, Value>);

impl From<Map<String, Value>> for GameSettingsOverrides {
    /// Irori was called Ember before its first release; its own keys were
    /// `ember…`, so files written then keep their overrides.
    fn from(map: Map<String, Value>) -> Self {
        Self(
            map.into_iter()
                .map(|(k, v)| match k.strip_prefix("ember") {
                    Some(rest) if rest.starts_with(char::is_uppercase) => {
                        (format!("irori{rest}"), v)
                    }
                    _ => (k, v),
                })
                .collect(),
        )
    }
}

impl From<GameSettingsOverrides> for Map<String, Value> {
    fn from(overrides: GameSettingsOverrides) -> Self {
        overrides.0
    }
}

impl GameSettingsOverrides {
    /// Effective settings: `defaults` with these overrides applied.
    pub fn apply(&self, defaults: &GameSettings) -> GameSettings {
        if self.0.is_empty() {
            return defaults.clone();
        }
        let mut base = serde_json::to_value(defaults).expect("settings serialize");
        if let Value::Object(map) = &mut base {
            for (k, v) in &self.0 {
                // Emulator options merge per emulator and option, so a game
                // that sets one option keeps the defaults of the others.
                if k == "iroriEngineOptions"
                    && let (Some(Value::Object(base)), Value::Object(set)) = (map.get_mut(k), v)
                {
                    for (engine, options) in set {
                        match (base.get_mut(engine), options) {
                            (Some(Value::Object(b)), Value::Object(o)) => {
                                b.extend(o.iter().map(|(k, v)| (k.clone(), v.clone())))
                            }
                            _ => {
                                base.insert(engine.clone(), options.clone());
                            }
                        }
                    }
                    continue;
                }
                map.insert(k.clone(), v.clone());
            }
        }
        serde_json::from_value(base).unwrap_or_else(|_| defaults.clone())
    }

    /// Overrides that turn `defaults` into `effective`: every key whose value differs.
    pub fn diff(defaults: &GameSettings, effective: &GameSettings) -> Self {
        let (Value::Object(d), Value::Object(e)) = (
            serde_json::to_value(defaults).expect("settings serialize"),
            serde_json::to_value(effective).expect("settings serialize"),
        ) else {
            return Self::default();
        };
        Self(e.into_iter().filter(|(k, v)| d.get(k) != Some(v)).collect())
    }

    pub fn is_overridden(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.0.insert(key.to_string(), value);
    }

    pub fn reset(&mut self, key: &str) {
        self.0.remove(key);
    }
}

/// What the library shows above the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryTopSection {
    Disabled,
    #[default]
    RecentlyPlayed,
    RecentlyPlayedInstalled,
    Favourites,
}

/// Application-wide settings. Heroic stores these next to the default game
/// settings in one object; `game` is flattened to keep that shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(flatten)]
    pub game: GameSettings,
    pub add_desktop_shortcuts: bool,
    pub add_start_menu_shortcuts: bool,
    pub add_steam_shortcuts: bool,
    pub auto_update_games: bool,
    pub check_for_updates_on_startup: bool,
    /// Minutes between background update checks.
    pub check_updates_interval: u32,
    pub custom_wine_paths: Vec<String>,
    pub dark_tray_icon: bool,
    pub default_install_path: String,
    pub default_steam_path: String,
    pub default_wine_prefix_dir: String,
    pub disable_controller: bool,
    pub disable_playtime_sync: bool,
    pub disable_logs: bool,
    pub disable_animations: bool,
    #[serde(rename = "discordRPC")]
    pub discord_rpc: bool,
    /// Discord application ID used for Rich Presence (register one at
    /// discord.com/developers). Presence is off while empty.
    pub discord_client_id: String,
    #[serde(rename = "disableGOGPresence")]
    pub disable_gog_presence: bool,
    pub download_no_https: bool,
    pub download_proton_to_steam: bool,
    pub egs_linked_path: String,
    pub exit_to_tray: bool,
    pub gamepad_repeat_delay: u32,
    pub gamepad_initial_repeat_delay: u32,
    pub no_tray_icon: bool,
    pub hide_changelogs_on_startup: bool,
    pub hide_window_on_protocol_launch: bool,
    pub library_top_section: LibraryTopSection,
    pub max_recent_games: u32,
    /// Download workers (0 = store default).
    pub max_workers: u32,
    /// Download bandwidth limit in KiB/s (0 = unlimited).
    pub max_download_speed: u32,
    pub minimize_on_launch: bool,
    pub start_in_console_mode: bool,
    pub start_in_tray: bool,
    pub allow_installation_broken_anticheat: bool,
    pub show_valve_proton: bool,
    pub steam_grid_db_api_key: String,
    pub comet_support: bool,
    pub zoom_platform: bool,
    /// The itch.io store (off until turned on). Irori-only.
    #[serde(rename = "iroriItchIo", alias = "emberItchIo")]
    pub itch_io: bool,
    /// UI language code ("en", "de", …).
    #[serde(rename = "uiLanguage")]
    pub ui_language: String,
    #[serde(rename = "iroriUi", alias = "emberUi")]
    pub ui: UiPrefs,
    /// Where offline installers and goodies are saved (each game in its own
    /// folder inside).
    #[serde(rename = "iroriExtrasPath", alias = "emberExtrasPath")]
    pub extras_path: String,
    /// macOS: where CrossOver keeps its bottles, when moved from
    /// `~/Library/Application Support/CrossOver/Bottles` ("" = there).
    #[serde(
        rename = "iroriCrossoverBottlesPath",
        alias = "emberCrossoverBottlesPath"
    )]
    pub crossover_bottles_path: String,
    /// The default emulator of each system (`psx` → `duckstation`); systems
    /// not listed use the first installed recommended emulator.
    #[serde(rename = "iroriSystemEngines", alias = "emberSystemEngines")]
    pub system_engines: BTreeMap<String, String>,
    /// Folders scanned for console and retro games.
    #[serde(rename = "iroriRomFolders", alias = "emberRomFolders")]
    pub rom_folders: Vec<String>,
    /// Where the user keeps BIOS files ("" = not chosen).
    #[serde(rename = "iroriBiosFolder", alias = "emberBiosFolder")]
    pub bios_folder: String,
    /// List games of Battle.net, EA app, Ubisoft Connect and Rockstar
    /// (off until turned on). Irori-only.
    #[serde(
        rename = "iroriThirdPartyLaunchers",
        alias = "emberThirdPartyLaunchers"
    )]
    pub third_party_launchers: bool,
    /// Look for new versions of Irori once a day. Irori-only; about Irori
    /// itself, where `checkForUpdatesOnStartup` is about games.
    #[serde(rename = "iroriCheckForAppUpdates")]
    pub check_for_app_updates: bool,
    /// Open a game's log when it starts. Irori-only.
    #[serde(rename = "iroriShowLogOnLaunch")]
    pub show_log_on_launch: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let games = home.join("Games").join("Irori");
        let extras = dirs::download_dir()
            .unwrap_or_else(|| home.join("Downloads"))
            .join("Irori");
        Self {
            extras_path: extras.to_string_lossy().into_owned(),
            crossover_bottles_path: String::new(),
            system_engines: BTreeMap::new(),
            rom_folders: Vec::new(),
            bios_folder: String::new(),
            third_party_launchers: false,
            check_for_app_updates: true,
            show_log_on_launch: false,
            game: GameSettings::default(),
            add_desktop_shortcuts: false,
            add_start_menu_shortcuts: false,
            add_steam_shortcuts: false,
            auto_update_games: false,
            check_for_updates_on_startup: true,
            check_updates_interval: 10,
            custom_wine_paths: Vec::new(),
            dark_tray_icon: false,
            default_install_path: games.to_string_lossy().into_owned(),
            default_steam_path: crate::platform::steam_dir().to_string_lossy().into_owned(),
            default_wine_prefix_dir: games.join("Prefixes").to_string_lossy().into_owned(),
            disable_controller: false,
            disable_playtime_sync: false,
            disable_logs: false,
            disable_animations: false,
            discord_rpc: false,
            discord_client_id: String::new(),
            disable_gog_presence: false,
            download_no_https: false,
            download_proton_to_steam: false,
            egs_linked_path: String::new(),
            exit_to_tray: false,
            gamepad_repeat_delay: 50,
            gamepad_initial_repeat_delay: 300,
            no_tray_icon: false,
            hide_changelogs_on_startup: false,
            hide_window_on_protocol_launch: false,
            library_top_section: LibraryTopSection::RecentlyPlayed,
            max_recent_games: 8,
            max_workers: 0,
            max_download_speed: 0,
            minimize_on_launch: false,
            start_in_console_mode: false,
            start_in_tray: false,
            allow_installation_broken_anticheat: false,
            show_valve_proton: false,
            steam_grid_db_api_key: String::new(),
            comet_support: true,
            zoom_platform: true,
            itch_io: false,
            ui_language: "en".into(),
            ui: UiPrefs::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryLayout {
    #[default]
    Grid,
    List,
}

/// How much of the desktop shows through Irori's window chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Translucency {
    /// On where the system blurs what is behind the window (macOS,
    /// Windows 11, Linux compositors that offer blur), off elsewhere.
    #[default]
    Auto,
    Off,
    /// Translucent title bar and sidebar; pages stay solid.
    Subtle,
    /// Clearer chrome, and pages let a little of the desktop through too.
    Strong,
}

/// Irori-only presentation preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiPrefs {
    pub theme: ThemePreference,
    /// Theme name from the bundled/user theme registry ("" = default).
    pub theme_name: String,
    pub translucency: Translucency,
    /// Base font size in px; drives the rem scale (application zoom).
    pub font_size: f32,
    pub library_layout: LibraryLayout,
    /// Cover tile width in rem for the grid.
    pub tile_size: f32,
    pub reduce_motion: bool,
    pub show_store_badges: bool,
    pub sidebar_collapsed: bool,
    pub window_width: f32,
    pub window_height: f32,
    pub window_maximized: bool,
    /// Stores the library, accounts and store pages show.
    pub stores: ShownStores,
    /// Open Big Picture at startup in Steam's Game Mode or Big Picture.
    pub big_picture_in_game_mode: bool,
    /// Size of Big Picture's interface in percent, for TVs.
    pub big_picture_scale: u32,
}

/// Which stores Irori shows. A hidden store keeps its sign-in and installed
/// games on disk; its games, account card and store page are left out.
/// ZOOM Platform follows `zoomPlatform`, itch.io follows `iroriItchIo`, and
/// apps the user added are always shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShownStores {
    pub epic: bool,
    pub gog: bool,
    pub amazon: bool,
    pub steam: bool,
}

impl Default for ShownStores {
    fn default() -> Self {
        Self {
            epic: true,
            gog: true,
            amazon: true,
            steam: true,
        }
    }
}

impl ShownStores {
    /// Whether the store's games and pages are shown.
    pub fn shows(&self, runner: Runner) -> bool {
        match runner {
            Runner::Legendary => self.epic,
            Runner::Gog => self.gog,
            Runner::Nile => self.amazon,
            Runner::Steam => self.steam,
            Runner::Zoom
            | Runner::Itch
            | Runner::Sideload
            | Runner::Emulation
            | Runner::Launchers => true,
        }
    }

    /// Show or hide a store. ZOOM Platform, itch.io and added apps have no
    /// switch here, so they are left as they are.
    pub fn set(&mut self, runner: Runner, shown: bool) {
        match runner {
            Runner::Legendary => self.epic = shown,
            Runner::Gog => self.gog = shown,
            Runner::Nile => self.amazon = shown,
            Runner::Steam => self.steam = shown,
            Runner::Zoom
            | Runner::Itch
            | Runner::Sideload
            | Runner::Emulation
            | Runner::Launchers => {}
        }
    }

    /// Whether the store can be hidden with [`ShownStores::set`].
    pub fn can_hide(runner: Runner) -> bool {
        matches!(
            runner,
            Runner::Legendary | Runner::Gog | Runner::Nile | Runner::Steam
        )
    }
}

/// Sizes offered for Big Picture's interface, in percent.
pub const BIG_PICTURE_SCALES: [u32; 5] = [100, 125, 150, 175, 200];

impl UiPrefs {
    /// Big Picture's scale factor, limited to the offered range.
    pub fn big_picture_zoom(&self) -> f32 {
        self.big_picture_scale.clamp(100, 200) as f32 / 100.0
    }
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            theme_name: String::new(),
            translucency: Translucency::Auto,
            font_size: 16.0,
            library_layout: LibraryLayout::Grid,
            tile_size: 11.0,
            reduce_motion: false,
            show_store_badges: true,
            sidebar_collapsed: false,
            window_width: 1440.0,
            window_height: 900.0,
            window_maximized: false,
            stores: ShownStores::default(),
            big_picture_in_game_mode: true,
            big_picture_scale: 100,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ember_keys_load_under_irori_names() {
        let overrides: GameSettingsOverrides = serde_json::from_str(
            r#"{"emberEngine": "duckstation", "embers": 1, "wineVersion": {"name": "w"}}"#,
        )
        .unwrap();
        assert_eq!(overrides.0["iroriEngine"], "duckstation");
        assert!(overrides.0.contains_key("embers"));
        assert!(overrides.0.contains_key("wineVersion"));
        let json = serde_json::to_value(&overrides).unwrap();
        assert!(json.get("iroriEngine").is_some() && json.get("emberEngine").is_none());

        let app: AppSettings =
            serde_json::from_str(r#"{"emberItchIo": true, "emberEngine": "mgba"}"#).unwrap();
        assert!(app.itch_io);
        assert_eq!(serde_json::to_value(&app).unwrap()["iroriEngine"], "mgba");
    }

    #[test]
    fn app_update_check_is_on_unless_turned_off() {
        let app: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(app.check_for_app_updates);
        let app: AppSettings =
            serde_json::from_str(r#"{"iroriCheckForAppUpdates": false}"#).unwrap();
        assert!(!app.check_for_app_updates);
    }

    #[test]
    fn ui_prefs_from_older_files_keep_defaults() {
        let ui: UiPrefs = serde_json::from_str(r#"{"fontSize":18}"#).unwrap();
        assert_eq!(ui.stores, ShownStores::default());
        assert!(ui.big_picture_in_game_mode);
        assert_eq!(ui.big_picture_zoom(), 1.0);
        let ui: UiPrefs = serde_json::from_str(
            r#"{"stores":{"epic":false},"bigPictureScale":400,"bigPictureInGameMode":false}"#,
        )
        .unwrap();
        assert!(!ui.stores.shows(Runner::Legendary));
        assert!(ui.stores.shows(Runner::Gog));
        assert!(ui.stores.shows(Runner::Sideload));
        let mut stores = ui.stores;
        stores.set(Runner::Legendary, true);
        stores.set(Runner::Sideload, false);
        assert_eq!(stores, ShownStores::default());
        assert_eq!(ui.big_picture_zoom(), 2.0);
        assert!(!ui.big_picture_in_game_mode);
    }

    #[test]
    fn overrides_layer_on_defaults() {
        let defaults = GameSettings::default();
        let mut changed = defaults.clone();
        changed.show_fps = true;
        changed.launcher_args = "-dx11".into();
        let o = GameSettingsOverrides::diff(&defaults, &changed);
        assert_eq!(o.0.len(), 2);
        assert!(o.is_overridden("showFps"));
        assert_eq!(o.apply(&defaults), changed);
    }

    #[test]
    fn engine_options_merge_option_by_option() {
        let mut defaults = GameSettings::default();
        let options = |pairs: &[(&str, Value)]| -> Map<String, Value> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect()
        };
        defaults.engine_options.insert(
            "retroarch".into(),
            options(&[
                ("vsync", Value::Bool(true)),
                ("show_fps", Value::Bool(true)),
            ]),
        );
        let mut game = GameSettingsOverrides::default();
        game.set(
            "iroriEngineOptions",
            serde_json::json!({"retroarch": {"vsync": false}, "dolphin": {"fullscreen": false}}),
        );
        let effective = game.apply(&defaults);
        let ra = &effective.engine_options["retroarch"];
        assert_eq!(ra["vsync"], Value::Bool(false));
        assert_eq!(ra["show_fps"], Value::Bool(true));
        assert_eq!(
            effective.engine_options["dolphin"]["fullscreen"],
            Value::Bool(false)
        );
    }

    #[test]
    fn heroic_global_config_parses() {
        let json = r#"{
            "autoInstallDxvk": true, "enviromentOptions": [{"key":"A","value":"1"}],
            "wineVersion": {"bin":"/usr/bin/wine","name":"wine","type":"wine","wineserver":"/usr/bin/wineserver"},
            "steamRuntime": false, "DXVKFpsCap": "60", "disableUMU": false,
            "defaultInstallPath": "/games", "libraryTopSection": "disabled", "maxWorkers": 0
        }"#;
        let s: AppSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.game.environment_options[0].key, "A");
        assert_eq!(s.game.steam_runtime, None);
        assert_eq!(s.game.dxvk_fps_cap, "60");
        assert_eq!(s.default_install_path, "/games");
        assert_eq!(s.library_top_section, LibraryTopSection::Disabled);
        let back = serde_json::to_value(&s).unwrap();
        assert_eq!(back["steamRuntime"], Value::Bool(false));
        assert_eq!(back["enviromentOptions"][0]["value"], "1");
    }
}
