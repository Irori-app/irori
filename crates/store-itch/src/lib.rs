//! itch.io store backend (opt-in through `emberItchIo`).
//!
//! * Login: an API key the user creates on itch.io, checked against the
//!   account it belongs to and kept owner-only in `auth.json`.
//! * Library: the games the account owns (its download keys), cached in
//!   `library.json` so the library shows offline; games, tools and installed
//!   entries whose key went away.
//! * Install, update and repair: the best upload for the platform
//!   ([`ember_itch_api::best_upload`]) downloads through [`ember_depot`]
//!   (resumable, speed-limited, with a fresh link from the API for each
//!   attempt) and is unpacked into the install folder; an update unpacks
//!   beside it and swaps folders. Updates are offered when the chosen
//!   upload's build or date changes.
//! * Launch: the detected program, natively or through Wine.

mod detect;
mod extract;
mod install;
mod launch;
mod records;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use ember_core::{
    Event, EventBus, ExtraInfo, GameInfo, InstallInfo, InstallInfoQuery, InstallPlatform,
    InstallRequest, LaunchPlan, LaunchRequest, LoginFlow, OpContext, Outcome, Paths, RemoveArgs,
    Runner, SessionExpired, SettingsFn, Store, UpdateRequest, UserInfo,
};
use ember_itch_api::{ApiError, Client, Os, OwnedKey};
use parking_lot::RwLock;

pub use records::Layout;
use records::{Auth, InstalledRecord, LibraryCache};

const TURNED_OFF: &str = "itch.io support is turned off in the settings";

/// Heroic has no itch.io library, so there is nothing to import.
pub fn import_from_heroic(_heroic_config_dir: &Path) -> Result<()> {
    Ok(())
}

#[derive(Default)]
struct State {
    auth: Option<Auth>,
    /// Download key per app name (the game id).
    keys: HashMap<String, OwnedKey>,
    installed: BTreeMap<String, InstalledRecord>,
    games: Vec<Arc<GameInfo>>,
    by_app: HashMap<String, Arc<GameInfo>>,
}

impl State {
    fn recompose(&mut self) {
        let mut games: Vec<Arc<GameInfo>> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut keys: Vec<&OwnedKey> = self.keys.values().collect();
        keys.sort_by_key(|k| k.game_id);
        for key in keys {
            let Some(game) = key.game.as_ref() else {
                continue;
            };
            let app = game.id.to_string();
            if seen.insert(app.clone()) {
                games.push(Arc::new(game_info(key, game, self.installed.get(&app))));
            }
        }
        // Installed games whose key is gone (logged out, refunded) stay
        // playable and removable.
        for (app, record) in &self.installed {
            if seen.insert(app.clone()) {
                games.push(Arc::new(orphan_info(app, record)));
            }
        }
        self.by_app = games
            .iter()
            .map(|g| (g.app_name.clone(), g.clone()))
            .collect();
        self.games = games;
    }
}

fn game_info(
    key: &OwnedKey,
    game: &ember_itch_api::Game,
    installed: Option<&InstalledRecord>,
) -> GameInfo {
    let p = game.platforms;
    let mut platforms = Vec::new();
    for (has, platform) in [
        (p.windows, InstallPlatform::Windows),
        (p.linux, InstallPlatform::Linux),
        (p.osx, InstallPlatform::Mac),
    ] {
        if has {
            platforms.push(platform);
        }
    }
    let nonempty = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    // Animated covers come with a still version; Ember shows still art.
    let cover = nonempty(&game.still_cover_url).or_else(|| nonempty(&game.cover_url));
    GameInfo {
        runner: Some(Runner::Itch),
        app_name: game.id.to_string(),
        title: game.title.clone(),
        developer: game.user.as_ref().map(|u| u.name().to_string()),
        description: nonempty(&game.short_text),
        store_url: nonempty(&game.url),
        // itch.io covers are landscape (315×250).
        art_square: cover,
        is_installed: installed.is_some(),
        install: installed.map(InstalledRecord::to_installed_info),
        installable: p.any(),
        can_run_offline: true,
        is_linux_native: p.linux,
        is_mac_native: p.osx,
        platforms,
        release_date: nonempty(&game.published_at),
        date_added: nonempty(&key.created_at),
        ..Default::default()
    }
    .finalize()
}

fn orphan_info(app: &str, record: &InstalledRecord) -> GameInfo {
    GameInfo {
        runner: Some(Runner::Itch),
        app_name: app.to_string(),
        title: record.title.clone(),
        is_installed: true,
        install: Some(record.to_installed_info()),
        can_run_offline: true,
        platforms: vec![record.platform],
        ..Default::default()
    }
    .finalize()
}

