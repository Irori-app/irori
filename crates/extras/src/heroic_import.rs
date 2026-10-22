//! Import launcher-level data from a Heroic config directory
//! (`ember_core::paths::heroic_config_dir()`): global settings, per-game
//! settings, favourites, hidden games, recently played, custom categories,
//! playtime and title/art overrides.
//!
//! Nothing is written here: [`import`] returns a [`HeroicImport`] that the
//! caller applies to its services. Store credentials and installed games are
//! imported by each store crate.
//!
//! Heroic keys most library data by app name only, so the caller supplies a
//! lookup from app name to runner (for example its library, falling back to
//! [`heroic_runner_index`]). Entries whose runner cannot be determined are
//! listed in [`HeroicImport::unresolved`].

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use ember_core::game::{GameOverrides, PlaytimeRecord};
use ember_core::{AppSettings, GameId, GameSettings, GameSettingsOverrides, Runner};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// Everything imported from Heroic, ready to apply.
#[derive(Debug, Clone, Default)]
pub struct HeroicImport {
    /// Global settings: Heroic's values on top of Ember's defaults. `None`
    /// when Heroic's `config.json` has no `defaultSettings`.
    pub settings: Option<AppSettings>,
    /// Heroic stored the SteamGridDB key encrypted (unreadable outside
    /// Heroic); the user has to enter it again.
    pub steam_grid_db_key_needs_reentry: bool,
    /// Sparse per-game settings relative to `settings` (or Ember's defaults).
    pub game_settings: BTreeMap<GameId, GameSettingsOverrides>,
    pub favourites: Vec<GameId>,
    pub hidden: Vec<GameId>,
    /// Most recent first.
    pub recent: Vec<GameId>,
    pub categories: BTreeMap<String, Vec<GameId>>,
    pub playtime: BTreeMap<GameId, PlaytimeRecord>,
    /// Title/art overrides. Heroic's `art_square` (vertical) maps to Ember's
    /// `art_cover` and Heroic's `art_cover` (wide) to Ember's `art_square`.
    pub overrides: BTreeMap<GameId, GameOverrides>,
    /// App names whose runner could not be determined (skipped).
    pub unresolved: Vec<String>,
    /// Settings that could not be mapped, and unreadable files.
    pub warnings: Vec<String>,
}

/// Import from `dir` on a blocking thread. `resolve` maps an app name to its
/// runner.
pub async fn import<F>(dir: PathBuf, resolve: F) -> Result<HeroicImport>
where
    F: Fn(&str) -> Option<Runner> + Send + 'static,
{
    tokio::task::spawn_blocking(move || import_blocking(&dir, &resolve))
        .await
        .context("Heroic import task failed")?
}

