//! Steam library: the games the Steam client installed on this computer,
//! read from Steam's own files, plus — once the user signs in to Steam in
//! Irori — every game the account owns and the games its Steam Family
//! shares.
//!
//! Every Steam library folder (`steamapps/libraryfolders.vdf`) is scanned for
//! app manifests (`appmanifest_<id>.acf`); fully installed apps that are not
//! Steam tools (Proton, the Linux runtimes, redistributables) become library
//! entries keyed by their Steam app ID, and the folders are watched so
//! installs and removals made in Steam show at once. Playing hands the game
//! to Steam (`steam://rungameid/<id>`, or `-applaunch` with arguments), so
//! Steam starts it with its own compatibility settings, overlay and cloud
//! saves, and a [`watch`] follows the game until it ends. Installing,
//! uninstalling and verifying are handed to the Steam client
//! ([`Runner::manages_installs`] is false). Art comes from Steam's own cache,
//! else from the store. Playtime is the larger of what Steam recorded on this
//! computer (`localconfig.vdf`) and what the account reports.

mod account;
mod client;
mod entries;
mod scan;
pub mod signin;
mod watch;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use irori_core::{
    ClientAction, Event, EventBus, ExtraInfo, GameInfo, InstallInfo, InstallInfoQuery,
    InstallPlatform, InstallRequest, LaunchPlan, LaunchRequest, LoginFlow, MAIN_ACCOUNT, OpContext,
    Outcome, Paths, RemoveArgs, Runner, SettingsFn, Store, StoreAccount, UpdateRequest, UserInfo,
};
use irori_steam_api::{Client, Endpoints};
use parking_lot::{Mutex, RwLock};

use account::{AccountFiles, LibraryCache, Session};
use client::ClientLink;
use scan::{InstalledApp, LocalArt};

const NOT_MANAGED: &str =
    "Steam installs, updates and removes its own games. Use the Steam client for that.";

/// How long Steam's files must stay quiet before the library is read again:
/// an install or update rewrites a manifest many times.
const SETTLE: Duration = Duration::from_secs(2);

/// A refresh that isn't a full one asks Steam for the account's games at
/// most this often.
const LIBRARY_MAX_AGE: Duration = Duration::from_secs(15 * 60);

/// Heroic has no Steam library, so there is nothing to import.
pub fn import_from_heroic(_heroic_config_dir: &Path) -> Result<()> {
    Ok(())
}

#[derive(Default)]
struct Library {
    /// Steam's folder exists on this computer.
    found: bool,
    games: Vec<Arc<GameInfo>>,
    by_app: HashMap<String, Arc<GameInfo>>,
    /// Manifest of each installed game, by app ID.
    manifests: HashMap<String, PathBuf>,
    /// Games Steam's cache has no cover for.
    missing_art: Vec<String>,
}

/// The Steam library store.
pub struct SteamStore {
    bus: EventBus,
    settings: SettingsFn,
    /// Steam folder used instead of the configured one (tests).
    steam_dir: Option<PathBuf>,
    files: AccountFiles,
    client: Option<Client>,
    library: RwLock<Arc<Library>>,
    session: RwLock<Option<Session>>,
    account_library: RwLock<Arc<LibraryCache>>,
    /// When the account's games were last fetched.
    fetched: Mutex<Option<Instant>>,
    /// Store pages already read, by app ID.
    details: Mutex<HashMap<String, ExtraInfo>>,
    /// One refresh at a time.
    refreshing: tokio::sync::Mutex<()>,
    me: std::sync::Weak<SteamStore>,
    /// Watches Steam's library folders (the ones in the key).
    watcher: Mutex<Option<(Vec<PathBuf>, notify::RecommendedWatcher)>>,
    runtime: Mutex<Option<tokio::runtime::Handle>>,
}

impl SteamStore {
    pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self> {
        let paths = Paths::get();
        Self::build(
            bus,
            settings,
            None,
            AccountFiles::new(
                &paths.store_config_dir(Runner::Steam),
                &paths.store_cache_dir(Runner::Steam),
            ),
            Endpoints::default(),
        )
    }