/// The system an itch.io upload must support to install as `platform`.
fn os_of(platform: InstallPlatform) -> Result<Os> {
    Ok(match platform {
        InstallPlatform::Windows => Os::Windows,
        InstallPlatform::Linux => Os::Linux,
        InstallPlatform::Mac => Os::Mac,
        InstallPlatform::Browser => bail!("itch.io browser games open on their itch.io page"),
    })
}

/// A refused key becomes [`SessionExpired`], so Ember asks to log in again.
fn session_error(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<ApiError>() {
        Some(api) if api.unauthorized => {
            let reason = api.message.clone();
            e.context(SessionExpired::new(Runner::Itch, reason))
        }
        _ => e,
    }
}

/// The itch.io store.
pub struct ItchStore {
    bus: EventBus,
    settings: SettingsFn,
    layout: Layout,
    api_base: String,
    state: RwLock<State>,
    /// Serialises changes of `installed.json`.
    files_lock: tokio::sync::Mutex<()>,
}

impl ItchStore {
    pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self> {
        let paths = Paths::get();
        Self::with_layout(
            bus,
            settings,
            Layout {
                config: paths.store_config_dir(Runner::Itch),
                cache: paths.store_cache_dir(Runner::Itch),
            },
            ember_itch_api::API_BASE,
        )
    }

    /// The store of one more account, keeping its login, installs and
    /// caches in `config` and `cache`; shared tools stay in Ember's
    /// directories.
    pub fn for_account(
        bus: EventBus,
        settings: SettingsFn,
        config: PathBuf,
        cache: PathBuf,
    ) -> Arc<Self> {
        Self::with_layout(
            bus,
            settings,
            Layout { config, cache },
            ember_itch_api::API_BASE,
        )
    }

    /// A store keeping its files in `layout` and talking to `api_base`.
    pub fn with_layout(
        bus: EventBus,
        settings: SettingsFn,
        layout: Layout,
        api_base: &str,
    ) -> Arc<Self> {
        Arc::new(Self {
            bus,
            settings,
            layout,
            api_base: api_base.to_string(),
            state: RwLock::new(State::default()),
            files_lock: tokio::sync::Mutex::new(()),
        })
    }

    fn enabled(&self) -> bool {
        (self.settings)().itch_io
    }

    fn api(&self) -> Result<Client> {
        let key = self
            .state
            .read()
            .auth
            .as_ref()
            .map(|a| a.api_key.clone())
            .context("Log in to itch.io first")?;
        Client::with_base(&self.api_base, &key)
    }

    fn record(&self, app: &str) -> Option<InstalledRecord> {
        self.state.read().installed.get(app).cloned()
    }

    fn require_record(&self, app: &str) -> Result<InstalledRecord> {
        self.record(app)
            .with_context(|| format!("{app} is not installed"))
    }

    fn key_of(&self, app: &str) -> Result<OwnedKey> {
        self.state
            .read()
            .keys
            .get(app)
            .cloned()
            .with_context(|| format!("{app} is not in your itch.io library"))
    }

    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
        tokio::task::spawn_blocking(f)
            .await
            .context("a background task failed")
    }

    /// Change the install records, save them and publish the library.
    async fn update_installed(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, InstalledRecord>),
    ) -> Result<()> {
        let _guard = self.files_lock.lock().await;
        let mut installed = self.state.read().installed.clone();
        change(&mut installed);
        let layout = self.layout.clone();
        let snapshot = installed.clone();
        Self::blocking(move || records::save_installed(&layout, &snapshot)).await??;
        {
            let mut state = self.state.write();
            state.installed = installed;
            state.recompose();
        }
        self.bus.emit(Event::LibraryChanged(Runner::Itch));
        Ok(())
    }

    fn base_dir(&self, request: &InstallRequest) -> PathBuf {
        if request.path.as_os_str().is_empty() {
            ember_core::paths::expand_home(&(self.settings)().default_install_path)
        } else {
            ember_core::paths::expand_home(&request.path.to_string_lossy())
        }
    }
}

#[async_trait]
impl Store for ItchStore {
    fn runner(&self) -> Runner {
        Runner::Itch
    }

    async fn init(&self) -> Result<()> {
        let layout = self.layout.clone();
        let (auth, library, installed) = Self::blocking(move || {
            (
                records::load_auth(&layout),
                records::load_library(&layout),
                records::load_installed(&layout),
            )
        })
        .await?;
        let mut state = self.state.write();
        state.auth = auth;
        state.keys = keys_by_app(library.keys);
        state.installed = installed;
        state.recompose();
        Ok(())
    }