/// Synchronous import (reads files directly; call off the async runtime).
pub fn import_blocking(
    dir: &Path,
    resolve: &dyn Fn(&str) -> Option<Runner>,
) -> Result<HeroicImport> {
    if !dir.join("config.json").is_file() {
        anyhow::bail!("No Heroic configuration found in {}", dir.display());
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let mut out = HeroicImport::default();
    let mut ids = Resolver {
        resolve,
        unresolved: Vec::new(),
    };

    let config: Option<Value> = read_json(&dir.join("config.json"), &mut out.warnings);
    if let Some(defaults) = config.as_ref().and_then(|c| c.get("defaultSettings")) {
        let mapped = map_global_settings(defaults, &home);
        out.steam_grid_db_key_needs_reentry = mapped.key_needs_reentry;
        out.warnings.extend(mapped.warnings);
        out.settings = Some(mapped.settings);
    }
    let global_game = out
        .settings
        .as_ref()
        .map(|s| s.game.clone())
        .unwrap_or_default();

    if let Ok(rd) = std::fs::read_dir(dir.join("GamesConfig")) {
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();
        for path in files {
            let Some(app) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            let Some(file) = read_json::<Value>(&path, &mut out.warnings) else {
                continue;
            };
            let Some(obj) = file.get(&app).and_then(Value::as_object) else {
                continue;
            };
            let Some(id) = ids.id(&app) else { continue };
            let (overrides, warnings) = map_game_settings(&global_game, obj, &home);
            out.warnings
                .extend(warnings.into_iter().map(|w| format!("{app}: {w}")));
            if !overrides.0.is_empty() {
                out.game_settings.insert(id, overrides);
            }
        }
    }

    let store: Value =
        read_json(&dir.join("store/config.json"), &mut out.warnings).unwrap_or(Value::Null);
    let games = store.get("games");
    let list = |key: &str| -> Vec<String> {
        games
            .and_then(|g| g.get(key))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.get("appName").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    out.favourites = ids.ids(list("favourites"));
    out.hidden = ids.ids(list("hidden"));
    out.recent = ids.ids(list("recent"));
    if let Some(cats) = games
        .and_then(|g| g.get("customCategories"))
        .and_then(Value::as_object)
    {
        for (name, members) in cats {
            let members: Vec<GameId> = members
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(|m| ids.category_member(m))
                .collect();
            out.categories.insert(name.clone(), dedup(members));
        }
    }

    let timestamps: Value =
        read_json(&dir.join("store/timestamp.json"), &mut out.warnings).unwrap_or(Value::Null);
    if let Some(map) = timestamps.as_object() {
        for (app, entry) in map {
            let Some(record) = map_playtime(entry) else {
                continue;
            };
            if let Some(id) = ids.id(app) {
                out.playtime.insert(id, record);
            }
        }
    }

    let overrides: Value =
        read_json(&dir.join("store/game-overrides.json"), &mut out.warnings).unwrap_or(Value::Null);
    if let Some(map) = overrides.get("overrides").and_then(Value::as_object) {
        for (app, o) in map {
            let text = |k: &str| {
                o.get(k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            let mapped = GameOverrides {
                title: text("title"),
                art_cover: text("art_square"),
                art_square: text("art_cover"),
                ..GameOverrides::default()
            };
            if mapped.is_empty() {
                continue;
            }
            if let Some(id) = ids.id(app) {
                out.overrides.insert(id, mapped);
            }
        }
    }

    out.unresolved = ids.unresolved;
    out.unresolved.sort();
    out.unresolved.dedup();
    Ok(out)
}

struct Resolver<'a> {
    resolve: &'a dyn Fn(&str) -> Option<Runner>,
    unresolved: Vec<String>,
}

impl Resolver<'_> {
    fn id(&mut self, app: &str) -> Option<GameId> {
        match (self.resolve)(app) {
            Some(r) => Some(GameId::new(r, app)),
            None => {
                self.unresolved.push(app.to_string());
                None
            }
        }
    }

    fn ids(&mut self, apps: Vec<String>) -> Vec<GameId> {
        dedup(apps.iter().filter_map(|a| self.id(a)).collect())
    }

    /// Category members are `<appName>_<runner>`.
    fn category_member(&mut self, member: &str) -> Option<GameId> {
        if let Some((app, runner)) = member.rsplit_once('_')
            && let Some(runner) = Runner::from_id(runner)
            && !app.is_empty()
        {
            return Some(GameId::new(runner, app));
        }
        self.id(member)
    }
}

fn dedup(ids: Vec<GameId>) -> Vec<GameId> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter().filter(|i| seen.insert(i.clone())).collect()
}

fn read_json<T: DeserializeOwned>(path: &Path, warnings: &mut Vec<String>) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("{} is not valid JSON: {e}", path.display()));
            None
        }
    }
}

/// Replace the first `~` with the home directory, like Heroic.
fn expand_tilde(s: &str, home: &Path) -> String {
    s.replacen('~', &home.to_string_lossy(), 1)
}

/// Result of mapping Heroic's `defaultSettings`.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedSettings {
    pub settings: AppSettings,
    pub key_needs_reentry: bool,
    pub warnings: Vec<String>,
}

