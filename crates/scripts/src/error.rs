//! Typed errors callers match on: why a script was refused before it ran,
//! and the failures the installer window explains with a fix.

use std::fmt;
use std::path::PathBuf;

/// A launcher Ember supports natively, which scripts must not install
/// inside Wine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeLauncher {
    EpicGames,
    /// The EA app and its predecessor, Origin.
    EaApp,
    UbisoftConnect,
}

impl fmt::Display for NativeLauncher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NativeLauncher::EpicGames => "the Epic Games Launcher",
            NativeLauncher::EaApp => "the EA app",
            NativeLauncher::UbisoftConnect => "Ubisoft Connect",
        })
    }
}

/// Why a script was refused or failed. Found anywhere in an `anyhow` chain
/// with [`script_error`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError {
    #[error("The install script could not be read: {0}")]
    Syntax(String),
    #[error("This is not an install script: {0}")]
    NotAScript(String),
    #[error("The file holds {0} install scripts; choose one")]
    SeveralScripts(usize),
    #[error("Step {step}: “{name}” is not a command Ember supports, so the script was refused")]
    UnknownCommand { step: usize, name: String },
    #[error("Step {step}: “{name}” is not a task Ember supports, so the script was refused")]
    UnknownTask { step: usize, name: String },
    #[error("{location}: unknown setting “{key}”, so the script was refused")]
    UnknownKey { location: String, key: String },
    #[error("{location}: “{key}” is missing")]
    MissingKey { location: String, key: String },
    #[error("{location}: {message}")]
    Invalid { location: String, message: String },
    #[error("{location}: the variable ${name} does not exist")]
    UnknownVariable { location: String, name: String },
    #[error("{location}: the variable ${name} has no value yet at this point")]
    UnsetVariable { location: String, name: String },
    #[error("The script defines “{name}”, which would replace Ember’s own ${name}")]
    ShadowedVariable { name: String },
    #[error("The file “{id}” is listed twice")]
    DuplicateFile { id: String },
    #[error("The file “{id}” uses an address Ember does not download from: {url}")]
    UnsupportedUrl { id: String, url: String },
    #[error(
        "This script installs {launcher} inside Wine. Ember supports it natively; use the store or launcher setup instead"
    )]
    PreferNative { launcher: NativeLauncher },
    #[error("“{file}” is damaged or not the expected file (expected {expected}, got {actual})")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },
    #[error(
        "{location}: refusing to write to {} because it is outside the game folder, the cache and the Wine prefix",
        path.display()
    )]
    OutsideAllowedFolders { location: String, path: PathBuf },
    #[error("{location}: the program exited with code {code}")]
    ExitCode { location: String, code: i32 },
    #[error("{location}: {} does not exist", path.display())]
    NotFound { location: String, path: PathBuf },
    #[error("{}", missing_message(expected))]
    MissingExecutable { expected: Option<PathBuf> },
    #[error("The install was not approved: {0}")]
    NotApproved(String),
}

fn missing_message(expected: &Option<PathBuf>) -> String {
    match expected {
        Some(p) => format!(
            "The install finished, but the game’s program {} is missing. Choose the game’s program",
            p.display()
        ),
        None => "The install finished, but the script names no program to start. Choose the game’s program".into(),
    }
}

/// The [`ScriptError`] in `err`'s chain, if any.
pub fn script_error(err: &anyhow::Error) -> Option<&ScriptError> {
    err.chain().find_map(|e| e.downcast_ref::<ScriptError>())
}

/// Where in a script something is, for error messages.
pub(crate) fn step_loc(number: usize, command: &str) -> String {
    format!("Step {number} ({command})")
}
