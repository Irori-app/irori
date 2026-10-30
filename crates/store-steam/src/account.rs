//! The Steam account signed in to Irori, which is optional: installed games
//! need none. Signing in adds every game the account owns and the games its
//! Steam Family shares, with Steam's playtime for each.
//!
//! The account's tokens are kept in `session.json` in Steam's config folder
//! (through `irori_secrets`); the games in `library.json` in its cache,
//! so the library shows offline and before the first refresh.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result};
use irori_core::{Runner, SessionExpired, json_store};
use irori_steam_api::{Client, OwnedGame, SharedApp, SteamError, TokenClaims, Tokens};
use serde::{Deserialize, Serialize};

/// Renew the access token when it has less than this left.
const RENEW_MARGIN: Duration = Duration::from_secs(10 * 60);

/// Apps that are part of Steam rather than games.
const NOT_GAMES: [u32; 3] = [
    221410,  // Steam for Linux
    228980,  // Steamworks Common Redistributables
    1070560, // Steam Linux Runtime
];

/// A signed-in Steam account.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Session {
    pub account_name: String,
    /// SteamID64, as text so JSON readers keep every digit.
    pub steamid: String,
    pub refresh_token: String,
    pub access_token: String,
    /// Steam's remembered Steam Guard approval of this device.
    #[serde(default)]
    pub guard_data: Option<String>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("account_name", &self.account_name)
            .field("steamid", &self.steamid)
            .finish_non_exhaustive()
    }
}

impl From<Tokens> for Session {
    fn from(t: Tokens) -> Self {
        Self {
            account_name: t.account_name,
            steamid: t.steamid.to_string(),
            refresh_token: t.refresh_token,
            access_token: t.access_token,
            guard_data: t.guard_data,
        }
    }
}

impl Session {
    pub fn steamid(&self) -> Result<u64> {
        self.steamid
            .parse()
            .context("The saved Steam account is damaged. Sign in again.")
    }
}

/// What the sign-in dialog hands to [`irori_core::Store::login`].
pub fn login_input(tokens: Tokens) -> String {
    serde_json::to_string(&Session::from(tokens)).unwrap_or_default()
}

/// Parse what [`login_input`] produced.
pub(crate) fn parse_login_input(input: &str) -> Result<Session> {
    let session: Session = serde_json::from_str(input.trim())
        .context("Sign in to Steam with the Steam sign-in in Irori")?;
    anyhow::ensure!(
        !session.refresh_token.is_empty() && session.steamid().is_ok(),
        "Steam didn't complete the sign-in. Try again."
    );
    Ok(session)
}

/// One game of the account's library, as cached.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct LibraryGame {
    pub app_id: String,
    pub name: String,
    pub playtime_minutes: u64,
    /// Unix time.
    pub last_played: u64,
    /// The family member sharing it (SteamID64), for games the account
    /// doesn't own itself.
    pub shared_by: Option<String>,
}

/// Store-page art of one app, from Steam's store (newer apps keep their art
/// under addresses only the store knows).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct RemoteArt {
    pub cover: Option<String>,
    pub hero: Option<String>,
    pub logo: Option<String>,
    pub header: Option<String>,
}

/// `library.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct LibraryCache {
    /// Whose library this is.
    pub steamid: String,
    pub games: Vec<LibraryGame>,
    /// Art by app ID, for apps Steam has no art cached for. Also kept for
    /// installed games without an account.
    pub art: HashMap<String, RemoteArt>,
}

/// Where the account's files live.
#[derive(Debug, Clone)]
pub(crate) struct AccountFiles {
    pub session: PathBuf,
    pub library: PathBuf,
    pub device: PathBuf,
}

impl AccountFiles {
    pub fn new(config: &Path, cache: &Path) -> Self {
        Self {
            session: config.join("session.json"),
            library: cache.join("library.json"),
            device: config.join("device.json"),
        }
    }

    pub fn read_session(&self) -> Option<Session> {
        irori_secrets::load(&self.session)
    }

    pub fn write_session(&self, session: &Session) -> Result<()> {
        irori_secrets::write_json(&self.session, session)
    }

