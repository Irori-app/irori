//! History of play sessions: one JSON object per line in
//! `state/sessions.jsonl`. Appending keeps a long history cheap to write; the
//! totals in `library.json` stay the source of truth for playtime.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;
use ember_core::Paths;
use serde::{Deserialize, Serialize};

/// One finished session of one game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// `GameId::key()` of the game.
    pub game: String,
    /// RFC 3339 time the game was launched.
    pub started: String,
    /// How long it ran.
    pub seconds: u64,
    /// It exited with an error within seconds of starting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub crashed: bool,
}

fn file() -> PathBuf {
    Paths::get().state_dir().join("sessions.jsonl")
}

/// Add a session to the history.
pub fn append(session: &Session) -> Result<()> {
    append_to(&file(), session)
}

/// Every recorded session, oldest first. Lines that do not parse (a write
/// cut short) are skipped.
pub fn read_all() -> Vec<Session> {
    read_from(&file())
}

/// Replace the whole history (a restore merged another one into it).
pub fn replace_all(list: &[Session]) -> Result<()> {
    let mut text = String::new();
    for s in list {
        text.push_str(&serde_json::to_string(s)?);
        text.push('\n');
    }
    ember_core::json_store::write_bytes(&file(), text.as_bytes())
}

fn append_to(path: &Path, session: &Session) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(session)?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())?;
    Ok(())
}

fn read_from(path: &Path) -> Vec<Session> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_reads_back_skipping_torn_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("sessions.jsonl");
        let first = Session {
            game: "gog:1".into(),
            started: "2026-10-23T10:00:00Z".into(),
            seconds: 3600,
            crashed: false,
        };
        let second = Session {
            game: "legendary:Fowl".into(),
            started: "2026-10-23T12:00:00Z".into(),
            seconds: 3,
            crashed: true,
        };
        append_to(&path, &first).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"game\":\"gog:2\",\"sta")
            .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        append_to(&path, &second).unwrap();
        assert_eq!(read_from(&path), vec![first, second]);
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"crashed\":false")
        );
    }

    #[test]
    fn missing_history_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_from(&dir.path().join("none.jsonl")).is_empty());
    }
}
