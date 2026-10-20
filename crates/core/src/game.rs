use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::runner::{GameId, InstallPlatform, Runner};

/// Everything the launcher knows about one library entry. Built by a store and
/// shared read-only (`Arc<GameInfo>`) with the UI; mutations go through the
/// store, which publishes a fresh value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GameInfo {
    pub runner: Option<Runner>,
    pub app_name: String,
    pub title: String,
    /// Sort key derived from the title (lowercase, articles stripped).
    #[serde(skip)]
    pub sort_title: String,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    pub description: Option<String>,
    pub namespace: Option<String>,
    pub store_url: Option<String>,
    /// Vertical cover (library tile).
    pub art_cover: Option<String>,
    /// Square/landscape art used when no cover exists.
    pub art_square: Option<String>,
    pub art_logo: Option<String>,
    pub art_background: Option<String>,
    pub art_icon: Option<String>,
    pub is_installed: bool,
    pub install: Option<InstalledInfo>,
    /// False for entries that are owned but cannot be installed (e.g. browser-only).
    pub installable: bool,
    pub can_run_offline: bool,
    pub cloud_save_enabled: bool,
    /// Raw save folder template (store variables not expanded).
    pub save_folder: Option<String>,
    /// Save folder with variables expanded, once known.
    pub save_path: Option<String>,
    pub gog_save_locations: Vec<CloudSaveLocation>,
    pub folder_name: Option<String>,
    /// Name of a third-party launcher the game requires (Ubisoft Connect, EA app…).
    pub third_party_managed_app: Option<String>,
    pub is_ea_managed: bool,
    pub is_ubisoft_managed: bool,
    pub is_linux_native: bool,
    pub is_mac_native: bool,
    /// Platforms the store offers builds for.
    pub platforms: Vec<InstallPlatform>,
    /// URL for browser games.
    pub browser_url: Option<String>,
    pub custom_user_agent: Option<String>,
    pub launch_full_screen: bool,
    /// Latest store version, compared with the installed version for updates.
    pub version: Option<String>,
    pub dlcs: Vec<DlcInfo>,
    /// True when this entry is itself a DLC (hidden from the library grid).
    pub is_dlc: bool,
    pub genres: Vec<String>,
    pub release_date: Option<String>,
    /// Aggregated review score, 0–100 (from GOG's GamesDB).
    pub rating: Option<u8>,
    /// When the account got the game, as RFC 3339, when the store says.
    pub date_added: Option<String>,
    /// The user hid the game on the store's own website.
    pub store_hidden: bool,
    pub extra: Option<ExtraInfo>,
}

impl GameInfo {
    /// Platform to install when the user picks no options: the native Linux
    /// build when the store offers one, else Windows.
    pub fn default_platform(&self) -> InstallPlatform {
        if self.platforms.contains(&InstallPlatform::Linux)
            || (self.platforms.is_empty() && self.is_linux_native)
        {
            InstallPlatform::Linux
        } else {
            InstallPlatform::Windows
        }
    }

    pub fn id(&self) -> GameId {
        GameId::new(
            self.runner.unwrap_or(Runner::Sideload),
            self.app_name.as_str(),
        )
    }

    pub fn runner(&self) -> Runner {
        self.runner.unwrap_or(Runner::Sideload)
    }

    /// Best art for a vertical tile, falling back through the alternatives.
    pub fn cover_art(&self) -> Option<&str> {
        self.art_cover
            .as_deref()
            .or(self.art_square.as_deref())
            .filter(|s| !s.is_empty())
    }

    /// Best art for a wide hero banner.
    pub fn hero_art(&self) -> Option<&str> {
        self.art_background
            .as_deref()
            .or(self.art_square.as_deref())
            .or(self.art_cover.as_deref())
            .filter(|s| !s.is_empty())
    }

    pub fn install_path(&self) -> Option<PathBuf> {
        self.install
            .as_ref()
            .and_then(|i| (!i.install_path.is_empty()).then(|| PathBuf::from(&i.install_path)))
    }

    pub fn installed_platform(&self) -> Option<InstallPlatform> {
        self.install.as_ref().map(|i| i.platform)
    }

