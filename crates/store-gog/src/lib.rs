//! GOG store backend.
//!
//! Downloads, updates and repairs use [`ember_gog_api`] and the
//! [`ember_depot`] engine; cloud saves talk to GOG's cloud storage; the
//! Galaxy communication service games use for achievements and the overlay
//! runs in-process ([`ember_gog_galaxy`]). Authentication, the catalogue
//! (Galaxy library + GamesDB), update checks, playtime, presence and
//! achievements talk to GOG's web APIs directly.
//!
//! [`GogStore`] owns everything under `Paths::store_config_dir(Runner::Gog)`
//! (tokens, installed games, build records, support files) and
//! `Paths::store_cache_dir(Runner::Gog)` (catalogue, API caches, download
//! journals).

mod api;
mod auth;
mod build;
mod extra;
mod heroic_import;
mod install;
mod installed;
mod launch;
mod library;
mod manifest;
mod migrate;
mod offline;
mod paths;
mod playtime;
mod presence;
mod redist;
mod saves;
mod wine;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Weak};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde::Serialize;
use serde::de::DeserializeOwned;

use ember_core::{
    Achievement, Event, EventBus, ExtraInfo, GameInfo, InstallInfo, InstallInfoQuery,
    InstallPlatform, InstallRequest, LaunchOption, LaunchPlan, LaunchRequest, LoginFlow, OpContext,
    Outcome, RemoveArgs, Runner, SessionExpired, SettingsFn, Store, SyncDirection, UpdateRequest,
    UserInfo, json_store,
};

pub use heroic_import::import_from_heroic;
pub use installed::CyberpunkConfig;
pub use offline::FoundInstaller;
pub use paths::GogPaths;

use auth::{Auth, UserData};
use installed::{InstalledFile, InstalledRecord, PendingInstall};

/// Mutable store state. Entries are shared as `Arc<GameInfo>` and replaced,
/// never mutated in place.
#[derive(Default)]
struct State {
    user: Option<UserData>,
    /// Owned catalogue without install state.
    catalogue: HashMap<String, Arc<GameInfo>>,
    installed: HashMap<String, InstalledRecord>,
    pending: HashMap<String, PendingInstall>,
    /// Catalogue merged with install state: what `games()` returns.
    view: HashMap<String, Arc<GameInfo>>,
}

/// The GOG [`Store`].
pub struct GogStore {
    me: Weak<GogStore>,
    bus: EventBus,
    settings: SettingsFn,
    paths: GogPaths,
    auth: Auth,
    state: RwLock<State>,
    refresh_lock: tokio::sync::Mutex<()>,
    /// Serialises writes of the installed list.
    install_lock: tokio::sync::Mutex<()>,
    presence: presence::Presence,
    playtime_lock: tokio::sync::Mutex<()>,
    /// When GOG's playtime totals were last read (see `gog_totals`).
    totals_read: parking_lot::Mutex<Option<std::time::Instant>>,
    /// Content-system client without a token (see `content()`).
    content: ember_gog_api::content::Client,
    /// The in-process Galaxy service and the games using it.
    galaxy: tokio::sync::Mutex<launch::Galaxy>,
}

impl GogStore {
    /// Create the store using Ember's standard directories.
    pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self> {
        Self::with_paths(bus, settings, GogPaths::from_ember())
    }

    /// The store of one more account, keeping its login, installs and
    /// caches in `config` and `cache`; shared tools stay in Ember's
    /// directories.
    pub fn for_account(
        bus: EventBus,
        settings: SettingsFn,
        config: std::path::PathBuf,
        cache: std::path::PathBuf,
    ) -> Arc<Self> {
        let paths = GogPaths {
            config,
            cache,
            ..GogPaths::from_ember()
        };
        Self::with_paths(bus, settings, paths)
    }

    /// Create the store with an explicit directory layout.
    pub fn with_paths(bus: EventBus, settings: SettingsFn, paths: GogPaths) -> Arc<Self> {
        Self::with_endpoints(bus, settings, paths, Default::default())
    }

