//! Heroic's community "known fixes": per-game environment variables,
//! winetricks verbs and redistributables to run in the prefix.
//!
//! Consumers: `env_variables` are added to every launch of the game;
//! `winetricks` and `run_in_prefix` are applied when the game's prefix was
//! just created or updated; `wiki_link` is written to the game log.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use irori_core::Runner;
use irori_core::archive_guard::relative_entry_path;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::{releases, util};

/// Upstream data.
pub const DATA_URL: &str =
    "https://raw.githubusercontent.com/Heroic-Games-Launcher/known-fixes/main/known_fixes.json";

/// Fixes for one game.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct KnownFix {
    pub title: String,
    /// Explanations keyed by the fix they describe (e.g. a winetricks verb).
    pub notes: BTreeMap<String, String>,
    pub winetricks: Vec<String>,
    /// Files relative to the install folder to run with Wine.
    pub run_in_prefix: Vec<String>,
    #[serde(deserialize_with = "scalar_map")]
    pub env_variables: BTreeMap<String, String>,
    pub wiki_link: Option<String>,
}

impl KnownFix {
    /// Whether anything must be applied to the prefix.
    pub fn has_prefix_fixes(&self) -> bool {
        !self.winetricks.is_empty() || !self.run_in_prefix.is_empty()
    }
}

/// Environment values are strings in most entries but booleans or numbers in
/// some (`"USE_FAKE_EPIC_EXE": true`); store every scalar as its text form.
fn scalar_map<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    let raw: BTreeMap<String, serde_json::Value> = serde::Deserialize::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|(k, v)| {
            let text = match v {
                serde_json::Value::String(s) => s,
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => return None,
            };
            Some((k, text))
        })
        .collect())
}

/// The whole file: store key (`epic`, `gog`, `amazon`, `zoom`) → app → fix.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct KnownFixesFile {
    pub epic: BTreeMap<String, KnownFix>,
    pub gog: BTreeMap<String, KnownFix>,
    pub amazon: BTreeMap<String, KnownFix>,
    pub zoom: BTreeMap<String, KnownFix>,
}

impl KnownFixesFile {
    pub fn parse(json: &[u8]) -> Result<Self> {
        serde_json::from_slice(json).context("known fixes data is not valid")
    }

    /// Fixes for a game. Sideloaded apps and Steam games (which Steam
    /// starts) have none.
    pub fn get(&self, runner: Runner, app_name: &str) -> Option<&KnownFix> {
        let store = match runner {
            Runner::Legendary => &self.epic,
            Runner::Gog => &self.gog,
            Runner::Nile => &self.amazon,
            Runner::Zoom => &self.zoom,
            Runner::Itch
            | Runner::Steam
            | Runner::Sideload
            | Runner::Emulation
            | Runner::Launchers => return None,
        };
        store.get(app_name)
    }
}

fn data_file() -> PathBuf {
    util::cache_dir("known_fixes").join("known_fixes.json")
}

static DATA: Lazy<RwLock<Option<Arc<KnownFixesFile>>>> = Lazy::new(|| RwLock::new(None));

/// Download the data when missing or when `latest_md5` (from
/// [`releases::ReleasesInfo::known_fixes_sha`]) differs. The file is replaced
/// atomically and only after it parsed. Returns whether it changed.
pub async fn refresh(latest_md5: Option<&str>) -> Result<bool> {
    let path = data_file();
    if !releases::needs_download(&path, latest_md5).await {
        return Ok(false);
    }
    let text = util::get_text(DATA_URL, Duration::from_secs(30))
        .await
        .context("downloading known fixes")?;
    let parsed = KnownFixesFile::parse(text.as_bytes())?;
    util::write_atomic(&path, text.as_bytes()).await?;
    *DATA.write() = Some(Arc::new(parsed));
    Ok(true)
}

/// The cached data, loaded from disk once.
pub async fn data() -> Option<Arc<KnownFixesFile>> {
    if let Some(d) = DATA.read().clone() {
        return Some(d);
    }
    let bytes = tokio::fs::read(data_file()).await.ok()?;
    let parsed = match KnownFixesFile::parse(&bytes) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            tracing::warn!("Known fixes could not be loaded, ignoring: {e:#}");
            return None;
        }
    };
    *DATA.write() = Some(parsed.clone());
    Some(parsed)
}

/// Fixes for a game from the cached data.
pub async fn lookup(runner: Runner, app_name: &str) -> Option<KnownFix> {
    Some(data().await?.get(runner, app_name)?.clone().confined())
}

/// Variable families that tune a compatibility layer or driver. The list of
/// fixes is community data applied to launches without asking, so it must
/// not reach `LD_PRELOAD`, `BASH_ENV`, `PATH` or the like.
const FIX_VAR_PREFIXES: &[&str] = &[
    "PROTON_", "DXVK_", "VKD3D_", "MESA_", "RADV_", "__GL_", "SDL_",
];
const FIX_VARS: &[&str] = &[
    "USE_FAKE_EPIC_EXE",
    "WINEDLLOVERRIDES",
    "WINEESYNC",
    "WINEFSYNC",
    "WINE_LARGE_ADDRESS_AWARE",
];
/// In those families, but they run or load something.
const FIX_VARS_REFUSED: &[&str] = &["PROTON_REMOTE_DEBUG_CMD", "SDL_DYNAMIC_API"];

