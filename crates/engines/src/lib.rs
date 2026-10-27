//! Engines: the emulators and game engines that play games Ember has no
//! store for (RetroArch and its cores, DuckStation, PPSSPP, Dolphin,
//! ScummVM, DOSBox Staging, …). Wine and Proton stay in `ember-runtime`.
//!
//! An engine is data: a manifest (see [`manifest`]) names its sources per
//! OS, its launch template and its options. This crate owns:
//!
//! - the registry of manifests, built in and overridden by the user;
//! - discovery of installs: Ember-managed versions, Flatpaks, programs on
//!   `PATH`, macOS app bundles, Windows locations and a custom path;
//! - managed installs with several versions side by side ([`install`]);
//! - RetroArch cores ([`retroarch`]);
//! - the BIOS index and per-system BIOS status ([`bios`]);
//! - turning a game file into a command ([`launch`]).
//!
//! Nothing here has a UI or blocks a Tokio worker: discovery and hashing run
//! on the blocking pool.

pub mod archive;
pub mod bios;
pub mod discovery;
pub mod host;
pub mod install;
pub mod launch;
pub mod manifest;
pub mod releases;
pub mod retroarch;
pub mod state;
pub mod systems;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use ember_core::Paths;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde_json::Value;

pub use discovery::{Installation, Origin};
pub use host::{Host, Os};
pub use launch::{BiosMissing, EngineMissing, GameFile};
pub use manifest::{EngineKind, EngineManifest, EngineOption, OptionKind};
pub use systems::System;

/// The manifests shipped with Ember, by id.
pub const BUILT_IN: &[(&str, &str)] = &[
    (
        "retroarch",
        include_str!("../../../assets/engines/retroarch.json"),
    ),
    (
        "duckstation",
        include_str!("../../../assets/engines/duckstation.json"),
    ),
    (
        "ppsspp",
        include_str!("../../../assets/engines/ppsspp.json"),
    ),
    (
        "dolphin",
        include_str!("../../../assets/engines/dolphin.json"),
    ),
    (
        "scummvm",
        include_str!("../../../assets/engines/scummvm.json"),
    ),
    (
        "dosbox-staging",
        include_str!("../../../assets/engines/dosbox-staging.json"),
    ),
];

/// An engine choice as stored in settings: `duckstation`, or
/// `retroarch:<core>` for a libretro core.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EngineRef {
    pub engine: String,
    pub core: Option<String>,
}

impl EngineRef {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        Some(match s.split_once(':') {
            Some((engine, core)) if !core.is_empty() => Self {
                engine: engine.to_string(),
                core: Some(core.to_string()),
            },
            _ => Self {
                engine: s.trim_end_matches(':').to_string(),
                core: None,
            },
        })
    }
}

impl std::fmt::Display for EngineRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.core {
            Some(core) => write!(f, "{}:{core}", self.engine),
            None => f.write_str(&self.engine),
        }
    }
}

/// Where Ember keeps engine data.
pub mod dirs {
    use super::*;

    /// Managed installs: `<data>/engines/<id>/<version>/`.
    pub fn engine(id: &str) -> PathBuf {
        Paths::get().data.join("engines").join(id)
    }

    /// The folder Ember gives an engine for its settings and saves.
    pub fn home(id: &str) -> PathBuf {
        Paths::get().data.join("engines").join("home").join(id)
    }

    /// BIOS files linked for engines Ember configures (RetroArch's
    /// `system_directory`).
    pub fn system() -> PathBuf {
        Paths::get().data.join("engines").join("system")
    }

    /// Release lists and partial downloads.
    pub fn cache() -> PathBuf {
        Paths::get().cache.join("engines")
    }

    /// User manifest overrides: `<config>/engines/<id>.json`.
    pub fn overrides() -> PathBuf {
        Paths::get().config.join("engines")
    }
}

static REGISTRY: Lazy<RwLock<Arc<Vec<Arc<EngineManifest>>>>> =
    Lazy::new(|| RwLock::new(Arc::new(load_manifests())));

/// Every engine, built-in ones first, with the user's overrides applied.
pub fn engines() -> Arc<Vec<Arc<EngineManifest>>> {
    REGISTRY.read().clone()
}

pub fn engine(id: &str) -> Option<Arc<EngineManifest>> {
    REGISTRY.read().iter().find(|m| m.id == id).cloned()
}

/// Read the override folder again (after the user edited a manifest).
pub fn reload() {
    *REGISTRY.write() = Arc::new(load_manifests());
}

/// Engines that play `system`, as choices: RetroArch counts once per core.
/// The system's own recommendation order comes first.
pub fn choices_for(system: &str) -> Vec<EngineRef> {
    let mut out: Vec<EngineRef> = systems::get(system)
        .map(|s| {
            s.engines
                .iter()
                .filter_map(|e| EngineRef::parse(e))
                .collect()
        })
        .unwrap_or_default();
    for m in engines().iter() {
        if !m.systems.iter().any(|s| s == system) {
            continue;
        }
        let refs: Vec<EngineRef> = if m.kind == EngineKind::Retroarch {
            m.cores_for(system)
                .into_iter()
                .map(|c| EngineRef {
                    engine: m.id.clone(),
                    core: Some(c.id.clone()),
                })
                .collect()
        } else {
            vec![EngineRef {
                engine: m.id.clone(),
                core: None,
            }]
        };
        for r in refs {
            if !out.contains(&r) {
                out.push(r);
            }
        }
    }
    out.retain(|r| engine(&r.engine).is_some());
    out
}

