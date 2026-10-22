//! Release dates and review scores from GOG's GamesDB for Epic games, whose
//! store data has neither. GamesDB knows Epic games by their app name (GOG
//! Galaxy uses it for its Epic integration) and answers without an account.
//! GOG games already carry both from the store. Answers, including "GamesDB
//! does not know this game", are kept for a month.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result};
use ember_core::{GameInfo, Paths, Runner, json_store, util};
use futures::StreamExt as _;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How long an answer is trusted.
const FRESH_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// GamesDB requests in flight at once.
const CONCURRENCY: usize = 4;

/// What GamesDB says about one game.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Facts {
    /// `YYYY-MM-DD`.
    pub release_date: Option<String>,
    /// Aggregated review score, 0–100.
    pub rating: Option<u8>,
    /// Unix time of the answer.
    pub fetched: u64,
}

pub struct FactsService {
    known: RwLock<HashMap<String, Facts>>,
    busy: AtomicBool,
}

fn file() -> PathBuf {
    Paths::get()
        .store_cache_dir(Runner::Legendary)
        .join("gamesdb-facts.json")
}

/// GamesDB's platform name for a store whose games need facts.
fn platform(runner: Runner) -> Option<&'static str> {
    match runner {
        Runner::Legendary => Some("epic"),
        _ => None,
    }
}

impl FactsService {
    pub fn load() -> Self {
        Self {
            known: RwLock::new(json_store::read_or_default(&file())),
            busy: AtomicBool::new(false),
        }
    }

    /// Fill in a game's missing release date and score from what is known.
    pub fn complete(&self, game: &mut GameInfo) {
        let known = self.known.read();
        let Some(facts) = known.get(&game.id().key()) else {
            return;
        };
        if game.release_date.is_none() {
            game.release_date = facts.release_date.clone();
        }
        if game.rating.is_none() {
            game.rating = facts.rating;
        }
    }

    /// Games of a covered store with no answer, or only an old one.
    fn missing(&self, games: &[Arc<GameInfo>]) -> Vec<(String, String, String)> {
        let known = self.known.read();
        let now = util::unix_now();
        games
            .iter()
            .filter_map(|g| {
                let platform = platform(g.runner())?;
                let key = g.id().key();
                let stale = known
                    .get(&key)
                    .is_none_or(|f| now.saturating_sub(f.fetched) >= FRESH_FOR.as_secs());
                stale.then(|| (key, platform.to_string(), g.app_name.clone()))
            })
            .collect()
    }

    /// Ask GamesDB about the games it has no fresh answer for. Returns
    /// whether anything was learned (the library should be rebuilt). Only one
    /// round runs at a time; a second call while one runs does nothing.
    pub async fn refresh(&self, games: &[Arc<GameInfo>]) -> bool {
        let missing = self.missing(games);
        if missing.is_empty() || self.busy.swap(true, Ordering::AcqRel) {
            return false;
        }
        let answers: Vec<(String, Facts)> = futures::stream::iter(missing)
            .map(|(key, platform, app)| answer(key, platform, app))
            .buffer_unordered(CONCURRENCY)
            .filter_map(futures::future::ready)
            .collect()
            .await;
        let learned = !answers.is_empty();
        if learned {
            let snapshot = {
                let mut known = self.known.write();
                known.extend(answers);
                known.clone()
            };
            if let Err(e) = json_store::write(&file(), &snapshot) {
                tracing::warn!("saving GamesDB facts: {e:#}");
            }
        }
        self.busy.store(false, Ordering::Release);
        learned
    }
}

/// [`fetch`] for one game, keyed; failures are logged and dropped.
async fn answer(key: String, platform: String, app: String) -> Option<(String, Facts)> {
    match fetch(&platform, &app).await {
        Ok(facts) => Some((key, facts)),
        Err(e) => {
            tracing::debug!("GamesDB facts of {key}: {e:#}");
            None
        }
    }
}

/// One GamesDB lookup. A game GamesDB does not know is an answer too (empty
/// facts); network and server errors are not.
async fn fetch(platform: &str, id: &str) -> Result<Facts> {
    let resp = ember_core::http::client()
        .get(format!(
            "https://gamesdb.gog.com/platforms/{platform}/external_releases/{id}"
        ))
        .timeout(Duration::from_secs(20))
        .send()
        .await?;
    let fetched = util::unix_now();
    if resp.status().as_u16() == 404 {
        return Ok(Facts {
            fetched,
            ..Default::default()
        });
    }
    let json: Value = resp
        .error_for_status()?
        .json()
        .await
        .context("unexpected GamesDB data")?;
    Ok(Facts {
        fetched,
        ..parse(&json)
    })
}

/// Release date and score of a GamesDB `external_releases` record.
pub fn parse(v: &Value) -> Facts {
    let game = v.get("game");
    let release_date = game
        .and_then(|g| g.get("first_release_date"))
        .or_else(|| v.get("first_release_date"))
        .and_then(Value::as_str)
        .map(|s| s.chars().take(10).collect());
    Facts {
        release_date,
        rating: rating(game.and_then(|g| g.get("aggregated_rating"))),
        fetched: 0,
    }
}

/// GamesDB's `aggregated_rating` (a float, 0–100) as a whole score.
pub fn rating(v: Option<&Value>) -> Option<u8> {
    let score = v?.as_f64()?;
    (score > 0.0).then(|| score.round().clamp(0.0, 100.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_release_date_and_score() {
        let v = json!({
            "first_release_date": null,
            "game": {"first_release_date": "2022-03-24T00:00:00+0000", "aggregated_rating": 76.7143}
        });
        assert_eq!(
            parse(&v),
            Facts {
                release_date: Some("2022-03-24".into()),
                rating: Some(77),
                fetched: 0
            }
        );
        assert_eq!(
            parse(&json!({"game": {"aggregated_rating": null}})),
            Facts::default()
        );
    }

    #[test]
    fn completes_only_missing_fields_and_only_epic_needs_facts() {
        let facts = FactsService {
            known: RwLock::new(HashMap::new()),
            busy: AtomicBool::new(false),
        };
        let epic = GameInfo {
            runner: Some(Runner::Legendary),
            app_name: "Fowl".into(),
            ..Default::default()
        };
        let gog = GameInfo {
            runner: Some(Runner::Gog),
            app_name: "1".into(),
            ..Default::default()
        };
        let games = [Arc::new(epic.clone()), Arc::new(gog)];
        assert_eq!(facts.missing(&games).len(), 1);
        facts.known.write().insert(
            "legendary:Fowl".into(),
            Facts {
                release_date: Some("2020-01-01".into()),
                rating: Some(80),
                fetched: util::unix_now(),
            },
        );
        assert!(facts.missing(&games).is_empty());
        let mut g = GameInfo {
            release_date: Some("2019-05-05".into()),
            ..epic
        };
        facts.complete(&mut g);
        assert_eq!(g.release_date.as_deref(), Some("2019-05-05"));
        assert_eq!(g.rating, Some(80));
    }
}