    /// A store reading the Steam installation at `steam_dir`, keeping its
    /// own files under `data` and talking to Steam at `endpoints` (tests).
    pub fn with_layout(
        bus: EventBus,
        settings: SettingsFn,
        steam_dir: PathBuf,
        data: &Path,
        endpoints: Endpoints,
    ) -> Arc<Self> {
        Self::build(
            bus,
            settings,
            Some(steam_dir),
            AccountFiles::new(&data.join("config"), &data.join("cache")),
            endpoints,
        )
    }

    fn build(
        bus: EventBus,
        settings: SettingsFn,
        steam_dir: Option<PathBuf>,
        files: AccountFiles,
        endpoints: Endpoints,
    ) -> Arc<Self> {
        let client = Client::with_endpoints(endpoints)
            .inspect_err(|e| tracing::warn!("Steam's web client: {e:#}"))
            .ok();
        Arc::new_cyclic(|me| Self {
            bus,
            settings,
            steam_dir,
            files,
            client,
            library: RwLock::new(Arc::default()),
            session: RwLock::new(None),
            account_library: RwLock::new(Arc::default()),
            fetched: Mutex::new(None),
            details: Mutex::new(HashMap::new()),
            refreshing: tokio::sync::Mutex::new(()),
            me: me.clone(),
            watcher: Mutex::new(None),
            runtime: Mutex::new(None),
        })
    }

    /// Steam's folder: the "Steam path" setting, Flatpak Steam, or the
    /// platform's default location.
    fn steam_dir(&self) -> PathBuf {
        self.steam_dir
            .clone()
            .unwrap_or_else(|| irori_runtime::locations::steam_path(&(self.settings)()))
    }

    fn client(&self) -> Result<&Client> {
        self.client
            .as_ref()
            .context("Irori couldn't set up its connection to Steam")
    }

    /// Re-read Steam's files and publish the installed games together with
    /// the account's. Reports whether the library changed.
    async fn scan(&self) -> Result<bool> {
        let dir = self.steam_dir();
        let account = self.account_library.read().clone();
        let (found, apps, art) = tokio::task::spawn_blocking(move || {
            let found = dir.join("steamapps").is_dir();
            let apps = scan::installed_apps(&dir);
            let ids = apps
                .iter()
                .map(|a| a.app_id.as_str())
                .chain(account.games.iter().map(|g| g.app_id.as_str()));
            let art: HashMap<String, LocalArt> = ids
                .map(|id| (id.to_string(), scan::local_art(&dir, id)))
                .collect();
            (found, apps, art)
        })
        .await?;
        Ok(self.publish(found, &apps, &art))
    }

    fn publish(&self, found: bool, apps: &[InstalledApp], art: &HashMap<String, LocalArt>) -> bool {
        let account = self.account_library.read().clone();
        let remote = &account.art;
        let mut games: Vec<Arc<GameInfo>> = apps
            .iter()
            .map(|a| {
                Arc::new(entries::installed(
                    a,
                    art.get(&a.app_id),
                    remote.get(&a.app_id),
                ))
            })
            .collect();
        let installed: HashSet<&str> = apps.iter().map(|a| a.app_id.as_str()).collect();
        games.extend(
            account
                .games
                .iter()
                .filter(|g| !installed.contains(g.app_id.as_str()))
                .map(|g| Arc::new(entries::owned(g, art.get(&g.app_id), remote.get(&g.app_id)))),
        );
        games.sort_by(|a, b| a.app_name.cmp(&b.app_name));
        let missing_art = games
            .iter()
            .map(|g| g.app_name.clone())
            .filter(|id| !remote.contains_key(id) && entries::needs_remote_art(art.get(id)))
            .collect();
        let changed = {
            let old = self.library.read();
            old.found != found
                || old.games.len() != games.len()
                || old
                    .games
                    .iter()
                    .zip(&games)
                    .any(|(a, b)| !entries::same(a, b))
        };
        let by_app = games
            .iter()
            .map(|g| (g.app_name.clone(), g.clone()))
            .collect();
        let manifests = apps
            .iter()
            .map(|a| (a.app_id.clone(), a.manifest.clone()))
            .collect();
        *self.library.write() = Arc::new(Library {
            found,
            games,
            by_app,
            manifests,
            missing_art,
        });
        changed
    }

