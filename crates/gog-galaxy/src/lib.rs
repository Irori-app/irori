//! GOG Galaxy's communication service, provided by comet running inside
//! Ember. Games built with the Galaxy SDK connect to it on
//! `127.0.0.1:9977` for achievements, statistics, leaderboards and the
//! overlay.

#![forbid(unsafe_code)]

use std::collections::HashMap;
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
}

impl Service {
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
    let certificate =
        reqwest::tls::Certificate::from_pem(comet::CERT).context("invalid comet certificate")?;
    let http = reqwest::Client::builder()
        .user_agent(format!(
            "GOGGalaxyCommunicationService/2.0.13.27 (Windows_32bit) installation_source/gog Ember/{}",
            env!("CARGO_PKG_VERSION")
        ))
        .add_root_certificate(certificate)
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
        let mut pusher =
            NotificationPusherClient::new(&access_token, pusher_topics, pusher_stop).await;
        pusher.handle_loop().await;
    });

    let (overlay, _) = tokio::sync::broadcast::channel::<(u32, OverlayPeerMessage)>(32);
    let stop = shutdown.clone();
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
            tokio::spawn(api::handlers::entry_point(
                socket,
                http.clone(),
                tokens.clone(),
                user.clone(),
                topics.subscribe(),
                overlay.clone(),
                stop.clone(),
            ));
        }
        tracing::info!("Galaxy service stopped");
    });
    Ok(Service { shutdown })
}
