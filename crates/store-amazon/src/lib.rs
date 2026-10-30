//! Amazon Games store backend, built on the native [`irori_amazon_api`]
//! client and the [`irori_depot`] download engine.
//!
//! State lives in `Paths::store_config_dir(Runner::Nile)` (see [`state`]):
//! the device credentials, the owned games, the install records and the
//! installed builds' manifests. Install sizes are cached and interrupted
//! downloads keep a resume journal in `Paths::store_cache_dir(Runner::Nile)`.

mod import;
mod install;
mod launch;
mod mapping;
mod state;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use irori_amazon_api::{Client, Credentials, Entitlement, LoginRequest, Manifest, auth};
use irori_core::process::{self, RunOptions};
use irori_core::{
    Event, EventBus, ExtraInfo, GameInfo, InstallInfo, InstallInfoQuery, InstallPlatform,
    InstallRequest, LaunchPlan, LaunchRequest, LoginFlow, OpContext, Outcome, Paths, RemoveArgs,
    Runner, SessionExpired, SettingsFn, Store, UpdateRequest, UserInfo, WineLaunch, json_store,
    util,
};
use once_cell::sync::OnceCell;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use crate::install::{Mode, log_line};
use crate::launch::{LaunchInputs, build_launch_plan, launch_env, post_install_command};
use crate::state::{Installed, Layout, remove_quietly};

/// Lifetime of cached install sizes (Heroic: 6 hours).
const INSTALL_INFO_TTL_SECS: u64 = 6 * 60 * 60;

const LOGIN_INSTRUCTIONS: &str = "Sign in with your Amazon account. When the browser lands on \
the Amazon home page, copy the full address from the address bar and paste it here.";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedInstallInfo {
    fetched_at: u64,
    info: InstallInfo,
}

#[derive(Default)]
struct State {
    user: Option<UserInfo>,
    library: BTreeMap<String, Arc<GameInfo>>,
    entitlements: HashMap<String, Entitlement>,
    installed: Installed,
}

/// The Amazon Games store.
///
/// Owns the in-memory catalogue (published as fresh `Arc<GameInfo>` values on
/// change), the device credentials and the pending sign-in. Long operations
/// honour [`OpContext::cancel`].
pub struct AmazonStore {
    bus: EventBus,
    settings: SettingsFn,
    layout: Layout,
    cache_dir: PathBuf,
    sdk_dir: PathBuf,
    endpoints: irori_amazon_api::Endpoints,
    /// Manifest signing key other than Amazon's (tests).
    manifest_key: Option<String>,
    client: OnceCell<Client>,
    state: RwLock<State>,
    /// Credentials, locked while a token refresh is in flight.
    creds: tokio::sync::Mutex<Option<Credentials>>,
    /// The sign-in page currently shown (memory only).
    login: Mutex<Option<LoginRequest>>,
    install_info: Mutex<HashMap<String, CachedInstallInfo>>,
    /// Target folders of installs that have not completed, for cleanup when
    /// a canceled install is removed.
    pending_dirs: Mutex<HashMap<String, PathBuf>>,
    refresh_lock: tokio::sync::Mutex<()>,
    /// Serialises read-modify-write of `installed.json` and caches.
    files_lock: tokio::sync::Mutex<()>,
    sdk_lock: tokio::sync::Mutex<()>,
}

impl AmazonStore {
    /// Create the store. Nothing is read until [`Store::init`].
    pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self> {
        let paths = Paths::get();
        Self::with_dirs(
            bus,
            settings,
            paths.store_config_dir(Runner::Nile),
            paths.store_cache_dir(Runner::Nile),
            paths.tools_dir().join("amazon-games-sdk"),
            irori_amazon_api::Endpoints::default(),
            None,
        )
    }

