//! The ROM library ([`Runner::Emulation`]): console and retro games in the
//! folders the user chose (`iroriRomFolders`), played with emulators from
//! `irori-engines`.
//!
//! There is no account. The library is `<store config>/library.json`;
//! [`Store::refresh`] rescans the folders (incrementally: unchanged files are
//! not looked at again) and a file watcher rescans after changes while Irori
//! runs. Removing a game from the library never deletes its files; it is
//! remembered so a rescan doesn't bring it back.
//!
//! Launching picks the game's emulator (its own choice, else the system's
//! default, else the first recommended one installed), installs a missing
//! RetroArch core on the way, checks the system's BIOS files and hands back
//! the emulator's command for the services layer to run like any game.

mod names;
mod scan;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use irori_core::{
    Event, EventBus, ExtraInfo, GameInfo, HostPlatform, InstallInfo, InstallInfoQuery,
    InstallPlatform, InstallRequest, InstalledInfo, LaunchPlan, LaunchRequest, LoginFlow, Notice,
    NoticeLevel, OpContext, Outcome, Paths, RemoveArgs, Runner, SettingsFn, Store, UpdateRequest,
    UserInfo, json_store,
};
use irori_engines::launch::{GameFile, LaunchInput};
use irori_engines::{
    BiosMissing, EngineKind, EngineMissing, EngineRef, EngineUnavailable, Host, bios, systems,
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

pub use names::{RomName, parse as parse_name};
pub use scan::{RomEntry, dos_program};

/// How long the watcher waits for a burst of changes to settle.
const SETTLE: Duration = Duration::from_secs(2);
/// Availability checks give up on hung network mounts after this.
const AVAILABILITY_TIMEOUT: Duration = Duration::from_secs(3);
/// The `/dev/udmabuf` warning is shown once per run.
static UDMABUF_WARNED: AtomicBool = AtomicBool::new(false);

const NOT_INSTALLABLE: &str =
    "Games in your ROM folders are not installed by Irori; add the folder that holds them";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct LibraryFile {
    entries: Vec<RomEntry>,
    /// Games added one by one (Add game, Lutris import), kept across scans.
    added: Vec<RomEntry>,
    /// Files and folders the user removed from the library.
    removed: BTreeSet<PathBuf>,
}

#[derive(Default)]
struct State {
    entries: Vec<RomEntry>,
    added: Vec<RomEntry>,
    removed: BTreeSet<PathBuf>,
    games: Vec<Arc<GameInfo>>,
}

/// The ROM library store.
pub struct EmulationStore {
    me: Weak<EmulationStore>,
    bus: EventBus,
    settings: SettingsFn,
    library_file: PathBuf,
    cache_file: PathBuf,
    playlists: PathBuf,
    state: RwLock<State>,
    /// One scan at a time.
    scan_lock: tokio::sync::Mutex<()>,
    scanning: AtomicBool,
    watcher: Mutex<Option<(Vec<PathBuf>, notify::RecommendedWatcher)>>,
    runtime: Mutex<Option<tokio::runtime::Handle>>,
}

impl EmulationStore {
    pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self> {
        let paths = Paths::get();
        Self::with_dirs(
            bus,
            settings,
            paths.store_config_dir(Runner::Emulation),
            paths.store_cache_dir(Runner::Emulation),
        )
    }

    /// A store keeping its files in the given folders (tests).
    pub fn with_dirs(
        bus: EventBus,
        settings: SettingsFn,
        config_dir: PathBuf,
        cache_dir: PathBuf,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            bus,
            settings,
            library_file: config_dir.join("library.json"),
            cache_file: cache_dir.join("scan-cache.json"),
            playlists: cache_dir.join("playlists"),
            state: RwLock::new(State::default()),
            scan_lock: tokio::sync::Mutex::new(()),
            scanning: AtomicBool::new(false),
            watcher: Mutex::new(None),
            runtime: Mutex::new(None),
        })
    }

    /// Whether a scan is running.
    pub fn is_scanning(&self) -> bool {
        self.scanning.load(Ordering::Relaxed)
    }

    /// The library entry of `app_name`.
    pub fn entry(&self, app_name: &str) -> Option<RomEntry> {
        let s = self.state.read();
        s.entries
            .iter()
            .chain(&s.added)
            .find(|e| e.app_name == app_name)
            .cloned()
    }

    /// Add one game outside the ROM folders: a ROM, disc image or program,
    /// or a DOS or ScummVM game folder. `system` is detected from the file
    /// when not given. Adding a game that is already there returns its id.
    pub async fn add_game(
        &self,
        path: &Path,
        system: Option<&str>,
        title: Option<String>,
    ) -> Result<irori_core::GameId> {
        let meta = tokio::fs::metadata(path)
            .await
            .with_context(|| format!("{} was not found", path.display()))?;
        let system = match system {
            Some(id) => {
                systems::get(id).with_context(|| format!("Irori does not know the system {id}"))?
            }
            None if meta.is_dir() => bail!("Choose the system of the game in {}", path.display()),
            None => {
                let p = path.to_path_buf();
                tokio::task::spawn_blocking(move || systems::detect(&p, None))
                    .await
                    .context("detecting the system stopped")?
                    .with_context(|| {
                        format!("Irori can’t tell which system {} is for", path.display())
                    })?
            }
        };
        let main = if meta.is_dir() && system.id == "dos" {
            let dir = path.to_path_buf();
            tokio::task::spawn_blocking(move || scan::dos_program(&dir))
                .await
                .context("looking for the program stopped")?
                .with_context(|| format!("{} holds no DOS program", path.display()))?
        } else {
            path.to_path_buf()
        };
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = names::parse(&stem);
        let entry = RomEntry {
            app_name: scan::app_name_of(&system.id, path),
            system: system.id.clone(),
            title: title.filter(|t| !t.trim().is_empty()).unwrap_or(name.title),
            tags: name.tags,
            path: main,
            discs: Vec::new(),
            size: if meta.is_file() { meta.len() } else { 0 },
            stem,
            root: path.parent().map(Path::to_path_buf).unwrap_or_default(),
        };
        let id = irori_core::GameId::new(Runner::Emulation, entry.app_name.as_str());
        let (entries, mut added, mut removed) = {
            let s = self.state.read();
            (s.entries.clone(), s.added.clone(), s.removed.clone())
        };
        removed.remove(&entry.path);
        if !entries
            .iter()
            .chain(&added)
            .any(|e| e.app_name == entry.app_name)
        {
            added.push(entry);
        }
        self.publish(entries, added, removed);
        self.save().await?;
        Ok(id)
    }

    fn folders(&self) -> Vec<PathBuf> {
        (self.settings)()
            .rom_folders
            .iter()
            .filter(|f| !f.trim().is_empty())
            .map(|f| irori_core::paths::expand_home(f))
            .collect()
    }

    fn publish(&self, entries: Vec<RomEntry>, added: Vec<RomEntry>, removed: BTreeSet<PathBuf>) {
        let mut seen = std::collections::HashSet::new();
        let games = entries
            .iter()
            .chain(&added)
            .filter(|e| !removed.contains(&e.path) && seen.insert(e.app_name.clone()))
            .map(|e| Arc::new(game_info(e)))
            .collect();
        *self.state.write() = State {
            entries,
            added,
            removed,
            games,
        };
        self.bus.emit(Event::LibraryChanged(Runner::Emulation));
    }

    async fn save(&self) -> Result<()> {
        let file = {
            let s = self.state.read();
            LibraryFile {
                entries: s.entries.clone(),
                added: s.added.clone(),
                removed: s.removed.clone(),
            }
        };
        let path = self.library_file.clone();
        tokio::task::spawn_blocking(move || json_store::write(&path, &file))
            .await
            .context("saving the library stopped")?
            .context("Could not save the ROM library")
    }

    /// Scan the ROM folders again and publish the result.
    async fn rescan(&self) -> Result<()> {
        let _one = self.scan_lock.lock().await;
        self.scanning.store(true, Ordering::Relaxed);
        let folders = self.folders();
        let cache_file = self.cache_file.clone();
        let playlists = self.playlists.clone();
        let result = tokio::task::spawn_blocking(move || {
            let previous: scan::ScanCache = json_store::read_or_default(&cache_file);
            let scanned = scan::scan(&folders, &previous, &playlists, &|_| {});
            if let Err(e) = json_store::write(&cache_file, &scanned.cache) {
                tracing::warn!("saving the ROM scan cache: {e:#}");
            }
            scanned
        })
        .await;
        self.scanning.store(false, Ordering::Relaxed);
        let scanned = result.context("scanning stopped")?;
        tracing::info!(
            "ROM library: {} games, {} files not recognised",
            scanned.entries.len(),
            scanned.unrecognized
        );
        let (added, removed) = {
            let s = self.state.read();
            (s.added.clone(), s.removed.clone())
        };
        self.publish(scanned.entries, added, removed);
        self.save().await?;
        self.watch();
        Ok(())
    }

    /// Watch the ROM folders and rescan after changes settle. Re-armed when
    /// the folders change.
    fn watch(&self) {
        use notify::{RecursiveMode, Watcher as _};
        let folders = self.folders();
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
        let mut watcher =
            match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if event.is_ok_and(|e| !e.kind.is_access()) {
                    let _ = tx.send(());
                }
            }) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!("watching ROM folders: {e}");
                    return;
                }
            };
        for folder in &folders {
            if let Err(e) = watcher.watch(folder, RecursiveMode::Recursive) {
                tracing::warn!("watching {}: {e}", folder.display());
            }
        }
        let me = self.me.clone();
        runtime.spawn(async move {
            while rx.recv().await.is_some() {
                // Let a copy of a whole folder finish before rescanning.
                loop {
                    match tokio::time::timeout(SETTLE, rx.recv()).await {
                        Ok(Some(())) => continue,
                        Ok(None) => return,
                        Err(_) => break,
                    }
                }
                let Some(store) = me.upgrade() else { return };
                if let Err(e) = store.rescan().await {
                    tracing::warn!("rescanning ROM folders: {e:#}");
                }
            }
        });
        *slot = Some((folders, watcher));
    }

    /// The emulator `settings` chose for a game of `system`, else the
    /// system's default. Blocking.
    fn choice_for(&self, system: &str, chosen: &str) -> Option<EngineRef> {
        if let Some(c) = EngineRef::parse(chosen) {
            return Some(c);
        }
        let app = (self.settings)();
        let state = irori_engines::state::load();
        irori_engines::default_choice(
            system,
            app.system_engines.get(system).map(String::as_str),
            |c| irori_engines::plays_now(c, &state),
        )
    }
}

