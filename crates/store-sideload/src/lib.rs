//! Store backend for apps the user adds manually ("sideloaded" apps): native
//! Linux programs, Windows programs run through Wine/Proton, and web apps.
//!
//! There is no account: [`SideloadStore`] always reports a fixed local user so
//! the store counts as available. The library lives in
//! `<store config dir>/library.json`; the UI edits it through
//! [`SideloadStore::add_app`], [`SideloadStore::edit_app`] and
//! [`SideloadStore::remove_app`].
//!
//! Launching: Linux apps run directly with the runtime's wrappers and
//! environment; Windows apps run as `wine exec prefix + exe` with the cwd set
//! to the executable's folder; browser apps open with `xdg-open`, or, when a
//! custom user agent or full screen is requested, in a dedicated app window of
//! an installed Chromium-family browser (`--app`), falling back to Firefox
//! (`--kiosk`, user agent via the profile's `user.js`). Each web app host gets
//! its own browser profile under `browser-profiles/`.

mod app;
mod files;
mod heroic;
mod launch;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use ember_core::{
    Event, EventBus, ExtraInfo, GameId, GameInfo, InstallInfo, InstallInfoQuery, InstallPlatform,
    InstallRequest, LaunchPlan, LaunchRequest, LoginFlow, Notice, NoticeLevel, OpContext, Outcome,
    Paths, RemoveArgs, Runner, SettingsFn, Store, UpdateRequest, UserInfo, json_store,
};
use parking_lot::RwLock;

use app::{LIBRARY_VERSION, LibraryFile, StoredApp, generate_app_name};
pub use app::{SideloadApp, SideloadPlatform};

/// How long availability checks wait for a slow or unresponsive filesystem
/// (e.g. a disconnected network share) before reporting the app unavailable.
const AVAILABILITY_TIMEOUT: Duration = Duration::from_secs(3);

const NOT_INSTALLABLE: &str = "Apps added manually are not installed or updated by Ember";

/// Copy Heroic's sideloaded apps into Ember's sideload library, keeping apps
/// Ember already has. Call before [`Store::init`] (or call [`Store::refresh`]
/// afterwards) so the running store sees them. Per-app settings (Wine
/// version, prefix, launcher arguments) live in Heroic's `GamesConfig` and are
/// imported with the other game settings, not here.
pub fn import_from_heroic(heroic_config_dir: &Path) -> Result<()> {
    let target = library_file(&Paths::get().store_config_dir(Runner::Sideload));
    let added = heroic::import_into(heroic_config_dir, &target)?;
    tracing::info!("imported {added} sideloaded apps from Heroic");
    Ok(())
}

fn library_file(config_dir: &Path) -> PathBuf {
    config_dir.join("library.json")
}

#[derive(Default)]
struct State {
    apps: Vec<StoredApp>,
    games: Vec<Arc<GameInfo>>,
}

/// The sideloaded-apps store. Owns `library.json` and the per-host browser
/// profiles of web apps (`browser-profiles/` next to it).
pub struct SideloadStore {
    bus: EventBus,
    settings: SettingsFn,
    library_file: PathBuf,
    profiles_dir: PathBuf,
    state: RwLock<State>,
    /// Serialises read-modify-write cycles of the library file.
    write_lock: tokio::sync::Mutex<()>,
}

impl SideloadStore {
    pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self> {
        Self::with_config_dir(
            bus,
            settings,
            Paths::get().store_config_dir(Runner::Sideload),
        )
    }

