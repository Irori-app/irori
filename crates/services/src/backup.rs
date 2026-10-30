//! Backups of what the user set up in Irori, as one JSON file, and exports of
//! the game list.
//!
//! A backup holds up to four parts: the app settings, per-game settings,
//! library data (favourites, hidden games, categories, custom titles and
//! art) and playtime with the session history. Store sign-ins are never
//! included. Restoring replaces settings, but merges library data and
//! playtime into what is already there, so an older backup never erases
//! newer progress.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use irori_core::game::{GameOverrides, PlaytimeRecord};
use irori_core::{AppSettings, GameId, GameSettingsOverrides, Runner, json_store, util};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Backend;
use crate::sessions::{self, Session};

/// Marks a file as an Irori backup.
const FORMAT: &str = "irori-backup";
/// Newest layout this version reads and writes.
const VERSION: u32 = 1;

/// Which parts of a backup to write or restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupParts {
    pub settings: bool,
    pub game_settings: bool,
    pub library: bool,
    pub playtime: bool,
}

impl BackupParts {
    pub const ALL: BackupParts = BackupParts {
        settings: true,
        game_settings: true,
        library: true,
        playtime: true,
    };

    pub fn any(self) -> bool {
        self.settings || self.game_settings || self.library || self.playtime
    }
}

/// Favourites, hidden games, categories and custom titles and art.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryBackup {
    pub favourites: BTreeSet<String>,
    pub hidden: BTreeSet<String>,
    pub categories: BTreeMap<String, BTreeSet<String>>,
    pub overrides: BTreeMap<String, GameOverrides>,
}

/// Playtime totals and the session history.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaytimeBackup {
    pub records: BTreeMap<String, PlaytimeRecord>,
    pub sessions: Vec<Session>,
}

/// The contents of a backup file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Backup {
    pub format: String,
    pub version: u32,
    /// RFC 3339.
    pub created: String,
    pub irori_version: String,
    /// The app settings as written by this version; read leniently, so a
    /// newer or older layout still restores what it can.
    pub settings: Option<Value>,
    /// Per-game setting overrides by `GameId::key()`.
    pub game_settings: BTreeMap<String, Value>,
    pub library: Option<LibraryBackup>,
    pub playtime: Option<PlaytimeBackup>,
}

impl Backup {
    /// The parts this backup holds.
    pub fn parts(&self) -> BackupParts {
        BackupParts {
            settings: self.settings.is_some(),
            game_settings: !self.game_settings.is_empty(),
            library: self.library.is_some(),
            playtime: self.playtime.is_some(),
        }
    }
}

/// What a restore changed, for telling the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RestoreSummary {
    pub settings: bool,
    pub game_settings: usize,
    pub favourites_added: usize,
    pub categories: usize,
    pub playtime_games: usize,
}

/// File format of a game list export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
}

/// One game in an export.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExportedGame {
    pub title: String,
    pub store: String,
    pub app_name: String,
    pub installed: bool,
    pub platform: String,
    pub install_path: String,
    pub size_bytes: u64,
    pub playtime_minutes: u64,
    pub last_played: String,
    pub favourite: bool,
    pub hidden: bool,
    pub categories: Vec<String>,
    pub release_date: String,
    pub score: Option<u8>,
    pub date_added: String,
}

/// The game a `GameId::key()` names.
fn game_id(key: &str) -> Option<GameId> {
    let (runner, app) = key.split_once(':')?;
    Some(GameId::new(Runner::from_id(runner)?, app))
}

impl Backend {
    /// Setting overrides of every game in the library.
    fn all_game_overrides(&self) -> Vec<(GameId, GameSettingsOverrides)> {
        self.library()
            .games
            .iter()
            .map(|g| {
                let id = g.id();
                let o = self.settings.overrides(&id);
                (id, o)
            })
            .collect()
    }

    /// A backup of the chosen parts, as of now.
    pub fn create_backup(&self, parts: BackupParts) -> Result<Backup> {
        let user = self.userdata.snapshot();
        let game_settings = if parts.game_settings {
            self.all_game_overrides()
                .into_iter()
                .filter(|(_, o)| !o.0.is_empty())
                .map(|(id, o)| (id.key(), Value::Object(o.0)))
                .collect()
        } else {
            BTreeMap::new()
        };
        Ok(Backup {
            format: FORMAT.into(),
            version: VERSION,
            created: util::now_rfc3339(),
            irori_version: env!("CARGO_PKG_VERSION").into(),
            settings: parts.settings.then(|| self.backup_settings()).transpose()?,
            game_settings,
            library: parts.library.then(|| LibraryBackup {
                favourites: user.favourites.clone(),
                hidden: user.hidden.clone(),
                categories: user.categories.clone(),
                overrides: user.overrides.clone(),
            }),
            playtime: parts.playtime.then(|| PlaytimeBackup {
                records: user.playtime.clone(),
                sessions: sessions::read_all(),
            }),
        })
    }

