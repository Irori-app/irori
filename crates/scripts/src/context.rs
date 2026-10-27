//! What a script install starts from (the [`Context`]) and what it
//! produces: progress reports, the [`GameEntry`] and the [`ScriptRecord`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::model::*;
use crate::vars::Vars;

/// Where a script installs and what it may assume about the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// `$GAMEDIR`: the game folder.
    pub game_dir: PathBuf,
    /// `$CACHE`: this install's download cache.
    pub cache_dir: PathBuf,
    /// The Wine prefix Ember chose; else the game section's, else the game
    /// folder (Lutris's default).
    pub prefix: Option<PathBuf>,
    pub home: PathBuf,
    pub user: String,
    /// The desktop resolution for `$RESOLUTION…`.
    pub resolution: Option<(u32, u32)>,
    /// Folders a rollback must never delete (Ember's own, library roots);
    /// see `ember_core::fs_guard::protected_dirs`.
    pub protected: Vec<PathBuf>,
    /// Where the script came from, recorded with the game.
    pub source_url: Option<String>,
}

impl Context {
    pub fn new(
        game_dir: impl Into<PathBuf>,
        cache_dir: impl Into<PathBuf>,
        home: impl Into<PathBuf>,
    ) -> Self {
        Self {
            game_dir: game_dir.into(),
            cache_dir: cache_dir.into(),
            prefix: None,
            home: home.into(),
            user: std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_default(),
            resolution: None,
            protected: Vec::new(),
            source_url: None,
        }
    }
}

/// Install progress.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Download {
        file_id: String,
        bytes_done: u64,
        bytes_total: Option<u64>,
        bytes_per_sec: u64,
    },
    Step {
        number: usize,
        total: usize,
        command: &'static str,
        description: Option<String>,
    },
    /// The install failed or was cancelled and is being undone.
    RollingBack,
}

/// Receives [`Progress`] reports.
pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// The library entry a script produced, from its `game:` section.
#[derive(Debug, Clone, PartialEq)]
pub struct GameEntry {
    pub name: String,
    pub game_slug: String,
    pub runner: String,
    pub exe: Option<PathBuf>,
    pub main_file: Option<PathBuf>,
    pub iso: Option<PathBuf>,
    pub rom: Option<PathBuf>,
    pub path: Option<PathBuf>,
    pub args: Option<String>,
    pub working_dir: Option<PathBuf>,
    /// Wine scripts only.
    pub prefix: Option<PathBuf>,
    pub arch: Option<String>,
    /// The Wine build the script asked for; map it with
    /// [`crate::match_wine_version`].
    pub wine_version: Option<String>,
    pub env: BTreeMap<String, String>,
    /// The rest of the game section, substituted.
    pub options: Map<String, Value>,
    /// The script's section for its runner.
    pub runner_options: Map<String, Value>,
}

/// Which script installed a game, so it can be reinstalled or updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRecord {
    pub slug: String,
    pub installer_slug: Option<String>,
    pub version: String,
    pub revision: Option<String>,
    pub updated_at: Option<String>,
    pub source_url: Option<String>,
}

/// A finished install.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// `None` for scripts that `extends` another game.
    pub game: Option<GameEntry>,
    pub record: ScriptRecord,
    pub complete_text: Option<String>,
    /// Where each file id ended up.
    pub files: BTreeMap<String, PathBuf>,
}

/// Ember's variables for `script`, before any file or choice exists.
pub(crate) fn base_vars(script: &Script, ctx: &Context) -> Vars {
    let mut v = Vars::default();
    v.set("GAMEDIR", ctx.game_dir.to_string_lossy());
    v.set("CACHE", ctx.cache_dir.to_string_lossy());
    v.set("HOME", ctx.home.to_string_lossy());
    v.set("USER", ctx.user.clone());
    v.set("VERSION", script.info.version.clone());
    match ctx.resolution {
        Some((w, h)) => {
            v.set("RESOLUTION", format!("{w}x{h}"));
            v.set("RESOLUTION_WIDTH", w.to_string());
            v.set("RESOLUTION_HEIGHT", h.to_string());
            v.set("RESOLUTION_WIDTH_HEX", format!("{w:#x}"));
            v.set("RESOLUTION_HEIGHT_HEX", format!("{h:#x}"));
        }
        None => {
            for n in [
                "RESOLUTION",
                "RESOLUTION_WIDTH",
                "RESOLUTION_HEIGHT",
                "RESOLUTION_WIDTH_HEX",
                "RESOLUTION_HEIGHT_HEX",
            ] {
                v.declare(n);
            }
        }
    }
    for n in ["DISC", "INPUT", "WINEBIN"] {
        v.declare(n);
    }
    for s in &script.steps {
        if let Command::InputMenu(InputMenu { id: Some(id), .. }) = &s.command {
            v.declare(&format!("INPUT_{id}"));
        }
    }
    for (k, val) in &script.variables {
        v.set(k, val.clone());
    }
    for f in &script.files {
        v.declare(&f.id);
    }
    v
}
