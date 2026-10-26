//! Read-only Steam library: the games the Steam client installed on this
//! computer, read from Steam's own files without logging in.
//!
//! Every Steam library folder (`steamapps/libraryfolders.vdf`) is scanned for
//! app manifests (`appmanifest_<id>.acf`); fully installed apps that are not
//! Steam tools (Proton, the Linux runtimes, redistributables) become library
//! entries keyed by their Steam app ID. Playing opens
//! `steam://rungameid/<id>`, so Steam starts the game with its own
//! compatibility settings, overlay and cloud saves. Installing, updating,
//! verifying, moving and uninstalling stay in Steam
//! ([`Runner::manages_installs`] is false). The playtime Steam recorded for
//! the most recent user (`userdata/<id>/config/localconfig.vdf`) is merged
//! into Ember's.

mod scan;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;
use ember_core::process::CommandSpec;
use ember_core::util::format_bytes;
use ember_core::{
    Event, EventBus, ExtraInfo, GameInfo, HostPlatform, InstallInfo, InstallInfoQuery,
    InstallPlatform, InstallRequest, InstalledInfo, LaunchPlan, LaunchRequest, LoginFlow,
    OpContext, Outcome, RemoveArgs, Runner, SettingsFn, Store, UpdateRequest, UserInfo,
};
use parking_lot::RwLock;

use scan::InstalledApp;

const NOT_MANAGED: &str =
    "Steam installs, updates and removes its own games. Use the Steam client for that.";

const STEAM_CDN: &str = "https://cdn.cloudflare.steamstatic.com/steam/apps";

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
}

/// The Steam library store.
pub struct SteamStore {
    bus: EventBus,
    settings: SettingsFn,
    /// Steam folder used instead of the configured one (tests).
    steam_dir: Option<PathBuf>,
    library: RwLock<Arc<Library>>,
}

impl SteamStore {
    pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self> {
        Arc::new(Self {
            bus,
            settings,
            steam_dir: None,
            library: RwLock::new(Arc::default()),
        })
    }

    /// A store reading the Steam installation at `steam_dir`.
    pub fn with_steam_dir(bus: EventBus, settings: SettingsFn, steam_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            bus,
            settings,
            steam_dir: Some(steam_dir),
            library: RwLock::new(Arc::default()),
        })
    }

    /// Steam's folder: the "Steam path" setting, Flatpak Steam, or the
    /// platform's default location.
    fn steam_dir(&self) -> PathBuf {
        self.steam_dir
            .clone()
            .unwrap_or_else(|| ember_runtime::locations::steam_path(&(self.settings)()))
    }

    /// Re-read Steam's files and publish the result. Reports whether the
    /// library changed.
    async fn scan(&self) -> Result<bool> {
        let dir = self.steam_dir();
        let (found, apps) = tokio::task::spawn_blocking(move || {
            let found = dir.join("steamapps").is_dir();
            (found, scan::installed_apps(&dir))
        })
        .await?;
        let games: Vec<Arc<GameInfo>> = apps.iter().map(|a| Arc::new(game_info(a))).collect();
        let changed = {
            let old = self.library.read();
            old.found != found
                || old.games.len() != games.len()
                || old.games.iter().zip(&games).any(|(a, b)| !same_entry(a, b))
        };
        if changed {
            let by_app = games
                .iter()
                .map(|g| (g.app_name.clone(), g.clone()))
                .collect();
            *self.library.write() = Arc::new(Library {
                found,
                games,
                by_app,
            });
        }
        Ok(changed)
    }

    fn require_game(&self, app_name: &str) -> Result<Arc<GameInfo>> {
        match self.game(app_name) {
            Some(g) => Ok(g),
            None => bail!("{app_name} is not installed in Steam"),
        }
    }
}

fn same_entry(a: &GameInfo, b: &GameInfo) -> bool {
    a.app_name == b.app_name
        && a.title == b.title
        && a.install
            .as_ref()
            .map(|i| (&i.install_path, &i.version, i.install_size_bytes))
            == b.install
                .as_ref()
                .map(|i| (&i.install_path, &i.version, i.install_size_bytes))
}

