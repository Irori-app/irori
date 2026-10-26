//! ZOOM Platform store for Ember.
//!
//! The store talks to ZOOM's HTTP API directly
//! (Bearer `li_token` from the browser login), downloads installers through
//! the [`ember_depot`] engine (resumable, verified, speed-limited), and
//! installs them natively (Linux archives, MojoSetup and makeself installers,
//! unpacked without running them) or through Wine (InnoSetup/NSIS installers,
//! run silently via [`ember_runtime::run_in_prefix`]).
//!
//! Ownership: [`ZoomStore`] owns the credentials, the cached catalogue and the
//! installed-games records under the store config/cache directories. The
//! services layer drives installs through the [`Store`] trait and owns the
//! queue, statuses and game processes.

mod api;
mod detect;
mod extract;
mod heroic;
mod install;
mod launch;
mod mapping;
mod records;
#[cfg(test)]
mod test_server;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use ember_core::{
    Event, EventBus, ExtraInfo, GameInfo, InstallInfo, InstallInfoQuery, InstallPlatform,
    InstallRequest, LaunchPlan, LaunchRequest, LoginFlow, Notice, NoticeLevel, OpContext, Outcome,
    RemoveArgs, Runner, SessionExpired, SettingsFn, Store, UpdateRequest, UserInfo, async_trait,
    util,
};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use regex::Regex;

use crate::api::{DEFAULT_API_URL, LOGIN_URL, Unauthorized, ZoomApi, ZoomGame};
use crate::install::Mode;
use crate::records::{Auth, InstalledRecord, Layout, LibraryCache, PendingInstall};

pub use heroic::import_from_heroic;

/// Max age of cached file listings used by update checks.
const FILES_CACHE_SECS: u64 = 6 * 3600;

/// Construction parameters (tests point these at a local server and temp dirs).
#[derive(Debug, Clone)]
pub(crate) struct ZoomOptions {
    pub api_base: String,
    pub layout: Layout,
    /// Pause between library pages, to be gentle with the API.
    pub page_delay: Duration,
}

impl Default for ZoomOptions {
    fn default() -> Self {
        Self {
            api_base: DEFAULT_API_URL.to_string(),
            layout: Layout::default(),
            page_delay: Duration::from_secs(1),
        }
    }
}

#[derive(Default)]
struct State {
    auth: Option<Auth>,
    /// Raw catalogue as returned by the API.
    library: Arc<Vec<ZoomGame>>,
    installed: BTreeMap<String, InstalledRecord>,
    pending: BTreeMap<String, PendingInstall>,
    /// Published library entries; replaced (never mutated) on change.
    games: Arc<BTreeMap<String, Arc<GameInfo>>>,
}

/// The ZOOM Platform store backend.
pub struct ZoomStore {
    bus: EventBus,
    settings: SettingsFn,
    opts: ZoomOptions,
    state: RwLock<State>,
}

/// `li_token` as a parameter of the address the login redirects to. Anchored
/// so the login page's own `return_li_token=true` does not count.
const LI_TOKEN_CAPTURE: &str = r"(?:^|[?&#])li_token=([^&#\s]+)";
static LI_TOKEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(LI_TOKEN_CAPTURE).expect("token regex"));
static BARE_TOKEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Za-z0-9._~+/=-]{16,}$").expect("bare token regex"));

/// Pull the `li_token` out of what the user pasted: the redirect URL (or any
/// text containing it), or the bare token.
pub(crate) fn extract_token(input: &str) -> Option<String> {
    let input = input.trim();
    if let Some(c) = LI_TOKEN_RE.captures(input) {
        let raw = c.get(1)?.as_str();
        let decoded: String = url::form_urlencoded::parse(format!("t={raw}").as_bytes())
            .find(|(k, _)| k == "t")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_else(|| raw.to_string());
        return (!decoded.is_empty()).then_some(decoded);
    }
    BARE_TOKEN_RE.is_match(input).then(|| input.to_string())
}

