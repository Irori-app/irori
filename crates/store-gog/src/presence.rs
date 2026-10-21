//! GOG presence ("online", "playing X") with a five-minute heartbeat.

use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::{GogStore, api};

const HEARTBEAT: Duration = Duration::from_secs(5 * 60);

/// Presence state of the store.
#[derive(Default)]
pub struct Presence {
    current: parking_lot::Mutex<Option<String>>,
    heartbeat_running: AtomicBool,
}

/// Start the heartbeat task once; it ends when the store is dropped.
pub(crate) fn spawn_heartbeat(store: Weak<GogStore>) {
    let Some(s) = store.upgrade() else { return };
    if s.presence.heartbeat_running.swap(true, Ordering::SeqCst) {
        return;
    }
    drop(s);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(HEARTBEAT).await;
            let Some(s) = store.upgrade() else { return };
            if s.user_data().is_none() {
                s.presence.heartbeat_running.store(false, Ordering::SeqCst);
                return;
            }
            s.presence_tick().await;
        }
    });
}

impl GogStore {
    fn presence_enabled(&self) -> bool {
        let s = (self.settings)();
        !s.disable_gog_presence && !s.disable_playtime_sync
    }

    /// Set (or clear) the game shown as being played and post it.
    pub(crate) async fn presence_set_game(&self, game: Option<String>) {
        *self.presence.current.lock() = game;
        self.presence_tick().await;
    }

    /// Post the current presence (no-op when disabled or logged out).
    pub(crate) async fn presence_tick(&self) {
        if !self.presence_enabled() || self.user_data().is_none() {
            return;
        }
        let Ok(creds) = self.auth.credentials().await else {
            return;
        };
        let game = self.presence.current.lock().clone();
        if let Err(e) = api::set_presence(&creds, game.as_deref()).await {
            tracing::debug!("GOG presence: {e:#}");
        }
    }

    /// Remove the presence (logout, presence disabled). `force` ignores the
    /// presence setting.
    pub async fn delete_presence(&self, force: bool) {
        let s = (self.settings)();
        if s.disable_playtime_sync
            || (!force && s.disable_gog_presence)
            || self.user_data().is_none()
        {
            return;
        }
        let Ok(creds) = self.auth.credentials().await else {
            return;
        };
        if let Err(e) = api::delete_presence(&creds).await {
            tracing::debug!("GOG presence: {e:#}");
        }
    }
}