    /// The store of one more account, keeping its login, installs and
    /// caches in `config` and `cache`; shared tools stay in Irori's
    /// directories.
    pub fn for_account(
        bus: EventBus,
        settings: SettingsFn,
        config: PathBuf,
        cache: PathBuf,
    ) -> Arc<Self> {
        Self::with_dirs(
            bus,
            settings,
            config,
            cache,
            Paths::get().tools_dir().join("amazon-games-sdk"),
            irori_amazon_api::Endpoints::default(),
            None,
        )
    }

    fn with_dirs(
        bus: EventBus,
        settings: SettingsFn,
        config_dir: PathBuf,
        cache_dir: PathBuf,
        sdk_dir: PathBuf,
        endpoints: irori_amazon_api::Endpoints,
        manifest_key: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            bus,
            settings,
            layout: Layout::new(config_dir),
            cache_dir,
            sdk_dir,
            endpoints,
            manifest_key,
            client: OnceCell::new(),
            state: RwLock::new(State::default()),
            creds: tokio::sync::Mutex::new(None),
            login: Mutex::new(None),
            install_info: Mutex::new(HashMap::new()),
            pending_dirs: Mutex::new(HashMap::new()),
            refresh_lock: tokio::sync::Mutex::new(()),
            files_lock: tokio::sync::Mutex::new(()),
            sdk_lock: tokio::sync::Mutex::new(()),
        })
    }

    fn client(&self) -> Result<&Client> {
        self.client.get_or_try_init(|| {
            let client = Client::with_endpoints(self.endpoints.clone())?;
            match &self.manifest_key {
                Some(pem) => client.trust_manifest_key(pem),
                None => Ok(client),
            }
        })
    }

    fn download_client() -> Result<reqwest::Client> {
        static CLIENT: OnceCell<reqwest::Client> = OnceCell::new();
        CLIENT.get_or_try_init(Client::download_client).cloned()
    }

    fn install_info_cache_file(&self) -> PathBuf {
        self.cache_dir.join("install_info.json")
    }

    // ---- files ---------------------------------------------------------

    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
        Ok(tokio::task::spawn_blocking(f).await?)
    }

    async fn save_install_info_cache(&self) {
        let _guard = self.files_lock.lock().await;
        let snapshot = self.install_info.lock().clone();
        let path = self.install_info_cache_file();
        match Self::blocking(move || json_store::write(&path, &snapshot)).await {
            Ok(Err(e)) | Err(e) => tracing::warn!("{e:#}"),
            Ok(Ok(())) => {}
        }
    }

    async fn forget_install_info(&self, app_name: &str) {
        if self.install_info.lock().remove(app_name).is_some() {
            self.save_install_info_cache().await;
        }
    }

    // ---- state ---------------------------------------------------------

    /// Rebuild the catalogue from entitlements and install records, keeping
    /// installed games that dropped out of the owned list.
    fn rebuild_library(&self) {
        let mut state = self.state.write();
        let mut library: BTreeMap<String, Arc<GameInfo>> = state
            .entitlements
            .values()
            .map(|e| {
                let info = mapping::game_info(e, state.installed.get(&e.product.id));
                (info.app_name.clone(), Arc::new(info))
            })
            .collect();
        for (id, game) in &state.installed {
            if !library.contains_key(id) {
                library.insert(id.clone(), Arc::new(mapping::orphan_info(id, game)));
            }
        }
        state.library = library;
    }

    /// Change the install records, persist them and republish the entries
    /// whose install state changed.
    async fn update_installed(&self, change: impl FnOnce(&mut Installed)) -> Result<()> {
        let _guard = self.files_lock.lock().await;
        let mut installed = self.state.read().installed.clone();
        change(&mut installed);
        let layout = self.layout.clone();
        let snapshot = installed.clone();
        Self::blocking(move || layout.write_installed(&snapshot)).await??;
        self.state.write().installed = installed;
        self.rebuild_library();
        self.bus.emit(Event::LibraryChanged(Runner::Nile));
        Ok(())
    }

    fn set_entitlements(&self, list: Vec<Entitlement>) {
        self.state.write().entitlements = list
            .into_iter()
            .map(|e| (e.product.id.clone(), e))
            .collect();
        self.rebuild_library();
    }

    fn require_game(&self, app_name: &str) -> Result<Arc<GameInfo>> {
        self.game(app_name)
            .with_context(|| format!("{app_name} is not in your Amazon library"))
    }

    fn require_entitlement(&self, app_name: &str) -> Result<Entitlement> {
        self.state
            .read()
            .entitlements
            .get(app_name)
            .cloned()
            .with_context(|| {
                format!("{app_name} is not in your Amazon library; sign in and refresh the library")
            })
    }

    fn installed(&self, app_name: &str) -> Option<state::InstalledGame> {
        self.state.read().installed.get(app_name).cloned()
    }

    // ---- account -------------------------------------------------------

    /// Valid credentials, refreshing the access token when it is about to
    /// expire.
    async fn credentials(&self) -> Result<Credentials> {
        let mut guard = self.creds.lock().await;
        let creds = guard
            .clone()
            .context("You are not signed in to Amazon Games")?;
        if !creds.needs_refresh(util::unix_now()) {
            return Ok(creds);
        }
        let fresh = match self.client()?.refresh(&creds).await {
            Ok(fresh) => fresh,
            Err(e) => {
                if let Some(rejected) = e.downcast_ref::<irori_amazon_api::SignInRejected>() {
                    return Err(SessionExpired::new(Runner::Nile, rejected.0.clone()).into());
                }
                return Err(e.context("Your Amazon session could not be renewed"));
            }
        };
        let layout = self.layout.clone();
        let saved = fresh.clone();
        Self::blocking(move || layout.write_credentials(&saved)).await??;
        *guard = Some(fresh.clone());
        Ok(fresh)
    }

    fn user_info(creds: &Credentials) -> UserInfo {
        UserInfo {
            account_id: creds.user_id.clone(),
            display_name: creds.display_name.clone(),
        }
    }

    // ---- launch --------------------------------------------------------

    /// Download the Amazon Games SDK the first time a game starts. Games that
    /// use it fail without it; the others do not care, so a failure is only
    /// logged.
    async fn ensure_sdk(&self, ctx: &OpContext) -> Option<PathBuf> {
        let _guard = self.sdk_lock.lock().await;
        let marker = self.sdk_dir.join(".version");
        if marker.is_file() {
            return Some(self.sdk_dir.clone());
        }
        (ctx.step)("Downloading the Amazon Games SDK");
        let result = async {
            let (download, files) = irori_amazon_api::sdk::sdk_files(self.client()?).await?;
            let finished = irori_depot::download(
                &AmazonStore::download_client()?,
                irori_depot::Job {
                    root: &self.sdk_dir,
                    files: &files,
                    remove: &[],
                },
                &irori_depot::Options::default(),
                irori_depot::ignore_progress(),
                &ctx.cancel,
            )
            .await?;
            if finished == irori_depot::Finished::Done {
                tokio::fs::write(&marker, download.version_id).await?;
            }
            anyhow::Ok(())
        }
        .await;
        match result {
            Ok(()) if marker.is_file() => Some(self.sdk_dir.clone()),
            Ok(()) => None,
            Err(e) => {
                tracing::warn!("Amazon Games SDK unavailable: {e:#}");
                log_line(ctx, &format!("Amazon Games SDK unavailable: {e:#}"));
                None
            }
        }
    }

    /// Run fuel.json's post-install steps the first time the game starts in
    /// a prefix, recording it in the prefix's `installed_games` list.
    async fn post_install_once(
        &self,
        game: &GameInfo,
        install_dir: &Path,
        fuel: &irori_amazon_api::fuel::Fuel,
        wine: Option<&WineLaunch>,
        request: &LaunchRequest,
        ctx: &OpContext,
    ) -> Result<()> {
        let list_file = wine
            .map(|w| launch::prefix_games_file(&w.prefix))
            .unwrap_or_else(|| install_dir.join(".irori-installed-games.json"));
        let mut done: Vec<String> = match tokio::fs::read(&list_file).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        if done.iter().any(|a| a == &game.app_name) {
            return Ok(());
        }
        if !fuel.post_install.is_empty() {
            tracing::info!(
                "running {} post-install steps of {}",
                fuel.post_install.len(),
                game.title
            );
            (ctx.step)("Installing dependencies (Amazon)");
            let env = launch_env(&request.environment, &game.app_name);
            let mut succeeded = true;
            for action in &fuel.post_install {
                let spec = post_install_command(wine, &env, action, install_dir);
                let opts = RunOptions::default()
                    .cancel(ctx.cancel.clone())
                    .log_file(request.log_file.clone());
                match process::run(&spec, opts).await {
                    Ok(r) if r.aborted => bail!("Launch canceled"),
                    Ok(r) if !r.success() => {
                        succeeded = false;
                        tracing::warn!(
                            "post-install step {} of {} exited with {:?}",
                            action.command,
                            game.title,
                            r.code
                        );
                    }
                    Err(e) => {
                        succeeded = false;
                        tracing::warn!("post-install step of {}: {e:#}", game.title);
                    }
                    Ok(_) => {}
                }
            }
            if !succeeded {
                return Ok(());
            }
        }

        done.push(game.app_name.clone());
        if let Err(e) = async {
            if let Some(parent) = list_file.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&list_file, serde_json::to_vec(&done)?).await?;
            anyhow::Ok(())
        }
        .await
        {
            tracing::warn!("could not record setup in {}: {e:#}", list_file.display());
        }
        Ok(())
    }

    /// Carry over state an earlier Irori build left for the nile helper in
    /// `<config>/nile`, then keep it as `<config>/nile-backup`.
    async fn migrate_nile_dir(&self) {
        let old = self.layout.dir.join("nile");
        let earlier_backup = self.layout.dir.join("nile-backup");
        let _ = Self::blocking(move || import::remove_nile_login(&earlier_backup)).await;
        if !old.is_dir() {
            return;
        }
        let layout = self.layout.clone();
        let cache_dir = self.cache_dir.clone();
        let res = Self::blocking(move || -> Result<()> {
            import::import_nile_dir(&old, &layout)?;
            let backup = layout.dir.join("nile-backup");
            if backup.exists() {
                std::fs::remove_dir_all(&backup)?;
            }
            std::fs::rename(&old, &backup)?;
            import::remove_nile_login(&backup);
            remove_quietly(&layout.dir.join("account.json"));
            remove_quietly(&cache_dir.join("library.json"));
            Ok(())
        })
        .await;
        match res {
            Ok(Ok(())) => tracing::info!("moved the Amazon login and installs to Irori's format"),
            Ok(Err(e)) | Err(e) => tracing::warn!("converting nile's Amazon data: {e:#}"),
        }
    }
}