impl ZoomStore {
    /// Create the store. Nothing is read from disk until [`Store::init`].
    pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self> {
        Self::with_options(bus, settings, ZoomOptions::default())
    }

    pub(crate) fn with_options(
        bus: EventBus,
        settings: SettingsFn,
        opts: ZoomOptions,
    ) -> Arc<Self> {
        Arc::new(Self {
            bus,
            settings,
            opts,
            state: RwLock::new(State::default()),
        })
    }

    fn layout(&self) -> &Layout {
        &self.opts.layout
    }

    /// ZOOM support can be switched off in the settings.
    fn enabled(&self) -> bool {
        (self.settings)().zoom_platform
    }

    fn api(&self) -> Result<ZoomApi> {
        let st = self.state.read();
        let auth = st.auth.as_ref().context("Not logged in to ZOOM Platform")?;
        Ok(ZoomApi::new(&self.opts.api_base, &auth.token))
    }

    /// Forget a token the API rejected and tell the UI.
    fn expire_session_if_unauthorized(&self, err: &anyhow::Error) {
        if !err.chain().any(|e| e.is::<Unauthorized>()) {
            return;
        }
        let had = self.state.write().auth.take().is_some();
        if had {
            let _ = std::fs::remove_file(self.layout().auth_file());
            self.bus.emit(Event::AccountChanged(Runner::Zoom));
        }
    }

    fn build_entry(st: &State, raw: Option<&ZoomGame>, app: &str) -> Option<Arc<GameInfo>> {
        let installed = st.installed.get(app);
        match (raw, installed) {
            (Some(raw), inst) => Some(Arc::new(mapping::to_game_info(raw, inst))),
            (None, Some(rec)) => Some(Arc::new(mapping::orphan_game_info(rec))),
            (None, None) => None,
        }
    }

    fn rebuild_all(st: &mut State) {
        let mut map = BTreeMap::new();
        for raw in st.library.iter() {
            if let Some(g) = Self::build_entry(st, Some(raw), &raw.id) {
                map.insert(raw.id.clone(), g);
            }
        }
        for app in st.installed.keys() {
            if !map.contains_key(app)
                && let Some(g) = Self::build_entry(st, None, app)
            {
                map.insert(app.clone(), g);
            }
        }
        st.games = Arc::new(map);
    }

    /// Republish one entry after its install state changed.
    fn publish(st: &mut State, app: &str) {
        let library = st.library.clone();
        let raw = library.iter().find(|g| g.id == app);
        let mut map = (*st.games).clone();
        match Self::build_entry(st, raw, app) {
            Some(g) => {
                map.insert(app.to_string(), g);
            }
            None => {
                map.remove(app);
            }
        }
        st.games = Arc::new(map);
    }

    fn save_installed(&self, st: &State) -> Result<()> {
        records::save_installed(self.layout(), &st.installed)
    }

    fn save_pending(&self, st: &State) -> Result<()> {
        records::save_pending(self.layout(), &st.pending)
    }

    fn installed_record(&self, app: &str) -> Option<InstalledRecord> {
        self.state.read().installed.get(app).cloned()
    }

    /// Persist a record and publish the updated entry.
    fn put_record(&self, record: InstalledRecord) -> Result<()> {
        let app = record.app_name.clone();
        {
            let mut st = self.state.write();
            st.installed.insert(app.clone(), record);
            self.save_installed(&st)?;
            Self::publish(&mut st, &app);
        }
        self.bus.emit(Event::LibraryChanged(Runner::Zoom));
        Ok(())
    }