    fn with_config_dir(bus: EventBus, settings: SettingsFn, config_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            bus,
            settings,
            library_file: library_file(&config_dir),
            profiles_dir: config_dir.join("browser-profiles"),
            state: RwLock::new(State::default()),
            write_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// The app as entered by the user, for prefilling the edit dialog.
    pub fn app(&self, app_name: &str) -> Option<SideloadApp> {
        self.stored(app_name).map(|s| s.app)
    }

    /// Add an app and return its new library id (a short random id).
    pub async fn add_app(&self, app: SideloadApp) -> Result<GameId> {
        let app = app.normalized()?;
        if let Some(exe) = &app.executable {
            ensure_file(exe).await?;
        }
        let _write = self.write_lock.lock().await;
        let mut apps = self.state.read().apps.clone();
        let app_name = loop {
            let id = generate_app_name();
            if !apps.iter().any(|a| a.app_name == id) {
                break id;
            }
        };
        apps.push(StoredApp {
            app_name: app_name.clone(),
            app,
        });
        self.commit(apps).await?;
        Ok(GameId::new(Runner::Sideload, app_name))
    }

    /// Replace an app's details, keeping its id.
    pub async fn edit_app(&self, app_name: &str, app: SideloadApp) -> Result<()> {
        let app = app.normalized()?;
        let _write = self.write_lock.lock().await;
        let mut apps = self.state.read().apps.clone();
        let entry = apps
            .iter_mut()
            .find(|a| a.app_name == app_name)
            .with_context(|| format!("{app_name} is not in the library"))?;
        if let Some(exe) = &app.executable
            && entry.app.executable.as_ref() != Some(exe)
        {
            ensure_file(exe).await?;
        }
        entry.app = app;
        self.commit(apps).await
    }

    /// Remove an app from the library. With `delete_files`, also delete its
    /// install folder, unless that folder is shared with another app or is not
    /// clearly the app's own (home, system and launcher folders are refused);
    /// a refused deletion fails before anything changes.
    pub async fn remove_app(&self, app_name: &str, delete_files: bool) -> Result<()> {
        let _write = self.write_lock.lock().await;
        let mut apps = self.state.read().apps.clone();
        let index = apps
            .iter()
            .position(|a| a.app_name == app_name)
            .with_context(|| format!("{app_name} is not in the library"))?;
        let to_delete = match apps[index].app.install_dir() {
            Some(dir) if delete_files => self.deletable_dir(&dir, &apps, app_name).await?,
            _ => None,
        };
        let removed = apps.remove(index);
        self.commit(apps).await?;
        if let Some(dir) = to_delete {
            tokio::task::spawn_blocking({
                let dir = dir.clone();
                move || std::fs::remove_dir_all(&dir)
            })
            .await?
            .with_context(|| {
                format!(
                    "{} was removed from the library, but deleting {} failed",
                    removed.app.title,
                    dir.display()
                )
            })?;
        }
        Ok(())
    }

    /// The canonical folder to delete, `None` when it no longer exists, or an
    /// error when deleting it would be unsafe.
    async fn deletable_dir(
        &self,
        dir: &Path,
        apps: &[StoredApp],
        app_name: &str,
    ) -> Result<Option<PathBuf>> {
        let canonical = match tokio::fs::canonicalize(dir).await {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("checking {}", dir.display())),
        };
        let home = dirs::home_dir();
        let protected = self.protected_dirs();
        for candidate in [dir, canonical.as_path()] {
            files::check_deletable(candidate, home.as_deref(), &protected)?;
        }
        if let Some(other) = apps.iter().find(|a| {
            a.app_name != app_name
                && [a.app.install_dir(), a.app.executable.clone()]
                    .into_iter()
                    .flatten()
                    .any(|p| p.starts_with(dir) || p.starts_with(&canonical))
        }) {
            bail!(
                "Not deleting {} because {} also uses it",
                dir.display(),
                other.app.title
            );
        }
        Ok(Some(canonical))
    }

    fn protected_dirs(&self) -> Vec<PathBuf> {
        let settings = (self.settings)();
        let paths = Paths::get();
        let mut dirs = vec![
            paths.config.clone(),
            paths.data.clone(),
            paths.cache.clone(),
        ];
        dirs.extend(self.library_file.parent().map(Path::to_path_buf));
        for p in [
            &settings.default_install_path,
            &settings.default_wine_prefix_dir,
        ] {
            if !p.is_empty() {
                dirs.push(PathBuf::from(p));
            }
        }
        dirs
    }

    fn stored(&self, app_name: &str) -> Option<StoredApp> {
        self.state
            .read()
            .apps
            .iter()
            .find(|a| a.app_name == app_name)
            .cloned()
    }

    /// Persist `apps`, publish them and notify. Callers hold `write_lock`.
    async fn commit(&self, apps: Vec<StoredApp>) -> Result<()> {
        let file = LibraryFile {
            version: LIBRARY_VERSION,
            apps,
        };
        let path = self.library_file.clone();
        let file = tokio::task::spawn_blocking(move || {
            json_store::write(&path, &file)
                .context("Could not save the list of apps added manually")
                .map(|_| file)
        })
        .await??;
        self.publish(file.apps);
        self.bus.emit(Event::LibraryChanged(Runner::Sideload));
        Ok(())
    }

    fn publish(&self, apps: Vec<StoredApp>) {
        let games = apps.iter().map(|a| Arc::new(a.to_game_info())).collect();
        *self.state.write() = State { apps, games };
    }