    /// Fetch the signed-in account's games (and art the cache lacks) when
    /// they are older than [`LIBRARY_MAX_AGE`] or `full` is set. Reports
    /// whether anything changed.
    async fn refresh_account(&self, full: bool) -> Result<bool> {
        let session = self.session.read().clone();
        let stale = self
            .fetched
            .lock()
            .is_none_or(|at| at.elapsed() >= LIBRARY_MAX_AGE);
        let mut cache = (*self.account_library.read().clone()).clone();
        let before = cache.clone();
        if let Some(session) = session.filter(|_| full || stale) {
            let client = self.client()?;
            let session = match account::fresh(client, &session).await? {
                Some(renewed) => {
                    self.files.write_session(&renewed)?;
                    *self.session.write() = Some(renewed.clone());
                    renewed
                }
                None => session,
            };
            cache.games = account::fetch_library(client, &session).await?;
            cache.steamid = session.steamid.clone();
            *self.fetched.lock() = Some(Instant::now());
        }
        // Art the cache has none for, asked for once per app.
        let missing: Vec<u32> = self
            .library
            .read()
            .missing_art
            .iter()
            .filter_map(|id| id.parse().ok())
            .collect();
        if !missing.is_empty()
            && let Some(client) = &self.client
        {
            let language = entries::store_language(&(self.settings)().ui_language);
            match client.store_assets(&missing, language, "US").await {
                Ok(found) => {
                    for id in missing {
                        let art = found.get(&id).map(entries::remote_art).unwrap_or_default();
                        cache.art.insert(id.to_string(), art);
                    }
                }
                Err(e) => tracing::warn!("reading Steam store art: {e:#}"),
            }
        }
        if cache == before {
            return Ok(false);
        }
        let files = self.files.clone();
        let to_write = cache.clone();
        tokio::task::spawn_blocking(move || files.write_library(&to_write)).await??;
        *self.account_library.write() = Arc::new(cache);
        Ok(true)
    }

    /// Watch the `steamapps` folders of every Steam library and read the
    /// library again once they settle, so installs and removals made in Steam
    /// show without a refresh. Re-armed when the libraries change.
    fn watch_libraries(&self) {
        use notify::{RecursiveMode, Watcher as _};
        let dir = self.steam_dir();
        let folders: Vec<PathBuf> = scan::libraries(&dir)
            .into_iter()
            .map(|l| l.join("steamapps"))
            .collect();
        let mut slot = self.watcher.lock();
        if slot.as_ref().is_some_and(|(f, _)| *f == folders) {
            return;
        }
        *slot = None;
        if folders.is_empty() {
            return;
        }
        let Some(runtime) = self.runtime.lock().clone() else {
            return;
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            // Only manifests and the library list matter; game files
            // changing under `common/` are not watched.
            let relevant = event.is_ok_and(|e| {
                !e.kind.is_access()
                    && e.paths
                        .iter()
                        .any(|p| p.extension().is_some_and(|x| x == "acf" || x == "vdf"))
            });
            if relevant {
                let _ = tx.send(());
            }
        });
        let mut watcher = match watcher {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("watching Steam libraries: {e}");
                return;
            }
        };
        for folder in &folders {
            if let Err(e) = watcher.watch(folder, RecursiveMode::NonRecursive) {
                tracing::warn!("watching {}: {e}", folder.display());
            }
        }
        let me = self.me.clone();
        runtime.spawn(async move {
            while rx.recv().await.is_some() {
                loop {
                    match tokio::time::timeout(SETTLE, rx.recv()).await {
                        Ok(Some(())) => continue,
                        Ok(None) => return,
                        Err(_) => break,
                    }
                }
                let Some(store) = me.upgrade() else { return };
                match store.scan().await {
                    Ok(true) => store.bus.emit(Event::LibraryChanged(Runner::Steam)),
                    Ok(false) => {}
                    Err(e) => tracing::warn!("reading the Steam library again: {e:#}"),
                }
            }
        });
        *slot = Some((folders, watcher));
    }

    fn require_game(&self, app_name: &str) -> Result<Arc<GameInfo>> {
        match self.game(app_name) {
            Some(g) => Ok(g),
            None => bail!("{app_name} is not in your Steam library"),
        }
    }

    /// The signed-in account's name as Steam shows it on this computer, else
    /// its account name.
    fn display_name(&self, session: &Session) -> String {
        scan::persona_name(&self.steam_dir(), &session.steamid)
            .unwrap_or_else(|| session.account_name.clone())
    }
}