/// The emulator a game of `system` plays with when it names none:
/// `chosen` (the user's default for the system), else the first
/// recommended choice that `plays` accepts, else the first recommended.
pub fn default_choice(
    system: &str,
    chosen: Option<&str>,
    plays: impl Fn(&EngineRef) -> bool,
) -> Option<EngineRef> {
    if let Some(chosen) = chosen.and_then(EngineRef::parse) {
        return Some(chosen);
    }
    let choices = choices_for(system);
    choices
        .iter()
        .find(|c| plays(c))
        .or_else(|| choices.first())
        .cloned()
}

/// Whether `choice` can play right now: its engine has an installation and,
/// for RetroArch, the core is installed. Blocking (reads the disk).
pub fn plays_now(choice: &EngineRef, state: &state::EngineState) -> bool {
    let Some(m) = engine(&choice.engine) else {
        return false;
    };
    !discovery::find(&m, state).is_empty()
        && choice
            .core
            .as_deref()
            .is_none_or(|c| retroarch::installed_core(c).is_some())
}

fn load_manifests() -> Vec<Arc<EngineManifest>> {
    let mut out = Vec::new();
    let overrides = read_overrides();
    for (id, json) in BUILT_IN {
        let mut value: Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("built-in engine {id} does not parse: {e}");
                continue;
            }
        };
        if let Some(patch) = overrides.iter().find(|(o, _)| o == id).map(|(_, p)| p) {
            manifest::merge_patch(&mut value, patch);
        }
        match serde_json::from_value::<EngineManifest>(value)
            .context("invalid manifest")
            .and_then(|m| m.validate().map(|()| m))
        {
            Ok(m) => out.push(Arc::new(m)),
            Err(e) => {
                tracing::warn!("engine {id}: {e:#}; using the built-in manifest");
                if let Ok(m) = EngineManifest::parse(json) {
                    out.push(Arc::new(m));
                }
            }
        }
    }
    // Overrides for ids Ember does not ship are whole manifests of their own.
    for (id, value) in overrides {
        if BUILT_IN.iter().any(|(b, _)| *b == id) {
            continue;
        }
        match serde_json::from_value::<EngineManifest>(value)
            .context("invalid manifest")
            .and_then(|m| m.validate().map(|()| m))
        {
            Ok(m) if m.id == id => out.push(Arc::new(m)),
            Ok(m) => tracing::warn!("engines/{id}.json declares id {}; ignored", m.id),
            Err(e) => tracing::warn!("engines/{id}.json: {e:#}"),
        }
    }
    out
}

fn read_overrides() -> Vec<(String, Value)> {
    let Ok(entries) = std::fs::read_dir(dirs::overrides()) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Value)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension()? != "json" || path.file_name()? == "state.json" {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_string();
            let text = std::fs::read_to_string(&path).ok()?;
            match serde_json::from_str(&text) {
                Ok(v) => Some((id, v)),
                Err(e) => {
                    tracing::warn!("{}: {e}", path.display());
                    None
                }
            }
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Initialise [`Paths`] once for this crate's tests, each test getting its
/// own subfolder of one temporary root.
#[cfg(test)]
pub(crate) fn test_root(name: &str) -> PathBuf {
    static ROOT: Lazy<tempfile::TempDir> = Lazy::new(|| {
        let dir = tempfile::tempdir().expect("temp dir");
        Paths::init(Paths::under(dir.path()));
        dir
    });
    let root = ROOT.path();
    assert!(
        Paths::get().config.starts_with(root),
        "a test touched Paths before test_root"
    );
    let dir = root.join("tests").join(name);
    std::fs::create_dir_all(&dir).expect("test dir");
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_refs_parse_and_print() {
        let r = EngineRef::parse("retroarch:snes9x").unwrap();
        assert_eq!(r.engine, "retroarch");
        assert_eq!(r.core.as_deref(), Some("snes9x"));
        assert_eq!(r.to_string(), "retroarch:snes9x");
        assert_eq!(EngineRef::parse("duckstation").unwrap().core, None);
        assert!(EngineRef::parse(" ").is_none());
    }

    #[test]
    fn every_system_engine_exists() {
        let _ = test_root("registry");
        for system in systems::all() {
            for e in &system.engines {
                let r = EngineRef::parse(e).unwrap();
                let m = engine(&r.engine).unwrap_or_else(|| panic!("{}: no {e}", system.id));
                assert!(
                    m.systems.contains(&system.id),
                    "{e} does not list {}",
                    system.id
                );
                if let Some(core) = &r.core {
                    let c = m.core(core).unwrap_or_else(|| panic!("no core {core}"));
                    assert!(
                        c.systems.contains(&system.id),
                        "{core} does not play {}",
                        system.id
                    );
                }
            }
        }
    }

    #[test]
    fn choices_put_the_recommended_engine_first() {
        let _ = test_root("choices");
        let psx = choices_for("psx");
        assert_eq!(psx[0].to_string(), "duckstation");
        assert!(psx.iter().any(|r| r.to_string() == "retroarch:swanstation"));
        let gb = choices_for("gb");
        assert_eq!(gb[0].to_string(), "retroarch:gambatte");
        assert_eq!(gb.len(), 2);
    }
}
