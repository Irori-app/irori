//! Accounts over a fake store that keeps its files like the real ones.

use super::*;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

/// What each account owns on the fake store's "server".
fn owned_by(account: &str) -> &'static [&'static str] {
    match account {
        "a" => &["shared", "only-a"],
        "b" => &["shared", "only-b"],
        _ => &[],
    }
}

/// A store keeping a login (`user.json`) and installs (`installed.json`)
/// in its config dir, like the real ones. Logging in takes `id:name`.
struct FakeStore {
    dirs: SlotDirs,
    user: RwLock<Option<UserInfo>>,
    installed: RwLock<BTreeSet<String>>,
    refused: AtomicBool,
}

impl FakeStore {
    fn new(dirs: &SlotDirs) -> Arc<Self> {
        Arc::new(Self {
            dirs: dirs.clone(),
            user: RwLock::new(None),
            installed: RwLock::new(BTreeSet::new()),
            refused: AtomicBool::new(false),
        })
    }

    fn save(&self) {
        json_store::write(
            &self.dirs.config.join("installed.json"),
            &*self.installed.read(),
        )
        .unwrap();
    }
}

#[async_trait]
impl Store for FakeStore {
    fn runner(&self) -> Runner {
        Runner::Gog
    }
    async fn init(&self) -> Result<()> {
        *self.user.write() = json_store::read(&self.dirs.config.join("user.json"))?;
        *self.installed.write() =
            json_store::read_or_default(&self.dirs.config.join("installed.json"));
        Ok(())
    }
    fn user(&self) -> Option<UserInfo> {
        self.user.read().clone()
    }
    fn login_flow(&self) -> LoginFlow {
        LoginFlow::None
    }
    async fn login(&self, input: &str) -> Result<UserInfo> {
        let (id, name) = input.split_once(':').context("id:name")?;
        let user = UserInfo {
            account_id: id.into(),
            display_name: name.into(),
        };
        json_store::write(&self.dirs.config.join("user.json"), &user)?;
        std::fs::create_dir_all(&self.dirs.cache)?;
        *self.user.write() = Some(user.clone());
        Ok(user)
    }
    async fn logout(&self) -> Result<()> {
        let _ = std::fs::remove_file(self.dirs.config.join("user.json"));
        *self.user.write() = None;
        Ok(())
    }
    async fn refresh(&self, _: bool) -> Result<()> {
        if self.refused.load(Ordering::SeqCst) {
            bail!(SessionExpired::new(Runner::Gog, "token revoked"));
        }
        Ok(())
    }
    fn games(&self) -> Vec<Arc<GameInfo>> {
        let Some(user) = self.user() else {
            return Vec::new();
        };
        owned_by(&user.account_id)
            .iter()
            .map(|app| self.game(app).expect("owned"))
            .collect()
    }
    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        let user = self.user()?;
        owned_by(&user.account_id).contains(&app_name).then(|| {
            Arc::new(GameInfo {
                runner: Some(Runner::Gog),
                app_name: app_name.into(),
                title: format!("{app_name} of {}", user.display_name),
                is_installed: self.installed.read().contains(app_name),
                ..Default::default()
            })
        })
    }
    async fn extra_info(&self, _: &str) -> Result<ExtraInfo> {
        Ok(ExtraInfo::default())
    }
    async fn install_info(
        &self,
        _: &str,
        _: InstallPlatform,
        _: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        bail!("unused")
    }
    async fn updatable_games(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    async fn install(&self, app: &str, _: &InstallRequest, _: OpContext) -> Result<Outcome> {
        self.game(app).context("not owned")?;
        self.installed.write().insert(app.into());
        self.save();
        Ok(Outcome::Done)
    }
    async fn update(&self, _: &str, _: &UpdateRequest, _: OpContext) -> Result<Outcome> {
        bail!("unused")
    }
    async fn repair(&self, _: &str, _: OpContext) -> Result<Outcome> {
        bail!("unused")
    }
    async fn uninstall(&self, app: &str, _: RemoveArgs) -> Result<()> {
        self.installed.write().remove(app);
        self.save();
        Ok(())
    }
    async fn import(&self, _: &str, _: &Path, _: InstallPlatform) -> Result<()> {
        bail!("unused")
    }
    async fn set_install_path(&self, _: &str, _: &Path) -> Result<()> {
        bail!("unused")
    }
    async fn launch(&self, app: &str, _: &LaunchRequest, _: OpContext) -> Result<LaunchPlan> {
        let user = self.user().context("signed out")?;
        bail!("launching {app} as {}", user.display_name)
    }
}

struct Setup {
    _root: tempfile::TempDir,
    config: PathBuf,
    cache: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        Self {
            config: root.path().join("config/stores/gog"),
            cache: root.path().join("cache/accounts/gog"),
            _root: root,
        }
    }

    fn main_dirs(&self) -> SlotDirs {
        SlotDirs {
            config: self.config.clone(),
            cache: self.config.with_file_name("gog-cache"),
        }
    }

    /// A store over these directories, as Ember builds one at startup.
    async fn open(&self) -> Arc<MultiAccountStore<FakeStore>> {
        let store = MultiAccountStore::with_roots(
            FakeStore::new(&self.main_dirs()),
            EventBus::new(),
            Arc::new(|dirs: &SlotDirs| FakeStore::new(dirs)),
            self.config.clone(),
            self.cache.clone(),
        );
        store.init().await.unwrap();
        store
    }
}