    /// The app settings for a backup, which travels: without the API key.
    fn backup_settings(&self) -> Result<Value> {
        let mut settings = serde_json::to_value(&*self.settings.app())?;
        if let Some(map) = settings.as_object_mut() {
            map.remove("steamGridDbApiKey");
        }
        Ok(settings)
    }

    /// Write a backup of the chosen parts to `path`.
    pub fn write_backup(&self, path: &Path, parts: BackupParts) -> Result<()> {
        let backup = self.create_backup(parts)?;
        let text = serde_json::to_string_pretty(&backup)?;
        json_store::write_bytes(path, text.as_bytes())
            .with_context(|| format!("Cannot write {}", path.display()))
    }

    /// Read and check a backup file.
    pub fn read_backup(path: &Path) -> Result<Backup> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read {}", path.display()))?;
        let backup: Backup =
            serde_json::from_str(&text).context("This file is not an Irori backup")?;
        if backup.format != FORMAT {
            bail!("This file is not an Irori backup");
        }
        if backup.version > VERSION {
            bail!("This backup was made by a newer Irori; update Irori to restore it");
        }
        Ok(backup)
    }

    /// Restore the chosen parts of `backup` (see the module notes on what is
    /// replaced and what is merged).
    pub fn restore_backup(&self, backup: &Backup, parts: BackupParts) -> Result<RestoreSummary> {
        let mut summary = RestoreSummary::default();
        if parts.settings
            && let Some(value) = &backup.settings
        {
            let mut settings: AppSettings = serde_json::from_value(value.clone())
                .context("The settings in this backup could not be read")?;
            if value.get("steamGridDbApiKey").is_none() {
                settings.steam_grid_db_api_key = self.settings.app().steam_grid_db_api_key.clone();
            }
            self.settings.replace_app(settings);
            summary.settings = true;
        }
        if parts.game_settings {
            for (key, value) in &backup.game_settings {
                let (Some(id), Value::Object(map)) = (game_id(key), value) else {
                    continue;
                };
                self.settings
                    .replace_overrides(&id, GameSettingsOverrides(map.clone()));
                summary.game_settings += 1;
            }
        }
        if parts.library
            && let Some(lib) = &backup.library
        {
            let before = self.userdata.snapshot().favourites.len();
            self.userdata.update(None, |d| merge_library(d, lib));
            summary.favourites_added = self.userdata.snapshot().favourites.len() - before;
            summary.categories = lib.categories.len();
        }
        if parts.playtime
            && let Some(pt) = &backup.playtime
        {
            self.userdata
                .update(None, |d| merge_playtime(&mut d.playtime, &pt.records));
            summary.playtime_games = pt.records.len();
            let merged = merge_sessions(sessions::read_all(), &pt.sessions);
            sessions::replace_all(&merged)?;
        }
        if summary.favourites_added > 0 || summary.categories > 0 || summary.playtime_games > 0 {
            self.rebuild_library();
        }
        Ok(summary)
    }

    /// Every game in the library, for an export.
    pub fn exported_games(&self) -> Vec<ExportedGame> {
        let library = self.library();
        let user = self.userdata.snapshot();
        library
            .games
            .iter()
            .map(|g| {
                let key = g.id().key();
                let record = user.playtime.get(&key);
                let mut categories: Vec<String> = user
                    .categories
                    .iter()
                    .filter(|(_, games)| games.contains(&key))
                    .map(|(name, _)| name.clone())
                    .collect();
                categories.sort();
                ExportedGame {
                    title: g.title.clone(),
                    store: g.runner().store_name().to_string(),
                    app_name: g.app_name.clone(),
                    installed: g.is_installed,
                    platform: g
                        .install
                        .as_ref()
                        .map(|i| i.platform.label().to_string())
                        .unwrap_or_default(),
                    install_path: g
                        .install
                        .as_ref()
                        .map(|i| i.install_path.clone())
                        .unwrap_or_default(),
                    size_bytes: g.install.as_ref().map_or(0, |i| i.install_size_bytes),
                    playtime_minutes: record.map_or(0, |r| r.total_minutes),
                    last_played: record
                        .and_then(|r| r.last_played.clone())
                        .unwrap_or_default(),
                    favourite: user.favourites.contains(&key),
                    hidden: user.hidden.contains(&key),
                    categories,
                    release_date: g.release_date.clone().unwrap_or_default(),
                    score: g.rating,
                    date_added: g
                        .date_added
                        .clone()
                        .or_else(|| user.first_seen.get(&key).cloned())
                        .unwrap_or_default(),
                }
            })
            .collect()
    }

    /// Write the game list to `path`. Returns how many games it holds.
    pub fn export_games(&self, path: &Path, format: ExportFormat) -> Result<usize> {
        let games = self.exported_games();
        let text = match format {
            ExportFormat::Csv => to_csv(&games),
            ExportFormat::Json => serde_json::to_string_pretty(&games)?,
        };
        json_store::write_bytes(path, text.as_bytes())
            .with_context(|| format!("Cannot write {}", path.display()))?;
        Ok(games.len())
    }
}

