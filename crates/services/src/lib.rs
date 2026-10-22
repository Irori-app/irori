//! Ember's backend facade. The UI talks only to [`Backend`]; stores, the Wine
//! runtime and the filesystem sit behind it.
//!
//! Threading: the backend owns a small Tokio runtime. Async methods must run on
//! it — UI code calls [`Backend::spawn`] and awaits the returned handle from its
//! own executor (Tokio join handles are executor-agnostic). Synchronous
//! accessors (`library`, `settings`, `status`…) are lock-protected snapshots and
//! safe to call from render.

pub mod backup;
mod discord;
mod downloads;
mod facts;
mod files;
mod games;
mod hooks;
mod inhibit;
mod library;
pub mod sessions;
pub mod settings;
pub mod status;
pub mod userdata;

use std::collections::BTreeMap;
use std::future::Future;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ember_core::{DynStore, Event, EventBus, Runner};
use parking_lot::{Mutex, RwLock};
use tokio::runtime::Runtime;
use tokio::sync::{Notify, broadcast};

pub use backup::{Backup, BackupParts, ExportFormat, RestoreSummary};
pub use downloads::{DownloadItem, DownloadKind, DownloadQueueSnapshot, DownloadState};
pub use files::{FileDownload, FileState, key_of as file_download_key};
pub use games::{LaunchParams, RunningGame};
pub use library::{LibraryFilter, LibrarySnapshot, LibrarySort, StoreFilter};
pub use sessions::Session;
pub use settings::SettingsService;
pub use status::StatusRegistry;
pub use userdata::{UserData, UserDataService};

/// How often connectivity is re-checked.
const CONNECTIVITY_INTERVAL: Duration = Duration::from_secs(60);

/// Shared handle to the backend. Cheap to clone.
#[derive(Clone)]
pub struct Backend(Arc<Inner>);

pub struct Inner {
    rt: &'static Runtime,
    pub bus: EventBus,
    pub settings: Arc<SettingsService>,
    pub userdata: UserDataService,
    facts: facts::FactsService,
    pub statuses: StatusRegistry,
    stores: BTreeMap<Runner, DynStore>,
    library: RwLock<Arc<LibrarySnapshot>>,
    downloads: Mutex<downloads::QueueState>,
    downloads_wake: Notify,
    files: Mutex<files::FilesState>,
    files_wake: Notify,
    running: Mutex<BTreeMap<ember_core::GameId, games::RunningGame>>,
    updates: RwLock<Arc<std::collections::HashSet<ember_core::GameId>>>,
    online: AtomicBool,
    /// The queue was paused by [`Backend::pause_downloads_while_offline`].
    auto_paused: AtomicBool,
    presence: discord::Presence,
}

impl Deref for Backend {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl Backend {
    /// Build the backend. `make_stores` receives the event bus and a settings
    /// accessor and returns the store backends to use. Loads settings and cached
    /// state synchronously; store initialisation and the first library build
    /// run in the background (watch for [`Event::LibraryUpdated`]).
    pub fn start(
        make_stores: impl FnOnce(EventBus, ember_core::SettingsFn) -> Vec<DynStore>,
    ) -> anyhow::Result<Self> {
        Self::start_with(make_stores, true)
    }

    /// Like [`Backend::start`] for one-shot command-line use: no startup
    /// refresh, compatibility-data update or periodic checks.
    pub fn start_headless(
        make_stores: impl FnOnce(EventBus, ember_core::SettingsFn) -> Vec<DynStore>,
    ) -> anyhow::Result<Self> {
        Self::start_with(make_stores, false)
    }

