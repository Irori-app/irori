//! GOG Galaxy's communication service, provided by comet running inside
//! Irori. Games built with the Galaxy SDK connect to it on
//! `127.0.0.1:9977` for achievements, statistics and leaderboards. Every local
//! program can reach that port, so it only serves games Irori started, and
//! never the account's own token.

#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
mod peer;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use comet::api;
use comet::api::gog::overlay::OverlayPeerMessage;
use comet::api::notification_pusher::{NotificationPusherClient, PusherEvent};
use comet::api::structs::{Token, UserInfo};
use comet::constants;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Port the Galaxy SDK connects to.
pub const PORT: u16 = 9977;

/// The signed-in Galaxy account.
#[derive(Clone)]
pub struct Account {
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: String,
    pub username: String,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// A running service. Dropping it stops the service.
pub struct Service {
    shutdown: CancellationToken,
    allowed_clients: constants::AllowedClients,
}

impl Service {
    /// Let a game Irori is starting sign in, by its Galaxy client id. It stays
    /// allowed until the service stops, with the last game.
    pub async fn allow_client(&self, client_id: String) {
        self.allowed_clients.lock().await.insert(client_id);
    }

    /// Stop accepting games and close every connection.
    pub fn stop(&self) {
        self.shutdown.cancel();
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Start the service on [`PORT`]. Fails when the port is taken (another
/// Galaxy service, or a comet started elsewhere).
pub async fn start(account: Account) -> Result<Service> {
    let listener = TcpListener::bind(("127.0.0.1", PORT))
        .await
        .with_context(|| format!("Port {PORT} is in use; another Galaxy service may be running"))?;
    let http = reqwest::Client::builder()
        .user_agent(format!(
            "GOGGalaxyCommunicationService/2.0.13.27 (Windows_32bit) installation_source/gog Irori/{}",
            env!("CARGO_PKG_VERSION")
        ))
        .build()?;
    let user = Arc::new(UserInfo {
        username: account.username.clone(),
        galaxy_user_id: account.user_id.clone(),
    });
    let tokens: constants::TokenStorage = Arc::new(Mutex::new(HashMap::new()));
    tokens.lock().await.insert(
        constants::GALAXY_CLIENT_ID.to_string(),
        Token::new(account.access_token.clone(), account.refresh_token.clone()),
    );
    let allowed_clients: constants::AllowedClients = Arc::new(Mutex::new(HashSet::new()));
    let shutdown = CancellationToken::new();

    // The Galaxy peer library games load (comet keeps it in its own data folder).
    let peer_http = http.clone();
    let peer_stop = shutdown.clone();
    tokio::spawn(async move {
        for attempt in 0..10u64 {
            tokio::select! {
                _ = peer_stop.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(attempt * 5)) => {}
            }
            let res = api::gog::components::get_component(
                &peer_http,
                comet::paths::REDISTS_STORAGE.clone(),
                api::gog::components::Platform::Windows,
                api::gog::components::Component::Peer,
            )
            .await;
            if res.is_ok() {
                return;
            }
        }
        tracing::warn!("Galaxy: could not download the peer library");
    });

    let (topics, _) = tokio::sync::broadcast::channel::<PusherEvent>(20);
    let pusher_topics = topics.clone();
    let pusher_stop = shutdown.clone();
    let access_token = account.access_token.clone();
    tokio::spawn(async move {
        if let Some(mut pusher) =
            NotificationPusherClient::new(&access_token, pusher_topics, pusher_stop).await
        {
            pusher.handle_loop().await;
        }
    });

    let (overlay, _) = tokio::sync::broadcast::channel::<(u32, OverlayPeerMessage)>(32);
    let stop = shutdown.clone();
    let allowed = allowed_clients.clone();
    tokio::spawn(async move {
        tracing::info!("Galaxy service listening on port {PORT}");
        loop {
            let (socket, _) = tokio::select! {
                _ = stop.cancelled() => break,
                accepted = listener.accept() => match accepted {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::warn!("Galaxy service: {e}");
                        continue;
                    }
                },
            };
            let (http, tokens, allowed, user) =
                (http.clone(), tokens.clone(), allowed.clone(), user.clone());
            let (topics, overlay, stop) = (topics.subscribe(), overlay.clone(), stop.clone());
            tokio::spawn(async move {
                #[cfg(target_os = "linux")]
                if !peer::same_user(&socket).await {
                    tracing::warn!("Galaxy service: refused a connection from another user");
                    return;
                }
                api::handlers::entry_point(
                    socket, http, tokens, allowed, user, topics, overlay, stop,
                )
                .await;
            });
        }
        tracing::info!("Galaxy service stopped");
    });
    Ok(Service {
        shutdown,
        allowed_clients,
    })
}