/// Add a backup's library data: favourites, hidden games and category
/// members are added to what is there; custom titles and art from the
/// backup replace the current ones of the same game.
fn merge_library(d: &mut crate::UserData, lib: &LibraryBackup) {
    d.favourites.extend(lib.favourites.iter().cloned());
    d.hidden.extend(lib.hidden.iter().cloned());
    for (name, games) in &lib.categories {
        d.categories
            .entry(name.clone())
            .or_default()
            .extend(games.iter().cloned());
    }
    for (key, o) in &lib.overrides {
        if !o.is_empty() {
            d.overrides.insert(key.clone(), o.clone());
        }
    }
}

/// Keep, per game, the larger total and the widest first/last played span.
fn merge_playtime(
    current: &mut BTreeMap<String, PlaytimeRecord>,
    backup: &BTreeMap<String, PlaytimeRecord>,
) {
    for (key, b) in backup {
        let c = current.entry(key.clone()).or_default();
        if (b.total_minutes, b.extra_seconds) > (c.total_minutes, c.extra_seconds) {
            c.total_minutes = b.total_minutes;
            c.extra_seconds = b.extra_seconds;
        }
        c.last_played = c.last_played.clone().max(b.last_played.clone());
        c.first_played = match (c.first_played.clone(), b.first_played.clone()) {
            (Some(x), Some(y)) => Some(x.min(y)),
            (x, y) => x.or(y),
        };
    }
}

/// Both histories without duplicates, oldest first.
fn merge_sessions(current: Vec<Session>, backup: &[Session]) -> Vec<Session> {
    let mut seen: BTreeSet<(String, String)> = current
        .iter()
        .map(|s| (s.game.clone(), s.started.clone()))
        .collect();
    let mut all = current;
    for s in backup {
        if seen.insert((s.game.clone(), s.started.clone())) {
            all.push(s.clone());
        }
    }
    all.sort_by(|a, b| a.started.cmp(&b.started));
    all
}

