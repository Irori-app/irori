//! Lutris's YAML config cascade for one game: `system.yml`, then
//! `runners/<runner>.yml`, then `games/<configpath>.yml`, each overriding the
//! one before. Lutris merges `env` key by key and replaces every other
//! mapping whole; this module does the same.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_yaml_ng::{Mapping, Value};

use super::paths::{LutrisDirs, expand_home};

/// The effective `game`, `<runner>` and `system` sections of one game.
#[derive(Debug, Default)]
pub(crate) struct Cascade {
    pub game: Mapping,
    pub runner: Mapping,
    pub system: Mapping,
}

/// Game-section keys that name the main file, in Lutris's order
/// (`installer/__init__.py`); `files[0]` comes after them.
const ENTRY_KEYS: [&str; 6] = ["exe", "main_file", "iso", "rom", "disk-a", "path"];

impl Cascade {
    /// Build the cascade. Missing files are normal (a runner without
    /// settings has no file); unreadable ones add a warning.
    pub fn load(
        dirs: &LutrisDirs,
        runner: &str,
        configpath: Option<&str>,
        warnings: &mut Vec<String>,
    ) -> Cascade {
        let system_file = read_yaml(dirs, "system.yml", warnings);
        let runner_file = (!runner.is_empty())
            .then(|| read_yaml(dirs, &format!("runners/{runner}.yml"), warnings))
            .flatten();
        let game_file = configpath
            .filter(|c| is_plain_name(c))
            .and_then(|c| read_yaml(dirs, &format!("games/{c}.yml"), warnings));

        let mut out = Cascade::default();
        for (file, section) in [
            (&system_file, "system"),
            (&runner_file, "system"),
            (&game_file, "system"),
        ] {
            layer(&mut out.system, file.as_ref().and_then(|f| f.get(section)));
        }
        if !runner.is_empty() {
            for file in [&runner_file, &game_file] {
                layer(&mut out.runner, file.as_ref().and_then(|f| f.get(runner)));
            }
        }
        layer(
            &mut out.game,
            game_file.as_ref().and_then(|f| f.get("game")),
        );
        out
    }

    /// A game-section value as text.
    pub fn game_str(&self, key: &str) -> Option<String> {
        self.game.get(key).and_then(scalar)
    }

    /// A runner-section value as text.
    pub fn runner_str(&self, key: &str) -> Option<String> {
        self.runner.get(key).and_then(scalar)
    }

    /// The raw entry point: the first of [`ENTRY_KEYS`] that is set, else the
    /// first entry of `files`.
    pub fn entry_point(&self) -> Option<String> {
        ENTRY_KEYS
            .iter()
            .find_map(|k| self.game_str(k))
            .or_else(|| {
                self.game
                    .get("files")
                    .and_then(Value::as_sequence)
                    .and_then(|files| files.first())
                    .and_then(scalar)
            })
    }

    /// The effective `system.env`, without empty entries (Lutris drops them).
    pub fn env(&self) -> BTreeMap<String, String> {
        self.system
            .get("env")
            .and_then(Value::as_mapping)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((scalar(k)?, scalar(v)?)))
            .collect()
    }
}

/// `configpath` is a file stem (`<slug>-<unix time>`); anything that could
/// leave the `games` folder is ignored.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// Apply one level's section over the accumulated one.
fn layer(into: &mut Mapping, from: Option<&Value>) {
    let Some(from) = from.and_then(Value::as_mapping) else {
        return;
    };
    for (key, value) in from {
        if key.as_str() == Some("env")
            && let (Some(Value::Mapping(dst)), Some(src)) = (into.get_mut(key), value.as_mapping())
        {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
            continue;
        }
        into.insert(key.clone(), value.clone());
    }
}

/// Read the first copy of `rel` among the YAML folders.
fn read_yaml(dirs: &LutrisDirs, rel: &str, warnings: &mut Vec<String>) -> Option<Value> {
    let path = dirs
        .yaml_dirs()
        .into_iter()
        .map(|d| d.join(rel))
        .find(|p| p.is_file())?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            warnings.push(format!("Could not read {}: {e}", path.display()));
            return None;
        }
    };
    match serde_yaml_ng::from_str::<Value>(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("{} is not valid YAML: {e}", path.display()));
            None
        }
    }
}

/// A scalar as text; empty strings and non-scalars are `None`. YAML turns
/// `appid: 220` into a number and `args: true` into a bool, so both are
/// accepted.
pub(crate) fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}

/// Resolve a Lutris path: `~` expands to the home folder, and a relative
/// path is taken relative to the game's install folder when it has one.
pub(crate) fn resolve_path(raw: &str, directory: Option<&Path>, home: &Path) -> PathBuf {
    let path = expand_home(raw, home);
    match directory {
        Some(dir) if path.is_relative() => dir.join(path),
        _ => path,
    }
}
