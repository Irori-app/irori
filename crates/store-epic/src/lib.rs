//! Epic Games Store backend, built on the native [`ember_epic_api`] client
//! and the [`ember_depot`] download engine.
//!
//! State lives in `Paths::store_config_dir(Runner::Legendary)` (see
//! [`state`]): the signed-in session, the account's apps, the install
//! records and the installed builds' manifests. API responses are cached in
//! `Paths::store_cache_dir(Runner::Legendary)`.

mod achievements;
mod cache;
mod deploy;
mod egl_sync;
mod extra;
mod heroic_import;
mod install_info;
mod launch;
mod metadata;
mod overlay;
mod playtime;
mod run;
mod saves;
mod state;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use ember_core::process::{self, CommandSpec, ExecResult, RunOptions, split_args};
use ember_core::{
    Achievement, Event, EventBus, ExtraInfo, GameInfo, InstallInfo, InstallInfoQuery,
    InstallPlatform, InstallProgress, InstallRequest, LaunchOption, LaunchPlan, LaunchRequest,
    LoginFlow, Notice, NoticeLevel, OpContext, Outcome, Paths, RemoveArgs, Runner, SdlOption,
    SessionExpired, SettingsFn, Store, SyncDirection, UpdateRequest, UserInfo, WineLaunch, http,
    util,
};
use ember_epic_api::library::GameRecord;
use ember_epic_api::{Client, Endpoints, Identity, Manifest, ManifestInfo, Session, auth};
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use tokio::io::AsyncWriteExt as _;

use crate::cache::TimedCache;
use crate::install_info::BuildSummary;
use crate::metadata::CatalogEntry;
use crate::state::{InstalledGame, InstalledMap, Layout, real_prefix, remove_quietly};

pub use overlay::{EOS_OVERLAY_APP_NAME, EosOverlayStatus};

const SIX_HOURS: Duration = Duration::from_secs(6 * 3600);
const SEVEN_DAYS: Duration = Duration::from_secs(7 * 24 * 3600);
const STATUS_TTL: Duration = Duration::from_secs(60);
/// Manifests fetched for the install dialog are reused by the install.
const BUILD_TTL: Duration = Duration::from_secs(10 * 60);
const SETUP_MARKER: &str = ".ember-legendary-setup.json";
const EA_INSTALLER: (&str, &str) = (
    "EAappInstaller.exe",
    "https://origin-a.akamaihd.net/EA-Desktop-Client-Download/installer-releases/EAappInstaller.exe",
);
const UBISOFT_INSTALLER: (&str, &str) = (
    "UbisoftConnectInstaller.exe",
    "https://static3.cdn.ubi.com/orbit/launcher_installer/UbisoftConnectInstaller.exe",
);
/// Ubisoft Connect inside a prefix's `drive_c`.
const UBISOFT_CONNECT_EXE: &str =
    r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\UbisoftConnect.exe";
const OVERRIDES_URL: &str = "https://heroic.legendary.gl/v1/version.json";
const SDL_URL: &str = "https://heroic.legendary.gl/v1/sdl";
const UMU_API: &str = "https://umu.openwinecomponents.org/umu_api.php";

/// A build's manifest as downloaded.
pub(crate) struct FetchedBuild {
    pub info: ManifestInfo,
    pub bytes: Vec<u8>,
    pub manifest: Manifest,
}

#[derive(Default)]
struct State {
    records: HashMap<String, Arc<GameRecord>>,
    catalog: HashMap<String, Arc<CatalogEntry>>,
    installed: InstalledMap,
    games: HashMap<String, Arc<GameInfo>>,
}

impl State {
    fn set_records(&mut self, list: Vec<GameRecord>) {
        self.catalog = list
            .iter()
            .filter_map(metadata::entry_of)
            .map(|e| (e.game.app_name.clone(), Arc::new(e)))
            .collect();
        self.records = list
            .into_iter()
            .map(|r| (r.app_name.clone(), Arc::new(r)))
            .collect();
    }

    /// Publish fresh `GameInfo`s from the catalogue and install state.
    fn recompose(&mut self) {
        let owned: HashSet<String> = self.records.keys().cloned().collect();
        let assets = metadata::assets_of(self.records.values().map(|r| r.as_ref()));
        self.games = self
            .catalog
            .iter()
            .map(|(k, e)| {
                let g = metadata::compose(e, &self.installed, &owned, &assets);
                (k.clone(), Arc::new(g))
            })
            .collect();
    }
}

/// The Epic Games Store. Construct with [`EpicStore::new`]; the EOS overlay
/// and EGL sync functions are exposed for the settings UI.
pub struct EpicStore {
    bus: EventBus,
    settings: SettingsFn,
    layout: Layout,
    weak: Weak<EpicStore>,
    endpoints: Endpoints,
    state: RwLock<State>,
    user: RwLock<Option<UserInfo>>,
    /// The session, locked while a token refresh is in flight.
    session: tokio::sync::Mutex<Option<Session>>,
    client: Mutex<Option<Client>>,
    install_info: TimedCache,
    extra_info: TimedCache,
    overrides: TimedCache,
    umu: TimedCache,
    builds: Mutex<HashMap<String, (Instant, Arc<FetchedBuild>)>>,
    service_status: Mutex<Option<(Instant, bool)>>,
    list_lock: tokio::sync::Mutex<()>,
    /// Serialises read-modify-write of the install records.
    files_lock: tokio::sync::Mutex<()>,
    /// Serialises the playtime upload queue.
    playtime_lock: tokio::sync::Mutex<()>,
}