    /// Drop an interrupted install's staging files (and the install
    /// directory it created, when `delete_install_dir`).
    async fn discard_pending(&self, app: &str, delete_install_dir: bool) -> Result<()> {
        let pending = {
            let mut st = self.state.write();
            let p = st.pending.remove(app);
            if p.is_some() {
                self.save_pending(&st)?;
            }
            p
        };
        let Some(p) = pending else { return Ok(()) };
        self.remove_dir_if_safe(&p.staging_dir).await;
        if let Some(parent) = p.staging_dir.parent() {
            let _ = tokio::fs::remove_dir(parent).await;
        }
        if delete_install_dir && p.created_install_dir {
            self.remove_dir_if_safe(&p.install_path).await;
        }
        Ok(())
    }

    /// Delete a directory tree unless it is obviously not a game folder.
    pub(crate) async fn remove_dir_if_safe(&self, path: &Path) {
        let app = (self.settings)();
        let protected = [ember_core::paths::expand_home(&app.default_install_path)];
        if !is_safe_to_delete(path, &protected) {
            tracing::warn!("refusing to delete {}", path.display());
            return;
        }
        // The launcher-wide guard as well: system folders, Ember's own
        // folders and anything holding them, after resolving symlinks.
        if let Err(e) = ember_core::fs_guard::ensure_deletable(path, &app) {
            tracing::warn!("refusing to delete {}: {e:#}", path.display());
            return;
        }
        if let Err(e) = tokio::fs::remove_dir_all(path).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("deleting {}: {e}", path.display());
        }
    }

    fn notice_missing_executable(&self, app: &str, title: &str) {
        self.bus.notice(
            Notice::new(NoticeLevel::Warning, format!("Executable of {title} not found"))
                .message(
                    "Ember could not tell which program starts the game. Choose it in the game settings (target executable).",
                )
                .game(ember_core::GameId::new(Runner::Zoom, app)),
        );
    }

    async fn fetch_library(&self, api: &ZoomApi) -> Result<Vec<ZoomGame>> {
        let mut page = api.library_page(None).await?;
        let mut games = std::mem::take(&mut page.games);
        let mut current = page.current_page.max(1);
        let total = page.total_pages.min(1000);
        while current < total {
            tokio::time::sleep(self.opts.page_delay).await;
            let mut next = api.library_page(Some(current + 1)).await?;
            if next.games.is_empty() {
                break;
            }
            games.append(&mut next.games);
            current = next.current_page.max(current + 1);
        }
        let mut seen = std::collections::HashSet::new();
        games.retain(|g| !g.id.is_empty() && seen.insert(g.id.clone()));
        Ok(games)
    }

    async fn files_for_update_check(&self, api: &ZoomApi, app: &str) -> Result<api::FilesResponse> {
        if let Some(files) = records::load_files_cache(self.layout(), app, FILES_CACHE_SECS) {
            return Ok(files);
        }
        let files = api.files(app).await?;
        records::save_files_cache(self.layout(), app, &files);
        Ok(files)
    }
}

/// Whether `path` may be deleted as a game or staging folder: never a
/// filesystem root, a top-level directory, the home directory or one of the
/// `protected` folders (install base directories).
pub(crate) fn is_safe_to_delete(path: &Path, protected: &[PathBuf]) -> bool {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    path.is_absolute()
        && path.components().count() >= 3
        && home.as_deref() != Some(path)
        && !protected.iter().any(|p| p == path)
}

#[async_trait]
#[async_trait]
impl Store for ZoomStore {
    fn runner(&self) -> Runner {
        Runner::Zoom
    }

    async fn init(&self) -> Result<()> {
        let layout = self.layout().clone();
        let (auth, installed, pending, cache) = tokio::task::spawn_blocking(move || {
            (
                records::load_auth(&layout),
                records::load_installed(&layout),
                records::load_pending(&layout),
                ember_core::json_store::read_or_default::<LibraryCache>(&layout.library_cache()),
            )
        })
        .await
        .context("loading ZOOM Platform data")?;
        {
            let mut st = self.state.write();
            st.auth = auth;
            st.installed = installed;
            st.pending = pending;
            st.library = Arc::new(cache.games);
            Self::rebuild_all(&mut st);
        }
        self.bus.emit(Event::LibraryChanged(Runner::Zoom));
        Ok(())
    }