    pub fn remove_session(&self) -> Result<()> {
        match std::fs::remove_file(&self.session) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).context("Couldn't remove the saved Steam sign-in")
            }
            _ => Ok(()),
        }
    }

    pub fn read_library(&self) -> LibraryCache {
        json_store::read_or_default(&self.library)
    }

    pub fn write_library(&self, cache: &LibraryCache) -> Result<()> {
        json_store::write(&self.library, cache)
    }
}

/// The session with an access token valid for a while yet, renewed with
/// the refresh token when needed. A refused refresh means the user has to
/// sign in again.
pub(crate) async fn fresh(client: &Client, session: &Session) -> Result<Option<Session>> {
    let now = irori_core::util::unix_now();
    let valid = TokenClaims::parse(&session.access_token)
        .is_ok_and(|c| !c.is_expiring(now, RENEW_MARGIN.as_secs()));
    if valid {
        return Ok(None);
    }
    match client
        .refresh_access_token(&session.refresh_token, session.steamid()?)
        .await
    {
        Ok(access_token) => Ok(Some(Session {
            access_token,
            ..session.clone()
        })),
        Err(e) => Err(map_rejection(e)),
    }
}

/// Steam refusing the saved sign-in becomes [`SessionExpired`], which the
/// app turns into one "sign in again" prompt.
pub(crate) fn map_rejection(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<SteamError>() {
        Some(SteamError::SessionRejected) => {
            anyhow::Error::new(SessionExpired::new(Runner::Steam, format!("{e:#}")))
        }
        _ => e,
    }
}

/// Fetch the games the account owns and those its family shares.
pub(crate) async fn fetch_library(client: &Client, session: &Session) -> Result<Vec<LibraryGame>> {
    let steamid = session.steamid()?;
    let token = &session.access_token;
    let owned = client
        .owned_games(token, steamid)
        .await
        .map_err(map_rejection)
        .context("Couldn't read your Steam library")?;
    // A family is optional; failing to read it keeps the owned games.
    let shared = match client.family_group(token, steamid).await {
        Ok(Some(group)) => client
            .family_shared_apps(token, group, steamid)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("reading the Steam Family library: {e:#}");
                Vec::new()
            }),
        Ok(None) => Vec::new(),
        Err(e) => {
            tracing::warn!("reading the Steam Family: {e:#}");
            Vec::new()
        }
    };
    Ok(merge(owned, shared, steamid))
}

/// The account's own games, then the family's games it doesn't own, without
/// Steam's own apps.
fn merge(owned: Vec<OwnedGame>, shared: Vec<SharedApp>, steamid: u64) -> Vec<LibraryGame> {
    let me = steamid.to_string();
    let mut games: Vec<LibraryGame> = owned
        .into_iter()
        .filter(|g| !NOT_GAMES.contains(&g.appid) && !g.name.trim().is_empty())
        .map(|g| LibraryGame {
            app_id: g.appid.to_string(),
            name: g.name,
            playtime_minutes: g.playtime_forever,
            last_played: g.rtime_last_played,
            shared_by: None,
        })
        .collect();
    let known: std::collections::HashSet<String> = games.iter().map(|g| g.app_id.clone()).collect();
    games.extend(
        shared
            .into_iter()
            .filter(|a| !NOT_GAMES.contains(&a.appid) && !a.name.trim().is_empty())
            .filter(|a| !known.contains(&a.appid.to_string()))
            .filter(|a| !a.owner_steamids.contains(&me))
            .map(|a| LibraryGame {
                app_id: a.appid.to_string(),
                name: a.name,
                // The family response's time fields are the owner's or
                // timestamps; the account's own playtime comes from Steam's
                // local files.
                playtime_minutes: 0,
                last_played: 0,
                shared_by: a.owner_steamids.into_iter().next(),
            }),
    );
    games.sort_by(|a, b| a.app_id.cmp(&b.app_id));
    games
}