#[async_trait]
impl Store for SteamStore {
    fn runner(&self) -> Runner {
        Runner::Steam
    }

    /// Reads Steam's files (no network).
    /// Reads Steam's files and the saved account (no network).
    async fn init(&self) -> Result<()> {
        *self.runtime.lock() = tokio::runtime::Handle::try_current().ok();
        let files = self.files.clone();
        let (session, cache) =
            tokio::task::spawn_blocking(move || (files.read_session(), files.read_library()))
                .await?;
        // A library cached for another account is not this one's.
        let cache = match &session {
            Some(s) if s.steamid == cache.steamid => cache,
            _ => LibraryCache {
                art: cache.art,
                ..Default::default()
            },
        };
        *self.session.write() = session;
        *self.account_library.write() = Arc::new(cache);
        self.scan().await?;
        self.watch_libraries();
        Ok(())
    }

    /// The signed-in Steam account, else a local "account" while Steam is
    /// installed, so its games count as available; none otherwise.
    fn user(&self) -> Option<UserInfo> {
        if let Some(session) = self.session.read().as_ref() {
            return Some(UserInfo {
                account_id: session.steamid.clone(),
                display_name: self.display_name(session),
            });
        }
        self.library.read().found.then(|| UserInfo {
            account_id: "local".into(),
            display_name: String::new(),
        })
    }

    /// The signed-in Steam account; installed games need none.
    fn accounts(&self) -> Vec<StoreAccount> {
        let Some(session) = self.session.read().clone() else {
            return Vec::new();
        };
        let library = self.library.read();
        vec![StoreAccount {
            slot: MAIN_ACCOUNT.to_string(),
            account_id: session.steamid.clone(),
            display_name: self.display_name(&session),
            signed_in: true,
            games: self.account_library.read().games.len(),
            installed: library.games.iter().filter(|g| g.is_installed).count(),
            expired: None,
        }]
    }

    /// Irori draws Steam's sign-in itself ([`signin`]).
    fn login_flow(&self) -> LoginFlow {
        LoginFlow::InApp
    }

    /// Completes a sign-in with the tokens [`signin::login_input`] made:
    /// reads the account's games (which proves the tokens work), then keeps
    /// the tokens.
    async fn login(&self, input: &str) -> Result<UserInfo> {
        let session = account::parse_login_input(input)?;
        let client = self.client()?;
        let session = account::fresh(client, &session).await?.unwrap_or(session);
        let games = account::fetch_library(client, &session).await?;
        let files = self.files.clone();
        let mut cache = (*self.account_library.read().clone()).clone();
        cache.steamid = session.steamid.clone();
        cache.games = games;
        let (to_save, cache_to_save) = (session.clone(), cache.clone());
        tokio::task::spawn_blocking(move || -> Result<()> {
            files.write_session(&to_save)?;
            files.write_library(&cache_to_save)
        })
        .await??;
        *self.session.write() = Some(session.clone());
        *self.account_library.write() = Arc::new(cache);
        *self.fetched.lock() = Some(Instant::now());
        self.scan().await?;
        // Store art for the games new to the library.
        let _ = self.refresh_account(false).await;
        let _ = self.scan().await;
        self.bus.emit(Event::AccountChanged(Runner::Steam));
        self.bus.emit(Event::LibraryChanged(Runner::Steam));
        Ok(UserInfo {
            account_id: session.steamid.clone(),
            display_name: self.display_name(&session),
        })
    }