fn names(store: &MultiAccountStore<FakeStore>) -> Vec<(String, String, bool)> {
    store
        .accounts()
        .into_iter()
        .map(|a| (a.slot, a.display_name, a.signed_in))
        .collect()
}

fn row(slot: &str, name: &str, signed_in: bool) -> (String, String, bool) {
    (slot.into(), name.into(), signed_in)
}

async fn launch_error(store: &MultiAccountStore<FakeStore>, app: &str) -> String {
    let request = LaunchRequest {
        settings: Default::default(),
        environment: Default::default(),
        option: None,
        extra_args: Vec::new(),
        offline: false,
        skip_version_check: false,
        log_file: PathBuf::new(),
    };
    let e = store.launch(app, &request, OpContext::detached()).await;
    format!("{:#}", e.unwrap_err())
}

#[tokio::test]
async fn a_single_account_from_earlier_versions_keeps_working() {
    let setup = Setup::new();
    // What an earlier Ember left: the login and an install, nothing else.
    let main = setup.main_dirs();
    json_store::write(
        &main.config.join("user.json"),
        &UserInfo {
            account_id: "a".into(),
            display_name: "Alice".into(),
        },
    )
    .unwrap();
    json_store::write(&main.config.join("installed.json"), &["only-a"]).unwrap();

    let store = setup.open().await;
    assert_eq!(names(&store), [row(MAIN_ACCOUNT, "Alice", true)]);
    assert_eq!(store.user().unwrap().display_name, "Alice");
    assert!(store.game("only-a").unwrap().is_installed);
    assert_eq!(store.games().len(), 2);
    assert_eq!(store.game_account("only-a"), None, "one account: not named");
    assert!(!setup.config.join("accounts").exists());
    assert!(
        launch_error(&store, "only-a")
            .await
            .contains("launching only-a as Alice")
    );
}

#[tokio::test]
async fn adds_an_account_and_merges_the_libraries() {
    let setup = Setup::new();
    let store = setup.open().await;
    // The first login goes to the store's usual directories.
    store.account_login(None, "a:Alice").await.unwrap();
    assert!(setup.config.join("user.json").is_file());

    let _ = store.account_login_flow(None);
    let bob = store.account_login(None, "b:Bob").await.unwrap();
    assert_eq!(bob.display_name, "Bob");
    assert!(setup.config.join("accounts/2/user.json").is_file());
    assert!(setup.cache.join("2").is_dir());
    assert_eq!(
        names(&store),
        [row(MAIN_ACCOUNT, "Alice", true), row("2", "Bob", true)]
    );

    let mut apps: Vec<String> = store.games().iter().map(|g| g.app_name.clone()).collect();
    apps.sort();
    assert_eq!(apps, ["only-a", "only-b", "shared"]);
    assert_eq!(store.game_account("only-b").unwrap().display_name, "Bob");
    assert_eq!(store.game_account("shared").unwrap().display_name, "Alice");

    // Each game installs and launches with the account that owns it.
    let request = InstallRequest::default();
    store
        .install("only-b", &request, OpContext::detached())
        .await
        .unwrap();
    let installed: Vec<String> =
        json_store::read_or_default(&setup.config.join("accounts/2/installed.json"));
    assert_eq!(installed, ["only-b"]);
    assert!(launch_error(&store, "only-b").await.contains("as Bob"));
    assert!(launch_error(&store, "shared").await.contains("as Alice"));

    // A restart finds both accounts again.
    let reopened = setup.open().await;
    assert_eq!(names(&reopened), names(&store));
    assert!(reopened.game("only-b").unwrap().is_installed);
}

#[tokio::test]
async fn the_account_a_game_was_installed_with_owns_it() {
    let setup = Setup::new();
    let store = setup.open().await;
    store.account_login(None, "a:Alice").await.unwrap();
    store.account_login(None, "b:Bob").await.unwrap();
    let bob = store.slot("2").unwrap();
    bob.store
        .install("shared", &InstallRequest::default(), OpContext::detached())
        .await
        .unwrap();

    assert_eq!(store.game("shared").unwrap().title, "shared of Bob");
    let shared: Vec<_> = store
        .games()
        .into_iter()
        .filter(|g| g.app_name == "shared")
        .collect();
    assert_eq!(shared.len(), 1, "one entry per game");
    assert!(shared[0].is_installed);
    assert_eq!(store.game_account("shared").unwrap().display_name, "Bob");
    assert!(launch_error(&store, "shared").await.contains("as Bob"));
}