    fn user(&self) -> Option<UserInfo> {
        let st = self.state.read();
        st.auth.as_ref().map(|a| UserInfo {
            account_id: a.username.clone(),
            display_name: a.username.clone(),
        })
    }

    fn login_flow(&self) -> LoginFlow {
        LoginFlow::Browser {
            url: LOGIN_URL.to_string(),
            capture: LI_TOKEN_CAPTURE.to_string(),
            instructions: "Sign in to ZOOM Platform. When the page finishes loading, copy its full address (it contains li_token=…) and paste it here.".to_string(),
        }
    }

    async fn login(&self, input: &str) -> Result<UserInfo> {
        if !self.enabled() {
            bail!("ZOOM Platform support is turned off in the settings");
        }
        let token = extract_token(input).context(
            "No ZOOM login token found. Paste the full address of the page shown after signing in (it contains li_token=).",
        )?;
        let api = ZoomApi::new(&self.opts.api_base, &token);
        let name = api.logged_in().await.map_err(|e| {
            if e.chain().any(|c| c.is::<Unauthorized>()) {
                anyhow::anyhow!("ZOOM Platform did not accept this login; please sign in again")
            } else {
                e
            }
        })?;
        let auth = Auth {
            token,
            username: name.clone(),
        };
        records::save_auth(self.layout(), &auth).context("saving ZOOM Platform login")?;
        self.state.write().auth = Some(auth);
        self.bus.emit(Event::AccountChanged(Runner::Zoom));
        Ok(UserInfo {
            account_id: name.clone(),
            display_name: name,
        })
    }