#[async_trait]
impl Store for AmazonStore {
    fn runner(&self) -> Runner {
        Runner::Nile
    }

    async fn init(&self) -> Result<()> {
        let dir = self.layout.dir.clone();
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
        self.migrate_nile_dir().await;

        let layout = self.layout.clone();
        let cache_file = self.install_info_cache_file();
        let (creds, entitlements, installed, install_info) = Self::blocking(move || {
            (
                layout.read_credentials(),
                layout.read_entitlements(),
                layout.read_installed(),
                json_store::read::<HashMap<String, CachedInstallInfo>>(&cache_file)
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
            )
        })
        .await?;
        *self.install_info.lock() = install_info;
        {
            let mut state = self.state.write();
            state.user = creds.as_ref().map(Self::user_info);
            state.installed = installed;
        }
        *self.creds.lock().await = creds;
        self.set_entitlements(entitlements);
        if !self.state.read().library.is_empty() {
            self.bus.emit(Event::LibraryChanged(Runner::Nile));
        }
        Ok(())
    }

    fn user(&self) -> Option<UserInfo> {
        self.state.read().user.clone()
    }

    fn login_flow(&self) -> LoginFlow {
        let request = self
            .login
            .lock()
            .get_or_insert_with(LoginRequest::new)
            .clone();
        LoginFlow::Browser {
            url: request.url,
            capture: auth::CODE_CAPTURE.to_string(),
            instructions: LOGIN_INSTRUCTIONS.to_string(),
        }
    }