impl EpicStore {
    /// Create the store using Ember's standard directories.
    pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self> {
        let paths = Paths::get();
        Self::with_layout(
            bus,
            settings,
            Layout {
                config: paths.store_config_dir(Runner::Legendary),
                cache: paths.store_cache_dir(Runner::Legendary),
                tools: paths.tools_dir(),
            },
            Endpoints::default(),
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
            Layout {
                config,
                cache,
                tools: Paths::get().tools_dir(),
            },
            Endpoints::default(),
        )
    }

    fn with_layout(
        bus: EventBus,
        settings: SettingsFn,
        layout: Layout,
        endpoints: Endpoints,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            bus,
            settings,
            install_info: TimedCache::new(layout.install_info_cache(), Some(SIX_HOURS)),
            extra_info: TimedCache::new(layout.extra_info_cache(), Some(SIX_HOURS)),
            overrides: TimedCache::new(layout.overrides_cache(), Some(SEVEN_DAYS)),
            umu: TimedCache::new(layout.umu_cache(), Some(SIX_HOURS)),
            layout,
            weak: weak.clone(),
            endpoints,
            state: RwLock::new(State::default()),
            user: RwLock::new(None),
            session: tokio::sync::Mutex::new(None),
            client: Mutex::new(None),
            builds: Mutex::new(HashMap::new()),
            service_status: Mutex::new(None),
            list_lock: tokio::sync::Mutex::new(()),
            files_lock: tokio::sync::Mutex::new(()),
            playtime_lock: tokio::sync::Mutex::new(()),
        })
    }

    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
        Ok(tokio::task::spawn_blocking(f).await?)
    }

    // ---- clients and sessions -----------------------------------------

    /// The API client, with the launcher identity the overrides service
    /// publishes (its user agent and label; sign-in always uses the
    /// built-in client so tokens stay valid).
    async fn client(&self) -> Result<Client> {
        if let Some(c) = self.client.lock().clone() {
            return Ok(c);
        }
        let mut identity = Identity::default();
        if let Some(Value::Object(mut egl)) = self.egl_config().await {
            egl.remove("client_id");
            egl.remove("client_secret");
            identity.apply_overrides(&Value::Object(egl));
        }
        let mut client = Client::with_endpoints(identity, self.endpoints.clone())?;
        let language = (self.settings)().ui_language.clone();
        if let Some((lang, country)) = language.split_once(['-', '_']) {
            client.language = lang.to_string();
            client.country = country.to_uppercase();
        } else if !language.is_empty() {
            client.language = language;
        }
        *self.client.lock() = Some(client.clone());
        Ok(client)
    }

    /// A valid session, refreshing the access token when it is about to
    /// expire.
    async fn session(&self) -> Result<Session> {
        let mut guard = self.session.lock().await;
        let session = guard
            .clone()
            .context("You are not signed in to Epic Games")?;
        if !session.needs_refresh(util::unix_now()) {
            return Ok(session);
        }
        let fresh = match self.client().await?.refresh(&session).await {
            Ok(s) => s,
            Err(e) => {
                // The launcher tells the user once and offers to log in again.
                if let Some(rejected) = e.downcast_ref::<ember_epic_api::SignInRejected>() {
                    return Err(SessionExpired::new(Runner::Legendary, rejected.0.clone()).into());
                }
                return Err(e.context("Your Epic Games session could not be renewed"));
            }
        };
        let layout = self.layout.clone();
        let saved = fresh.clone();
        Self::blocking(move || state::write_session(&layout, &saved)).await??;
        *guard = Some(fresh.clone());
        Ok(fresh)
    }

    /// Hands out fresh sessions to long downloads (chunk signing).
    fn session_source(&self) -> ember_epic_api::install::SessionSource {
        let weak = self.weak.clone();
        Arc::new(move || {
            let weak = weak.clone();
            Box::pin(async move {
                let this = weak.upgrade().context("the Epic store was closed")?;
                this.session().await
            })
        })
    }

    // ---- state ---------------------------------------------------------

    /// Load the session, library and install state from disk.
    async fn load(&self) -> Result<()> {
        let layout = self.layout.clone();
        let (session, records, installed) = Self::blocking(move || {
            (
                state::read_session(&layout),
                state::read_library(&layout),
                state::read_installed(&layout),
            )
        })
        .await
        .context("loading the Epic library")?;
        *self.user.write() = session.as_ref().map(|s| UserInfo {
            account_id: s.account_id.clone(),
            display_name: s.display_name.clone(),
        });
        let logged_in = session.is_some();
        *self.session.lock().await = session;
        let mut state = self.state.write();
        state.set_records(if logged_in { records } else { Vec::new() });
        state.installed = installed;
        state.recompose();
        Ok(())
    }

    /// Re-read the install records and publish them.
    async fn reload_installed(&self) {
        let layout = self.layout.clone();
        let Ok(installed) = Self::blocking(move || state::read_installed(&layout)).await else {
            return;
        };
        {
            let mut state = self.state.write();
            state.installed = installed;
            state.recompose();
        }
        self.bus.emit(Event::LibraryChanged(Runner::Legendary));
    }

    /// Change the install records, persist them and publish the result.
    async fn update_installed(&self, change: impl FnOnce(&mut InstalledMap)) -> Result<()> {
        let _guard = self.files_lock.lock().await;
        let mut installed = self.state.read().installed.clone();
        change(&mut installed);
        let layout = self.layout.clone();
        let snapshot = installed.clone();
        Self::blocking(move || state::write_installed(&layout, &snapshot)).await??;
        {
            let mut state = self.state.write();
            state.installed = installed;
            state.recompose();
        }
        self.bus.emit(Event::LibraryChanged(Runner::Legendary));
        Ok(())
    }

    fn installed_entry(&self, app: &str) -> Option<InstalledGame> {
        self.state.read().installed.get(app).cloned()
    }

    fn record(&self, app: &str) -> Option<Arc<GameRecord>> {
        self.state.read().records.get(app).cloned()
    }

    fn require_record(&self, app: &str) -> Result<Arc<GameRecord>> {
        self.record(app)
            .with_context(|| format!("{app} is not in your Epic Games library"))
    }

    fn require_game(&self, app: &str) -> Result<Arc<GameInfo>> {
        self.game(app)
            .with_context(|| format!("{app} is not in your Epic Games library"))
    }

    fn require_installed(&self, app: &str) -> Result<(Arc<GameInfo>, InstalledGame)> {
        let game = self.require_game(app)?;
        let installed = self
            .installed_entry(app)
            .with_context(|| format!("{} is not installed", game.title))?;
        Ok((game, installed))
    }

    /// Records of the DLCs of `game`.
    fn dlc_records(&self, game: &GameRecord) -> Vec<Arc<GameRecord>> {
        let id = game.catalog_item_id();
        self.state
            .read()
            .records
            .values()
            .filter(|r| !id.is_empty() && r.main_game_id() == Some(id))
            .cloned()
            .collect()
    }

    // ---- network helpers ---------------------------------------------

    /// Whether Epic's status page reports a major store outage (cached for a
    /// minute). Network failures count as online.
    pub async fn epic_service_offline(&self) -> bool {
        let cached = *self.service_status.lock();
        if let Some((at, offline)) = cached
            && at.elapsed() < STATUS_TTL
        {
            return offline;
        }
        let offline = extra::fetch_service_offline().await;
        *self.service_status.lock() = Some((Instant::now(), offline));
        if offline {
            self.bus.notice(
                Notice::new(NoticeLevel::Warning, "Epic Games Store offline")
                    .message("Epic reports a major outage; some features may not work."),
            );
        }
        offline
    }

    /// legendary's version service: `game_overrides` and `egl_config`,
    /// cached for a week.
    async fn version_info(&self) -> Option<Value> {
        if let Some(v) = self.overrides.get::<Value>("versionInfo").await {
            return Some(v);
        }
        let v: Value = match http::get_json(OVERRIDES_URL).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("fetching legendary game overrides: {e:#}");
                return None;
            }
        };
        self.overrides.set("versionInfo", &v).await;
        Some(v)
    }

    async fn game_overrides(&self) -> Option<Value> {
        self.version_info().await?.get("game_overrides").cloned()
    }

    async fn egl_config(&self) -> Option<Value> {
        self.version_info().await?.get("egl_config").cloned()
    }

    /// An executable to use instead of the manifest's for games whose
    /// launcher does not work under Wine (legendary's workarounds).
    async fn executable_override(&self, app: &str) -> Option<String> {
        let os = if cfg!(windows) {
            "win32"
        } else if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        };
        let builtin = (app.eq_ignore_ascii_case("kinglet") && os != "win32")
            .then(|| "Base/Binaries/Win64EOS/CivilizationVI.exe".to_string());
        self.game_overrides()
            .await
            .and_then(|o| {
                o.get("executable_override")?
                    .get(app.to_lowercase())?
                    .get(os)?
                    .as_str()
                    .map(str::to_string)
            })
            .or(builtin)
    }

    /// Selective-download options of games Epic splits into components.
    async fn sdl_options(&self, app: &str) -> Vec<SdlOption> {
        let has_sdl = self.game_overrides().await.is_some_and(|o| {
            o.pointer(&format!("/sdl_config/{app}"))
                .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
        });
        if !has_sdl {
            return Vec::new();
        }
        let resp = match http::client()
            .get(format!("{SDL_URL}/{app}.json"))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r,
            Ok(_) => return Vec::new(),
            Err(e) => {
                tracing::warn!("fetching SDL definitions of {app}: {e}");
                return Vec::new();
            }
        };
        let is_json = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("application/json"));
        if !is_json {
            return Vec::new();
        }
        match resp.text().await {
            Ok(text) => install_info::parse_sdl(&text),
            Err(_) => Vec::new(),
        }
    }

    /// The current build of `app` on `platform` (cached briefly).
    async fn fetch_build(&self, app: &str, platform: &str) -> Result<Arc<FetchedBuild>> {
        let key = format!("{app}:{platform}");
        if let Some((at, b)) = self.builds.lock().get(&key)
            && at.elapsed() < BUILD_TTL
        {
            return Ok(b.clone());
        }
        let record = self.require_record(app)?;
        let asset = record
            .asset_infos
            .get(platform)
            .with_context(|| format!("{} has no {platform} build", record.app_title))?;
        let session = self.session().await?;
        let client = self.client().await?;
        let info = client
            .manifest_info(
                &session,
                &asset.namespace,
                &asset.catalog_item_id,
                app,
                platform,
            )
            .await?;
        let bytes = client.download_manifest(&info).await?;
        let secrets = info.secrets.clone();
        let parse_bytes = bytes.clone();
        let manifest = Self::blocking(move || Manifest::parse(&parse_bytes, &secrets)).await??;
        let build = Arc::new(FetchedBuild {
            info,
            bytes,
            manifest,
        });
        self.builds
            .lock()
            .insert(key, (Instant::now(), build.clone()));
        Ok(build)
    }

    /// The manifest of the installed build, if Ember has it.
    async fn installed_manifest(&self, installed: &InstalledGame) -> Option<Manifest> {
        let path = self.layout.build_manifest(&installed.app_name);
        let secrets = installed.secrets();
        Self::blocking(move || -> Option<Manifest> {
            let bytes = std::fs::read(path).ok()?;
            Manifest::parse(&bytes, &secrets)
                .map_err(|e| tracing::warn!("stored Epic manifest: {e:#}"))
                .ok()
        })
        .await
        .ok()
        .flatten()
    }

    /// Download the EA app / Ubisoft Connect installer if missing.
    async fn ensure_redist(&self, (file, url): (&str, &str), ctx: &OpContext) -> Result<PathBuf> {
        let dir = self.layout.redist_dir();
        let path = dir.join(file);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(path);
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
        (ctx.step)(&format!("Downloading {file}"));
        let mut resp = http::client()
            .get(url)
            .timeout(Duration::from_secs(600))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("downloading {file}"))?;
        let total = resp.content_length();
        let tmp = path.with_extension("part");
        let mut out = tokio::fs::File::create(&tmp).await?;
        let mut done = 0u64;
        loop {
            tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    drop(out);
                    let _ = tokio::fs::remove_file(&tmp).await;
                    bail!("canceled");
                }
                chunk = resp.chunk() => match chunk.with_context(|| format!("downloading {file}"))? {
                    Some(bytes) => {
                        out.write_all(&bytes).await?;
                        done += bytes.len() as u64;
                        (ctx.progress)(InstallProgress {
                            percent: total.filter(|t| *t > 0).map(|t| done as f32 * 100.0 / t as f32),
                            bytes_done: Some(done),
                            bytes_total: total,
                            ..Default::default()
                        });
                    }
                    None => break,
                }
            }
        }
        out.flush().await?;
        drop(out);
        tokio::fs::rename(&tmp, &path).await?;
        Ok(path)
    }

    /// umu database id of an Epic game (cached, including misses).
    async fn umu_id(&self, app: &str) -> Option<String> {
        if let Some(cached) = self.umu.get::<Option<String>>(app).await {
            return cached;
        }
        let resp = http::client()
            .get(UMU_API)
            .query(&[("codename", app.to_lowercase().as_str()), ("store", "egs")])
            .send()
            .await
            .ok()?;
        let v: Value = resp.json().await.ok()?;
        let id = v
            .get(0)
            .and_then(|e| e.get("umu_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        self.umu.set(app, &id).await;
        id
    }

    // ---- install helpers ---------------------------------------------

    async fn install_third_party(
        &self,
        game: &GameInfo,
        platform: &str,
        ctx: &OpContext,
    ) -> Result<Outcome> {
        let installer = if game.is_ea_managed {
            EA_INSTALLER
        } else if game.is_ubisoft_managed {
            UBISOFT_INSTALLER
        } else {
            let app = game.third_party_managed_app.clone().unwrap_or_default();
            return Ok(Outcome::Error(format!(
                "Third party app {app} not supported"
            )));
        };
        if let Err(e) = self.ensure_redist(installer, ctx).await {
            if ctx.cancel.is_cancelled() {
                return Ok(Outcome::Abort);
            }
            return Ok(Outcome::Error(format!("{e:#}")));
        }
        let layout = self.layout.clone();
        let (app, platform) = (game.app_name.clone(), platform.to_string());
        Self::blocking(move || state::add_third_party(&layout, &app, &platform)).await??;
        self.reload_installed().await;
        Ok(Outcome::Done)
    }

    // ---- launch helpers -----------------------------------------------

    async fn run_windows(
        &self,
        wine: Option<&WineLaunch>,
        env: &BTreeMap<String, String>,
        argv: Vec<String>,
        cwd: Option<&Path>,
        log: &Path,
    ) -> Result<ExecResult> {
        anyhow::ensure!(
            wine.is_some() || cfg!(windows),
            "Windows programs require Wine on this host"
        );
        let mut parts = wine.map(launch::setup_exec_prefix).unwrap_or_default();
        parts.extend(argv);
        let program = parts.remove(0);
        let mut spec = CommandSpec::new(program).args(parts).envs(env.clone());
        if let Some(dir) = cwd {
            spec = spec.cwd(dir);
        }
        let result = process::run(&spec, RunOptions::default().log_file(log)).await?;
        anyhow::ensure!(
            result.success() || matches!(result.code, Some(3010 | 1641)),
            "Windows setup failed: {}",
            result.error_summary()
        );
        Ok(result)
    }

    /// Epic's one-time prefix setup for a game: the launcher URL handler,
    /// the game's prerequisites, EA app / Ubisoft Connect, and the EOS
    /// overlay. Failures are logged; the launch continues. Ember records
    /// done games in its own marker file inside the prefix.
    async fn first_launch_setup(
        &self,
        game: &GameInfo,
        installed: &InstalledGame,
        req: &LaunchRequest,
        wine: Option<&WineLaunch>,
        ctx: &OpContext,
    ) {
        let marker = wine
            .map(|w| w.prefix.join(SETUP_MARKER))
            .unwrap_or_else(|| Path::new(&installed.install_path).join(SETUP_MARKER));
        let mut done: Vec<String> = tokio::fs::read(&marker)
            .await
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if done.iter().any(|a| a == &game.app_name) {
            return;
        }
        (ctx.step)("Installing Epic dependencies");
        let env = &req.environment.env;
        let log = req.log_file.as_path();
        let reg = vec![
            "reg".to_string(),
            "add".to_string(),
            r"HKEY_CLASSES_ROOT\com.epicgames.launcher".to_string(),
            "/f".to_string(),
        ];
        if wine.is_some()
            && let Err(e) = self.run_windows(wine, env, reg, None, log).await
        {
            tracing::warn!("registering the Epic URL handler: {e:#}");
        }

        let third_party = game.is_ea_managed || game.is_ubisoft_managed;
        if !third_party
            && !installed.install_path.is_empty()
            && let Some(p) = installed.prereq.as_ref().filter(|p| !p.path.is_empty())
        {
            (ctx.step)(&format!("Installing {}", p.name));
            let install_dir = Path::new(&installed.install_path);
            let exe = install_dir.join(p.path.replace('\\', "/"));
            let mut argv = vec![exe.to_string_lossy().into_owned()];
            argv.extend(split_args(&p.args));
            if let Err(e) = self
                .run_windows(wine, env, argv, Some(install_dir), log)
                .await
            {
                tracing::warn!("running prerequisites of {}: {e:#}", game.app_name);
                return; // Retry incomplete setup on the next launch.
            }
        }

        if third_party {
            let (installer, args): (_, &[&str]) = if game.is_ea_managed {
                (EA_INSTALLER, &["EAX_LAUNCH_CLIENT=0", "IGNORE_INSTALLED=1"])
            } else {
                (UBISOFT_INSTALLER, &["/S"])
            };
            match self.ensure_redist(installer, ctx).await {
                Ok(path) => {
                    (ctx.step)(&format!("Running {}", installer.0));
                    let mut argv = vec![path.to_string_lossy().into_owned()];
                    argv.extend(args.iter().map(|s| s.to_string()));
                    if let Err(e) = self.run_windows(wine, env, argv, None, log).await {
                        tracing::warn!("running {}: {e:#}", installer.0);
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!("downloading {}: {e:#}", installer.0);
                    return;
                }
            }
        }

        if let Some(pfx) = wine.and_then(|w| real_prefix(&w.prefix)) {
            if self.eos_overlay_status().await.installed {
                if !self.eos_overlay_is_enabled(&pfx).await.unwrap_or(false) {
                    tracing::info!("EOS Overlay: Enabling");
                    if let Err(e) = self.eos_overlay_enable(&pfx).await {
                        tracing::warn!("{e:#}");
                    }
                }
            } else {
                tracing::info!("EOS Overlay: Not Installed");
            }
        }

        done.push(game.app_name.clone());
        if let Ok(bytes) = serde_json::to_vec(&done) {
            let _ = tokio::fs::write(&marker, bytes).await;
        }
    }

    /// Copy the fake `EpicGamesLauncher.exe` into the prefix when the user
    /// set `USE_FAKE_EPIC_EXE`; returns its Windows path to start the game
    /// through.
    async fn install_fake_exe(
        &self,
        wine: &WineLaunch,
        env: &BTreeMap<String, String>,
    ) -> Option<&'static str> {
        let value = env
            .get("USE_FAKE_EPIC_EXE")
            .cloned()
            .or_else(|| std::env::var("USE_FAKE_EPIC_EXE").ok());
        if !launch::env_enabled(value.as_deref()) {
            return None;
        }
        let src = launch::find_fake_exe(&Paths::get().bin_dir())?;
        let pfx = real_prefix(&wine.prefix)?;
        let dst = launch::fake_exe_in_prefix(&pfx);
        if !tokio::fs::try_exists(&dst).await.unwrap_or(false) {
            if let Some(dir) = dst.parent() {
                let _ = tokio::fs::create_dir_all(dir).await;
            }
            if let Err(e) = tokio::fs::copy(&src, &dst).await {
                tracing::warn!("copying EpicGamesLauncher.exe into the prefix: {e}");
                return None;
            }
        }
        Some(launch::FAKE_EXE_WIN_PATH)
    }

    /// Start an EA app / Ubisoft Connect title through its launcher in the
    /// game's prefix.
    async fn third_party_plan(
        &self,
        game: &GameInfo,
        record: &GameRecord,
        request: &LaunchRequest,
        env: BTreeMap<String, String>,
        offline: bool,
    ) -> Result<LaunchPlan> {
        let wine = request.environment.wine.as_ref();
        anyhow::ensure!(
            wine.is_some() || cfg!(windows),
            "No Wine runtime is configured for this game"
        );
        let target = if game.is_ea_managed {
            let code = if offline {
                None
            } else {
                let session = self.session().await?;
                Some(self.client().await?.exchange_code(&session).await?)
            };
            let user = self.user().unwrap_or(UserInfo {
                account_id: String::new(),
                display_name: String::new(),
            });
            ember_epic_api::launch::origin_uri(
                &game.app_name,
                code.as_deref(),
                &user.display_name,
                &user.account_id,
                &self.language(request),
                record.additional_command_line(),
            )
        } else {
            UBISOFT_CONNECT_EXE.to_string()
        };
        if wine.is_none() {
            let command = if game.is_ea_managed {
                CommandSpec::new("rundll32.exe")
                    .arg("url.dll,FileProtocolHandler")
                    .arg(target)
            } else {
                let root = std::env::var_os("ProgramFiles(x86)")
                    .context("Program Files directory not found")?;
                CommandSpec::new(
                    PathBuf::from(root).join("Ubisoft/Ubisoft Game Launcher/UbisoftConnect.exe"),
                )
            };
            return Ok(LaunchPlan {
                command: command.envs(env).wrapped(&request.environment.wrappers),
                track_wineserver: false,
            });
        }
        let mut argv = wine.expect("checked above").exec_prefix();
        if game.is_ea_managed {
            argv.extend(["start".to_string(), target]);
        } else {
            argv.push(target);
        }
        let program = argv.remove(0);
        Ok(LaunchPlan {
            command: CommandSpec::new(program)
                .args(argv)
                .envs(env)
                .wrapped(&request.environment.wrappers),
            track_wineserver: true,
        })
    }

    fn language(&self, request: &LaunchRequest) -> String {
        let lang = if request.settings.language.is_empty() {
            (self.settings)().ui_language.clone()
        } else {
            request.settings.language.clone()
        };
        if lang.is_empty() { "en".into() } else { lang }
    }

    // ---- extras for the UI -------------------------------------------

    /// Drop Ember's Epic API caches.
    pub async fn clear_cache(&self) -> Result<()> {
        for cache in [&self.install_info, &self.extra_info, &self.overrides] {
            cache.clear().await;
        }
        self.builds.lock().clear();
        *self.client.lock() = None;
        Ok(())
    }

    /// Delete the files of an install listed by its manifest (and the
    /// folders left empty); without a manifest the whole folder goes.
    async fn delete_install_files(&self, installed: &InstalledGame) -> Result<()> {
        let dir = PathBuf::from(&installed.install_path);
        if installed.install_path.is_empty() || !dir.is_dir() {
            return Ok(());
        }
        let Some(manifest) = self.installed_manifest(installed).await else {
            if installed.is_dlc {
                // A DLC shares its game's folder: never delete it wholesale.
                tracing::warn!("no manifest for {}; leaving its files", installed.app_name);
                return Ok(());
            }
            ember_core::fs_guard::ensure_deletable(&dir, &(self.settings)())?;
            return tokio::fs::remove_dir_all(&dir)
                .await
                .with_context(|| format!("Could not delete {}", dir.display()));
        };
        let tags = installed.install_tags.clone();
        let keep_root = installed.is_dlc;
        Self::blocking(move || deploy::delete_listed(&dir, &manifest, &tags, keep_root)).await?
    }
}

#[async_trait]
impl Store for EpicStore {
    fn runner(&self) -> Runner {
        Runner::Legendary
    }

    async fn init(&self) -> Result<()> {
        self.migrate_legendary_dir().await;
        self.load().await?;
        self.bus.emit(Event::LibraryChanged(Runner::Legendary));
        Ok(())
    }

    fn user(&self) -> Option<UserInfo> {
        self.user.read().clone()
    }

    fn login_flow(&self) -> LoginFlow {
        LoginFlow::Browser {
            url: auth::login_url(&Identity::default().client_id),
            capture: auth::CAPTURE.into(),
            instructions: "Sign in to Epic Games in the browser. When the page shows a block of \
                text containing \"authorizationCode\", copy all of it (or just the code) and \
                paste it here."
                .into(),
        }
    }

    async fn login(&self, input: &str) -> Result<UserInfo> {
        let code = auth::extract_code(input)?;
        tracing::info!("Logging in to Epic Games");
        let session = self
            .client()
            .await?
            .login(&code)
            .await
            .context("Epic Games login failed")?;
        let layout = self.layout.clone();
        let saved = session.clone();
        Self::blocking(move || state::write_session(&layout, &saved)).await??;
        let user = UserInfo {
            account_id: session.account_id.clone(),
            display_name: session.display_name.clone(),
        };
        *self.session.lock().await = Some(session);
        *self.user.write() = Some(user.clone());
        // Show what is known (an earlier library) right away.
        self.load().await?;
        self.bus.emit(Event::AccountChanged(Runner::Legendary));
        self.bus.emit(Event::LibraryChanged(Runner::Legendary));
        Ok(user)
    }

    async fn logout(&self) -> Result<()> {
        if let Some(session) = self.session.lock().await.clone()
            && let Ok(client) = self.client().await
            && let Err(e) = client.kill_session(&session).await
        {
            tracing::warn!("ending the Epic session: {e:#}");
        }
        for file in [self.layout.session_json(), self.layout.library_json()] {
            remove_quietly(&file);
        }
        *self.session.lock().await = None;
        *self.user.write() = None;
        {
            let mut state = self.state.write();
            state.set_records(Vec::new());
            state.recompose();
        }
        if let Err(e) = self.clear_cache().await {
            tracing::warn!("{e:#}");
        }
        self.bus.emit(Event::AccountChanged(Runner::Legendary));
        self.bus.emit(Event::LibraryChanged(Runner::Legendary));
        Ok(())
    }

    async fn refresh(&self, full: bool) -> Result<()> {
        if self.user.read().is_none() {
            return Ok(());
        }
        if full {
            self.install_info.clear().await;
            self.extra_info.clear().await;
        }
        if self.epic_service_offline().await {
            tracing::warn!("Epic is offline right now, cannot refresh game list");
            return Ok(());
        }
        {
            let _guard = self.list_lock.lock().await;
            let session = self.session().await?;
            let client = self.client().await?;
            let (known, mut platforms): (HashMap<String, GameRecord>, BTreeSet<String>) = {
                let state = self.state.read();
                (
                    state
                        .records
                        .iter()
                        .map(|(k, v)| (k.clone(), (**v).clone()))
                        .collect(),
                    state
                        .installed
                        .values()
                        .filter(|g| !g.third_party)
                        .map(|g| g.platform_arg().to_string())
                        .collect(),
                )
            };
            platforms.insert("Windows".into());
            if cfg!(target_os = "macos") {
                platforms.insert("Mac".into());
            }
            let list = ember_epic_api::library::sync(&client, &session, &platforms, &known, full)
                .await
                .context("Refreshing the Epic library failed")?;
            tracing::info!("Epic library: {} apps", list.len());
            let layout = self.layout.clone();
            let saved = list.clone();
            Self::blocking(move || state::write_library(&layout, &saved)).await??;
            let mut state = self.state.write();
            state.set_records(list);
            state.recompose();
        }
        if let Some(prefix) = self.egl_prefix()
            && let Err(e) = self.egl_sync_with(&prefix).await
        {
            tracing::warn!("Epic Games Launcher sync: {e:#}");
        }
        // Sessions played offline go up once Epic is reachable again.
        self.flush_playtime().await;
        self.bus.emit(Event::LibraryChanged(Runner::Legendary));
        Ok(())
    }

    async fn after_exit(&self, app_name: &str, played_secs: u64) -> Result<()> {
        self.record_playtime(app_name, played_secs).await
    }

    async fn remote_playtime(&self) -> Result<HashMap<String, u64>> {
        self.epic_playtime().await
    }

    async fn achievements(&self, app_name: &str, language: &str) -> Result<Vec<Achievement>> {
        self.game_achievements(app_name, language).await
    }

    fn games(&self) -> Vec<Arc<GameInfo>> {
        self.state.read().games.values().cloned().collect()
    }

    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        self.state.read().games.get(app_name).cloned()
    }

    async fn extra_info(&self, app_name: &str) -> Result<ExtraInfo> {
        let game = self.require_game(app_name)?;
        let fallback = game.extra.clone().unwrap_or_default();
        let Some(namespace) = game.namespace.clone() else {
            return Ok(fallback);
        };
        if let Some(cached) = self.extra_info.get::<ExtraInfo>(&namespace).await {
            return Ok(cached);
        }
        let language = (self.settings)().ui_language.clone();
        match extra::fetch_extra_info(&namespace, &game.title, &language).await {
            Some(mut info) => {
                if info.description.as_deref().is_none_or(str::is_empty) {
                    info.description = fallback.description;
                }
                self.extra_info.set(&namespace, &info).await;
                Ok(info)
            }
            None => Ok(fallback),
        }
    }

    async fn install_info(
        &self,
        app_name: &str,
        platform: InstallPlatform,
        _query: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        let game = self.require_game(app_name)?;
        if game.third_party_managed_app.is_some() {
            return Ok(InstallInfo {
                app_name: app_name.to_string(),
                title: game.title.clone(),
                platform,
                third_party_managed_app: game.third_party_managed_app.clone(),
                ..Default::default()
            });
        }
        let record = self.require_record(app_name)?;
        let platform_arg = match self.installed_entry(app_name) {
            Some(i) if i.install_platform() == platform && !i.platform.is_empty() => i.platform,
            _ => deploy::platform_arg(platform)?.to_string(),
        };
        let key = format!("{app_name}_{platform_arg}");
        let summary = match self.install_info.get::<BuildSummary>(&key).await {
            Some(s) => s,
            None => {
                let build = self.fetch_build(app_name, &platform_arg).await?;
                let s = BuildSummary::of(&build.manifest);
                self.install_info.set(&key, &s).await;
                s
            }
        };
        let sdl = self.sdl_options(app_name).await;
        let resumable = tokio::fs::try_exists(self.layout.resume_file(app_name))
            .await
            .unwrap_or(false);
        let dlcs = {
            let state = self.state.read();
            install_info::owned_dlcs(
                &record,
                state.records.values().map(|r| r.as_ref()),
                &platform_arg,
                &state.installed,
            )
        };
        Ok(install_info::to_install_info(
            &summary, &record, platform, dlcs, sdl, resumable,
        ))
    }

    async fn updatable_games(&self) -> Result<Vec<String>> {
        if self.user.read().is_none() || self.epic_service_offline().await {
            return Ok(Vec::new());
        }
        if let Err(e) = self.refresh(false).await {
            tracing::warn!("Checking for Epic game updates failed: {e:#}");
            return Ok(Vec::new());
        }
        let state = self.state.read();
        let mut out: Vec<String> = state
            .installed
            .values()
            .filter(|g| !g.third_party && !g.pinned_version)
            .filter(|g| {
                state
                    .records
                    .get(&g.app_name)
                    .and_then(|r| r.version(g.platform_arg()))
                    .is_some_and(|v| v != g.version)
            })
            .map(|g| g.app_name.clone())
            .collect();
        out.sort();
        Ok(out)
    }

    async fn install(
        &self,
        app_name: &str,
        request: &InstallRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        let game = self.require_game(app_name)?;
        if self.epic_service_offline().await {
            return Ok(Outcome::Error(
                "Epic Servers are having major outage right now, the game cannot be installed!"
                    .into(),
            ));
        }
        let platform = deploy::platform_arg(request.platform)?;
        if game.third_party_managed_app.is_some() {
            return self.install_third_party(&game, platform, &ctx).await;
        }
        let base = if request.path.as_os_str().is_empty() {
            PathBuf::from(&(self.settings)().default_install_path)
        } else {
            request.path.clone()
        };
        let tags: Vec<String> = request
            .sdl
            .iter()
            .filter(|t| !t.is_empty())
            .cloned()
            .collect();
        let mut outcome = self
            .deploy(
                app_name,
                platform,
                deploy::Mode::Install {
                    base: base.clone(),
                    tags,
                },
                &ctx,
            )
            .await?;
        if outcome.is_done() {
            for dlc in request.dlcs.iter().filter(|d| d.as_str() != app_name) {
                (ctx.step)(&format!("Installing DLC {dlc}"));
                outcome = self
                    .deploy(
                        dlc,
                        platform,
                        deploy::Mode::Install {
                            base: base.clone(),
                            tags: Vec::new(),
                        },
                        &ctx,
                    )
                    .await?;
                if !outcome.is_done() {
                    break;
                }
            }
        }
        Ok(outcome)
    }

    async fn update(
        &self,
        app_name: &str,
        _request: &UpdateRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        let (_, installed) = self.require_installed(app_name)?;
        if self.epic_service_offline().await {
            return Ok(Outcome::Error(
                "Epic Servers are having major outage right now, the game cannot be updated!"
                    .into(),
            ));
        }
        self.deploy(
            app_name,
            installed.platform_arg(),
            deploy::Mode::Update,
            &ctx,
        )
        .await
    }

    async fn repair(&self, app_name: &str, ctx: OpContext) -> Result<Outcome> {
        let (_, installed) = self.require_installed(app_name)?;
        self.deploy(
            app_name,
            installed.platform_arg(),
            deploy::Mode::Repair,
            &ctx,
        )
        .await
    }

    async fn uninstall(&self, app_name: &str, args: RemoveArgs) -> Result<()> {
        let installed = self
            .installed_entry(app_name)
            .with_context(|| format!("{app_name} is not installed"))?;
        if installed.third_party {
            let layout = self.layout.clone();
            let app = app_name.to_string();
            Self::blocking(move || state::remove_third_party(&layout, &app)).await??;
            self.reload_installed().await;
            return Ok(());
        }
        if !installed.egl_guid.is_empty()
            && let Some(prefix) = self.egl_prefix()
        {
            egl_sync::forget_export(&prefix, &installed, args.delete_files);
        }
        if args.delete_files {
            self.delete_install_files(&installed)
                .await
                .context("Uninstalling failed")?;
        }
        remove_quietly(&self.layout.build_manifest(app_name));
        remove_quietly(&self.layout.resume_file(app_name));
        self.update_installed(|map| {
            map.remove(app_name);
        })
        .await
    }

    async fn import(&self, app_name: &str, path: &Path, platform: InstallPlatform) -> Result<()> {
        if self.epic_service_offline().await {
            bail!("Epic Servers are having major outage right now, the game cannot be imported!");
        }
        let platform = deploy::platform_arg(platform)?;
        self.import_one(app_name, path, platform, true)
            .await
            .context("Importing failed")
    }

    async fn set_install_path(&self, app_name: &str, path: &Path) -> Result<()> {
        self.require_installed(app_name)?;
        let want = path.to_string_lossy().into_owned();
        self.update_installed(|map| {
            if let Some(g) = map.get_mut(app_name) {
                g.install_path = want;
            }
        })
        .await
    }

    async fn set_version_pinned(&self, app_name: &str, pinned: bool) -> Result<()> {
        self.require_installed(app_name)?;
        self.update_installed(|map| {
            if let Some(g) = map.get_mut(app_name) {
                g.pinned_version = pinned;
            }
        })
        .await
    }

    async fn is_available(&self, app_name: &str) -> bool {
        match self.installed_entry(app_name) {
            Some(i) if i.third_party => true,
            Some(i) => {
                !i.install_path.is_empty()
                    && tokio::fs::try_exists(&i.install_path)
                        .await
                        .unwrap_or(false)
            }
            None => false,
        }
    }

    fn supports_cloud_saves(&self) -> bool {
        true
    }

    async fn default_save_path(&self, app_name: &str, prefix: Option<&Path>) -> Result<String> {
        self.resolve_save_path(app_name, prefix).await
    }

    async fn sync_saves(
        &self,
        app_name: &str,
        direction: SyncDirection,
        save_path: &str,
        ctx: OpContext,
    ) -> Result<String> {
        self.sync_cloud_saves(app_name, direction, save_path, &ctx)
            .await
    }

    async fn launch_options(&self, app_name: &str) -> Result<Vec<LaunchOption>> {
        let (game, installed) = self.require_installed(app_name)?;
        if game.third_party_managed_app.is_some() || installed.platform.is_empty() {
            return Ok(Vec::new());
        }
        let record = self.require_record(app_name)?;
        let dlcs = self.dlc_records(&record);
        let state = self.state.read();
        Ok(install_info::launch_options(
            &record,
            dlcs.iter().map(|d| d.as_ref()),
            &state.installed,
        ))
    }

    async fn launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        ctx: OpContext,
    ) -> Result<LaunchPlan> {
        self.launch_plan(app_name, request, &ctx).await
    }
}

/// Carry a Heroic user's Epic login and installs over into Ember's format:
/// converts Heroic's `legendaryConfig/legendary` directory. An existing
/// Ember login is kept; library records and installs are merged.
pub fn import_from_heroic(heroic_config_dir: &Path) -> Result<()> {
    let src = heroic_config_dir.join("legendaryConfig").join("legendary");
    let paths = Paths::get();
    let layout = Layout {
        config: paths.store_config_dir(Runner::Legendary),
        cache: paths.store_cache_dir(Runner::Legendary),
        tools: paths.tools_dir(),
    };
    let imported = heroic_import::import_legendary_dir(&src, &layout)
        .context("importing the Epic Games login and installs from Heroic")?;
    tracing::info!(
        "Imported from Heroic: Epic login {}, {} installs, {} library entries",
        imported.login,
        imported.installs,
        imported.records
    );
    Ok(())
}

#[cfg(test)]
mod tests;