fn fix_var_allowed(key: &str) -> bool {
    !FIX_VARS_REFUSED.contains(&key)
        && (FIX_VARS.contains(&key) || FIX_VAR_PREFIXES.iter().any(|p| key.starts_with(p)))
}

/// A Winetricks component name; Winetricks runs `.verb` files as shell code.
fn fix_verb_allowed(verb: &str) -> bool {
    !verb.is_empty()
        && !verb.starts_with('-')
        && !verb.ends_with(".verb")
        && verb
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '=' | '.' | '-'))
}

impl KnownFix {
    /// Without what a fix has no business doing. Dropped entries are logged.
    fn confined(mut self) -> Self {
        let title = self.title.clone();
        let refused = |what: &str, value: &str| {
            tracing::warn!("known fix for {title}: ignoring {what} {value}");
        };
        self.env_variables.retain(|k, _| {
            let keep = fix_var_allowed(k);
            if !keep {
                refused("the variable", k);
            }
            keep
        });
        self.winetricks.retain(|v| {
            let keep = fix_verb_allowed(v);
            if !keep {
                refused("the component", v);
            }
            keep
        });
        self.run_in_prefix.retain(|f| {
            let keep = relative_entry_path(Path::new(f)).is_some();
            if !keep {
                refused("the file", f);
            }
            keep
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixes_stay_within_what_a_fix_may_do() {
        let fix = KnownFix {
            env_variables: [
                ("USE_FAKE_EPIC_EXE", "1"),
                ("PROTON_NO_ESYNC", "1"),
                ("LD_PRELOAD", "/x.so"),
                ("BASH_ENV", "$(curl x|sh)"),
                ("PATH", "/evil"),
                ("PROTON_REMOTE_DEBUG_CMD", "/evil"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
            winetricks: vec!["vcrun2022".into(), "/tmp/evil.verb".into(), "x.verb".into()],
            run_in_prefix: vec![
                "redist/vc.exe".into(),
                "/bin/evil".into(),
                "../../evil.exe".into(),
            ],
            ..Default::default()
        }
        .confined();
        let keys: Vec<_> = fix.env_variables.keys().map(String::as_str).collect();
        assert_eq!(keys, ["PROTON_NO_ESYNC", "USE_FAKE_EPIC_EXE"]);
        assert_eq!(fix.winetricks, ["vcrun2022"]);
        assert_eq!(fix.run_in_prefix, ["redist/vc.exe"]);
    }

    const FIXTURE: &str = r#"{
      "amazon": {},
      "epic": {
        "0006475a83f143ba807174654bc09d38": {"title": "HOT WHEELS UNLEASHED™",
          "notes": {"vcrun2022": "Games fails to launch without it."}, "winetricks": ["vcrun2022"]},
        "wiki": {"title": "GTA", "wikiLink": "https://example.org/wiki",
          "envVariables": {"PROTON_NO_ESYNC": "1", "USE_FAKE_EPIC_EXE": true}, "runInPrefix": ["redist/vcredist.exe"]}
      },
      "gog": {"1": {"title": "G"}},
      "zoom": {}
    }"#;

    #[test]
    fn maps_and_looks_up() {
        let f = KnownFixesFile::parse(FIXTURE.as_bytes()).unwrap();
        let hw = f
            .get(Runner::Legendary, "0006475a83f143ba807174654bc09d38")
            .unwrap();
        assert_eq!(hw.winetricks, ["vcrun2022"]);
        assert_eq!(hw.notes["vcrun2022"], "Games fails to launch without it.");
        assert!(hw.has_prefix_fixes());
        let gta = f.get(Runner::Legendary, "wiki").unwrap();
        assert_eq!(gta.env_variables["PROTON_NO_ESYNC"], "1");
        assert_eq!(gta.env_variables["USE_FAKE_EPIC_EXE"], "true");
        assert_eq!(gta.run_in_prefix, ["redist/vcredist.exe"]);
        assert_eq!(gta.wiki_link.as_deref(), Some("https://example.org/wiki"));
        assert!(f.get(Runner::Gog, "1").is_some());
        assert!(f.get(Runner::Sideload, "1").is_none());
        assert!(f.get(Runner::Nile, "1").is_none());
        assert!(!f.get(Runner::Gog, "1").unwrap().has_prefix_fixes());
    }

    #[test]
    fn partial_file_is_rejected() {
        assert!(KnownFixesFile::parse(&FIXTURE.as_bytes()[..40]).is_err());
    }
}

#[cfg(test)]
mod real_data {
    /// Parses the user's real Heroic copy when present (skipped otherwise).
    #[test]
    fn parses_heroic_copy() {
        let Some(home) = dirs::home_dir() else { return };
        let p = home.join(".config/heroic/known_fixes.json");
        if let Ok(bytes) = std::fs::read(&p) {
            super::KnownFixesFile::parse(&bytes).unwrap();
        }
    }
}