    /// Create the store against other GOG service addresses (tests).
    pub fn with_endpoints(
        bus: EventBus,
        settings: SettingsFn,
        paths: GogPaths,
        endpoints: ember_gog_api::content::Endpoints,
    ) -> Arc<Self> {
        let content =
            ember_gog_api::content::Client::new(endpoints).expect("the HTTP client can be built");
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            bus,
            settings,
            auth: Auth::new(paths.auth_file()),
            paths,
            state: RwLock::new(State::default()),
            refresh_lock: tokio::sync::Mutex::new(()),
            install_lock: tokio::sync::Mutex::new(()),
            presence: presence::Presence::default(),
            playtime_lock: tokio::sync::Mutex::new(()),
            totals_read: parking_lot::Mutex::new(None),
            content,
            galaxy: Default::default(),
        })
    }

    /// Directory layout of this store.
    pub fn paths(&self) -> &GogPaths {
        &self.paths
    }

    fn arc(&self) -> Option<Arc<GogStore>> {
        self.me.upgrade()
    }

    fn user_data(&self) -> Option<UserData> {
        self.state.read().user.clone()
    }

    pub(crate) fn record(&self, app_name: &str) -> Option<InstalledRecord> {
        self.state.read().installed.get(app_name).cloned()
    }

    pub(crate) fn installed_record(&self, app_name: &str) -> Result<InstalledRecord> {
        self.record(app_name).with_context(|| {
            format!(
                "{} is not installed",
                self.game(app_name)
                    .map(|g| g.title.clone())
                    .unwrap_or_else(|| app_name.to_string())
            )
        })
    }

    pub(crate) fn title_of(&self, app_name: &str) -> String {
        self.state
            .read()
            .view
            .get(app_name)
            .map(|g| g.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| app_name.to_string())
    }

    /// Rebuild the merged view from catalogue and install state.
    fn rebuild_view(&self) {
        let mut st = self.state.write();
        let view = compose_view(&st.catalogue, &st.installed, &self.paths);
        st.view = view;
    }

    /// Persist the installed list and publish the change.
    pub(crate) async fn save_installed(&self) -> Result<()> {
        let file = {
            let st = self.state.read();
            let mut installed: Vec<InstalledRecord> = st.installed.values().cloned().collect();
            installed.sort_by(|a, b| a.app_name.cmp(&b.app_name));
            InstalledFile { installed }
        };
        save_json(self.paths.installed_file(), file).await?;
        self.rebuild_view();
        self.bus.emit(Event::LibraryChanged(Runner::Gog));
        Ok(())
    }

    /// Insert or replace an install record, persist and publish.
    pub(crate) async fn put_record(&self, rec: InstalledRecord) -> Result<()> {
        let _g = self.install_lock.lock().await;
        self.state
            .write()
            .installed
            .insert(rec.app_name.clone(), rec);
        self.save_installed().await
    }

    /// Change an existing record in place, persist and publish.
    pub(crate) async fn modify_record(
        &self,
        app_name: &str,
        f: impl FnOnce(&mut InstalledRecord),
    ) -> Result<()> {
        let _g = self.install_lock.lock().await;
        {
            let mut st = self.state.write();
            let rec = st
                .installed
                .get_mut(app_name)
                .with_context(|| format!("{app_name} is not installed"))?;
            f(rec);
        }
        self.save_installed().await
    }

    pub(crate) async fn remove_record(&self, app_name: &str) -> Result<Option<InstalledRecord>> {
        let _g = self.install_lock.lock().await;
        let removed = self.state.write().installed.remove(app_name);
        self.save_installed().await?;
        Ok(removed)
    }

    pub(crate) async fn set_pending(&self, app_name: &str, pending: Option<PendingInstall>) {
        let map = {
            let mut st = self.state.write();
            match pending {
                Some(p) => st.pending.insert(app_name.to_string(), p),
                None => st.pending.remove(app_name),
            };
            st.pending.clone()
        };
        if let Err(e) = save_json(self.paths.pending_file(), map).await {
            tracing::warn!("{e:#}");
        }
    }

    /// Private branch password of a game (empty when none).
    pub async fn private_branch_password(&self, app_name: &str) -> String {
        let map: HashMap<String, String> = load_json(self.paths.private_branches_file()).await;
        map.get(app_name).cloned().unwrap_or_default()
    }

    /// Store (or clear, with an empty string) the password of a private branch.
    pub async fn set_private_branch_password(&self, app_name: &str, password: &str) -> Result<()> {
        let path = self.paths.private_branches_file();
        let mut map: HashMap<String, String> = load_json(path.clone()).await;
        if password.is_empty() {
            map.remove(app_name);
        } else {
            map.insert(app_name.to_string(), password.to_string());
        }
        save_json(path, map).await?;
        self.clear_install_info_cache(app_name).await;
        Ok(())
    }

    async fn load_state(&self) {
        let paths = self.paths.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            let installed: InstalledFile = json_store::read_or_default(&paths.installed_file());
            let pending: HashMap<String, PendingInstall> =
                json_store::read_or_default(&paths.pending_file());
            let user: Option<UserData> = json_store::read(&paths.user_file()).ok().flatten();
            let catalogue: Vec<GameInfo> = json_store::read_or_default(&paths.library_cache());
            let logged_in = paths.auth_file().is_file();
            (installed, pending, user, catalogue, logged_in)
        })
        .await;
        let Ok((installed, pending, user, catalogue, logged_in)) = loaded else {
            return;
        };
        let mut st = self.state.write();
        st.installed = installed
            .into_records()
            .into_iter()
            .map(|r| (r.app_name.clone(), r))
            .collect();
        st.pending = pending;
        st.user = user.filter(|_| logged_in);
        st.catalogue = catalogue
            .into_iter()
            .filter(|g| !g.app_name.is_empty())
            .map(|g| (g.app_name.clone(), Arc::new(g.finalize())))
            .collect();
    }

    /// Startup work that needs the network: restore missing manifests, flush
    /// queued playtime, start presence. Runs in the background.
    async fn startup(self: Arc<Self>) {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if self.user_data().is_none() {
            return;
        }
        let apps: Vec<String> = self.state.read().installed.keys().cloned().collect();
        for app in apps {
            if let Err(e) = self.restore_build_record(&app).await {
                tracing::warn!("recording the installed build of {app}: {e:#}");
            }
        }
        self.flush_playtime().await;
        self.presence_tick().await;
        presence::spawn_heartbeat(Arc::downgrade(&self));
    }
}