    async fn login(&self, input: &str) -> Result<UserInfo> {
        let code = auth::extract_auth_code(input).context(
            "Paste the address of the page Amazon opened after signing in (it contains \
             openid.oa2.authorization_code), or the code itself.",
        )?;
        // Authorization codes are single-use; the next attempt needs a new page.
        let request = self.login.lock().take().context(
            "The Amazon sign-in page has expired. Open it again and repeat the sign-in.",
        )?;
        tracing::info!("registering Irori as an Amazon Games device");
        let creds = self
            .client()?
            .register(&request, &code)
            .await
            .context("Amazon login failed")?;
        let layout = self.layout.clone();
        let saved = creds.clone();
        Self::blocking(move || layout.write_credentials(&saved)).await??;
        let info = Self::user_info(&creds);
        *self.creds.lock().await = Some(creds);
        self.state.write().user = Some(info.clone());
        self.bus.emit(Event::AccountChanged(Runner::Nile));
        Ok(info)
    }

    async fn logout(&self) -> Result<()> {
        match self.credentials().await {
            Ok(creds) => {
                if let Err(e) = self.client()?.deregister(&creds).await {
                    tracing::warn!("{e:#}");
                }
            }
            Err(e) => tracing::warn!("signing out of Amazon: {e:#}"),
        }
        for file in [
            self.layout.credentials(),
            self.layout.entitlements(),
            self.install_info_cache_file(),
        ] {
            remove_quietly(&file);
        }
        *self.creds.lock().await = None;
        self.install_info.lock().clear();
        *self.login.lock() = None;
        {
            let mut state = self.state.write();
            state.user = None;
            state.entitlements.clear();
            state.library.clear();
        }
        self.bus.emit(Event::AccountChanged(Runner::Nile));
        self.bus.emit(Event::LibraryChanged(Runner::Nile));
        Ok(())
    }

