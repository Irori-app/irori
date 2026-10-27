//! What the interpreter asks of Ember: prompts, Wine, programs and store
//! installs. Every request is fully resolved (variables substituted, paths
//! checked) before it reaches the host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use ember_core::process::CancellationToken;

use crate::model::{AutosetupStore, Checksum, MenuOption, RegType};

/// A file the user picks (`N/A:<prompt>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRequest {
    pub file_id: String,
    pub prompt: Option<String>,
    /// The file name the script expects, if it names one.
    pub filename: Option<String>,
    /// Checked after the pick, when the script gives one.
    pub checksum: Option<Checksum>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuRequest {
    pub id: Option<String>,
    pub description: Option<String>,
    pub options: Vec<MenuOption>,
    pub preselect: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscRequest {
    /// A path (relative to the disc root) that must exist on the disc.
    pub requires: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixRequest {
    pub prefix: PathBuf,
    pub arch: Option<String>,
    pub overrides: BTreeMap<String, String>,
    pub install_gecko: Option<bool>,
    pub install_mono: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WineExecRequest {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub prefix: PathBuf,
    pub arch: Option<String>,
    pub working_dir: Option<PathBuf>,
    pub blocking: bool,
    /// The script's `system.env` plus the task's own `env`.
    pub env: BTreeMap<String, String>,
    pub overrides: BTreeMap<String, String>,
    pub include_processes: Vec<String>,
    pub exclude_processes: Vec<String>,
    pub disable_runtime: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WinetricksRequest {
    pub verbs: Vec<String>,
    pub prefix: PathBuf,
    pub arch: Option<String>,
    pub silent: bool,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRequest {
    pub path: String,
    pub key: String,
    pub value: String,
    pub kind: RegType,
    pub prefix: PathBuf,
    pub arch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WineKillRequest {
    pub prefix: PathBuf,
    pub arch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DosExecRequest {
    pub config_file: Option<PathBuf>,
    pub executable: Option<PathBuf>,
    pub args: Vec<String>,
    /// Close DOSBox when the program ends.
    pub exit: bool,
    pub working_dir: Option<PathBuf>,
}

/// A program the script runs directly (`execute` with `file`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRequest {
    /// A path, or a bare name to look up on `PATH`.
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub working_dir: PathBuf,
    pub terminal: bool,
    pub include_processes: Vec<String>,
    pub exclude_processes: Vec<String>,
    pub disable_runtime: bool,
}

/// A shell command line (`execute` with `command`), already approved by the
/// user. Lutris runs these with `bash -c`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellRequest {
    pub command: String,
    pub working_dir: PathBuf,
    pub env: BTreeMap<String, String>,
    pub terminal: bool,
}

/// A store install replacing a Lutris autosetup command.
#[derive(Debug, Clone, PartialEq)]
pub struct AutosetupRequest {
    pub store: AutosetupStore,
    /// The script command it replaces.
    pub command: &'static str,
    pub id: String,
    /// The store installer the script downloaded, if any.
    pub file: Option<PathBuf>,
    pub game_dir: PathBuf,
    pub silent: bool,
    pub params: serde_json::Value,
}

/// An archive Ember does not unpack itself (7z, rar, Inno Setup, MSI…).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractRequest {
    pub archive: PathBuf,
    /// An empty folder to unpack into; the interpreter moves the result into
    /// place.
    pub dst: PathBuf,
    /// Lutris's extractor name (`7z`, `gog`, `exe`…), or `auto`.
    pub format: String,
}

/// Ember's side of a script install. Methods returning `Option` return
/// `None` when the user cancelled the prompt, which cancels the install.
/// Long-running methods receive the install's cancellation token and
/// should stop promptly when it fires.
#[async_trait]
pub trait ScriptHost: Send + Sync {
    /// Ask the user for a file (`N/A:<prompt>`).
    async fn pick_file(&self, request: &FileRequest) -> anyhow::Result<Option<PathBuf>>;

    /// The path of `path` inside the installed Steam app `appid`
    /// (`$STEAM:<appid>:<path>`), installing the app first if needed.
    async fn steam_file(
        &self,
        appid: &str,
        path: &str,
        cancel: &CancellationToken,
    ) -> anyhow::Result<PathBuf>;

    /// Show a menu and return the chosen option's value.
    async fn choose(&self, menu: &MenuRequest) -> anyhow::Result<Option<String>>;

    /// Wait for a disc (or a folder the user picks) containing
    /// `request.requires`; returns its root.
    async fn insert_disc(&self, request: &DiscRequest) -> anyhow::Result<Option<PathBuf>>;

    /// The Wine program used for the install (`$WINEBIN`).
    async fn wine_bin(&self) -> anyhow::Result<PathBuf>;

    async fn create_prefix(
        &self,
        request: &PrefixRequest,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()>;

    /// Run a Windows program under Wine; returns its exit code.
    async fn wineexec(
        &self,
        request: &WineExecRequest,
        cancel: &CancellationToken,
    ) -> anyhow::Result<i32>;

    async fn winetricks(
        &self,
        request: &WinetricksRequest,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()>;

    async fn set_regedit(
        &self,
        request: &RegistryRequest,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()>;

    async fn winekill(
        &self,
        request: &WineKillRequest,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()>;

    /// Run a program in DOSBox; returns its exit code.
    async fn dosexec(
        &self,
        request: &DosExecRequest,
        cancel: &CancellationToken,
    ) -> anyhow::Result<i32>;

    /// Run a program; returns its exit code.
    async fn execute(
        &self,
        request: &ExecRequest,
        cancel: &CancellationToken,
    ) -> anyhow::Result<i32>;

    /// Run an approved shell command line; returns its exit code.
    async fn run_shell(
        &self,
        request: &ShellRequest,
        cancel: &CancellationToken,
    ) -> anyhow::Result<i32>;

    /// Install the game through Ember's native store instead of the
    /// script's autosetup command.
    async fn store_autosetup(
        &self,
        request: &AutosetupRequest,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()>;

    /// Unpack an archive format Ember has no Rust extractor for. The
    /// default refuses.
    async fn extract_external(
        &self,
        request: &ExtractRequest,
        _cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        anyhow::bail!(
            "Ember cannot unpack {} ({} archives are not supported yet)",
            file_label(&request.archive),
            request.format
        )
    }
}

fn file_label(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}