    /// Fill derived fields. Stores call this once after building an entry.
    pub fn finalize(mut self) -> Self {
        self.sort_title = sort_key(&self.title);
        self
    }
}

/// Lowercase title with leading English articles removed, for natural sorting.
pub fn sort_key(title: &str) -> String {
    let lower = title.trim().to_lowercase();
    for article in ["the ", "a ", "an "] {
        if let Some(rest) = lower.strip_prefix(article) {
            return rest.to_string();
        }
    }
    lower
}

/// State of an installed game.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct InstalledInfo {
    pub install_path: String,
    pub executable: String,
    /// Human readable size ("12.4 GiB") as reported by the store.
    pub install_size: String,
    pub install_size_bytes: u64,
    pub version: String,
    pub platform: InstallPlatform,
    pub is_dlc: bool,
    pub installed_dlcs: Vec<String>,
    pub installed_with_dlcs: bool,
    pub language: Option<String>,
    pub version_etag: Option<String>,
    pub build_id: Option<String>,
    pub branch: Option<String>,
    /// Skip update checks (GOG version rollback).
    pub pinned_version: bool,
    pub is_dosbox: bool,
    pub sdl: Vec<String>,
}

/// A DLC entry of a base game.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DlcInfo {
    pub app_name: String,
    pub title: String,
    pub is_installed: bool,
    /// Owned but stored separately (installable through the base game).
    pub owned: bool,
}

/// GOG cloud-save location.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudSaveLocation {
    pub name: String,
    pub location: String,
}

/// Store-side details that are fetched lazily for the game page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtraInfo {
    pub description: Option<String>,
    pub short_description: Option<String>,
    pub requirements: Vec<Requirement>,
    pub release_date: Option<String>,
    pub store_url: Option<String>,
    pub changelog: Option<String>,
    pub genres: Vec<String>,
    /// Full-size screenshot URLs from the store page.
    pub screenshots: Vec<String>,
}

/// One row of system requirements.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Requirement {
    pub title: String,
    pub minimum: String,
    pub recommended: String,
}

/// An alternative way to launch a game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LaunchOption {
    /// Extra parameters appended to the launch command.
    #[serde(alias = "basic")]
    Basic { name: String, parameters: String },
    /// Run a different executable.
    AltExe { executable: String },
    /// Launch a DLC that ships as its own game.
    Dlc {
        #[serde(rename = "dlcAppName")]
        dlc_app_name: String,
        #[serde(rename = "dlcTitle")]
        dlc_title: String,
    },
}

impl LaunchOption {
    pub fn label(&self) -> String {
        match self {
            LaunchOption::Basic { name, .. } => name.clone(),
            LaunchOption::AltExe { executable } => executable.clone(),
            LaunchOption::Dlc { dlc_title, .. } => dlc_title.clone(),
        }
    }
}

/// A store achievement (GOG today).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Achievement {
    pub id: String,
    pub key: String,
    pub visible: bool,
    pub name: String,
    pub description: String,
    pub image_unlocked: String,
    pub image_locked: String,
    /// Percentage of players that unlocked it.
    pub rarity: f32,
    pub rarity_label: String,
    /// RFC 3339 timestamp, if unlocked.
    pub unlocked_at: Option<String>,
}

/// User account of a store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserInfo {
    pub account_id: String,
    pub display_name: String,
}

/// Per-game metadata kept by the launcher itself (not the store).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameOverrides {
    pub title: Option<String>,
    pub art_cover: Option<String>,
    pub art_square: Option<String>,
    pub art_background: Option<String>,
    pub art_logo: Option<String>,
}

impl GameOverrides {
    /// Whether any field replaces the store's value.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Playtime record of one game.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaytimeRecord {
    /// Total minutes played.
    pub total_minutes: u64,
    /// Seconds played beyond `total_minutes` (carried to the next session).
    pub extra_seconds: u64,
    /// RFC 3339 timestamp of the last session start.
    pub last_played: Option<String>,
    /// RFC 3339 timestamp of the first session start.
    pub first_played: Option<String>,
}

pub type PlaytimeMap = BTreeMap<String, PlaytimeRecord>;
