//! The typed form of a Lutris install script. Every command and task is an
//! enum variant with typed parameters; [`crate::parse`] builds these and
//! refuses anything it does not know.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// A parsed, validated install script.
#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    pub info: ScriptInfo,
    pub files: Vec<FileSpec>,
    pub steps: Vec<Step>,
    pub game: GameSection,
    /// `system.env`: set for every program the script runs.
    pub system_env: BTreeMap<String, String>,
    /// The whole `system` section, for the game's settings.
    pub system: Map<String, Value>,
    /// Runner sections such as `wine:` or `dosbox:`, by runner name.
    pub runner_sections: BTreeMap<String, Map<String, Value>>,
    /// Dependencies: every group is needed, any slug of a group will do.
    pub requires: Vec<Vec<String>>,
    /// Like `requires`, but the script adds to that game instead of
    /// creating a new one.
    pub extends: Option<String>,
    pub variables: BTreeMap<String, String>,
    /// Host programs that must exist; every group is needed, any of a group
    /// will do.
    pub require_binaries: Vec<Vec<String>>,
    pub install_complete_text: Option<String>,
}

impl Script {
    /// The Wine build the script asks for (`wine: {version: …}`).
    pub fn wine_version(&self) -> Option<&str> {
        self.runner_sections
            .get("wine")
            .and_then(|w| w.get("version"))
            .and_then(Value::as_str)
    }

    /// Whether any step runs a shell command.
    pub fn runs_shell(&self) -> bool {
        self.steps.iter().any(|s| {
            matches!(
                &s.command,
                Command::Execute(Execute {
                    program: Program::Shell(_),
                    ..
                })
            )
        })
    }
}

/// Who wrote a script and where it comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptInfo {
    pub name: String,
    pub game_slug: String,
    /// The installer's own name, such as "GOG" or "Setup".
    pub version: String,
    /// The installer slug on lutris.net.
    pub slug: Option<String>,
    pub installer_slug: Option<String>,
    pub runner: String,
    pub description: Option<String>,
    pub notes: Option<String>,
    pub credits: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub author: Option<String>,
    pub year: Option<String>,
    /// The lutris.net installer id, which changes with each published
    /// revision.
    pub id: Option<u64>,
    pub revision: Option<String>,
    pub steamid: Option<String>,
    pub gogid: Option<String>,
    pub service: Option<String>,
    pub service_id: Option<String>,
}

impl ScriptInfo {
    /// The game's page on lutris.net.
    pub fn page_url(&self) -> String {
        crate::api::page_url(&self.game_slug)
    }

    /// The installer slug, from whichever field the source used.
    pub fn installer_slug(&self) -> Option<&str> {
        self.installer_slug.as_deref().or(self.slug.as_deref())
    }
}

/// A file the script needs, by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSpec {
    /// The id, with `-` turned into `_` as Lutris does.
    pub id: String,
    pub source: FileSource,
    pub filename: Option<String>,
    pub checksum: Option<Checksum>,
    pub referer: Option<String>,
    /// Size in bytes, when the script states it.
    pub size: Option<u64>,
}

/// Where a file comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSource {
    /// An http(s) address, possibly with variables.
    Url(String),
    /// `N/A:<prompt>`: the user picks a file.
    UserPick { prompt: Option<String> },
    /// `$STEAM:<appid>:<path>`: a path inside an installed Steam game.
    Steam { appid: String, path: String },
}

/// Hash algorithms a script's `checksum` may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl HashAlgorithm {
    pub fn name(self) -> &'static str {
        match self {
            HashAlgorithm::Md5 => "md5",
            HashAlgorithm::Sha1 => "sha1",
            HashAlgorithm::Sha256 => "sha256",
            HashAlgorithm::Sha512 => "sha512",
        }
    }

    pub(crate) fn hex_len(self) -> usize {
        match self {
            HashAlgorithm::Md5 => 32,
            HashAlgorithm::Sha1 => 40,
            HashAlgorithm::Sha256 => 64,
            HashAlgorithm::Sha512 => 128,
        }
    }
}