    /// Load `library.json`. A corrupt file is moved aside (`library.json.corrupt`)
    /// and reported instead of being silently replaced.
    async fn load(&self) -> Result<()> {
        let path = self.library_file.clone();
        let loaded =
            tokio::task::spawn_blocking(move || match json_store::read::<LibraryFile>(&path) {
                Ok(file) => Ok(file.unwrap_or_default()),
                Err(e) => {
                    let backup = path.with_extension("json.corrupt");
                    std::fs::rename(&path, &backup)
                        .with_context(|| format!("moving aside {}", path.display()))?;
                    Err(e.context(format!(
                        "The list of apps added manually was unreadable and was moved to {}",
                        backup.display()
                    )))
                }
            })
            .await?;
        match loaded {
            Ok(file) => {
                self.publish(file.apps);
                Ok(())
            }
            Err(e) => {
                tracing::warn!("{e:#}");
                self.bus.notice(
                    Notice::new(
                        NoticeLevel::Warning,
                        "Apps added manually could not be loaded",
                    )
                    .message(format!("{e:#}")),
                );
                self.publish(Vec::new());
                Ok(())
            }
        }
    }

    async fn browser_plan(&self, app: &SideloadApp) -> Result<LaunchPlan> {
        let launch = launch::browser_launch(app, &self.profiles_dir, &launch::which_lookup)?;
        if let Some((path, contents)) = launch.user_js {
            if let Some(dir) = path.parent() {
                tokio::fs::create_dir_all(dir)
                    .await
                    .with_context(|| format!("creating browser profile {}", dir.display()))?;
            }
            tokio::fs::write(&path, contents)
                .await
                .with_context(|| format!("writing {}", path.display()))?;
        }
        Ok(LaunchPlan {
            command: launch.command,
            track_wineserver: false,
        })
    }
}

/// Fail unless `path` is an existing regular file.
async fn ensure_file(path: &Path) -> Result<()> {
    match tokio::time::timeout(AVAILABILITY_TIMEOUT, tokio::fs::metadata(path)).await {
        Ok(Ok(m))
            if m.is_file()
                || (cfg!(target_os = "macos")
                    && m.is_dir()
                    && path.extension().is_some_and(|e| e == "app")) =>
        {
            Ok(())
        }
        Ok(Ok(_)) => bail!("{} is not a file", path.display()),
        Ok(Err(_)) => bail!("{} was not found", path.display()),
        Err(_) => bail!(
            "{} did not respond; is the drive connected?",
            path.display()
        ),
    }
}

/// Add execute permission to a native program that lacks it.
#[cfg(unix)]
async fn ensure_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return;
    };
    let mode = meta.permissions().mode();
    if mode & 0o111 == 0 {
        let perms = std::fs::Permissions::from_mode(mode | 0o755);
        if let Err(e) = tokio::fs::set_permissions(path, perms).await {
            tracing::warn!("making {} executable: {e}", path.display());
        }
    }
}

#[cfg(not(unix))]
async fn ensure_executable(_path: &Path) {}

#[async_trait]
impl Store for SideloadStore {
    fn runner(&self) -> Runner {
        Runner::Sideload
    }