    fn user(&self) -> Option<UserInfo> {
        if !self.enabled() {
            return None;
        }
        let state = self.state.read();
        let auth = state.auth.as_ref()?;
        Some(UserInfo {
            account_id: auth.user.id.to_string(),
            display_name: auth.user.name().to_string(),
        })
    }

    fn login_flow(&self) -> LoginFlow {
        LoginFlow::Browser {
            url: ember_itch_api::LOGIN_URL.into(),
            capture: ember_itch_api::LOGIN_CAPTURE.into(),
            instructions: "Sign in to itch.io, choose \"Generate new API key\" (or \"View\" next to a key you made for Ember), then copy the key and paste it here.".into(),
        }
    }

    async fn login(&self, input: &str) -> Result<UserInfo> {
        if !self.enabled() {
            bail!(TURNED_OFF);
        }
        let key = ember_itch_api::extract_api_key(input)
            .context("No itch.io API key found. Copy the key shown on itch.io’s API keys page and paste it here.")?;
        let api = Client::with_base(&self.api_base, &key)?;
        let user = match api.profile().await {
            Ok(u) => u,
            Err(e) if e.downcast_ref::<ApiError>().is_some_and(|a| a.unauthorized) => {
                bail!("itch.io did not accept this API key. Copy it again, or generate a new one.")
            }
            Err(e) => return Err(e.context("Could not log in to itch.io")),
        };
        let auth = Auth { api_key: key, user };
        let layout = self.layout.clone();
        let saved = auth.clone();
        Self::blocking(move || records::save_auth(&layout, &saved)).await??;
        self.state.write().auth = Some(auth);
        self.bus.emit(Event::AccountChanged(Runner::Itch));
        self.user().context("the login was not kept")
    }