#[tokio::test]
async fn refuses_an_account_that_is_already_here() {
    let setup = Setup::new();
    let store = setup.open().await;
    store.account_login(None, "a:Alice").await.unwrap();
    let e = store.account_login(None, "a:Alice").await.unwrap_err();
    assert!(e.to_string().contains("already logged in"), "{e}");
    assert_eq!(names(&store), [row(MAIN_ACCOUNT, "Alice", true)]);
    assert!(!setup.config.join("accounts/2").exists());
    // The refused login's instance is gone; the next one starts fresh.
    store.account_login(None, "b:Bob").await.unwrap();
    assert_eq!(store.accounts().len(), 2);
}

#[tokio::test]
async fn logging_out_keeps_accounts_that_installed_games() {
    let setup = Setup::new();
    let store = setup.open().await;
    store.account_login(None, "a:Alice").await.unwrap();
    store.account_login(None, "b:Bob").await.unwrap();
    store.account_login(None, "c:Carol").await.unwrap();
    store
        .install("only-b", &InstallRequest::default(), OpContext::detached())
        .await
        .unwrap();

    // Carol has nothing installed: her account and files go.
    store.account_logout("3").await.unwrap();
    assert!(!setup.config.join("accounts/3").exists());
    assert!(!setup.cache.join("3").exists());

    // Bob's game stays installed; his account stays listed, signed out.
    store.account_logout("2").await.unwrap();
    assert_eq!(
        names(&store),
        [row(MAIN_ACCOUNT, "Alice", true), row("2", "Bob", false)]
    );
    assert_eq!(store.accounts()[1].installed, 1);
    assert!(setup.config.join("accounts/2/installed.json").is_file());
    let reopened = setup.open().await;
    assert_eq!(names(&reopened), names(&store));

    // Signing Bob in as a new account would strand his game.
    let e = store.account_login(None, "b:Bob").await.unwrap_err();
    assert!(e.to_string().contains("logged out"), "{e}");
    store.account_login(Some("2"), "b:Bob").await.unwrap();
    assert!(store.game("only-b").unwrap().is_installed);
    assert!(store.accounts()[1].signed_in);
}

#[tokio::test]
async fn a_new_login_reuses_the_main_slot_once_it_is_free() {
    let setup = Setup::new();
    let store = setup.open().await;
    store.account_login(None, "a:Alice").await.unwrap();
    store.account_login(None, "b:Bob").await.unwrap();
    store.account_logout(MAIN_ACCOUNT).await.unwrap();
    assert_eq!(names(&store), [row("2", "Bob", true)]);
    assert_eq!(store.user().unwrap().display_name, "Bob");
    store.account_login(None, "c:Carol").await.unwrap();
    assert_eq!(
        names(&store),
        [row(MAIN_ACCOUNT, "Carol", true), row("2", "Bob", true)]
    );
}

#[tokio::test]
async fn names_the_account_whose_login_was_refused() {
    let setup = Setup::new();
    let store = setup.open().await;
    store.account_login(None, "a:Alice").await.unwrap();
    store.account_login(None, "b:Bob").await.unwrap();
    store
        .slot("2")
        .unwrap()
        .store
        .refused
        .store(true, Ordering::SeqCst);

    let e = store.refresh(false).await.unwrap_err();
    let expired = SessionExpired::of(&e).expect("a refused login");
    assert_eq!(expired.reason, "Bob: token revoked");
    let accounts = store.accounts();
    assert_eq!(accounts[0].expired, None);
    assert_eq!(accounts[1].expired.as_deref(), Some("token revoked"));

    store
        .slot("2")
        .unwrap()
        .store
        .refused
        .store(false, Ordering::SeqCst);
    store.refresh_account("2", false).await.unwrap();
    assert_eq!(store.accounts()[1].expired, None);
}

#[tokio::test]
async fn lists_owners_and_installs_with_the_chosen_account() {
    let setup = Setup::new();
    let store = setup.open().await;
    store.account_login(None, "a:Alice").await.unwrap();
    assert!(
        store.game_owners().is_empty(),
        "one account: nothing to tell"
    );
    store.account_login(None, "b:Bob").await.unwrap();
    let owners = store.game_owners();
    assert_eq!(owners["shared"], ["a", "b"]);
    assert_eq!(owners["only-b"], ["b"]);

    // Bob installs the game both own; it is his from then on.
    let request = InstallRequest {
        account: Some("b".into()),
        ..Default::default()
    };
    store
        .install("shared", &request, OpContext::detached())
        .await
        .unwrap();
    let bobs: Vec<String> =
        json_store::read_or_default(&setup.config.join("accounts/2/installed.json"));
    assert_eq!(bobs, ["shared"]);
    assert_eq!(store.game_account("shared").unwrap().display_name, "Bob");
    assert!(launch_error(&store, "shared").await.contains("as Bob"));

    // An account that does not own the game is ignored.
    store
        .install("only-a", &request, OpContext::detached())
        .await
        .unwrap();
    let alices: Vec<String> = json_store::read_or_default(&setup.config.join("installed.json"));
    assert_eq!(alices, ["only-a"]);
}
