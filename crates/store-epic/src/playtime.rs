//! Playtime sync with Epic, both ways: finished sessions are queued on disk
//! and uploaded (so the Epic Games Launcher and Epic's records include time
//! played in Ember, and a session played offline is never lost), and Epic's
//! totals are read back so time played elsewhere shows in Ember.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use ember_core::{json_store, util};
use ember_epic_api::PlaytimeUpload;
use serde::{Deserialize, Serialize};

use crate::EpicStore;

/// One finished session waiting for upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedSession {
    pub account_id: String,
    pub app_name: String,
    /// RFC 3339.
    pub start: String,
    pub end: String,
}

/// The session to queue for `played_secs` ending at `now` (unix seconds);
/// `None` under a minute, which is not worth recording.
pub fn session_for(
    account_id: &str,
    app_name: &str,
    played_secs: u64,
    now: i64,
) -> Option<QueuedSession> {
    if played_secs < 60 {
        return None;
    }
    Some(QueuedSession {
        account_id: account_id.to_string(),
        app_name: app_name.to_string(),
        start: util::rfc3339_from_unix(now - played_secs as i64)?,
        end: util::rfc3339_from_unix(now)?,
    })
}

/// This computer's id in the form Epic's launcher sends (a GUID), from the
/// systemd machine id; a fixed id when there is none.
fn machine_id() -> String {
    let raw = ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()));
    match raw {
        Some(h) => format!(
            "{}-{}-{}-{}-{}",
            &h[..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..]
        ),
        None => "00000000-0000-4000-8000-0000656d6265".into(),
    }
}

fn read_queue(path: &Path) -> Vec<QueuedSession> {
    json_store::read_or_default(path)
}

impl EpicStore {
    fn sync_enabled(&self) -> bool {
        !(self.settings)().disable_playtime_sync
    }

    /// Queue a finished session and try to upload everything queued.
    pub(crate) async fn record_playtime(&self, app_name: &str, played_secs: u64) -> Result<()> {
        if !self.sync_enabled() {
            return Ok(());
        }
        let Some(account) = self.user.read().as_ref().map(|u| u.account_id.clone()) else {
            return Ok(());
        };
        let Some(session) = session_for(&account, app_name, played_secs, util::unix_now() as i64)
        else {
            return Ok(());
        };
        {
            let _g = self.playtime_lock.lock().await;
            let path = self.layout.playtime_queue();
            let mut queue = read_queue(&path);
            queue.push(session);
            json_store::write(&path, &queue)?;
        }
        self.flush_playtime().await;
        Ok(())
    }

    /// Upload the signed-in account's queued sessions in order. Sessions
    /// Epic refuses for good are dropped; on any other failure the rest stay
    /// queued for the next try.
    pub(crate) async fn flush_playtime(&self) {
        if !self.sync_enabled() {
            return;
        }
        let Ok(_g) = self.playtime_lock.try_lock() else {
            return;
        };
        let path = self.layout.playtime_queue();
        let queue = read_queue(&path);
        if queue.is_empty() {
            return;
        }
        let (Ok(session), Ok(client)) = (self.session().await, self.client().await) else {
            return;
        };
        let machine = machine_id();
        let mut keep = Vec::new();
        let mut stop = false;
        for s in queue {
            if stop || s.account_id != session.account_id {
                keep.push(s);
                continue;
            }
            match client
                .add_playtime(&session, &machine, &s.app_name, &s.start, &s.end)
                .await
            {
                Ok(PlaytimeUpload::Accepted) => {}
                Ok(PlaytimeUpload::Rejected) => {
                    tracing::warn!("Epic refused a playtime session of {}", s.app_name);
                }
                Ok(PlaytimeUpload::Retry) | Err(_) => {
                    stop = true;
                    keep.push(s);
                }
            }
        }
        if let Err(e) = json_store::write(&path, &keep) {
            tracing::warn!("saving the Epic playtime queue: {e:#}");
        }
    }

    /// Whole minutes Epic recorded per app name.
    pub(crate) async fn epic_playtime(&self) -> Result<HashMap<String, u64>> {
        if !self.sync_enabled() || self.user.read().is_none() {
            return Ok(HashMap::new());
        }
        let session = self.session().await?;
        let client = self.client().await?;
        Ok(client
            .playtime(&session)
            .await?
            .into_iter()
            .filter(|p| !p.artifact_id.is_empty())
            .map(|p| (p.artifact_id, p.total_time / 60))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_under_a_minute_are_skipped() {
        assert!(session_for("a", "Fowl", 59, 1_000_000).is_none());
        let s = session_for("a", "Fowl", 3600, 1_600_003_600).unwrap();
        assert_eq!(s.start, "2020-09-13T12:26:40Z");
        assert_eq!(s.end, "2020-09-13T13:26:40Z");
    }

    #[test]
    fn machine_ids_look_like_guids() {
        let id = machine_id();
        assert_eq!(id.len(), 36);
        assert_eq!(id.matches('-').count(), 4);
    }
}