/// `<algorithm>:<hex>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checksum {
    pub algorithm: HashAlgorithm,
    /// Lower-case hex.
    pub hex: String,
}

impl std::fmt::Display for Checksum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.algorithm.name(), self.hex)
    }
}

/// One installer step, numbered from 1 as shown to the user.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub number: usize,
    /// Status text the script gives for the step.
    pub description: Option<String>,
    pub command: Command,
}

/// Every command Ember runs. Strings are raw script text; variables are
/// substituted when the step runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Set the execute bit.
    Chmodx {
        file: String,
    },
    Extract(Extract),
    /// Copy a file into a folder, or merge a folder into another.
    Merge(CopyParams),
    /// Same as `merge`.
    Copy(CopyParams),
    Move(MoveParams),
    Rename {
        src: String,
        dst: String,
    },
    /// `mkdir` (also spelled `create_dir`).
    Mkdir {
        path: String,
    },
    WriteFile {
        file: String,
        content: String,
        append: bool,
    },
    WriteJson {
        file: String,
        data: Value,
        merge: bool,
    },
    WriteConfig {
        file: String,
        entries: Vec<ConfigEntry>,
        merge: bool,
    },
    SubstituteVars {
        file: String,
    },
    InputMenu(InputMenu),
    InsertDisc {
        requires: String,
        message: Option<String>,
    },
    Execute(Execute),
    Task(Task),
    /// A store's own installer (GOG, Amazon, ZOOM): Ember installs it
    /// natively instead.
    StoreAutosetup(StoreAutosetup),
}