    async fn init(&self) -> Result<()> {
        self.load().await
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

    /// Re-read `library.json` (picks up external edits and Heroic imports).
    async fn refresh(&self, _full: bool) -> Result<()> {
        let _write = self.write_lock.lock().await;
        self.load().await?;
        self.bus.emit(Event::LibraryChanged(Runner::Sideload));
        Ok(())
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
        let stored = self
            .stored(app_name)
            .with_context(|| format!("{app_name} is not in the library"))?;
        Ok(ExtraInfo {
            description: stored.app.description,
            store_url: stored.app.browser_url,
            ..Default::default()
        })
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
        Ok(Outcome::Error(
            "Apps added manually cannot be verified or repaired".into(),
        ))
    }

    /// Removes the app from the library; `delete_files` also deletes its
    /// install folder (see [`SideloadStore::remove_app`]). The Wine prefix is
    /// removed by the services layer.
    async fn uninstall(&self, app_name: &str, args: RemoveArgs) -> Result<()> {
        self.remove_app(app_name, args.delete_files).await
    }

    /// Register an existing program. `path` is the executable, or a folder
    /// containing exactly one likely executable. An existing `app_name` is
    /// re-pointed at the new location (a folder re-roots its executable);
    /// otherwise a new app is added under `app_name` (or a generated id when
    /// empty), titled after the file or folder.
    async fn import(&self, app_name: &str, path: &Path, platform: InstallPlatform) -> Result<()> {
        let platform = SideloadPlatform::from_install_platform(platform)
            .filter(|p| *p != SideloadPlatform::Browser)
            .context("Only Windows and Linux programs can be imported")?;
        let meta = tokio::fs::metadata(path)
            .await
            .with_context(|| format!("{} was not found", path.display()))?;
        let existing = self.stored(app_name);
        let (executable, install_folder) = if meta.is_file() {
            (path.to_path_buf(), None)
        } else {
            let rerooted = existing.as_ref().and_then(|s| {
                let old_dir = s.app.install_dir()?;
                let rel = s.app.executable.as_deref()?.strip_prefix(&old_dir).ok()?;
                Some(path.join(rel)).filter(|p| p.is_file())
            });
            let exe = match rerooted {
                Some(exe) => exe,
                None => {
                    let dir = path.to_path_buf();
                    tokio::task::spawn_blocking(move || {
                        files::find_single_executable(&dir, platform)
                    })
                    .await?
                    .with_context(|| {
                        format!("Choose the program to run inside {}", path.display())
                    })?
                }
            };
            (exe, Some(path.to_path_buf()))
        };
        let app = match existing {
            Some(stored) => SideloadApp {
                executable: Some(executable),
                platform,
                install_folder,
                ..stored.app
            },
            None => {
                let named = if meta.is_file() {
                    executable.as_path()
                } else {
                    path
                };
                let title = named
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Imported app".into());
                SideloadApp {
                    title,
                    executable: Some(executable),
                    platform,
                    install_folder,
                    ..Default::default()
                }
            }
        }
        .normalized()?;

        let _write = self.write_lock.lock().await;
        let mut apps = self.state.read().apps.clone();
        match apps.iter_mut().find(|a| a.app_name == app_name) {
            Some(entry) => entry.app = app,
            None => apps.push(StoredApp {
                app_name: if app_name.is_empty() {
                    generate_app_name()
                } else {
                    app_name.to_string()
                },
                app,
            }),
        }
        self.commit(apps).await
    }

    /// Re-root the app at `path`: an executable inside the old install folder
    /// keeps its relative location.
    async fn set_install_path(&self, app_name: &str, path: &Path) -> Result<()> {
        let _write = self.write_lock.lock().await;
        let mut apps = self.state.read().apps.clone();
        let entry = apps
            .iter_mut()
            .find(|a| a.app_name == app_name)
            .with_context(|| format!("{app_name} is not in the library"))?;
        let old_dir = entry
            .app
            .install_dir()
            .context("This app has no install folder")?;
        if let Some(exe) = &entry.app.executable
            && let Ok(rel) = exe.strip_prefix(&old_dir)
        {
            entry.app.executable = Some(path.join(rel));
        }
        if entry.app.install_folder.is_some() {
            entry.app.install_folder = Some(path.to_path_buf());
        }
        self.commit(apps).await
    }

    /// Browser apps are available when they have an address; others when the
    /// executable exists. Never waits longer than a few seconds, even on a
    /// hung network mount.
    async fn is_available(&self, app_name: &str) -> bool {
        let Some(stored) = self.stored(app_name) else {
            return false;
        };
        match (stored.app.platform, stored.app.executable) {
            (SideloadPlatform::Browser, _) => stored.app.browser_url.is_some(),
            (_, Some(exe)) => matches!(
                tokio::time::timeout(AVAILABILITY_TIMEOUT, tokio::fs::try_exists(&exe)).await,
                Ok(Ok(true))
            ),
            (_, None) => false,
        }
    }

    async fn launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        _ctx: OpContext,
    ) -> Result<LaunchPlan> {
        let stored = self
            .stored(app_name)
            .with_context(|| format!("{app_name} is not in the library"))?;
        let app = &stored.app;
        if app.platform == SideloadPlatform::Browser {
            return self.browser_plan(app).await;
        }
        anyhow::ensure!(
            ember_core::HostPlatform::current().supports(app.platform.install_platform()),
            "This app was built for a different operating system"
        );
        let exe = launch::resolve_executable(app, &request.settings.target_exe)
            .context("This app has no executable; edit it and choose one")?;
        ensure_file(&exe).await?;
        let args = launch::launch_args(app, request);
        match app.platform {
            p if !p.install_platform().needs_wine() => {
                ensure_executable(&exe).await;
                Ok(launch::native_plan(&exe, args, &request.environment))
            }
            _ => launch::wine_plan(&exe, args, &request.environment),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use ember_core::{AppSettings, GameSettings, LaunchEnvironment};

    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        store: Arc<SideloadStore>,
        bus: EventBus,
    }

    fn settings() -> SettingsFn {
        Arc::new(|| Arc::new(AppSettings::default()))
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let bus = EventBus::new();
        let store = SideloadStore::with_config_dir(bus.clone(), settings(), root.join("config"));
        Fixture {
            _dir: dir,
            root,
            store,
            bus,
        }
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"#!/bin/sh\n").unwrap();
    }

