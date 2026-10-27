//! A safe interpreter for Lutris community install scripts.
//!
//! * [`Script::parse`] reads a script (YAML or JSON, bare or as lutris.net's
//!   installer object) into a typed plan. Unknown commands, tasks,
//!   parameters and variables refuse the script, naming the step; scripts
//!   that install the Epic Games Launcher, the EA app or Ubisoft Connect in
//!   Wine are refused with [`ScriptError::PreferNative`].
//! * [`consent`] lists what the script will do (downloads, programs, shell
//!   commands verbatim, registry edits, writes outside the game folder) for
//!   the user to approve as [`Approvals`].
//! * [`run`] executes an approved script. File commands run here, confined
//!   to the game folder, the cache, the prefix and approved paths; Wine,
//!   programs, prompts and store installs go through [`ScriptHost`]. Failure
//!   or cancel rolls the game folder back and keeps downloads; an install
//!   that only lacks the game's program keeps its files
//!   ([`UnfinishedInstall`]).
//! * [`LutrisClient`] searches lutris.net and fetches a game's scripts, on
//!   request only, rate limited and cached.
//! * [`match_wine_version`] maps a script's Wine build to an available one.
//!
//! Lutris is a reference for behaviour only; no Lutris code is used.

#![forbid(unsafe_code)]

pub mod api;
mod archive;
mod consent;
mod context;
mod error;
mod exec;
mod fetch;
mod fsops;
mod host;
pub mod model;
mod parse;
mod validate;
mod vars;
mod wine;

pub use api::{GameResult, InstallerListing, LutrisClient};
pub use consent::{
    Approvals, AutosetupUse, Consent, Download, OutsideWrite, ProgramKind, ProgramRun,
    RegistryEdit, ShellCommand, SteamFile, UserFile, WinetricksUse, consent,
};
pub use context::{Context, GameEntry, Outcome, Progress, ProgressFn, ScriptRecord};
pub use error::{NativeLauncher, ScriptError, UnfinishedInstall, script_error};
pub use exec::run;
pub use host::{
    AutosetupRequest, DiscRequest, DosExecRequest, ExecRequest, ExtractRequest, FileRequest,
    MenuRequest, PrefixRequest, RegistryRequest, ScriptHost, ShellRequest, WineExecRequest,
    WineKillRequest, WinetricksRequest,
};
pub use model::{Command, FileSource, FileSpec, Script, ScriptInfo, Step, Task};
pub use validate::native_launcher;
pub use vars::BUILTINS;
pub use wine::{WineMatch, match_wine_version};

#[cfg(test)]
mod parse_tests;
#[cfg(test)]
mod tests;