/// What the library shows for an entry.
fn game_info(e: &RomEntry) -> GameInfo {
    let native = HostPlatform::current().native();
    // ScummVM games are their folder; everything else is a file in one.
    let install_dir = if e.system == "scummvm" {
        e.path.clone()
    } else {
        e.discs
            .first()
            .unwrap_or(&e.path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    };
    let system = systems::get(&e.system);
    GameInfo {
        runner: Some(Runner::Emulation),
        app_name: e.app_name.clone(),
        title: e.title.clone(),
        art_cover: system
            .filter(|s| !s.libretro.is_empty())
            .and_then(|s| thumbnail_url(&s.libretro, &e.stem)),
        is_installed: true,
        install: Some(InstalledInfo {
            install_path: install_dir.display().to_string(),
            executable: e.path.display().to_string(),
            install_size: String::new(),
            install_size_bytes: e.size,
            platform: native,
            ..Default::default()
        }),
        installable: false,
        can_run_offline: true,
        is_linux_native: native == InstallPlatform::Linux,
        is_mac_native: native == InstallPlatform::Mac,
        platforms: vec![native],
        system: Some(e.system.clone()),
        tags: e.tags.clone(),
        ..Default::default()
    }
    .finalize()
}

/// The libretro-thumbnails box art of a No-Intro/Redump named file. Fetched
/// on demand by the art pipeline, like any cover; a miss shows the
/// generated cover.
fn thumbnail_url(system: &str, stem: &str) -> Option<String> {
    let mut url = url::Url::parse("https://thumbnails.libretro.com/").ok()?;
    url.path_segments_mut()
        .ok()?
        .push(system)
        .push("Named_Boxarts")
        .push(&format!("{}.png", names::thumbnail_name(stem)));
    Some(url.to_string())
}

#[async_trait]
impl Store for EmulationStore {
    fn runner(&self) -> Runner {
        Runner::Emulation
    }

    async fn init(&self) -> Result<()> {
        *self.runtime.lock() = tokio::runtime::Handle::try_current().ok();
        let path = self.library_file.clone();
        let file: LibraryFile =
            tokio::task::spawn_blocking(move || json_store::read_or_default(&path))
                .await
                .context("loading the ROM library stopped")?;
        self.publish(file.entries, file.added, file.removed);
        self.watch();
        Ok(())
    }

    fn user(&self) -> Option<UserInfo> {
        Some(UserInfo {
            account_id: "local".into(),
            display_name: "Local".into(),
        })
    }

    fn login_flow(&self) -> LoginFlow {
        LoginFlow::None
    }

    async fn login(&self, _input: &str) -> Result<UserInfo> {
        self.user().context("no account")
    }

    async fn logout(&self) -> Result<()> {
        Ok(())
    }

    /// Rescan the ROM folders.
    async fn refresh(&self, _full: bool) -> Result<()> {
        self.rescan().await
    }

    fn games(&self) -> Vec<Arc<GameInfo>> {
        self.state.read().games.clone()
    }

    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        self.state
            .read()
            .games
            .iter()
            .find(|g| g.app_name == app_name)
            .cloned()
    }

    async fn extra_info(&self, app_name: &str) -> Result<ExtraInfo> {
        let entry = self
            .entry(app_name)
            .context("This game is not in the library")?;
        // The page shows the system; ROM files carry nothing more.
        let _ = entry;
        Ok(ExtraInfo::default())
    }

    async fn install_info(
        &self,
        _app_name: &str,
        _platform: InstallPlatform,
        _query: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        bail!(NOT_INSTALLABLE)
    }

    async fn updatable_games(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn install(
        &self,
        _app_name: &str,
        _request: &InstallRequest,
        _ctx: OpContext,
    ) -> Result<Outcome> {
        Ok(Outcome::Error(NOT_INSTALLABLE.into()))
    }

    async fn update(
        &self,
        _app_name: &str,
        _request: &UpdateRequest,
        _ctx: OpContext,
    ) -> Result<Outcome> {
        Ok(Outcome::Error(NOT_INSTALLABLE.into()))
    }

    async fn repair(&self, _app_name: &str, _ctx: OpContext) -> Result<Outcome> {
        Ok(Outcome::Error(NOT_INSTALLABLE.into()))
    }

    /// Remove the game from the library. Its files are never deleted, and a
    /// rescan doesn't add it back.
    async fn uninstall(&self, app_name: &str, _args: RemoveArgs) -> Result<()> {
        let entry = self
            .entry(app_name)
            .context("This game is not in the library")?;
        let (entries, mut added, mut removed) = {
            let s = self.state.read();
            (s.entries.clone(), s.added.clone(), s.removed.clone())
        };
        // A game added by hand is simply forgotten; a scanned one is
        // remembered as removed so the next scan doesn't bring it back.
        if added.iter().any(|e| e.app_name == app_name) {
            added.retain(|e| e.app_name != app_name);
        } else {
            removed.insert(entry.path);
        }
        self.publish(entries, added, removed);
        self.save().await
    }

    async fn import(
        &self,
        _app_name: &str,
        _path: &Path,
        _platform: InstallPlatform,
    ) -> Result<()> {
        bail!(NOT_INSTALLABLE)
    }

    async fn set_install_path(&self, _app_name: &str, _path: &Path) -> Result<()> {
        bail!(NOT_INSTALLABLE)
    }

    async fn is_available(&self, app_name: &str) -> bool {
        let Some(entry) = self.entry(app_name) else {
            return false;
        };
        matches!(
            tokio::time::timeout(AVAILABILITY_TIMEOUT, tokio::fs::try_exists(&entry.path)).await,
            Ok(Ok(true))
        )
    }

    async fn launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        ctx: OpContext,
    ) -> Result<LaunchPlan> {
        let entry = self
            .entry(app_name)
            .context("This game is not in the library")?;
        let system = systems::get(&entry.system)
            .with_context(|| format!("Irori does not know the system {}", entry.system))?;
        let settings = request.settings.clone();

        // Checked first: no emulator download helps a game without its BIOS.
        let app = (self.settings)();
        let bios_root = (!app.bios_folder.trim().is_empty())
            .then(|| irori_core::paths::expand_home(&app.bios_folder));
        let sys_id = entry.system.clone();
        let index = tokio::task::spawn_blocking(move || -> Result<bios::BiosIndex> {
            let mut index = bios::cached_index();
            if let Some(root) = &bios_root
                && &index.root != root
            {
                index = bios::scan(root, &index)?;
            }
            let system = systems::get(&sys_id).context("unknown system")?;
            BiosMissing::check(system, &index)?;
            Ok(index)
        })
        .await
        .context("checking BIOS files stopped")??;

        let me = self.me.upgrade().context("the store is shutting down")?;
        let (sys, chosen) = (entry.system.clone(), settings.engine.clone());
        let choice = tokio::task::spawn_blocking(move || me.choice_for(&sys, &chosen))
            .await
            .context("choosing the emulator stopped")?
            .with_context(|| format!("No emulator plays {} games", system.name))?;
        let manifest = irori_engines::engine(&choice.engine)
            .with_context(|| format!("Irori does not know the emulator {}", choice.engine))?;

        // RetroArch cores are small: fetch a missing one on the way.
        if let Some(core) = &choice.core
            && irori_engines::retroarch::installed_core(core).is_none()
        {
            (ctx.step)("Installing the emulator core");
            let installed = irori_engines::retroarch::install_core(core, &ctx).await;
            if !matches!(installed, Ok(Some(_))) {
                let missing = EngineMissing {
                    engine: manifest.id.clone(),
                    name: manifest.name.clone(),
                    core: Some(core.clone()),
                    pinned: None,
                };
                return Err(match installed {
                    Err(e) => anyhow::Error::new(missing).context(format!("{e:#}")),
                    _ => missing.into(),
                });
            }
            self.bus.emit(Event::EnginesChanged);
        }

        let (m, game_path, sys_id, values_set) = (
            manifest.clone(),
            entry.path.clone(),
            entry.system.clone(),
            settings
                .engine_options
                .get(&manifest.id)
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
        );
        let (pin, own_settings, core) = (
            settings.engine_version.clone(),
            settings.engine_own_settings,
            choice.core.clone(),
        );
        let game = GameFile {
            path: game_path,
            system: sys_id,
        };
        let m2 = m.clone();
        let installation = tokio::task::spawn_blocking(move || -> Result<_> {
            let state = irori_engines::state::load();
            let installs = irori_engines::discovery::find(&m2, &state);
            let found = irori_engines::launch::resolve(
                &m2,
                &installs,
                Some(pin.as_str()),
                state.preferred.get(&m2.id).map(String::as_str),
            );
            match found {
                Ok(installation) => Ok(installation.clone()),
                // Offering to install an engine Irori can't download here
                // would only fail: say why instead.
                Err(missing) if missing.pinned.is_none() => {
                    Err(match EngineUnavailable::no_build(&m2, Host::current()) {
                        Some(unavailable) => unavailable.into(),
                        None => missing.into(),
                    })
                }
                Err(missing) => Err(missing.into()),
            }
        })
        .await
        .context("finding the emulator stopped")??;

        if manifest.kind == EngineKind::Relinker {
            if irori_engines::relink::udmabuf_denied()
                && !UDMABUF_WARNED.swap(true, Ordering::Relaxed)
            {
                self.bus.notice(
                    Notice::new(
                        NoticeLevel::Warning,
                        format!(
                            "{}: this account can't use /dev/udmabuf, so games may run slowly. Add yourself to the group that owns it (often kvm) and sign in again.",
                            manifest.name
                        ),
                    )
                    .keys(
                        "shell.notice.udmabuf",
                        None,
                        &[("engine", &manifest.name)],
                    ),
                );
            }
            if irori_engines::relink::prepare(&manifest, &installation, &game, &ctx)
                .await?
                .is_none()
            {
                bail!("The game's conversion was stopped");
            }
        }

        let first_time_hotkey = tokio::task::spawn_blocking(move || -> Result<_> {
            let values = m.option_values(&values_set);
            let command = irori_engines::launch::command(&LaunchInput {
                manifest: &m,
                installation: &installation,
                game: &game,
                core: core.as_deref(),
                values: &values,
                bios: &index,
                own_settings,
            })?;
            let first = irori_engines::state::update(|s| s.hotkey_shown.insert(m.id.clone()))
                .unwrap_or(false);
            Ok((command, first))
        })
        .await
        .context("preparing the emulator stopped")??;
        let (command, first_time) = first_time_hotkey;
        if first_time && !manifest.exit_hotkey.is_empty() {
            self.bus.notice(
                Notice::new(
                    NoticeLevel::Info,
                    format!(
                        "{}: to leave a game, {}",
                        manifest.name, manifest.exit_hotkey
                    ),
                )
                .keys(
                    "shell.notice.exit_hotkey",
                    None,
                    &[("engine", &manifest.name), ("keys", &manifest.exit_hotkey)],
                ),
            );
        }
        let env = &request.environment;
        Ok(LaunchPlan {
            command: command
                .envs(env.env.clone())
                .args(request.extra_args.clone())
                .wrapped(&env.wrappers),
            track_wineserver: false,
            watch: None,
        })
    }
}

#[cfg(test)]
mod tests;