    fn start_with(
        make_stores: impl FnOnce(EventBus, ember_core::SettingsFn) -> Vec<DynStore>,
        interactive: bool,
    ) -> anyhow::Result<Self> {
        let rt: &'static Runtime = Box::leak(Box::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(3)
                .max_blocking_threads(16)
                .thread_name("ember-io")
                .enable_all()
                .build()?,
        ));
        let _guard = rt.enter();
        let bus = EventBus::new();
        let settings = Arc::new(SettingsService::load(bus.clone()));
        let settings_fn: ember_core::SettingsFn = {
            let settings = settings.clone();
            Arc::new(move || settings.app())
        };
        let stores = make_stores(bus.clone(), settings_fn);
        let backend = Backend(Arc::new(Inner {
            rt,
            settings,
            userdata: UserDataService::load(bus.clone()),
            facts: facts::FactsService::load(),
            statuses: StatusRegistry::new(bus.clone()),
            stores: stores.into_iter().map(|s| (s.runner(), s)).collect(),
            library: RwLock::new(Arc::new(LibrarySnapshot::default())),
            downloads: Mutex::new(downloads::QueueState::load()),
            downloads_wake: Notify::new(),
            files: Mutex::new(files::FilesState::default()),
            files_wake: Notify::new(),
            running: Mutex::new(BTreeMap::new()),
            updates: RwLock::new(Arc::default()),
            online: AtomicBool::new(true),
            auto_paused: AtomicBool::new(false),
            presence: discord::Presence::default(),
            bus,
        }));
        ember_runtime::set_hooks(Arc::new(hooks::KnownFixHooks {
            backend: backend.clone(),
        }));
        backend.spawn_background_tasks(interactive);
        Ok(backend)
    }

    /// Run a future on the backend runtime.
    pub fn spawn<F>(&self, fut: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.rt.spawn(fut)
    }

    /// Run blocking work (file scans, image decoding) on the blocking pool.
    pub fn spawn_blocking<F, R>(&self, f: F) -> tokio::task::JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        self.rt.spawn_blocking(f)
    }

    pub fn runtime(&self) -> &'static Runtime {
        self.rt
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.bus.subscribe()
    }

    pub fn store(&self, runner: Runner) -> Option<DynStore> {
        self.stores.get(&runner).cloned()
    }

    pub fn stores(&self) -> impl Iterator<Item = &DynStore> {
        self.stores.values()
    }

    pub fn is_online(&self) -> bool {
        self.online.load(Ordering::Relaxed)
    }

    fn spawn_background_tasks(&self, interactive: bool) {
        // Initialise stores from disk, then build the library.
        let b = self.clone();
        self.spawn(async move {
            let inits = b.stores.values().map(|s| {
                let s = s.clone();
                async move {
                    if let Err(e) = s.init().await {
                        tracing::error!("initialising {}: {e:#}", s.runner().store_name());
                    }
                }
            });
            futures::future::join_all(inits).await;
            // Items restored from the persisted queue are waiting again.
            for item in b.downloads().active {
                b.statuses.set(&item.id, ember_core::StatusKind::Queued);
            }
            b.check_connectivity().await;
            b.rebuild_library();
            b.downloads_wake.notify_one();
            if !interactive {
                return;
            }
            b.refresh_compat_data().await;
            if b.settings.app().check_for_updates_on_startup {
                b.refresh_all(false).await;
            }
        });

        // Rebuild the aggregated library when stores or user data change.
        let b = self.clone();
        self.spawn(async move {
            let mut rx = b.subscribe();
            loop {
                match rx.recv().await {
                    Ok(Event::LibraryChanged(_) | Event::GameChanged(_)) => {
                        // Coalesce bursts (a refresh touches many games).
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        while rx.try_recv().is_ok() {}
                        b.rebuild_library();
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => b.rebuild_library(),
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // File downloads (offline installers, goodies).
        let b = self.clone();
        self.spawn(async move { b.file_worker().await });

        // Download queue worker.
        let b = self.clone();
        self.spawn(async move { b.download_worker().await });

        // Periodic update checks and connectivity.
        if interactive {
            let b = self.clone();
            self.spawn(async move { b.periodic_tasks().await });
        }
    }

    async fn periodic_tasks(&self) {
        let mut since_refresh = Duration::ZERO;
        loop {
            // Connectivity is cheap to probe; library refreshes follow the
            // user's interval.
            tokio::time::sleep(CONNECTIVITY_INTERVAL).await;
            since_refresh += CONNECTIVITY_INTERVAL;
            let was_online = self.is_online();
            let online = self.check_connectivity().await;
            self.pause_downloads_while_offline(was_online, online);
            let interval =
                Duration::from_secs(self.settings.app().check_updates_interval.max(5) as u64 * 60);
            let reconnected = online && !was_online;
            if online && (since_refresh >= interval || reconnected) {
                since_refresh = Duration::ZERO;
                self.refresh_all(false).await;
                if self.settings.app().auto_update_games {
                    self.enqueue_all_updates().await;
                }
            }
        }
    }

    /// Update the anticheat and known-fixes databases when their published
    /// checksums change.
    pub async fn refresh_compat_data(&self) {
        match ember_extras::releases::fetch().await {
            Ok(info) => {
                if let Err(e) =
                    ember_extras::anticheat::refresh(Some(&info.anticheat_files.sha_linux)).await
                {
                    tracing::warn!("anticheat data: {e:#}");
                }
                if let Err(e) =
                    ember_extras::known_fixes::refresh(Some(&info.known_fixes_sha)).await
                {
                    tracing::warn!("known fixes data: {e:#}");
                }
            }
            Err(e) => tracing::info!("release data unavailable: {e:#}"),
        }
    }

    /// Pause the queue when the connection drops and resume it when it
    /// returns, unless the user paused it themselves.
    fn pause_downloads_while_offline(&self, was_online: bool, online: bool) {
        if was_online && !online && !self.downloads().paused && self.downloads().current().is_some()
        {
            self.auto_paused.store(true, Ordering::Relaxed);
            self.pause_downloads();
            self.bus.notice(
                ember_core::Notice::new(ember_core::NoticeLevel::Warning, "Downloads paused")
                    .message("You're offline. Downloads resume when the connection is back.")
                    .keys(
                        "shell.notice.downloads_paused",
                        Some("shell.notice.downloads_paused_message"),
                        &[],
                    ),
            );
        } else if online && !was_online && self.auto_paused.swap(false, Ordering::Relaxed) {
            self.resume_downloads();
        }
    }

    pub async fn check_connectivity(&self) -> bool {
        let ok = ember_core::http::client()
            .head("https://www.gstatic.com/generate_204")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .is_ok();
        if self.online.swap(ok, Ordering::Relaxed) != ok {
            self.bus.emit(Event::Connectivity(ok));
        }
        ok
    }

    /// Refresh every logged-in store's catalogue.
    /// Take stores' own playtime totals (time played in their launchers)
    /// into Ember's.
    async fn sync_store_playtime(&self) {
        for store in self.stores.values().filter(|s| s.is_logged_in()) {
            match store.remote_playtime().await {
                Ok(totals) if !totals.is_empty() => {
                    let grown = self.userdata.merge_store_playtime(store.runner(), &totals);
                    if grown > 0 {
                        tracing::info!(
                            "{} playtime: {grown} games had more time recorded there",
                            store.runner().store_name()
                        );
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("{} playtime: {e:#}", store.runner().store_name()),
            }
        }
    }

    pub async fn refresh_all(&self, full: bool) {
        let jobs = self
            .stores
            .values()
            .filter(|s| s.is_logged_in() || s.runner() == Runner::Sideload)
            .map(|s| {
                let s = s.clone();
                async move {
                    if let Err(e) = s.refresh(full).await {
                        tracing::warn!("refreshing {}: {e:#}", s.runner().store_name());
                    }
                }
            });
        futures::future::join_all(jobs).await;
        self.sync_store_playtime().await;
        self.rebuild_library();
        self.refresh_updates().await;
    }

    /// Games with a pending update (cached; refreshed with the library).
    pub fn updates(&self) -> Arc<std::collections::HashSet<ember_core::GameId>> {
        self.updates.read().clone()
    }

    /// Ask every store which installed games have updates.
    pub async fn refresh_updates(&self) {
        let jobs = self.stores.values().filter(|s| s.is_logged_in()).map(|s| {
            let s = s.clone();
            async move {
                match s.updatable_games().await {
                    Ok(apps) => apps
                        .into_iter()
                        .map(|a| ember_core::GameId::new(s.runner(), a))
                        .collect::<Vec<_>>(),
                    Err(e) => {
                        tracing::warn!("update check for {}: {e:#}", s.runner().store_name());
                        Vec::new()
                    }
                }
            }
        });
        let all: std::collections::HashSet<_> = futures::future::join_all(jobs)
            .await
            .into_iter()
            .flatten()
            .filter(|id| !self.settings.game(id).ignore_game_updates)
            .collect();
        *self.updates.write() = Arc::new(all);
        self.bus
            .emit(Event::LibraryUpdated(self.library().revision));
    }

    pub async fn refresh_store(&self, runner: Runner, full: bool) -> anyhow::Result<()> {
        let store = self
            .store(runner)
            .ok_or_else(|| anyhow::anyhow!("{runner} is not available"))?;
        store.refresh(full).await?;
        self.rebuild_library();
        Ok(())
    }
}