    async fn logout(&self) -> Result<()> {
        let layout = self.layout().clone();
        {
            let mut st = self.state.write();
            st.auth = None;
            st.library = Arc::new(Vec::new());
            Self::rebuild_all(&mut st);
        }
        tokio::task::spawn_blocking(move || {
            for f in [layout.auth_file(), layout.library_cache()] {
                match std::fs::remove_file(&f) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        return Err(e).with_context(|| format!("removing {}", f.display()));
                    }
                    _ => {}
                }
            }
            let _ = std::fs::remove_dir_all(layout.files_cache_dir());
            Ok(())
        })
        .await
        .context("logging out of ZOOM Platform")??;
        self.bus.emit(Event::AccountChanged(Runner::Zoom));
        self.bus.emit(Event::LibraryChanged(Runner::Zoom));
        Ok(())
    }

    async fn refresh(&self, full: bool) -> Result<()> {
        if !self.enabled() || !self.is_logged_in() {
            return Ok(());
        }
        let api = self.api()?;
        if full {
            let dir = self.layout().files_cache_dir();
            let _ = tokio::fs::remove_dir_all(dir).await;
        }
        let games = match self.fetch_library(&api).await {
            Ok(g) => g,
            Err(e) => {
                self.expire_session_if_unauthorized(&e);
                if e.chain().any(|c| c.is::<Unauthorized>()) {
                    return Err(SessionExpired::new(Runner::Zoom, format!("{e}")).into());
                }
                return Err(e.context("Could not load the ZOOM Platform library"));
            }
        };
        let cache = LibraryCache {
            fetched_at: util::unix_now(),
            games,
        };
        let path = self.layout().library_cache();
        if let Err(e) = ember_core::json_store::write(&path, &cache) {
            tracing::warn!("caching ZOOM library: {e:#}");
        }
        {
            let mut st = self.state.write();
            st.library = Arc::new(cache.games);
            Self::rebuild_all(&mut st);
        }
        self.bus.emit(Event::LibraryChanged(Runner::Zoom));
        Ok(())
    }

    fn games(&self) -> Vec<Arc<GameInfo>> {
        if !self.enabled() {
            return Vec::new();
        }
        self.state.read().games.values().cloned().collect()
    }

    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        self.state.read().games.get(app_name).cloned()
    }

    async fn extra_info(&self, app_name: &str) -> Result<ExtraInfo> {
        let st = self.state.read();
        let raw = st
            .library
            .iter()
            .find(|g| g.id == app_name)
            .with_context(|| format!("{app_name} is not in your ZOOM Platform library"))?;
        let non_empty = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
        Ok(ExtraInfo {
            description: non_empty(&raw.description),
            store_url: non_empty(&raw.store_url),
            ..Default::default()
        })
    }

    async fn install_info(
        &self,
        app_name: &str,
        platform: InstallPlatform,
        _query: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        let game = self
            .game(app_name)
            .with_context(|| format!("{app_name} is not in your ZOOM Platform library"))?;
        if !matches!(platform, InstallPlatform::Windows | InstallPlatform::Linux) {
            bail!("ZOOM Platform only offers Windows and Linux builds");
        }
        let api = self.api()?;
        let files = api.files(app_name).await.inspect_err(|e| {
            self.expire_session_if_unauthorized(e);
        })?;
        records::save_files_cache(self.layout(), app_name, &files);
        let list = mapping::platform_files(&files, platform);
        if list.is_empty() {
            bail!(
                "{} has no {} build on ZOOM Platform",
                game.title,
                platform.label()
            );
        }
        let resumable = self
            .state
            .read()
            .pending
            .get(app_name)
            .is_some_and(|p| p.platform == platform && p.staging_dir.is_dir());
        Ok(mapping::install_info(&game, list, platform, resumable))
    }

    async fn updatable_games(&self) -> Result<Vec<String>> {
        if !self.enabled() || !self.is_logged_in() {
            return Ok(Vec::new());
        }
        let api = self.api()?;
        let candidates: Vec<InstalledRecord> = self
            .state
            .read()
            .installed
            .values()
            .filter(|r| !r.pinned_version)
            .cloned()
            .collect();
        let mut updatable = Vec::new();
        let mut baselines = Vec::new();
        for rec in candidates {
            let files = match self.files_for_update_check(&api, &rec.app_name).await {
                Ok(f) => f,
                Err(e) => {
                    self.expire_session_if_unauthorized(&e);
                    if e.chain().any(|c| c.is::<Unauthorized>()) {
                        return Err(SessionExpired::new(Runner::Zoom, format!("{e}")).into());
                    }
                    tracing::warn!("checking ZOOM updates of {}: {e:#}", rec.app_name);
                    continue;
                }
            };
            let current = mapping::fingerprint(mapping::platform_files(&files, rec.build_platform));
            if current.is_empty() {
                continue;
            }
            if rec.files.is_empty() {
                baselines.push((rec.app_name.clone(), current));
            } else if current != rec.files {
                updatable.push(rec.app_name.clone());
            }
        }
        if !baselines.is_empty() {
            let mut st = self.state.write();
            for (app, fp) in baselines {
                if let Some(r) = st.installed.get_mut(&app) {
                    r.files = fp;
                }
            }
            self.save_installed(&st)?;
        }
        Ok(updatable)
    }

    async fn install(
        &self,
        app_name: &str,
        request: &InstallRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        if !self.enabled() {
            bail!("ZOOM Platform support is turned off in the settings");
        }
        let game = self
            .game(app_name)
            .with_context(|| format!("{app_name} is not in your ZOOM Platform library"))?;
        let base = if request.path.as_os_str().is_empty() {
            ember_core::paths::expand_home(&(self.settings)().default_install_path)
        } else {
            ember_core::paths::expand_home(&request.path.to_string_lossy())
        };
        let folder = game
            .folder_name
            .clone()
            .filter(|f| !f.trim().is_empty())
            .unwrap_or_else(|| game.title.clone());
        let install_path = base.join(ember_core::paths::sanitize(folder.trim()));
        self.run_pipeline(
            app_name,
            install_path,
            request.platform,
            request.language.clone(),
            Mode::Install,
            &ctx,
        )
        .await
    }

    async fn update(
        &self,
        app_name: &str,
        request: &UpdateRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        let rec = self
            .installed_record(app_name)
            .context("This ZOOM Platform game is not installed")?;
        let language = request.language.clone().or(rec.language.clone());
        self.run_pipeline(
            app_name,
            PathBuf::from(&rec.install_path),
            rec.build_platform,
            language,
            Mode::Update,
            &ctx,
        )
        .await
    }

    async fn repair(&self, app_name: &str, ctx: OpContext) -> Result<Outcome> {
        let rec = self
            .installed_record(app_name)
            .context("This ZOOM Platform game is not installed")?;
        self.run_pipeline(
            app_name,
            PathBuf::from(&rec.install_path),
            rec.build_platform,
            rec.language.clone(),
            Mode::Repair,
            &ctx,
        )
        .await
    }

    async fn uninstall(&self, app_name: &str, args: RemoveArgs) -> Result<()> {
        let removed = {
            let mut st = self.state.write();
            let rec = st.installed.remove(app_name);
            if rec.is_some() {
                self.save_installed(&st)?;
                Self::publish(&mut st, app_name);
            }
            rec
        };
        // Also covers cancelled installs, which have no record yet.
        self.discard_pending(app_name, args.delete_files).await?;
        if let Some(rec) = removed {
            if args.delete_files {
                self.remove_dir_if_safe(Path::new(&rec.install_path)).await;
            }
            self.bus.emit(Event::LibraryChanged(Runner::Zoom));
        }
        Ok(())
    }

    async fn import(&self, app_name: &str, path: &Path, platform: InstallPlatform) -> Result<()> {
        if !path.is_dir() {
            bail!("{} is not a folder", path.display());
        }
        if !matches!(platform, InstallPlatform::Windows | InstallPlatform::Linux) {
            bail!("ZOOM Platform games can be imported as Windows or Linux installs only");
        }
        let title = self
            .game(app_name)
            .map(|g| g.title.clone())
            .unwrap_or_else(|| app_name.to_string());
        let root = path.to_path_buf();
        let t = title.clone();
        let detected = tokio::task::spawn_blocking(move || {
            install::detect_installed(&root, &t, platform, &[])
        })
        .await
        .context("scanning the game folder")?;
        let size = {
            let root = path.to_path_buf();
            tokio::task::spawn_blocking(move || detect::dir_size(&root))
                .await
                .unwrap_or(0)
        };
        let record = detected.into_record(app_name, path, platform, size);
        let missing = record.executable.is_empty();
        self.put_record(record)?;
        if missing {
            self.notice_missing_executable(app_name, &title);
        }
        Ok(())
    }

    async fn set_install_path(&self, app_name: &str, path: &Path) -> Result<()> {
        let mut rec = self
            .installed_record(app_name)
            .context("This ZOOM Platform game is not installed")?;
        rec.install_path = path.to_string_lossy().into_owned();
        self.put_record(rec)
    }

    async fn set_version_pinned(&self, app_name: &str, pinned: bool) -> Result<()> {
        let mut rec = self
            .installed_record(app_name)
            .context("This ZOOM Platform game is not installed")?;
        rec.pinned_version = pinned;
        self.put_record(rec)
    }

    async fn is_available(&self, app_name: &str) -> bool {
        self.installed_record(app_name)
            .is_some_and(|r| Path::new(&r.install_path).is_dir())
    }

    async fn launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        _ctx: OpContext,
    ) -> Result<LaunchPlan> {
        let rec = self
            .installed_record(app_name)
            .context("This ZOOM Platform game is not installed")?;
        let title = self
            .game(app_name)
            .map(|g| g.title.clone())
            .unwrap_or_else(|| app_name.to_string());
        let dosbox = if rec.is_dosbox {
            tokio::task::spawn_blocking(detect::system_dosbox)
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        launch::build_plan(&rec, &title, request, dosbox.as_deref())
    }
}

#[cfg(test)]
mod tests;