/// The games as CSV with a header row (RFC 4180 quoting; categories joined
/// by `; `).
pub fn to_csv(games: &[ExportedGame]) -> String {
    fn field(out: &mut String, value: &str) {
        if value.contains([',', '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&value.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(value);
        }
    }
    let mut out = String::from(
        "title,store,app_name,installed,platform,install_path,size_bytes,playtime_minutes,\
         last_played,favourite,hidden,categories,release_date,score,date_added\r\n",
    );
    for g in games {
        let values = [
            g.title.clone(),
            g.store.clone(),
            g.app_name.clone(),
            g.installed.to_string(),
            g.platform.clone(),
            g.install_path.clone(),
            g.size_bytes.to_string(),
            g.playtime_minutes.to_string(),
            g.last_played.clone(),
            g.favourite.to_string(),
            g.hidden.to_string(),
            g.categories.join("; "),
            g.release_date.clone(),
            g.score.map(|s| s.to_string()).unwrap_or_default(),
            g.date_added.clone(),
        ];
        for (ix, v) in values.iter().enumerate() {
            if ix > 0 {
                out.push(',');
            }
            field(&mut out, v);
        }
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(game: &str, started: &str) -> Session {
        Session {
            game: game.into(),
            started: started.into(),
            seconds: 60,
            crashed: false,
            exit_code: None,
        }
    }

    #[test]
    fn library_merges_without_losing_current_data() {
        let mut d = crate::UserData::default();
        d.favourites.insert("gog:1".into());
        d.categories
            .entry("RPG".into())
            .or_default()
            .insert("gog:1".into());
        let lib = LibraryBackup {
            favourites: ["gog:2".to_string()].into(),
            categories: [("RPG".to_string(), ["gog:3".to_string()].into())].into(),
            overrides: [(
                "gog:1".to_string(),
                GameOverrides {
                    title: Some("Mine".into()),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        };
        merge_library(&mut d, &lib);
        assert_eq!(d.favourites.len(), 2);
        assert_eq!(d.categories["RPG"].len(), 2);
        assert_eq!(d.overrides["gog:1"].title.as_deref(), Some("Mine"));
    }

    #[test]
    fn playtime_keeps_the_larger_total_and_the_full_span() {
        let mut current = BTreeMap::from([(
            "gog:1".to_string(),
            PlaytimeRecord {
                total_minutes: 120,
                extra_seconds: 0,
                last_played: Some("2026-10-23T10:00:00Z".into()),
                first_played: Some("2026-05-01T10:00:00Z".into()),
            },
        )]);
        let backup = BTreeMap::from([
            (
                "gog:1".to_string(),
                PlaytimeRecord {
                    total_minutes: 90,
                    extra_seconds: 30,
                    last_played: Some("2026-09-01T10:00:00Z".into()),
                    first_played: Some("2025-01-01T10:00:00Z".into()),
                },
            ),
            (
                "gog:2".to_string(),
                PlaytimeRecord {
                    total_minutes: 5,
                    ..Default::default()
                },
            ),
        ]);
        merge_playtime(&mut current, &backup);
        let one = &current["gog:1"];
        assert_eq!(one.total_minutes, 120);
        assert_eq!(one.last_played.as_deref(), Some("2026-10-23T10:00:00Z"));
        assert_eq!(one.first_played.as_deref(), Some("2025-01-01T10:00:00Z"));
        assert_eq!(current["gog:2"].total_minutes, 5);
    }

    #[test]
    fn sessions_merge_without_duplicates_in_order() {
        let merged = merge_sessions(
            vec![
                session("gog:1", "2026-10-02"),
                session("gog:1", "2026-10-03"),
            ],
            &[
                session("gog:1", "2026-10-02"),
                session("gog:2", "2026-10-01"),
            ],
        );
        let starts: Vec<&str> = merged.iter().map(|s| s.started.as_str()).collect();
        assert_eq!(starts, ["2026-10-01", "2026-10-02", "2026-10-03"]);
    }

    #[test]
    fn csv_quotes_what_needs_it() {
        let game = ExportedGame {
            title: "Baldur's Gate: \"Enhanced\", Edition".into(),
            store: "GOG".into(),
            app_name: "1".into(),
            installed: true,
            platform: "Windows".into(),
            install_path: "/games/bg".into(),
            size_bytes: 10,
            playtime_minutes: 61,
            last_played: String::new(),
            favourite: true,
            hidden: false,
            categories: vec!["RPG".into(), "Classics".into()],
            release_date: "1998-12-21".into(),
            score: Some(91),
            date_added: String::new(),
        };
        let csv = to_csv(&[game]);
        let row = csv.lines().nth(1).unwrap();
        assert!(row.starts_with("\"Baldur's Gate: \"\"Enhanced\"\", Edition\",GOG,1,true,"));
        assert!(row.contains(",RPG; Classics,1998-12-21,91,"));
        assert_eq!(csv.lines().count(), 2);
    }

    #[test]
    fn backups_round_trip_and_reject_other_files() {
        let backup = Backup {
            format: FORMAT.into(),
            version: VERSION,
            library: Some(LibraryBackup::default()),
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.json");
        std::fs::write(&path, serde_json::to_string(&backup).unwrap()).unwrap();
        let read = Backend::read_backup(&path).unwrap();
        assert_eq!(read, backup);
        assert!(read.parts().library && !read.parts().settings);
        std::fs::write(&path, "{\"format\": \"something\"}").unwrap();
        assert!(Backend::read_backup(&path).is_err());
        let newer = Backup {
            version: VERSION + 1,
            ..backup
        };
        std::fs::write(&path, serde_json::to_string(&newer).unwrap()).unwrap();
        assert!(Backend::read_backup(&path).is_err());
    }
}