/// This computer as Steam's device list shows it, with an identity kept in
/// `device.json` so Steam sees the same device at every sign-in.
pub(crate) fn device(files: &AccountFiles) -> irori_steam_api::DeviceInfo {
    #[derive(Serialize, Deserialize)]
    struct Device {
        seed: String,
    }
    let seed = match json_store::read::<Device>(&files.device).ok().flatten() {
        Some(d) => d.seed,
        None => {
            let seed = uuid::Uuid::new_v4().to_string();
            if let Err(e) = json_store::write(&files.device, &Device { seed: seed.clone() }) {
                tracing::warn!("saving the Steam device identity: {e:#}");
            }
            seed
        }
    };
    let host = sysinfo::System::host_name().unwrap_or_default();
    let friendly_name = if host.trim().is_empty() {
        "Irori".to_string()
    } else {
        format!("Irori on {}", host.trim())
    };
    irori_steam_api::DeviceInfo {
        friendly_name,
        machine_id: machine_id(&seed),
    }
}

/// The machine ID the Steam client sends: a binary KeyValues object of three
/// hex digests. Derived from `seed`, so it is stable and reveals nothing
/// about the computer.
fn machine_id(seed: &str) -> Vec<u8> {
    use sha1::{Digest as _, Sha1};
    let mut out = Vec::new();
    out.push(0x00);
    out.extend_from_slice(b"MessageObject\0");
    for key in ["BB3", "FF2", "3B3"] {
        let digest = Sha1::digest(format!("{seed}{key}").as_bytes());
        out.push(0x01);
        out.extend_from_slice(key.as_bytes());
        out.push(0x00);
        out.extend_from_slice(hex::encode(digest).as_bytes());
        out.push(0x00);
    }
    out.extend_from_slice(&[0x08, 0x08]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(appid: u32, name: &str, minutes: u64) -> OwnedGame {
        serde_json::from_value(serde_json::json!({
            "appid": appid, "name": name, "playtime_forever": minutes
        }))
        .unwrap()
    }

    fn shared(appid: u32, name: &str, owners: &[&str]) -> SharedApp {
        serde_json::from_value(serde_json::json!({
            "appid": appid, "name": name, "owner_steamids": owners, "rt_playtime": 7200
        }))
        .unwrap()
    }

    #[test]
    fn merges_owned_and_family_games() {
        let games = merge(
            vec![owned(620, "Portal 2", 95), owned(228980, "Redist", 0)],
            vec![
                shared(620, "Portal 2", &["2"]),
                shared(70, "Half-Life", &["2"]),
                shared(400, "Portal", &["1"]),
            ],
            1,
        );
        let ids: Vec<&str> = games.iter().map(|g| g.app_id.as_str()).collect();
        assert_eq!(ids, ["620", "70"]);
        assert_eq!(games[0].playtime_minutes, 95);
        assert_eq!(games[0].shared_by, None);
        assert_eq!(games[1].shared_by.as_deref(), Some("2"));
        assert_eq!(games[1].playtime_minutes, 0);
    }

    #[test]
    fn login_input_round_trips_without_leaking_in_debug() {
        let tokens = Tokens {
            account_name: "player".into(),
            steamid: 76561197960265729,
            refresh_token: "refresh-secret".into(),
            access_token: "access-secret".into(),
            guard_data: None,
        };
        let session = parse_login_input(&login_input(tokens)).unwrap();
        assert_eq!(session.steamid().unwrap(), 76561197960265729);
        assert_eq!(session.refresh_token, "refresh-secret");
        let debug = format!("{session:?}");
        assert!(!debug.contains("secret"), "{debug}");
        assert!(parse_login_input("some pasted code").is_err());
    }

    #[test]
    fn session_file_is_private_and_removable() {
        let dir = tempfile::tempdir().unwrap();
        let files = AccountFiles::new(dir.path(), dir.path());
        assert!(files.read_session().is_none());
        let session = parse_login_input(
            r#"{"accountName":"a","steamid":"1","refreshToken":"r","accessToken":"x"}"#,
        )
        .unwrap();
        files.write_session(&session).unwrap();
        assert_eq!(files.read_session().unwrap().account_name, "a");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&files.session)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        files.remove_session().unwrap();
        files.remove_session().unwrap();
        assert!(files.read_session().is_none());
    }

    #[test]
    fn device_identity_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let files = AccountFiles::new(dir.path(), dir.path());
        let first = device(&files);
        let second = device(&files);
        assert_eq!(first.machine_id, second.machine_id);
        assert!(first.friendly_name.starts_with("Irori"));
        assert!(first.machine_id.starts_with(b"\0MessageObject\0\x01BB3\0"));
        assert!(first.machine_id.ends_with(&[0x08, 0x08]));
    }
}