/// Merge catalogue and install state into what the UI sees.
fn compose_view(
    catalogue: &HashMap<String, Arc<GameInfo>>,
    installed: &HashMap<String, InstalledRecord>,
    paths: &GogPaths,
) -> HashMap<String, Arc<GameInfo>> {
    let installed_dlcs: std::collections::HashSet<&str> = installed
        .values()
        .flat_map(|r| r.installed_dlcs.iter().map(String::as_str))
        .collect();
    let mut view: HashMap<String, Arc<GameInfo>> = HashMap::with_capacity(catalogue.len() + 1);
    for (app, base) in catalogue {
        let rec = installed.get(app);
        let dlc_installed = base.is_dlc && installed_dlcs.contains(app.as_str());
        let any_dlc = base
            .dlcs
            .iter()
            .any(|d| installed_dlcs.contains(d.app_name.as_str()));
        if rec.is_none() && !dlc_installed && !any_dlc {
            view.insert(app.clone(), base.clone());
            continue;
        }
        let mut g = (**base).clone();
        for d in &mut g.dlcs {
            d.is_installed = installed_dlcs.contains(d.app_name.as_str());
        }
        if let Some(r) = rec {
            g.is_installed = true;
            g.install = Some(r.to_info());
        } else if dlc_installed {
            g.is_installed = true;
        }
        view.insert(app.clone(), Arc::new(g));
    }
    for (app, rec) in installed {
        if view.contains_key(app) {
            continue;
        }
        let title = rec
            .rest
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| app.clone());
        let g = GameInfo {
            runner: Some(Runner::Gog),
            app_name: app.clone(),
            title,
            is_installed: true,
            installable: true,
            can_run_offline: true,
            is_linux_native: rec.platform == InstallPlatform::Linux,
            platforms: vec![rec.platform],
            install: Some(rec.to_info()),
            ..Default::default()
        };
        view.insert(app.clone(), Arc::new(g.finalize()));
    }
    view.insert(
        library::REDIST_APP.to_string(),
        Arc::new(library::redist_game_info(&paths.redist)),
    );
    view
}

/// Read a small JSON file off the runtime threads (default on error).
pub(crate) async fn load_json<T: DeserializeOwned + Default + Send + 'static>(
    path: std::path::PathBuf,
) -> T {
    tokio::task::spawn_blocking(move || json_store::read_or_default(&path))
        .await
        .unwrap_or_default()
}