/// Layer Heroic's `defaultSettings` over Ember's defaults, key by key. A
/// value that does not fit Ember's schema is skipped with a warning, so one
/// odd key never discards the rest. Heroic's `language` (the UI language)
/// becomes `uiLanguage`.
pub fn map_global_settings(defaults: &Value, home: &Path) -> MappedSettings {
    let mut warnings = Vec::new();
    let mut key_needs_reentry = false;
    let Some(heroic) = defaults.as_object() else {
        return MappedSettings {
            settings: AppSettings::default(),
            key_needs_reentry,
            warnings: vec!["Heroic's defaultSettings is not an object".into()],
        };
    };
    let mut heroic = heroic.clone();
    if let Some(lang) = heroic.remove("language")
        && lang.as_str().is_some_and(|s| !s.is_empty())
    {
        heroic.insert("uiLanguage".into(), lang);
    }
    let legacy_prefix_dir = heroic.remove("defaultWinePrefix");
    if !heroic
        .get("defaultWinePrefixDir")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
        && let Some(v) = legacy_prefix_dir
    {
        heroic.insert("defaultWinePrefixDir".into(), v);
    }
    if let Some(Value::String(p)) = heroic.get_mut("winePrefix") {
        *p = expand_tilde(p, home);
    }
    if let Some(Value::String(k)) = heroic.get_mut("steamGridDbApiKey")
        && k.starts_with("sgdb:v1:")
    {
        k.clear();
        key_needs_reentry = true;
    }

    let base = serde_json::to_value(AppSettings::default()).unwrap_or(Value::Null);
    let merged = layer::<AppSettings>(base, &heroic, &mut warnings);
    let settings = serde_json::from_value(Value::Object(merged)).unwrap_or_default();
    MappedSettings {
        settings,
        key_needs_reentry,
        warnings,
    }
}

/// Insert `overlay` keys into `base` one at a time, keeping only those that
/// still deserialize as `T`.
fn layer<T: DeserializeOwned>(
    base: Value,
    overlay: &Map<String, Value>,
    warnings: &mut Vec<String>,
) -> Map<String, Value> {
    let mut base = match base {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    for (key, value) in overlay {
        let previous = base.insert(key.clone(), value.clone());
        if serde_json::from_value::<T>(Value::Object(base.clone())).is_err() {
            warnings.push(format!("setting {key} has an unsupported value; skipped"));
            match previous {
                Some(p) => base.insert(key.clone(), p),
                None => base.remove(key),
            };
        }
    }
    base
}

/// Per-game overrides from a `GamesConfig/<app>.json` object: the file's
/// keys over `global`, diffed against `global`.
pub fn map_game_settings(
    global: &GameSettings,
    file: &Map<String, Value>,
    home: &Path,
) -> (GameSettingsOverrides, Vec<String>) {
    let mut file = file.clone();
    if let Some(Value::String(p)) = file.get_mut("winePrefix") {
        *p = expand_tilde(p, home);
    }
    let mut warnings = Vec::new();
    let base = serde_json::to_value(global).unwrap_or(Value::Null);
    let merged = layer::<GameSettings>(base, &file, &mut warnings);
    let effective: GameSettings =
        serde_json::from_value(Value::Object(merged)).unwrap_or_else(|_| global.clone());
    (GameSettingsOverrides::diff(global, &effective), warnings)
}

fn map_playtime(entry: &Value) -> Option<PlaytimeRecord> {
    let date = |k: &str| {
        entry
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| ember_core::util::parse_rfc3339(s).is_some())
            .map(str::to_string)
    };
    let minutes = entry
        .get("totalPlayed")
        .and_then(Value::as_f64)
        .filter(|m| m.is_finite() && *m > 0.0)
        .map(|m| m.floor() as u64)
        .unwrap_or(0);
    let record = PlaytimeRecord {
        total_minutes: minutes,
        last_played: date("lastPlayed"),
        first_played: date("firstPlayed"),
        ..Default::default()
    };
    (record.total_minutes > 0 || record.last_played.is_some() || record.first_played.is_some())
        .then_some(record)
}