impl Command {
    /// The command's name as scripts spell it.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Chmodx { .. } => "chmodx",
            Command::Extract(_) => "extract",
            Command::Merge(_) => "merge",
            Command::Copy(_) => "copy",
            Command::Move(_) => "move",
            Command::Rename { .. } => "rename",
            Command::Mkdir { .. } => "mkdir",
            Command::WriteFile { .. } => "write_file",
            Command::WriteJson { .. } => "write_json",
            Command::WriteConfig { .. } => "write_config",
            Command::SubstituteVars { .. } => "substitute_vars",
            Command::InputMenu(_) => "input_menu",
            Command::InsertDisc { .. } => "insert_disc",
            Command::Execute(_) => "execute",
            Command::Task(t) => t.name(),
            Command::StoreAutosetup(a) => a.command,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extract {
    /// A file id, a path or a glob.
    pub file: String,
    /// Destination folder; the game folder when absent.
    pub dst: Option<String>,
    pub format: Option<ArchiveFormat>,
    /// Move the contents of a single top-level folder up (Lutris's default;
    /// `nomerge` turns it off).
    pub merge_single: bool,
}

/// Archive kinds. Tar and zip are unpacked by Ember itself; the rest go to
/// [`crate::ScriptHost::extract_external`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveFormat {
    /// Tar with any compression (gzip, bzip2, xz, zstd or none).
    Tar,
    Zip,
    /// One gzip-compressed file.
    Gzip,
    /// 7z, rar, Inno Setup, MSI, deb, AppImage, exe…, by Lutris's name.
    External(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyParams {
    pub src: String,
    pub dst: String,
    pub optional: bool,
    pub remove_source: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveParams {
    pub src: String,
    pub dst: String,
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    pub section: String,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputMenu {
    pub id: Option<String>,
    pub description: Option<String>,
    pub options: Vec<MenuOption>,
    pub preselect: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Execute {
    pub program: Program,
    /// Raw argument string, split like a shell would.
    pub args: String,
    pub terminal: bool,
    pub working_dir: Option<String>,
    pub env: BTreeMap<String, String>,
    pub include_processes: Vec<String>,
    pub exclude_processes: Vec<String>,
    /// An exit code accepted besides 0.
    pub return_code: Option<i32>,
    pub disable_runtime: bool,
}

/// What `execute` runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Program {
    /// A file id, a path, or a program on `PATH`.
    File(String),
    /// A shell command line, run with `bash -c`.
    Shell(String),
}

/// `task` commands, by name (a `wine.` or `dosbox.` prefix is accepted).
#[derive(Debug, Clone, PartialEq)]
pub enum Task {
    CreatePrefix(PrefixTask),
    WineExec(WineExecTask),
    Winetricks(WinetricksTask),
    SetRegedit(RegeditTask),
    WineKill(WineKillTask),
    DosExec(DosExecTask),
}

impl Task {
    pub fn name(&self) -> &'static str {
        match self {
            Task::CreatePrefix(_) => "create_prefix",
            Task::WineExec(_) => "wineexec",
            Task::Winetricks(_) => "winetricks",
            Task::SetRegedit(_) => "set_regedit",
            Task::WineKill(_) => "winekill",
            Task::DosExec(_) => "dosexec",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixTask {
    pub prefix: Option<String>,
    pub arch: Option<String>,
    pub overrides: BTreeMap<String, String>,
    pub install_gecko: Option<bool>,
    pub install_mono: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WineExecTask {
    pub executable: String,
    pub args: String,
    pub prefix: Option<String>,
    pub arch: Option<String>,
    pub working_dir: Option<String>,
    pub blocking: bool,
    pub env: BTreeMap<String, String>,
    pub overrides: BTreeMap<String, String>,
    pub include_processes: Vec<String>,
    pub exclude_processes: Vec<String>,
    pub disable_runtime: bool,
    pub return_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WinetricksTask {
    /// Space-separated verbs.
    pub app: String,
    pub prefix: Option<String>,
    pub arch: Option<String>,
    pub silent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegeditTask {
    pub path: String,
    pub key: String,
    pub value: String,
    pub kind: RegType,
    pub prefix: Option<String>,
    pub arch: Option<String>,
}

/// Registry value types `set_regedit` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegType {
    Sz,
    ExpandSz,
    MultiSz,
    Dword,
    Qword,
    Binary,
}

impl RegType {
    pub fn name(self) -> &'static str {
        match self {
            RegType::Sz => "REG_SZ",
            RegType::ExpandSz => "REG_EXPAND_SZ",
            RegType::MultiSz => "REG_MULTI_SZ",
            RegType::Dword => "REG_DWORD",
            RegType::Qword => "REG_QWORD",
            RegType::Binary => "REG_BINARY",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_uppercase().as_str() {
            "REG_SZ" => RegType::Sz,
            "REG_EXPAND_SZ" => RegType::ExpandSz,
            "REG_MULTI_SZ" => RegType::MultiSz,
            "REG_DWORD" => RegType::Dword,
            "REG_QWORD" => RegType::Qword,
            "REG_BINARY" => RegType::Binary,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WineKillTask {
    pub prefix: Option<String>,
    pub arch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DosExecTask {
    pub config_file: Option<String>,
    pub executable: Option<String>,
    pub args: String,
    pub exit: bool,
    pub working_dir: Option<String>,
}

/// Stores whose Lutris autosetup Ember replaces with its native install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutosetupStore {
    Gog,
    Amazon,
    Zoom,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoreAutosetup {
    pub store: AutosetupStore,
    /// The script's command name (`autosetup_gog_game`, `gogdl_setup`…).
    pub command: &'static str,
    /// The store's id for the game.
    pub id: String,
    /// The installer's file id, when the command names one.
    pub file: Option<String>,
    pub silent: bool,
    /// The command's parameters as written, for the store install.
    pub params: Value,
}

/// The `game:` section: the entry point and the game's options.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GameSection {
    pub exe: Option<String>,
    pub main_file: Option<String>,
    pub iso: Option<String>,
    pub rom: Option<String>,
    pub path: Option<String>,
    pub args: Option<String>,
    pub working_dir: Option<String>,
    pub prefix: Option<String>,
    pub arch: Option<String>,
    /// Every other key, as written.
    pub options: Map<String, Value>,
}

/// Lutris's markers asking it to find the program itself.
pub(crate) const AUTO_EXE_MARKERS: [&str; 2] = ["_xXx_AUTO_ELF_xXx_", "_xXx_AUTO_WIN32_xXx_"];