    async fn refresh(&self, _full: bool) -> Result<()> {
        if !self.is_logged_in() {
            return Ok(());
        }
        let _guard = self.refresh_lock.lock().await;
        let creds = self.credentials().await?;
        let list = self
            .client()?
            .entitlements(&creds)
            .await
            .context("Could not sync the Amazon library")?;
        tracing::info!("Amazon library: {} games", list.len());
        let layout = self.layout.clone();
        let saved = list.clone();
        Self::blocking(move || layout.write_entitlements(&saved)).await??;
        self.set_entitlements(list);
        self.bus.emit(Event::LibraryChanged(Runner::Nile));
        Ok(())
    }

    fn games(&self) -> Vec<Arc<GameInfo>> {
        self.state.read().library.values().cloned().collect()
    }

    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        self.state.read().library.get(app_name).cloned()
    }

    async fn extra_info(&self, app_name: &str) -> Result<ExtraInfo> {
        let game = self.require_game(app_name)?;
        Ok(game.extra.clone().unwrap_or_else(|| ExtraInfo {
            description: game.description.clone(),
            short_description: game.description.clone(),
            release_date: game.release_date.clone(),
            genres: game.genres.clone(),
            ..Default::default()
        }))
    }

    async fn install_info(
        &self,
        app_name: &str,
        _platform: InstallPlatform,
        _query: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        let now = util::unix_now();
        let resumable = self.journal_file(app_name).is_file();
        if let Some(c) = self.install_info.lock().get(app_name)
            && now.saturating_sub(c.fetched_at) < INSTALL_INFO_TTL_SECS
        {
            return Ok(InstallInfo {
                resumable,
                ..c.info.clone()
            });
        }
        let game = self.require_game(app_name)?;
        let ent = self.require_entitlement(app_name)?;
        let (_, fetched) = self
            .current_build(&ent)
            .await
            .with_context(|| format!("Could not read the download size of {}", game.title))?;
        let size = fetched.manifest.size();
        let info = InstallInfo {
            app_name: app_name.to_string(),
            title: game.title.clone(),
            platform: InstallPlatform::Windows,
            download_size: size,
            disk_size: size,
            version: self.installed(app_name).map(|g| g.version),
            is_native: false,
            ..Default::default()
        };
        self.install_info.lock().insert(
            app_name.to_string(),
            CachedInstallInfo {
                fetched_at: now,
                info: info.clone(),
            },
        );
        self.save_install_info_cache().await;
        Ok(InstallInfo { resumable, ..info })
    }

    async fn updatable_games(&self) -> Result<Vec<String>> {
        if !self.is_logged_in() {
            return Ok(Vec::new());
        }
        let installed: Vec<(String, String)> = {
            let state = self.state.read();
            state
                .installed
                .iter()
                .filter(|(id, _)| state.entitlements.contains_key(*id))
                .map(|(id, g)| (id.clone(), g.version.clone()))
                .collect()
        };
        if installed.is_empty() {
            return Ok(Vec::new());
        }
        let creds = self.credentials().await?;
        let ids: Vec<String> = installed.iter().map(|(id, _)| id.clone()).collect();
        let live = self.client()?.live_versions(&creds, &ids).await?;
        Ok(installed
            .into_iter()
            .filter(|(id, version)| live.get(id).is_some_and(|v| !v.is_empty() && v != version))
            .map(|(id, _)| id)
            .collect())
    }

    async fn install(
        &self,
        app_name: &str,
        request: &InstallRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        let game = self.require_game(app_name)?;
        let base = if request.path.as_os_str().is_empty() {
            PathBuf::from(&(self.settings)().default_install_path)
        } else {
            request.path.clone()
        };
        let folder = game
            .folder_name
            .clone()
            .filter(|f| !f.trim().is_empty())
            .unwrap_or_else(|| irori_core::paths::sanitize(app_name));
        let dir = base.join(&folder);
        // Only a folder this install creates may be deleted on cancel.
        if !dir.exists() {
            self.pending_dirs
                .lock()
                .insert(app_name.to_string(), dir.clone());
        }
        let outcome = self.deploy(app_name, &dir, Mode::Install, &ctx).await?;
        if outcome.is_done() {
            self.pending_dirs.lock().remove(app_name);
        }
        Ok(outcome)
    }

    async fn update(
        &self,
        app_name: &str,
        _request: &UpdateRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        let installed = self
            .installed(app_name)
            .with_context(|| format!("{app_name} is not installed"))?;
        self.deploy(app_name, &installed.path, Mode::Update, &ctx)
            .await
    }

    async fn repair(&self, app_name: &str, ctx: OpContext) -> Result<Outcome> {
        let installed = self
            .installed(app_name)
            .with_context(|| format!("Could not find install path for {app_name}"))?;
        self.deploy(app_name, &installed.path, Mode::Repair, &ctx)
            .await
    }

    async fn uninstall(&self, app_name: &str, args: RemoveArgs) -> Result<()> {
        let title = self
            .game(app_name)
            .map(|g| g.title.clone())
            .unwrap_or_else(|| app_name.to_string());
        let installed = self.installed(app_name);
        if args.delete_files {
            match &installed {
                Some(game) => self
                    .delete_install_files(app_name, &game.path)
                    .await
                    .with_context(|| format!("Could not uninstall {title}"))?,
                None => {
                    // A canceled install: remove the partial download.
                    let pending = self.pending_dirs.lock().remove(app_name);
                    if let Some(dir) = pending.filter(|d| d.is_dir()) {
                        irori_core::fs_guard::ensure_deletable(&dir, &(self.settings)())?;
                        tokio::fs::remove_dir_all(&dir)
                            .await
                            .with_context(|| format!("Could not delete {}", dir.display()))?;
                    }
                }
            }
        }
        remove_quietly(&self.journal_file(app_name));
        if installed.is_some() {
            self.layout.remove_manifest(app_name);
            self.update_installed(|i| {
                i.remove(app_name);
            })
            .await?;
        }
        self.forget_install_info(app_name).await;
        Ok(())
    }

    async fn import(&self, app_name: &str, path: &Path, _platform: InstallPlatform) -> Result<()> {
        let game = self.require_game(app_name)?;
        if !path.is_dir() {
            bail!("{} is not a folder", path.display());
        }
        let ent = self.require_entitlement(app_name)?;
        let (download, fetched) = self.current_build(&ent).await?;
        let entries = Manifest::entries(&fetched.manifest.files, &download.download_url)?;
        let verified = irori_depot::verify(
            path,
            &entries,
            0,
            irori_depot::ignore_progress(),
            &irori_depot::CancellationToken::new(),
        )
        .await?;
        if let irori_depot::Verified::Checked(bad) = &verified
            && !bad.is_empty()
        {
            bail!(
                "Could not import {}: {} of its files are missing or damaged in {}. \
                 Install it there instead to repair them.",
                game.title,
                bad.len(),
                path.display()
            );
        }
        self.layout.write_manifest(app_name, &fetched.bytes)?;
        let record = state::InstalledGame {
            version: download.version_id,
            path: path.to_path_buf(),
            size: fetched.manifest.size(),
        };
        self.update_installed(|i| {
            i.insert(app_name.to_string(), record);
        })
        .await
    }

    async fn set_install_path(&self, app_name: &str, path: &Path) -> Result<()> {
        if self.installed(app_name).is_none() {
            bail!("{app_name} is not installed");
        }
        let path = path.to_path_buf();
        self.update_installed(|i| {
            if let Some(g) = i.get_mut(app_name) {
                g.path = path;
            }
        })
        .await
    }

    async fn launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        ctx: OpContext,
    ) -> Result<LaunchPlan> {
        let game = self.require_game(app_name)?;
        let installed = self
            .installed(app_name)
            .with_context(|| format!("{} is not installed", game.title))?;
        let dir = installed.path.clone();
        let fuel = {
            let dir = dir.clone();
            Self::blocking(move || irori_amazon_api::fuel::read(&dir)).await??
        };
        let info = fuel.launch_info(&dir)?;
        let sdk_dir = if request.offline {
            Some(self.sdk_dir.clone()).filter(|d| d.join(".version").is_file())
        } else {
            self.ensure_sdk(&ctx).await
        };
        self.post_install_once(
            &game,
            &dir,
            &fuel,
            request.environment.wine.as_ref(),
            request,
            &ctx,
        )
        .await?;
        let ent = self.state.read().entitlements.get(app_name).cloned();
        let display_name = self.user().map(|u| u.display_name).unwrap_or_default();
        build_launch_plan(&LaunchInputs {
            install_dir: &dir,
            info: &info,
            app_name,
            entitlement_id: ent.as_ref().map_or("", |e| e.id.as_str()),
            sku: ent
                .as_ref()
                .and_then(|e| e.product.sku.as_deref())
                .unwrap_or(""),
            display_name: &display_name,
            sdk_dir: sdk_dir.as_deref(),
            settings: &request.settings,
            environment: &request.environment,
            option: request.option.as_ref(),
            extra_args: &request.extra_args,
        })
    }
}

/// Carry the Amazon login and installed games over from a Heroic config dir
/// (`~/.config/heroic`), converting nile's files into Irori's. An existing
/// Irori login is kept; install records are merged. Does nothing when
/// Heroic has no Amazon data.
pub fn import_from_heroic(heroic_config_dir: &Path) -> Result<()> {
    let layout = Layout::new(Paths::get().store_config_dir(Runner::Nile));
    import::import_nile_dir(&heroic_config_dir.join("nile_config").join("nile"), &layout)
        .context("Could not import the Amazon Games data from Heroic")?;
    Ok(())
}

#[cfg(test)]
mod tests;