    /// Forgets the account and the games it added; installed games stay.
    async fn logout(&self) -> Result<()> {
        let files = self.files.clone();
        let art = self.account_library.read().art.clone();
        let cache = LibraryCache {
            art,
            ..Default::default()
        };
        let to_write = cache.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            files.remove_session()?;
            files.write_library(&to_write)
        })
        .await??;
        *self.session.write() = None;
        *self.account_library.write() = Arc::new(cache);
        *self.fetched.lock() = None;
        self.scan().await?;
        self.bus.emit(Event::AccountChanged(Runner::Steam));
        self.bus.emit(Event::LibraryChanged(Runner::Steam));
        Ok(())
    }

    /// Reads Steam's files again and, when signed in, the account's games
    /// (every [`LIBRARY_MAX_AGE`], or now when `full`). The installed games
    /// are published even when Steam can't be reached.
    async fn refresh(&self, full: bool) -> Result<()> {
        let _one = self.refreshing.lock().await;
        let found = self.library.read().found;
        let mut changed = self.scan().await?;
        let account = self.refresh_account(full).await;
        if matches!(account, Ok(true)) {
            changed |= self.scan().await?;
        }
        if self.library.read().found != found {
            self.bus.emit(Event::AccountChanged(Runner::Steam));
        }
        if changed {
            self.bus.emit(Event::LibraryChanged(Runner::Steam));
        }
        // A library added or removed in Steam changes what is watched.
        self.watch_libraries();
        account.map(|_| ())
    }

    fn games(&self) -> Vec<Arc<GameInfo>> {
        self.library.read().games.clone()
    }

    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        self.library.read().by_app.get(app_name).cloned()
    }

    /// The store page's description, requirements, genres and
    /// screenshots, in Irori's language when Steam has it.
    async fn extra_info(&self, app_name: &str) -> Result<ExtraInfo> {
        let game = self.require_game(app_name)?;
        if let Some(known) = self.details.lock().get(app_name) {
            return Ok(known.clone());
        }
        let fallback = ExtraInfo {
            store_url: game.store_url.clone(),
            ..Default::default()
        };
        let (Ok(appid), Ok(client)) = (app_name.parse::<u32>(), self.client()) else {
            return Ok(fallback);
        };
        let language = entries::store_language(&(self.settings)().ui_language);
        let info = match client.app_details(appid, language).await {
            Ok(Some(details)) => entries::extra_info(app_name, &details),
            // Apps the store no longer lists (delisted, tools) have no page.
            Ok(None) => fallback,
            Err(e) => return Err(e).context("Couldn't read the game's Steam store page"),
        };
        self.details
            .lock()
            .insert(app_name.to_string(), info.clone());
        Ok(info)
    }

    async fn install_info(
        &self,
        _app_name: &str,
        _platform: InstallPlatform,
        _query: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        bail!(NOT_MANAGED)
    }

    /// Steam keeps its games up to date itself.
    async fn updatable_games(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn install(
        &self,
        _app_name: &str,
        _request: &InstallRequest,
        _ctx: OpContext,
    ) -> Result<Outcome> {
        Ok(Outcome::Error(NOT_MANAGED.into()))
    }

    async fn update(
        &self,
        _app_name: &str,
        _request: &UpdateRequest,
        _ctx: OpContext,
    ) -> Result<Outcome> {
        Ok(Outcome::Error(NOT_MANAGED.into()))
    }

    async fn repair(&self, _app_name: &str, _ctx: OpContext) -> Result<Outcome> {
        Ok(Outcome::Error(NOT_MANAGED.into()))
    }

    async fn uninstall(&self, _app_name: &str, _args: RemoveArgs) -> Result<()> {
        bail!(NOT_MANAGED)
    }

    async fn import(
        &self,
        _app_name: &str,
        _path: &Path,
        _platform: InstallPlatform,
    ) -> Result<()> {
        bail!(NOT_MANAGED)
    }

    async fn set_install_path(&self, _app_name: &str, _path: &Path) -> Result<()> {
        bail!(NOT_MANAGED)
    }

    async fn forget_install(&self, _app_name: &str) -> Result<()> {
        bail!(NOT_MANAGED)
    }

    /// Hands the game to Steam, which applies its own launch options, Proton
    /// version and overlay; the game's arguments in Irori (and any given for
    /// this launch) are added to them. Irori's Wine and wrapper settings don't
    /// apply. The session follows the game's processes, not the opener.
    async fn launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        _ctx: OpContext,
    ) -> Result<LaunchPlan> {
        let game = self.require_game(app_name)?;
        if !game.is_installed {
            bail!("{} isn't installed. Install it in Steam first.", game.title);
        }
        let mut args = irori_core::process::split_args(&request.settings.launcher_args);
        args.extend(request.extra_args.iter().cloned());
        let manifest = self
            .library
            .read()
            .manifests
            .get(app_name)
            .cloned()
            .unwrap_or_default();
        let install_path = game.install.as_ref().map(|i| i.install_path.clone());
        Ok(LaunchPlan {
            command: client::launch_command(&self.steam_dir(), &game.app_name, &args),
            track_wineserver: false,
            watch: Some(Arc::new(watch::SteamGameWatch::new(
                game.app_name.to_string(),
                install_path.unwrap_or_default(),
                manifest,
            ))),
        })
    }

    /// Opens the matching `steam://` link; Steam asks the user itself.
    async fn client_action(&self, app_name: &str, action: ClientAction) -> Result<()> {
        let link = match action {
            ClientAction::Install => ClientLink::Install,
            ClientAction::Uninstall => ClientLink::Uninstall,
            ClientAction::Verify => ClientLink::Verify,
            ClientAction::Show => ClientLink::Details,
        };
        let command = client::open_url(&link.url(app_name));
        irori_core::process::run(&command, Default::default())
            .await
            .context("Steam didn't open")?
            .check()
            .context("Steam didn't open")?;
        Ok(())
    }

    /// The larger of what Steam recorded on this computer and what the
    /// account reports, per game.
    async fn remote_playtime(&self) -> Result<HashMap<String, u64>> {
        if (self.settings)().disable_playtime_sync {
            return Ok(HashMap::new());
        }
        let dir = self.steam_dir();
        let steamid = self.session.read().as_ref().map(|s| s.steamid.clone());
        let mut minutes =
            tokio::task::spawn_blocking(move || scan::playtime_minutes(&dir, steamid.as_deref()))
                .await?;
        for game in &self.account_library.read().games {
            if game.playtime_minutes > 0 {
                let entry = minutes.entry(game.app_id.clone()).or_default();
                *entry = (*entry).max(game.playtime_minutes);
            }
        }
        Ok(minutes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_core::{AppSettings, HostPlatform};

    fn write_manifest(lib: &Path, id: &str, name: &str) {
        std::fs::create_dir_all(lib.join("steamapps/common").join(name)).unwrap();
        std::fs::write(
            lib.join(format!("steamapps/appmanifest_{id}.acf")),
            format!(
                "\"AppState\" {{ \"appid\" \"{id}\" \"name\" \"{name}\" \"StateFlags\" \"4\" \"installdir\" \"{name}\" \"SizeOnDisk\" \"2048\" \"buildid\" \"7\" }}"
            ),
        )
        .unwrap();
    }

    /// Nothing listens there, so store art requests fail at once.
    fn offline() -> Endpoints {
        let dead = "http://127.0.0.1:9".to_string();
        Endpoints {
            api: dead.clone(),
            store: dead.clone(),
            assets: dead,
        }
    }

    fn store(steam: &Path, settings: AppSettings) -> Arc<SteamStore> {
        store_at(steam, settings, offline())
    }

    fn store_at(steam: &Path, settings: AppSettings, endpoints: Endpoints) -> Arc<SteamStore> {
        let settings = Arc::new(settings);
        SteamStore::with_layout(
            EventBus::new(),
            Arc::new(move || settings.clone()),
            steam.to_path_buf(),
            &steam.join("irori"),
            endpoints,
        )
    }

    /// An access token valid for an hour.
    fn token(steamid: u64) -> String {
        use base64::Engine as _;
        let b64 = |v: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        let exp = irori_core::util::unix_now() + 3600;
        format!(
            "{}.{}.sig",
            b64(r#"{"alg":"EdDSA"}"#),
            b64(&format!(
                r#"{{"sub":"{steamid}","exp":{exp},"aud":["web","client"]}}"#
            ))
        )
    }

    #[tokio::test]
    async fn signs_in_lists_owned_and_family_games_and_signs_out() {
        use irori_depot::test_server::{Resp, TestServer};
        let server = TestServer::start(|req| {
            let path = req.path.as_str();
            if path.starts_with("/IPlayerService/GetOwnedGames/") {
                Resp::json(serde_json::json!({"response": {"game_count": 2, "games": [
                    {"appid": 620, "name": "Portal 2", "playtime_forever": 300},
                    {"appid": 70, "name": "Half-Life", "playtime_forever": 42}
                ]}}))
            } else if path.starts_with("/IFamilyGroupsService/GetFamilyGroupForUser/") {
                Resp::json(serde_json::json!({"response": {"family_groupid": "9"}}))
            } else if path.starts_with("/IFamilyGroupsService/GetSharedLibraryApps/") {
                Resp::json(serde_json::json!({"response": {"apps": [
                    {"appid": 400, "name": "Portal", "owner_steamids": ["2"]}
                ]}}))
            } else if path.starts_with("/IStoreBrowseService/GetItems/") {
                Resp::json(serde_json::json!({"response": {"store_items": [
                    {"appid": 70, "assets": {
                        "asset_url_format": "steam/apps/70/abc/${FILENAME}",
                        "library_capsule": "library_capsule.jpg"
                    }}
                ]}}))
            } else {
                Resp::status(404)
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let steam = dir.path().join("Steam");
        write_manifest(&steam, "620", "Portal 2");
        let endpoints = Endpoints {
            api: server.base(),
            store: server.base(),
            assets: server.base(),
        };
        let s = store_at(&steam, AppSettings::default(), endpoints.clone());
        s.init().await.unwrap();
        assert_eq!(s.games().len(), 1);
        assert!(s.accounts().is_empty());
        assert_eq!(s.login_flow(), LoginFlow::InApp);

        let input = format!(
            r#"{{"accountName":"player","steamid":"76561197960265729","refreshToken":"r","accessToken":"{}"}}"#,
            token(76561197960265729)
        );
        let user = s.login(&input).await.unwrap();
        assert_eq!(user.account_id, "76561197960265729");
        assert_eq!(user.display_name, "player");
        let ids: Vec<String> = s.games().iter().map(|g| g.app_name.clone()).collect();
        assert_eq!(ids, ["400", "620", "70"]);
        assert!(s.game("620").unwrap().is_installed);
        let half_life = s.game("70").unwrap();
        assert!(!half_life.is_installed && half_life.installable);
        // Newer apps' art comes from the store's hashed address.
        assert!(
            half_life
                .cover_art()
                .unwrap()
                .ends_with("/steam/apps/70/abc/library_capsule.jpg")
        );
        let accounts = s.accounts();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].games, 3);
        assert_eq!(accounts[0].installed, 1);
        // The account's playtime counts where this computer has none.
        assert_eq!(s.remote_playtime().await.unwrap()["70"], 42);
        assert!(
            s.launch(
                "70",
                &LaunchRequest {
                    settings: Default::default(),
                    environment: Default::default(),
                    option: None,
                    extra_args: Vec::new(),
                    offline: false,
                    skip_version_check: false,
                    log_file: dir.path().join("log"),
                },
                OpContext::detached()
            )
            .await
            .is_err(),
            "games that aren't installed don't start"
        );

        // A new start reads the account from disk without the network.
        let again = store(&steam, AppSettings::default());
        again.init().await.unwrap();
        assert_eq!(again.games().len(), 3);
        assert_eq!(again.user().unwrap().display_name, "player");

        s.logout().await.unwrap();
        assert_eq!(s.games().len(), 1);
        assert!(s.accounts().is_empty());
        assert_eq!(s.user().unwrap().account_id, "local");
        assert!(s.login("a pasted code").await.is_err());
    }

    #[tokio::test]
    async fn lists_and_launches_installed_games() {
        let dir = tempfile::tempdir().unwrap();
        let steam = dir.path().join("Steam");
        let s = store(&steam, AppSettings::default());
        s.init().await.unwrap();
        assert!(s.user().is_none(), "no Steam folder, no local account");
        assert!(s.games().is_empty());

        write_manifest(&steam, "620", "Portal 2");
        let mut events = s.bus.subscribe();
        s.refresh(false).await.unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(Event::AccountChanged(Runner::Steam))
        ));
        assert!(matches!(
            events.try_recv(),
            Ok(Event::LibraryChanged(Runner::Steam))
        ));
        assert!(s.is_logged_in());
        let game = s.game("620").unwrap();
        assert_eq!(game.title, "Portal 2");
        assert!(game.is_installed && !game.installable);
        assert_eq!(
            game.installed_platform(),
            Some(HostPlatform::current().native())
        );
        assert_eq!(
            game.install_path().unwrap(),
            steam.join("steamapps/common/Portal 2")
        );
        assert!(s.is_available("620").await);
        assert!(
            game.cover_art()
                .unwrap()
                .ends_with("/620/library_600x900_2x.jpg")
        );

        // A refresh that finds nothing new stays quiet.
        s.refresh(false).await.unwrap();
        assert!(events.try_recv().is_err());

        let request = LaunchRequest {
            settings: Default::default(),
            environment: Default::default(),
            option: None,
            extra_args: vec!["-novid".into()],
            offline: false,
            skip_version_check: false,
            log_file: dir.path().join("log"),
        };
        let plan = s
            .launch("620", &request, OpContext::detached())
            .await
            .unwrap();
        let args: Vec<String> = plan
            .command
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // Arguments go through `-applaunch`, or Steam's `run` link when its
        // program isn't found.
        assert!(
            args.ends_with(&["-applaunch".into(), "620".into(), "-novid".into()])
                || args.last().unwrap() == "steam://run/620//-novid/",
            "{args:?}"
        );
        assert!(plan.watch.is_some(), "the session follows the game");
        let plain = LaunchRequest {
            extra_args: Vec::new(),
            ..request.clone()
        };
        let plan = s
            .launch("620", &plain, OpContext::detached())
            .await
            .unwrap();
        assert_eq!(
            plan.command.args.last().unwrap().to_string_lossy(),
            "steam://rungameid/620"
        );
        assert!(
            s.launch("1", &request, OpContext::detached())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn leaves_installs_to_steam() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "400", "Portal");
        let s = store(dir.path(), AppSettings::default());
        s.init().await.unwrap();
        assert!(s.uninstall("400", RemoveArgs::default()).await.is_err());
        assert!(s.forget_install("400").await.is_err());
        assert!(
            s.set_install_path("400", Path::new("/elsewhere"))
                .await
                .is_err()
        );
        assert!(matches!(
            s.repair("400", OpContext::detached()).await.unwrap(),
            Outcome::Error(_)
        ));
        assert!(s.updatable_games().await.unwrap().is_empty());
        assert!(s.game("400").unwrap().is_installed);
    }

    #[tokio::test]
    async fn playtime_follows_the_sync_setting() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("userdata/42/config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("localconfig.vdf"),
            "\"UserLocalConfigStore\" { \"Software\" { \"Valve\" { \"Steam\" { \"apps\" { \"400\" { \"Playtime\" \"61\" } } } } } }",
        )
        .unwrap();
        let s = store(dir.path(), AppSettings::default());
        assert_eq!(s.remote_playtime().await.unwrap()["400"], 61);
        let off = store(
            dir.path(),
            AppSettings {
                disable_playtime_sync: true,
                ..Default::default()
            },
        );
        assert!(off.remote_playtime().await.unwrap().is_empty());
    }
}