/// Atomically write a small JSON file off the runtime threads.
pub(crate) async fn save_json<T: Serialize + Send + 'static>(
    path: std::path::PathBuf,
    value: T,
) -> Result<()> {
    tokio::task::spawn_blocking(move || json_store::write(&path, &value))
        .await
        .context("background write failed")?
}

/// A login GOG refused, as the launcher's [`SessionExpired`] so it can ask the
/// user to log in again.
fn expired_login(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<ember_gog_api::auth::SignInRejected>() {
        Some(rejected) => SessionExpired::new(Runner::Gog, rejected.to_string()).into(),
        None => e,
    }
}

#[async_trait]
impl Store for GogStore {
    fn runner(&self) -> Runner {
        Runner::Gog
    }

    async fn init(&self) -> Result<()> {
        let paths = self.paths.clone();
        if let Err(e) =
            tokio::task::spawn_blocking(move || migrate::migrate_gogdl_state(&paths)).await?
        {
            tracing::warn!("migrating GOG install data: {e:#}");
        }
        self.load_state().await;
        self.rebuild_view();
        if let Some(me) = self.arc() {
            tokio::spawn(me.startup());
        }
        Ok(())
    }

    fn user(&self) -> Option<UserInfo> {
        self.state.read().user.as_ref().map(|u| UserInfo {
            account_id: u.id.clone(),
            display_name: if u.username.is_empty() {
                "GOG user".into()
            } else {
                u.username.clone()
            },
        })
    }

    fn login_flow(&self) -> LoginFlow {
        LoginFlow::Browser {
            url: auth::LOGIN_URL.into(),
            capture: auth::LOGIN_CAPTURE.into(),
            instructions: "Sign in to GOG. When the page shows a blank screen, copy the full \
                           address from the address bar (it contains \"on_login_success\") and \
                           paste it here."
                .into(),
        }
    }

