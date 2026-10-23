use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::game::DlcInfo;
use crate::runner::InstallPlatform;

/// What a store reports before installing: sizes, languages, DLCs, optional
/// components and channels. Shown by the install dialog.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct InstallInfo {
    pub app_name: String,
    pub title: String,
    pub platform: InstallPlatform,
    /// Bytes to download.
    pub download_size: u64,
    /// Bytes on disk after install.
    pub disk_size: u64,
    pub version: Option<String>,
    pub languages: Vec<String>,
    /// Per-language size override (GOG).
    pub language_sizes: Vec<LanguageSize>,
    pub dlcs: Vec<DlcInfo>,
    /// Selective-download components (Epic SDL / GOG optional components).
    pub sdl: Vec<SdlOption>,
    /// Beta branches (GOG).
    pub branches: Vec<String>,
    /// Pinnable builds (GOG), newest first.
    pub builds: Vec<BuildInfo>,
    /// Redistributables the game depends on.
    pub dependencies: Vec<String>,
    /// Owned game but this platform needs a third-party launcher.
    pub third_party_managed_app: Option<String>,
    /// Can the game run without Wine (native)?
    pub is_native: bool,
    /// The install can be resumed (an earlier partial download exists).
    pub resumable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LanguageSize {
    pub language: String,
    pub download_size: u64,
    pub disk_size: u64,
}

/// A selectable optional component.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SdlOption {
    pub id: String,
    pub name: String,
    /// Store-provided install tags passed to the downloader.
    pub tags: Vec<String>,
    /// Always installed; shown but not toggleable.
    pub mandatory: bool,
    pub default_selected: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildInfo {
    pub build_id: String,
    pub version_name: String,
    pub date: String,
    pub branch: Option<String>,
}

/// Parameters of an install chosen by the user.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct InstallRequest {
    /// Base directory; stores create the game folder inside it.
    pub path: PathBuf,
    pub platform: InstallPlatform,
    pub dlcs: Vec<String>,
    /// Selected SDL tags (flattened from the chosen [`SdlOption`]s).
    pub sdl: Vec<String>,
    pub language: Option<String>,
    pub branch: Option<String>,
    pub build: Option<String>,
    pub dependencies: Vec<String>,
    /// Install from this offline installer on disk instead of downloading
    /// (GOG; the installer's own platform applies).
    pub installer: Option<PathBuf>,
}

impl InstallRequest {
    /// An install with default options: into `install_dir`, no DLC, the
    /// store's default language, mandatory and default-selected optional
    /// components, and the required redistributables.
    pub fn with_defaults(
        info: &InstallInfo,
        install_dir: &std::path::Path,
        platform: InstallPlatform,
    ) -> Self {
        Self {
            path: install_dir.to_path_buf(),
            platform,
            sdl: info
                .sdl
                .iter()
                .filter(|o| o.mandatory || o.default_selected)
                .flat_map(|o| o.tags.iter().cloned())
                .collect(),
            dependencies: info.dependencies.clone(),
            ..Default::default()
        }
    }
}

/// What kind of file an [`ExtraFile`] is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtraGroup {
    /// A part of an offline installer of the game.
    #[default]
    Installer,
    Patch,
    LanguagePack,
    /// Bonus content: manuals, soundtracks, artwork, wallpapers…
    Goodie,
}

/// A file offered beside the game: an offline installer, a patch, a
/// language pack or a goodie.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtraFile {
    /// The store's handle for the file, passed back to
    /// [`crate::Store::extra_url`].
    pub id: String,
    pub name: String,
    pub group: ExtraGroup,
    /// The store's kind of goodie ("manuals", "audio", "video"…); empty for
    /// installers.
    pub category: String,
    pub platform: Option<InstallPlatform>,
    /// Language name as the store shows it ("English").
    pub language: Option<String>,
    pub version: Option<String>,
    /// 1-based part number and part count of a split installer.
    pub part: Option<(u32, u32)>,
    pub size: u64,
}

/// Parameters of an update. Empty fields keep the installed choice.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateRequest {
    pub dlcs: Option<Vec<String>>,
    pub language: Option<String>,
    pub branch: Option<String>,
    pub build: Option<String>,
    pub dependencies: Option<Vec<String>>,
}

/// Live progress of a long operation. All fields optional so stores report
/// what their tool prints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstallProgress {
    /// 0.0 ..= 100.0
    pub percent: Option<f32>,
    pub bytes_done: Option<u64>,
    pub bytes_total: Option<u64>,
    /// Seconds remaining.
    pub eta_secs: Option<u64>,
    /// Bytes per second.
    pub download_speed: Option<u64>,
    /// Bytes per second written to disk.
    pub disk_speed: Option<u64>,
    /// What holds the download back right now, when it can be told.
    pub bottleneck: Option<Bottleneck>,
    /// Work before the download proper, while it lasts.
    pub preparing: Option<Preparing>,
}

/// What limits a download's speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bottleneck {
    /// The internet connection is at its limit.
    Network,
    /// The download servers send slower than the connection could take.
    Server,
    /// Writing to disk is slower than downloading.
    Disk,
    /// The download speed limit in the settings.
    SpeedLimit,
    /// Requests keep failing; Ember retries until the connection is back.
    Reconnecting,
}

/// Progress of the work before a download starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preparing {
    pub stage: PrepareStage,
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrepareStage {
    /// Fetching the file lists of the build.
    FileLists,
    /// Checking files already on disk against the build.
    CheckingFiles,
}

/// Receives progress reports. Called from background threads; implementations
/// must be cheap (the services layer coalesces before notifying the UI).
pub type ProgressSink = Arc<dyn Fn(InstallProgress) + Send + Sync>;

/// No-op progress sink.
pub fn ignore_progress() -> ProgressSink {
    Arc::new(|_| {})
}

/// Result of a cancellable long operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "error", rename_all = "lowercase")]
pub enum Outcome {
    Done,
    Error(String),
    Abort,
}

impl Outcome {
    pub fn is_done(&self) -> bool {
        matches!(self, Outcome::Done)
    }
}

/// Options of an uninstall.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoveArgs {
    /// Delete the Wine prefix too.
    pub remove_prefix: bool,
    /// Delete game files (false keeps files, only forgets the install).
    pub delete_files: bool,
    /// Also remove per-game settings and logs.
    pub remove_settings: bool,
}

/// Direction of a cloud-save sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncDirection {
    /// Let the store decide from timestamps.
    Auto,
    Download,
    Upload,
    ForceDownload,
    ForceUpload,
}

impl SyncDirection {
    pub const ALL: [SyncDirection; 5] = [
        SyncDirection::Auto,
        SyncDirection::Download,
        SyncDirection::Upload,
        SyncDirection::ForceDownload,
        SyncDirection::ForceUpload,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SyncDirection::Auto => "Sync",
            SyncDirection::Download => "Download",
            SyncDirection::Upload => "Upload",
            SyncDirection::ForceDownload => "Force download",
            SyncDirection::ForceUpload => "Force upload",
        }
    }
}