/// App name → runner from Heroic's own library caches and install records
/// (Legendary metadata and installed list, GOG/Amazon/ZOOM library caches,
/// GOG/Amazon/ZOOM installed lists, sideloaded apps). Best effort: unreadable
/// files are skipped.
pub fn heroic_runner_index(dir: &Path) -> HashMap<String, Runner> {
    let mut index = HashMap::new();
    let sources: [(&str, Runner); 10] = [
        (
            "legendaryConfig/legendary/installed.json",
            Runner::Legendary,
        ),
        ("store_cache/legendary_library.json", Runner::Legendary),
        ("store_cache/gog_library.json", Runner::Gog),
        ("gog_store/installed.json", Runner::Gog),
        ("store_cache/nile_library.json", Runner::Nile),
        ("nile_store/installed.json", Runner::Nile),
        ("store_cache/zoom_library.json", Runner::Zoom),
        ("zoom_store/installed.json", Runner::Zoom),
        ("zoom_store/library.json", Runner::Zoom),
        ("sideload_apps/library.json", Runner::Sideload),
    ];
    for (rel, runner) in sources {
        let Ok(bytes) = std::fs::read(dir.join(rel)) else {
            continue;
        };
        let Ok(json) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        collect_app_names(&json, runner, &mut index, 0);
        if rel.starts_with("legendaryConfig")
            && let Some(map) = json.as_object()
        {
            for key in map.keys() {
                index.entry(key.clone()).or_insert(Runner::Legendary);
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir.join("legendaryConfig/legendary/metadata")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json")
                && let Some(stem) = p.file_stem()
            {
                index
                    .entry(stem.to_string_lossy().into_owned())
                    .or_insert(Runner::Legendary);
            }
        }
    }
    index
}

fn collect_app_names(v: &Value, default: Runner, out: &mut HashMap<String, Runner>, depth: u8) {
    if depth > 6 {
        return;
    }
    match v {
        Value::Object(map) => {
            let name = map
                .get("app_name")
                .or_else(|| map.get("appName"))
                .and_then(Value::as_str);
            if let Some(name) = name.filter(|s| !s.is_empty()) {
                let runner = map
                    .get("runner")
                    .and_then(Value::as_str)
                    .and_then(Runner::from_id)
                    .unwrap_or(default);
                out.entry(name.to_string()).or_insert(runner);
            }
            for child in map.values() {
                collect_app_names(child, default, out, depth + 1);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_app_names(child, default, out, depth + 1);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(dir: &Path, rel: &str, v: &Value) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, serde_json::to_vec_pretty(v).unwrap()).unwrap();
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(
            d,
            "config.json",
            &json!({"defaultSettings": {
                "addDesktopShortcuts": true, "autoInstallDxvk": false, "checkUpdatesInterval": 10,
                "customWinePaths": [], "defaultInstallPath": "/games/heroic",
                "defaultWinePrefix": "/games/prefixes", "language": "de",
                "libraryTopSection": "favourites", "maxWorkers": "lots",
                "enviromentOptions": [{"key": "A", "value": "1"}],
                "winePrefix": "~/Games/Heroic/Prefixes/default",
                "wineVersion": {"bin": "/usr/bin/wine", "name": "Wine Default", "type": "wine"},
                "steamGridDbApiKey": "sgdb:v1:Y2lwaGVy", "analyticsOptIn": false
            }, "version": "v0"}),
        );
        write(
            d,
            "GamesConfig/1423049311.json",
            &json!({"1423049311": {
                "autoInstallDxvk": false, "enableEsync": true, "showFps": true, "language": "",
                "steamRuntime": false, "winePrefix": "~/Games/Heroic/Prefixes/Cyberpunk",
                "wineVersion": {"bin": "/usr/bin/wine", "name": "Wine Default", "type": "wine"},
                "launcherArgs": "--skip-intro", "enableFsync": "yes please"
            }, "version": "v0", "explicit": true}),
        );
        write(
            d,
            "GamesConfig/Fortnite.json",
            &json!({"Fortnite": {"autoInstallDxvk": false}, "version": "v0", "explicit": true}),
        );
        write(
            d,
            "GamesConfig/unknownapp.json",
            &json!({"unknownapp": {"showFps": true}}),
        );
        write(
            d,
            "store/config.json",
            &json!({"userHome": "/home/u", "games": {
                "recent": [{"appName": "1423049311", "title": "Cyberpunk"}, {"appName": "Fortnite", "title": "F"}],
                "favourites": [{"appName": "Fortnite", "title": "F"}, {"appName": "Fortnite", "title": "F"}],
                "hidden": [{"appName": "ghost", "title": "G"}],
                "customCategories": {"RPG": ["1423049311_gog", "my_app_sideload", "Fortnite"]}
            }}),
        );
        write(
            d,
            "store/timestamp.json",
            &json!({"1423049311": {"firstPlayed": "", "lastPlayed": "2025-03-01T20:15:00.000Z", "totalPlayed": 6895},
                    "Fortnite": {"firstPlayed": "2024-01-01T00:00:00.000Z", "totalPlayed": 0}}),
        );
        write(
            d,
            "store/game-overrides.json",
            &json!({"overrides": {"Fortnite": {"title": "FN", "art_square": "https://tall", "art_cover": ""}}}),
        );
        dir
    }

    fn resolve(app: &str) -> Option<Runner> {
        match app {
            "1423049311" => Some(Runner::Gog),
            "Fortnite" => Some(Runner::Legendary),
            _ => None,
        }
    }

    #[test]
    fn imports_everything() {
        let dir = fixture();
        let imp = import_blocking(dir.path(), &resolve).unwrap();
        let s = imp.settings.as_ref().unwrap();
        assert!(s.add_desktop_shortcuts);
        assert!(!s.game.auto_install_dxvk);
        assert_eq!(s.ui_language, "de");
        assert_eq!(s.game.language, "");
        assert_eq!(s.default_install_path, "/games/heroic");
        assert_eq!(s.default_wine_prefix_dir, "/games/prefixes");
        assert_eq!(
            s.library_top_section,
            ember_core::LibraryTopSection::Favourites
        );
        assert_eq!(s.max_workers, 0);
        assert!(imp.warnings.iter().any(|w| w.contains("maxWorkers")));
        assert_eq!(s.game.environment_options[0].key, "A");
        assert!(!s.game.wine_prefix.starts_with('~'));
        assert!(
            s.game
                .wine_prefix
                .ends_with("Games/Heroic/Prefixes/default")
        );
        assert_eq!(s.steam_grid_db_api_key, "");
        assert!(imp.steam_grid_db_key_needs_reentry);
        assert!(s.check_for_updates_on_startup);

        let gog = GameId::new(Runner::Gog, "1423049311");
        let fortnite = GameId::new(Runner::Legendary, "Fortnite");
        let o = &imp.game_settings[&gog];
        let keys: Vec<&str> = o.0.keys().map(String::as_str).collect();
        assert_eq!(keys, ["launcherArgs", "showFps", "winePrefix"]);
        assert!(imp.warnings.iter().any(|w| w.contains("enableFsync")));
        assert_eq!(o.apply(&s.game).launcher_args, "--skip-intro");
        assert!(!imp.game_settings.contains_key(&fortnite));

        assert_eq!(imp.recent, [gog.clone(), fortnite.clone()]);
        assert_eq!(imp.favourites, std::slice::from_ref(&fortnite));
        assert!(imp.hidden.is_empty());
        assert_eq!(
            imp.categories["RPG"],
            [
                gog.clone(),
                GameId::new(Runner::Sideload, "my_app"),
                fortnite.clone()
            ]
        );
        assert_eq!(imp.unresolved, ["ghost", "unknownapp"]);

        let p = &imp.playtime[&gog];
        assert_eq!(p.total_minutes, 6895);
        assert_eq!(p.last_played.as_deref(), Some("2025-03-01T20:15:00.000Z"));
        assert_eq!(p.first_played, None);
        assert_eq!(
            imp.playtime[&fortnite].first_played.as_deref(),
            Some("2024-01-01T00:00:00.000Z")
        );

        let ov = &imp.overrides[&fortnite];
        assert_eq!(ov.title.as_deref(), Some("FN"));
        assert_eq!(ov.art_cover.as_deref(), Some("https://tall"));
        assert_eq!(ov.art_square, None);
    }

    #[test]
    fn missing_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(import_blocking(dir.path(), &resolve).is_err());
    }

    #[tokio::test]
    async fn async_wrapper() {
        let dir = fixture();
        let imp = import(dir.path().to_path_buf(), resolve).await.unwrap();
        assert!(imp.settings.is_some());
    }

    #[test]
    fn runner_index() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(
            d,
            "store_cache/gog_library.json",
            &json!({"games": [{"app_name": "1", "runner": "gog"}], "__timestamp": {"games": "x"}}),
        );
        write(
            d,
            "legendaryConfig/legendary/installed.json",
            &json!({"Fortnite": {"app_name": "Fortnite"}}),
        );
        write(
            d,
            "legendaryConfig/legendary/metadata/Sugar.json",
            &json!({"app_name": "Sugar"}),
        );
        write(
            d,
            "sideload_apps/library.json",
            &json!({"games": [{"app_name": "abc", "runner": "sideload"}]}),
        );
        write(
            d,
            "nile_store/installed.json",
            &json!({"installed": [{"appName": "amzn1.x"}]}),
        );
        let idx = heroic_runner_index(d);
        assert_eq!(idx["1"], Runner::Gog);
        assert_eq!(idx["Fortnite"], Runner::Legendary);
        assert_eq!(idx["Sugar"], Runner::Legendary);
        assert_eq!(idx["abc"], Runner::Sideload);
        assert_eq!(idx["amzn1.x"], Runner::Nile);
    }
}