    async fn login(&self, input: &str) -> Result<UserInfo> {
        let code = auth::extract_login_code(input).context(
            "No GOG login code found. Paste the address of the page shown after signing in",
        )?;
        let creds = self.auth.login_with_code(&code).await?;
        let user = match auth::fetch_user(&creds).await {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!("fetching GOG user details: {e:#}");
                UserData {
                    id: creds.user_id.clone(),
                    ..Default::default()
                }
            }
        };
        save_json(self.paths.user_file(), user.clone()).await?;
        self.state.write().user = Some(user);
        self.bus.emit(Event::AccountChanged(Runner::Gog));
        if let Some(me) = self.arc() {
            tokio::spawn(async move {
                me.flush_playtime().await;
                me.presence_tick().await;
                presence::spawn_heartbeat(Arc::downgrade(&me));
            });
        }
        self.user().context("login failed")
    }

    async fn logout(&self) -> Result<()> {
        self.delete_presence(true).await;
        self.auth.clear().await?;
        let _ = tokio::fs::remove_file(self.paths.user_file()).await;
        let cache = self.paths.cache.clone();
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(cache)).await;
        {
            let mut st = self.state.write();
            st.user = None;
            st.catalogue.clear();
        }
        self.rebuild_view();
        self.bus.emit(Event::AccountChanged(Runner::Gog));
        self.bus.emit(Event::LibraryChanged(Runner::Gog));
        Ok(())
    }

    async fn refresh(&self, full: bool) -> Result<()> {
        self.refresh_library(full).await.map_err(expired_login)?;
        if let Some(me) = self.arc() {
            tokio::spawn(async move { me.flush_playtime().await });
        }
        Ok(())
    }

    fn games(&self) -> Vec<Arc<GameInfo>> {
        self.state.read().view.values().cloned().collect()
    }

    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        self.state.read().view.get(app_name).cloned()
    }

    async fn extra_info(&self, app_name: &str) -> Result<ExtraInfo> {
        self.fetch_extra_info(app_name).await
    }

    async fn install_info(
        &self,
        app_name: &str,
        platform: InstallPlatform,
        query: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        self.get_install_info(app_name, platform, query).await
    }

    async fn updatable_games(&self) -> Result<Vec<String>> {
        self.list_updatable().await.map_err(expired_login)
    }

    async fn install(
        &self,
        app_name: &str,
        request: &InstallRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        self.install_game(app_name, request, ctx).await
    }

    async fn update(
        &self,
        app_name: &str,
        request: &UpdateRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        self.update_game(app_name, request, ctx).await
    }

    async fn repair(&self, app_name: &str, ctx: OpContext) -> Result<Outcome> {
        self.repair_game(app_name, ctx).await
    }

    async fn uninstall(&self, app_name: &str, args: RemoveArgs) -> Result<()> {
        self.uninstall_game(app_name, args).await
    }

    async fn import(&self, app_name: &str, path: &Path, platform: InstallPlatform) -> Result<()> {
        self.import_game(app_name, path, platform).await
    }

    async fn extras(&self, app_name: &str) -> Result<Vec<ember_core::ExtraFile>> {
        self.list_extras(app_name).await
    }

    async fn extra_url(
        &self,
        _app_name: &str,
        file: &ember_core::ExtraFile,
    ) -> Result<(String, String)> {
        self.extra_address(file).await
    }

    async fn set_install_path(&self, app_name: &str, path: &Path) -> Result<()> {
        let p = path.to_string_lossy().into_owned();
        self.modify_record(app_name, |r| r.install_path = p).await
    }

    async fn set_version_pinned(&self, app_name: &str, pinned: bool) -> Result<()> {
        self.modify_record(app_name, |r| r.pinned_version = pinned)
            .await
    }

    fn supports_cloud_saves(&self) -> bool {
        true
    }

    async fn default_save_path(&self, app_name: &str, prefix: Option<&Path>) -> Result<String> {
        self.resolve_default_save_path(app_name, prefix).await
    }

    async fn sync_saves(
        &self,
        app_name: &str,
        direction: SyncDirection,
        save_path: &str,
        ctx: OpContext,
    ) -> Result<String> {
        self.sync_game_saves(app_name, direction, save_path, ctx)
            .await
    }

    async fn launch_options(&self, app_name: &str) -> Result<Vec<LaunchOption>> {
        self.game_launch_options(app_name).await
    }

    async fn launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        ctx: OpContext,
    ) -> Result<LaunchPlan> {
        self.prepare_game_launch(app_name, request, ctx).await
    }

    async fn after_exit(&self, app_name: &str, played_secs: u64) -> Result<()> {
        self.release_galaxy(app_name).await;
        self.presence_set_game(None).await;
        self.record_playtime(app_name, played_secs).await
    }

    async fn remote_playtime(&self) -> Result<HashMap<String, u64>> {
        Ok(self.gog_totals().await)
    }

    async fn achievements(&self, app_name: &str, language: &str) -> Result<Vec<Achievement>> {
        self.game_achievements(app_name, language).await
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod view_tests {
    use super::*;

    #[test]
    fn view_merges_install_state() {
        let mut catalogue = HashMap::new();
        let base = GameInfo {
            runner: Some(Runner::Gog),
            app_name: "1".into(),
            title: "Game".into(),
            dlcs: vec![ember_core::DlcInfo {
                app_name: "2".into(),
                title: "DLC".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let other = GameInfo {
            app_name: "3".into(),
            ..Default::default()
        };
        catalogue.insert("1".to_string(), Arc::new(base));
        let other = Arc::new(other);
        catalogue.insert("3".to_string(), other.clone());
        let mut installed = HashMap::new();
        installed.insert(
            "1".to_string(),
            InstalledRecord {
                app_name: "1".into(),
                install_path: "/g".into(),
                installed_dlcs: vec!["2".into()],
                ..Default::default()
            },
        );
        installed.insert(
            "9".to_string(),
            InstalledRecord {
                app_name: "9".into(),
                platform: InstallPlatform::Linux,
                ..Default::default()
            },
        );
        let view = compose_view(&catalogue, &installed, &GogPaths::under(Path::new("/x")));
        let g = &view["1"];
        assert!(g.is_installed && g.dlcs[0].is_installed);
        assert_eq!(g.install.as_ref().unwrap().install_path, "/g");
        assert!(Arc::ptr_eq(&view["3"], &other));
        assert!(view["9"].is_installed && view["9"].is_linux_native);
        assert!(view[library::REDIST_APP].is_dlc);
    }
}
