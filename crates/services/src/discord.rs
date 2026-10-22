//! Discord Rich Presence: "Playing <game>" while a game runs. Discord's IPC
//! is blocking, so every call runs on the blocking pool.

use std::sync::Arc;

use discord_rich_presence::activity::{Activity, Timestamps};
use discord_rich_presence::{DiscordIpc as _, DiscordIpcClient};
use parking_lot::Mutex;

#[derive(Default, Clone)]
pub(crate) struct Presence {
    client: Arc<Mutex<Option<DiscordIpcClient>>>,
}

impl Presence {
    /// Show `title` as being played since `started_unix`. Failures (Discord
    /// not running) are logged and otherwise ignored.
    pub(crate) async fn playing(
        &self,
        client_id: String,
        title: String,
        store: &'static str,
        started_unix: i64,
    ) {
        let slot = self.client.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let mut guard = slot.lock();
            if guard.is_none() {
                let mut client = DiscordIpcClient::new(&client_id);
                if let Err(e) = client.connect() {
                    tracing::debug!("Discord unavailable: {e}");
                    return;
                }
                *guard = Some(client);
            }
            if let Some(client) = guard.as_mut() {
                let activity = Activity::new()
                    .details(title.as_str())
                    .state(store)
                    .timestamps(Timestamps::new().start(started_unix));
                if let Err(e) = client.set_activity(activity) {
                    tracing::debug!("Discord presence: {e}");
                    *guard = None;
                }
            }
        })
        .await;
    }

    /// Clear the activity and disconnect.
    pub(crate) async fn clear(&self) {
        let slot = self.client.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Some(mut client) = slot.lock().take() {
                let _ = client.clear_activity();
                let _ = client.close();
            }
        })
        .await;
    }
}