    /// Forgets the key and the owned games; installed games stay.
    async fn logout(&self) -> Result<()> {
        let layout = self.layout.clone();
        Self::blocking(move || {
            for file in [layout.auth_file(), layout.library_file()] {
                match std::fs::remove_file(&file) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(e).with_context(|| format!("removing {}", file.display()));
                    }
                }
            }
            Ok(())
        })
        .await??;
        {
            let mut state = self.state.write();
            state.auth = None;
            state.keys.clear();
            state.recompose();
        }
        self.bus.emit(Event::AccountChanged(Runner::Itch));
        self.bus.emit(Event::LibraryChanged(Runner::Itch));
        Ok(())
    }

    async fn refresh(&self, _full: bool) -> Result<()> {
        if !self.enabled() || self.state.read().auth.is_none() {
            return Ok(());
        }
        let keys = self
            .api()?
            .owned_keys()
            .await
            .map_err(session_error)
            .context("Could not refresh the itch.io library")?;
        let cache = LibraryCache { keys };
        let layout = self.layout.clone();
        let saved = cache.clone();
        Self::blocking(move || records::save_library(&layout, &saved)).await??;
        {
            let mut state = self.state.write();
            state.keys = keys_by_app(cache.keys);
            state.recompose();
        }
        self.bus.emit(Event::LibraryChanged(Runner::Itch));
        Ok(())
    }

    /// Empty while the store is turned off.
    fn games(&self) -> Vec<Arc<GameInfo>> {
        if !self.enabled() {
            return Vec::new();
        }
        self.state.read().games.clone()
    }

    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        if !self.enabled() {
            return None;
        }
        self.state.read().by_app.get(app_name).cloned()
    }

    async fn extra_info(&self, app_name: &str) -> Result<ExtraInfo> {
        let game = self
            .game(app_name)
            .with_context(|| format!("{app_name} is not in your itch.io library"))?;
        Ok(ExtraInfo {
            short_description: game.description.clone(),
            store_url: game.store_url.clone(),
            release_date: game.release_date.clone(),
            ..Default::default()
        })
    }

    async fn install_info(
        &self,
        app_name: &str,
        platform: InstallPlatform,
        _query: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        let (key, upload) = self.choose_upload(app_name, platform).await?;
        let title = key
            .game
            .as_ref()
            .map(|g| g.title.clone())
            .unwrap_or_default();
        let resumable = self.has_partial_download(app_name).await;
        Ok(InstallInfo {
            app_name: app_name.to_string(),
            title,
            platform,
            download_size: upload.size,
            disk_size: upload.size,
            version: Some(upload.version_label()),
            is_native: !platform.needs_wine(),
            resumable,
            ..Default::default()
        })
    }

    async fn updatable_games(&self) -> Result<Vec<String>> {
        if !self.enabled() || self.state.read().auth.is_none() {
            return Ok(Vec::new());
        }
        let candidates: Vec<(String, InstalledRecord)> = self
            .state
            .read()
            .installed
            .iter()
            .filter(|(_, r)| !r.pinned_version && r.upload_id != 0)
            .map(|(a, r)| (a.clone(), r.clone()))
            .collect();
        let mut out = Vec::new();
        for (app, record) in candidates {
            match self.choose_upload(&app, record.platform).await {
                Ok((_, upload)) if upload.revision() != record.revision => out.push(app),
                Ok(_) => {}
                Err(e) => {
                    let e = session_error(e);
                    if SessionExpired::of(&e).is_some() {
                        return Err(e);
                    }
                    tracing::warn!("checking {} for updates: {e:#}", record.title);
                }
            }
        }
        Ok(out)
    }

    async fn install(
        &self,
        app_name: &str,
        request: &InstallRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        if !self.enabled() {
            bail!(TURNED_OFF);
        }
        let game = self
            .game(app_name)
            .with_context(|| format!("{app_name} is not in your itch.io library"))?;
        let dir = self
            .base_dir(request)
            .join(ember_core::paths::sanitize(game.title.trim()));
        self.run_pipeline(
            app_name,
            dir,
            request.platform,
            install::Mode::Install,
            &ctx,
        )
        .await
    }

    async fn update(
        &self,
        app_name: &str,
        _request: &UpdateRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        let record = self.require_record(app_name)?;
        self.run_pipeline(
            app_name,
            PathBuf::from(&record.install_path),
            record.platform,
            install::Mode::Update,
            &ctx,
        )
        .await
    }

    /// Downloads the installed upload again and replaces the files.
    async fn repair(&self, app_name: &str, ctx: OpContext) -> Result<Outcome> {
        let record = self.require_record(app_name)?;
        self.run_pipeline(
            app_name,
            PathBuf::from(&record.install_path),
            record.platform,
            install::Mode::Update,
            &ctx,
        )
        .await
    }

    async fn uninstall(&self, app_name: &str, args: RemoveArgs) -> Result<()> {
        let record = self.require_record(app_name)?;
        if args.delete_files {
            let dir = PathBuf::from(&record.install_path);
            let app = (self.settings)();
            Self::blocking(move || -> Result<()> {
                if !dir.exists() {
                    return Ok(());
                }
                ember_core::fs_guard::ensure_deletable(&dir, &app)?;
                std::fs::remove_dir_all(&dir)
                    .with_context(|| format!("Could not delete {}", dir.display()))
            })
            .await??;
        }
        self.update_installed(|m| {
            m.remove(app_name);
        })
        .await
    }

    /// Registers a folder holding the game. Updates are not offered for it,
    /// since which upload it came from is unknown.
    async fn import(&self, app_name: &str, path: &Path, platform: InstallPlatform) -> Result<()> {
        let game = self
            .game(app_name)
            .with_context(|| format!("{app_name} is not in your itch.io library"))?;
        let dir = path.to_path_buf();
        let (exe, size) = Self::blocking(move || {
            (
                detect::find_executable(&dir, platform),
                install::folder_size(&dir),
            )
        })
        .await?;
        let exe = exe.with_context(|| {
            format!(
                "No {} program was found in {}",
                platform.label(),
                path.display()
            )
        })?;
        let record = InstalledRecord {
            title: game.title.clone(),
            install_path: path.to_string_lossy().into_owned(),
            platform,
            executable: exe.to_string_lossy().into_owned(),
            size,
            ..Default::default()
        };
        self.update_installed(|m| {
            m.insert(app_name.to_string(), record);
        })
        .await
    }

    async fn set_install_path(&self, app_name: &str, path: &Path) -> Result<()> {
        self.require_record(app_name)?;
        let p = path.to_string_lossy().into_owned();
        self.update_installed(|m| {
            if let Some(r) = m.get_mut(app_name) {
                r.install_path = p;
            }
        })
        .await
    }

    async fn set_version_pinned(&self, app_name: &str, pinned: bool) -> Result<()> {
        self.require_record(app_name)?;
        self.update_installed(|m| {
            if let Some(r) = m.get_mut(app_name) {
                r.pinned_version = pinned;
            }
        })
        .await
    }

    async fn launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        _ctx: OpContext,
    ) -> Result<LaunchPlan> {
        launch::build_plan(&self.require_record(app_name)?, request)
    }
}

fn keys_by_app(keys: Vec<OwnedKey>) -> HashMap<String, OwnedKey> {
    let mut out: HashMap<String, OwnedKey> = HashMap::new();
    for key in keys {
        let Some(game) = key.game.as_ref() else {
            continue;
        };
        if !game.is_playable_kind() {
            continue;
        }
        // Several keys can open one game; the first (oldest) one is used.
        out.entry(game.id.to_string()).or_insert(key);
    }
    out
}