    fn linux_app(exe: &Path) -> SideloadApp {
        SideloadApp {
            title: "Native".into(),
            executable: Some(exe.to_path_buf()),
            platform: if cfg!(target_os = "macos") {
                SideloadPlatform::Mac
            } else if cfg!(windows) {
                SideloadPlatform::Windows
            } else {
                SideloadPlatform::Linux
            },
            ..Default::default()
        }
    }

    fn request() -> LaunchRequest {
        LaunchRequest {
            settings: GameSettings::default(),
            environment: LaunchEnvironment::default(),
            option: None,
            extra_args: Vec::new(),
            offline: false,
            skip_version_check: false,
            log_file: "/tmp/ember-test.log".into(),
        }
    }

    #[tokio::test]
    async fn account_is_local() {
        let f = fixture();
        assert!(f.store.is_logged_in());
        assert_eq!(f.store.user().unwrap().account_id, "local");
        assert_eq!(f.store.login_flow(), LoginFlow::None);
    }

    #[tokio::test]
    async fn add_edit_remove_persist() {
        let f = fixture();
        let mut events = f.bus.subscribe();
        let exe = f.root.join("games/native/run.sh");
        touch(&exe);
        f.store.init().await.unwrap();
        assert!(f.store.games().is_empty());

        assert!(
            f.store
                .add_app(linux_app(&f.root.join("missing")))
                .await
                .is_err()
        );
        let id = f.store.add_app(linux_app(&exe)).await.unwrap();
        assert_eq!(id.runner, Runner::Sideload);
        assert_eq!(id.app_name.len(), 22);
        assert_eq!(
            events.recv().await.unwrap(),
            Event::LibraryChanged(Runner::Sideload)
        );
        assert_eq!(f.store.game(&id.app_name).unwrap().title, "Native");

        let mut edited = f.store.app(&id.app_name).unwrap();
        edited.title = "Renamed".into();
        f.store.edit_app(&id.app_name, edited).await.unwrap();
        assert!(f.store.edit_app("nope", linux_app(&exe)).await.is_err());

        let reopened =
            SideloadStore::with_config_dir(EventBus::new(), settings(), f.root.join("config"));
        reopened.init().await.unwrap();
        assert_eq!(reopened.game(&id.app_name).unwrap().title, "Renamed");

        f.store.remove_app(&id.app_name, false).await.unwrap();
        assert!(f.store.games().is_empty());
        assert!(exe.exists());
    }

