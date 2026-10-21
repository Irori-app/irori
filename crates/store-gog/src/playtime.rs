//! Playtime upload to GOG with a persistent offline queue.
//!
//! Every finished session is queued on disk first and then uploaded, so a
//! session played offline (or while GOG is unreachable) is never lost and the
//! queue does not expire.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ember_core::Store as _;
use futures::StreamExt as _;

use serde::{Deserialize, Serialize};

use ember_core::util;

use crate::api::{self, PostResult};
use crate::{GogStore, load_json, save_json};

/// One play session waiting for upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedSession {
    #[serde(alias = "appName")]
    pub app_name: String,
    /// Session start, unix seconds.
    pub session_date: u64,
    /// Whole minutes played.
    pub time: u64,
}

/// `{user_id: [sessions]}`.
pub type PlaytimeQueue = HashMap<String, Vec<QueuedSession>>;

/// The session to queue for `played_secs` ending at `now`; `None` under a minute.
pub fn session_for(app_name: &str, played_secs: u64, now: u64) -> Option<QueuedSession> {
    let minutes = played_secs / 60;
    (minutes >= 1).then(|| QueuedSession {
        app_name: app_name.to_string(),
        session_date: now.saturating_sub(played_secs),
        time: minutes,
    })
}

/// GOG has no all-games playtime request; totals are read game by game, at
/// most this often, so large libraries do not query hundreds of games on
/// every refresh.
const TOTALS_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Playtime requests in flight at once.
const TOTALS_CONCURRENCY: usize = 4;

/// Whether totals read at `last` (if ever) are due again at `now`.
fn totals_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= TOTALS_EVERY)
}

impl GogStore {
    /// Whole minutes GOG recorded per owned game (Galaxy's sessions
    /// included). Empty when playtime sync is off, nobody is signed in, or
    /// the totals were read recently; games GOG has no answer for are left
    /// out.
    pub(crate) async fn gog_totals(&self) -> HashMap<String, u64> {
        if (self.settings)().disable_playtime_sync || self.user_data().is_none() {
            return HashMap::new();
        }
        {
            let mut last = self.totals_read.lock();
            let now = Instant::now();
            if !totals_due(*last, now) {
                return HashMap::new();
            }
            *last = Some(now);
        }
        let Ok(creds) = self.auth.credentials().await else {
            // Try again on the next refresh.
            *self.totals_read.lock() = None;
            return HashMap::new();
        };
        let apps: Vec<String> = self
            .games()
            .iter()
            .filter(|g| !g.is_dlc)
            .map(|g| g.app_name.clone())
            .collect();
        let creds = &creds;
        futures::stream::iter(apps)
            .map(|app| async move {
                let minutes = api::playtime_minutes(creds, &app).await;
                (app, minutes)
            })
            .buffer_unordered(TOTALS_CONCURRENCY)
            .filter_map(|(app, minutes)| async move {
                match minutes {
                    Ok(m) if m > 0 => Some((app, m)),
                    Ok(_) => None,
                    Err(e) => {
                        tracing::debug!("GOG playtime of {app}: {e:#}");
                        None
                    }
                }
            })
            .collect()
            .await
    }

    /// Queue a finished session and try to upload the queue.
    pub(crate) async fn record_playtime(
        &self,
        app_name: &str,
        played_secs: u64,
    ) -> anyhow::Result<()> {
        if (self.settings)().disable_playtime_sync {
            return Ok(());
        }
        let Some(user) = self.user_data() else {
            return Ok(());
        };
        let Some(session) = session_for(app_name, played_secs, util::unix_now()) else {
            return Ok(());
        };
        {
            let _g = self.playtime_lock.lock().await;
            let path = self.paths.playtime_queue_file();
            let mut queue: PlaytimeQueue = load_json(path.clone()).await;
            queue.entry(user.id.clone()).or_default().push(session);
            save_json(path, queue).await?;
        }
        self.flush_playtime().await;
        Ok(())
    }

    /// Upload queued sessions of the logged-in user. Sessions GOG refuses
    /// permanently are dropped; anything else stays queued.
    pub(crate) async fn flush_playtime(&self) {
        if (self.settings)().disable_playtime_sync {
            return;
        }
        let Some(user) = self.user_data() else { return };
        let Ok(_g) = self.playtime_lock.try_lock() else {
            return;
        };
        let path = self.paths.playtime_queue_file();
        let mut queue: PlaytimeQueue = load_json(path.clone()).await;
        let Some(sessions) = queue.get(&user.id).filter(|s| !s.is_empty()).cloned() else {
            return;
        };
        let Ok(creds) = self.auth.credentials().await else {
            return;
        };
        let mut remaining = Vec::new();
        let mut stop = false;
        for s in sessions {
            if stop {
                remaining.push(s);
                continue;
            }
            match api::post_playtime(&creds, &s.app_name, s.session_date, s.time).await {
                PostResult::Accepted => {}
                PostResult::Rejected => {
                    tracing::warn!("GOG rejected a playtime session of {}", s.app_name);
                }
                PostResult::Retry => {
                    stop = true;
                    remaining.push(s);
                }
            }
        }
        if remaining.is_empty() {
            queue.remove(&user.id);
        } else {
            queue.insert(user.id.clone(), remaining);
        }
        if let Err(e) = save_json(path, queue).await {
            tracing::warn!("{e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_are_read_at_most_every_six_hours() {
        let now = Instant::now();
        assert!(totals_due(None, now));
        assert!(!totals_due(Some(now), now + Duration::from_secs(60)));
        assert!(totals_due(Some(now), now + TOTALS_EVERY));
    }

    #[test]
    fn sessions_under_a_minute_are_skipped() {
        assert!(session_for("1", 59, 1000).is_none());
        let s = session_for("1", 125, 10_000).unwrap();
        assert_eq!(s.time, 2);
        assert_eq!(s.session_date, 10_000 - 125);
    }

    #[test]
    fn queue_roundtrip() {
        let mut q = PlaytimeQueue::new();
        q.entry("u".into())
            .or_default()
            .push(session_for("1", 600, 5000).unwrap());
        let json = serde_json::to_string(&q).unwrap();
        let back: PlaytimeQueue = serde_json::from_str(&json).unwrap();
        assert_eq!(back["u"][0].time, 10);
    }
}