/// Library entry of an installed Steam app. Steam runs Windows games
/// through Proton itself, so for Ember every Steam game is native.
fn game_info(app: &InstalledApp) -> GameInfo {
    let native = HostPlatform::current().native();
    let id = &app.app_id;
    GameInfo {
        runner: Some(Runner::Steam),
        app_name: id.clone(),
        title: app.name.clone(),
        store_url: Some(format!("https://store.steampowered.com/app/{id}/")),
        art_cover: Some(format!("{STEAM_CDN}/{id}/library_600x900_2x.jpg")),
        art_square: Some(format!("{STEAM_CDN}/{id}/header.jpg")),
        art_background: Some(format!("{STEAM_CDN}/{id}/library_hero.jpg")),
        art_logo: Some(format!("{STEAM_CDN}/{id}/logo.png")),
        is_installed: true,
        install: Some(InstalledInfo {
            install_path: app.install_path.to_string_lossy().into_owned(),
            install_size: format_bytes(app.size_on_disk),
            install_size_bytes: app.size_on_disk,
            version: app.build_id.clone(),
            platform: native,
            ..Default::default()
        }),
        installable: false,
        can_run_offline: true,
        is_linux_native: native == InstallPlatform::Linux,
        is_mac_native: native == InstallPlatform::Mac,
        platforms: vec![native],
        ..Default::default()
    }
    .finalize()
}

/// The command that hands `url` to the program registered for it.
fn open_url(url: &str) -> CommandSpec {
    if cfg!(target_os = "macos") {
        CommandSpec::new("/usr/bin/open").arg(url)
    } else if cfg!(windows) {
        CommandSpec::new("rundll32.exe")
            .arg("url.dll,FileProtocolHandler")
            .arg(url)
    } else {
        CommandSpec::new("xdg-open").arg(url)
    }
}

#[async_trait]
impl Store for SteamStore {
    fn runner(&self) -> Runner {
        Runner::Steam
    }

    /// Reads Steam's files (no network).
    async fn init(&self) -> Result<()> {
        self.scan().await.map(|_| ())
    }

    /// A local "account" while Steam is installed, so its games count as
    /// available; none otherwise.
    fn user(&self) -> Option<UserInfo> {
        self.library.read().found.then(|| UserInfo {
            account_id: "local".into(),
            display_name: String::new(),
        })
    }

    fn login_flow(&self) -> LoginFlow {
        LoginFlow::None
    }

    async fn login(&self, _input: &str) -> Result<UserInfo> {
        bail!("Steam needs no login in Ember: it reads the games Steam installed")
    }

    async fn logout(&self) -> Result<()> {
        Ok(())
    }

    async fn refresh(&self, _full: bool) -> Result<()> {
        let found = self.library.read().found;
        if self.scan().await? {
            if self.library.read().found != found {
                self.bus.emit(Event::AccountChanged(Runner::Steam));
            }
            self.bus.emit(Event::LibraryChanged(Runner::Steam));
        }
        Ok(())
    }

    fn games(&self) -> Vec<Arc<GameInfo>> {
        self.library.read().games.clone()
    }

    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        self.library.read().by_app.get(app_name).cloned()
    }

    async fn extra_info(&self, app_name: &str) -> Result<ExtraInfo> {
        let game = self.require_game(app_name)?;
        Ok(ExtraInfo {
            store_url: game.store_url.clone(),
            ..Default::default()
        })
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

    /// Opens `steam://rungameid/<id>`; Steam applies the game's launch
    /// options, Proton version and overlay. Ember's own launch settings
    /// (Wine, wrappers, arguments) do not apply.
    async fn launch(
        &self,
        app_name: &str,
        _request: &LaunchRequest,
        _ctx: OpContext,
    ) -> Result<LaunchPlan> {
        let game = self.require_game(app_name)?;
        Ok(LaunchPlan {
            command: open_url(&format!("steam://rungameid/{}", game.app_name)),
            track_wineserver: false,
        })
    }

    async fn remote_playtime(&self) -> Result<HashMap<String, u64>> {
        if (self.settings)().disable_playtime_sync {
            return Ok(HashMap::new());
        }
        let dir = self.steam_dir();
        Ok(tokio::task::spawn_blocking(move || scan::playtime_minutes(&dir)).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_core::AppSettings;

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

    fn store(steam: &Path, settings: AppSettings) -> Arc<SteamStore> {
        let settings = Arc::new(settings);
        SteamStore::with_steam_dir(
            EventBus::new(),
            Arc::new(move || settings.clone()),
            steam.to_path_buf(),
        )
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
        assert_eq!(args.last().unwrap(), "steam://rungameid/620");
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