    #[tokio::test]
    async fn uninstall_deletes_own_folder_only() {
        let f = fixture();
        f.store.init().await.unwrap();
        let shared_a = f.root.join("games/shared/a.sh");
        let shared_b = f.root.join("games/shared/b.sh");
        let own = f.root.join("games/own/run.sh");
        for p in [&shared_a, &shared_b, &own] {
            touch(p);
        }
        let a = f.store.add_app(linux_app(&shared_a)).await.unwrap();
        f.store.add_app(linux_app(&shared_b)).await.unwrap();
        let o = f.store.add_app(linux_app(&own)).await.unwrap();

        let err = f
            .store
            .uninstall(
                &a.app_name,
                RemoveArgs {
                    delete_files: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("also uses it"), "{err}");
        assert!(f.store.game(&a.app_name).is_some());

        f.store
            .uninstall(
                &o.app_name,
                RemoveArgs {
                    delete_files: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(f.store.game(&o.app_name).is_none());
        assert!(!own.parent().unwrap().exists());
        assert!(shared_a.exists());
    }

    #[tokio::test]
    async fn availability_never_hangs() {
        let f = fixture();
        f.store.init().await.unwrap();
        let exe = f.root.join("g/run.sh");
        touch(&exe);
        let id = f.store.add_app(linux_app(&exe)).await.unwrap();
        let web = f
            .store
            .add_app(SideloadApp {
                title: "Web".into(),
                platform: SideloadPlatform::Browser,
                browser_url: Some("example.com".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let check = |name: String| {
            let store = f.store.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(5), store.is_available(&name))
                    .await
                    .expect("is_available must not hang")
            }
        };
        assert!(check(id.app_name.to_string()).await);
        assert!(check(web.app_name.to_string()).await);
        assert!(!check("unknown".into()).await);
        std::fs::remove_file(&exe).unwrap();
        assert!(!check(id.app_name.to_string()).await);
    }

    #[tokio::test]
    async fn launches_native_and_windows_apps() {
        let f = fixture();
        f.store.init().await.unwrap();
        let exe = f.root.join("g/run.sh");
        touch(&exe);
        let mut app = linux_app(&exe);
        app.launcher_args = "--fast".into();
        let id = f.store.add_app(app).await.unwrap();
        let plan = f
            .store
            .launch(&id.app_name, &request(), OpContext::detached())
            .await
            .unwrap();
        assert_eq!(plan.command.program, exe);
        assert_eq!(plan.command.args, vec![OsString::from("--fast")]);
        assert_eq!(plan.command.cwd.as_deref(), exe.parent());
        assert!(ember_core::util::is_executable(&exe));

        let win_exe = f.root.join("w/game.exe");
        touch(&win_exe);
        let win = f
            .store
            .add_app(SideloadApp {
                title: "Win".into(),
                executable: Some(win_exe.clone()),
                platform: SideloadPlatform::Windows,
                ..Default::default()
            })
            .await
            .unwrap();
        let result = f
            .store
            .launch(&win.app_name, &request(), OpContext::detached())
            .await;
        if cfg!(windows) {
            let plan = result.unwrap();
            assert_eq!(plan.command.program, win_exe);
            assert!(!plan.track_wineserver);
        } else {
            assert!(result.unwrap_err().to_string().contains("Wine or Proton"));
        }
    }

    #[tokio::test]
    async fn imports_executables_and_folders() {
        let f = fixture();
        f.store.init().await.unwrap();
        let exe = f.root.join("Imported Game/game.exe");
        touch(&exe);
        f.store
            .import("given-id", &exe, InstallPlatform::Windows)
            .await
            .unwrap();
        let g = f.store.game("given-id").unwrap();
        assert_eq!(g.title, "game");
        assert_eq!(
            g.install.as_ref().unwrap().executable,
            exe.to_string_lossy()
        );

        let moved = f.root.join("Moved");
        touch(&moved.join("game.exe"));
        f.store
            .import("given-id", &moved, InstallPlatform::Windows)
            .await
            .unwrap();
        let app = f.store.app("given-id").unwrap();
        assert_eq!(app.executable, Some(moved.join("game.exe")));
        assert_eq!(app.install_folder, Some(moved.clone()));

        let dir = f.root.join("Folder Game");
        touch(&dir.join("start.sh"));
        f.store
            .import("", &dir, InstallPlatform::Linux)
            .await
            .unwrap();
        assert!(f.store.games().iter().any(|g| g.title == "Folder Game"));

        assert!(
            f.store
                .import("x", &exe, InstallPlatform::Browser)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn set_install_path_reroots_executable() {
        let f = fixture();
        f.store.init().await.unwrap();
        let exe = f.root.join("old/bin/run.sh");
        touch(&exe);
        let mut app = linux_app(&exe);
        app.install_folder = Some(f.root.join("old"));
        let id = f.store.add_app(app).await.unwrap();
        f.store
            .set_install_path(&id.app_name, &f.root.join("new"))
            .await
            .unwrap();
        let app = f.store.app(&id.app_name).unwrap();
        assert_eq!(app.executable, Some(f.root.join("new/bin/run.sh")));
        assert_eq!(app.install_folder, Some(f.root.join("new")));
    }

    #[tokio::test]
    async fn corrupt_library_is_moved_aside() {
        let f = fixture();
        let file = f.root.join("config/library.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"{not json").unwrap();
        f.store.init().await.unwrap();
        assert!(f.store.games().is_empty());
        assert!(f.root.join("config/library.json.corrupt").exists());
    }

    #[tokio::test]
    async fn install_operations_are_rejected() {
        let f = fixture();
        let out = f
            .store
            .install("x", &InstallRequest::default(), OpContext::detached())
            .await
            .unwrap();
        assert!(matches!(out, Outcome::Error(_)));
        assert!(f.store.updatable_games().await.unwrap().is_empty());
        assert!(
            f.store
                .install_info("x", InstallPlatform::Windows, &InstallInfoQuery::default())
                .await
                .is_err()
        );
    }
}
