//! UI integration tests: the real game page in a headless window, backed by
//! a backend with one in-memory store whose files live in a temp directory.
//!
//! GPUI's test scheduler rejects wake-ups from other threads, and the
//! backend runs on its own Tokio runtime. The tests therefore let backend
//! work settle (real time) before running GPUI tasks, so every backend
//! result is ready when first polled, and drive only commands whose events
//! are emitted on the test thread.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use ember_core::{
    DynStore, ExtraInfo, GameId, GameInfo, InstallInfo, InstallInfoQuery, InstallPlatform,
    InstallRequest, LaunchPlan, LaunchRequest, LoginFlow, OpContext, Outcome, Paths, RemoveArgs,
    Runner, Store, UpdateRequest, UserInfo, async_trait,
};
use ember_services::Backend;
use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Entity, Keystroke, TestAppContext, WindowOptions,
};

use crate::{GamePage, PrimaryAction};

struct FakeStore {
    game: Arc<GameInfo>,
}

#[async_trait]
impl Store for FakeStore {
    fn runner(&self) -> Runner {
        Runner::Gog
    }
    async fn init(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn user(&self) -> Option<UserInfo> {
        None
    }
    fn login_flow(&self) -> LoginFlow {
        LoginFlow::None
    }
    async fn login(&self, _: &str) -> anyhow::Result<UserInfo> {
        anyhow::bail!("not supported")
    }
    async fn logout(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn refresh(&self, _: bool) -> anyhow::Result<()> {
        Ok(())
    }
    fn games(&self) -> Vec<Arc<GameInfo>> {
        vec![self.game.clone()]
    }
    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        (app_name == self.game.app_name).then(|| self.game.clone())
    }
    async fn extra_info(&self, _: &str) -> anyhow::Result<ExtraInfo> {
        Ok(ExtraInfo {
            description: Some("A test game.".into()),
            ..Default::default()
        })
    }
    async fn install_info(
        &self,
        app_name: &str,
        platform: InstallPlatform,
        _: &InstallInfoQuery,
    ) -> anyhow::Result<InstallInfo> {
        Ok(InstallInfo {
            app_name: app_name.into(),
            platform,
            download_size: 1 << 30,
            disk_size: 2 << 30,
            ..Default::default()
        })
    }
    async fn updatable_games(&self) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }
    async fn install(&self, _: &str, _: &InstallRequest, _: OpContext) -> anyhow::Result<Outcome> {
        Ok(Outcome::Done)
    }
    async fn update(&self, _: &str, _: &UpdateRequest, _: OpContext) -> anyhow::Result<Outcome> {
        Ok(Outcome::Done)
    }
    async fn repair(&self, _: &str, _: OpContext) -> anyhow::Result<Outcome> {
        Ok(Outcome::Done)
    }
    async fn uninstall(&self, _: &str, _: RemoveArgs) -> anyhow::Result<()> {
        Ok(())
    }
    async fn import(&self, _: &str, _: &Path, _: InstallPlatform) -> anyhow::Result<()> {
        Ok(())
    }
    async fn set_install_path(&self, _: &str, _: &Path) -> anyhow::Result<()> {
        Ok(())
    }
    async fn launch(&self, _: &str, _: &LaunchRequest, _: OpContext) -> anyhow::Result<LaunchPlan> {
        anyhow::bail!("not supported")
    }
}

fn test_game() -> Arc<GameInfo> {
    Arc::new(
        GameInfo {
            runner: Some(Runner::Gog),
            app_name: "1207658924".into(),
            title: "Test Quest".into(),
            installable: true,
            is_linux_native: true,
            platforms: vec![InstallPlatform::Windows, InstallPlatform::Linux],
            ..Default::default()
        }
        .finalize(),
    )
}

fn backend() -> Backend {
    static ROOT: OnceLock<std::path::PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = tempfile::tempdir().expect("temp dir").keep();
        Paths::init(Paths::under(&root));
        // No catalogue refresh at startup: it would emit events from the
        // backend runtime while the test runs.
        ember_core::json_store::write(
            &Paths::get().settings_file(),
            &serde_json::json!({ "checkForUpdatesOnStartup": false }),
        )
        .expect("write settings");
        root
    });
    let game = test_game();
    let id = game.id();
    let b = Backend::start(move |_, _| vec![Arc::new(FakeStore { game }) as DynStore])
        .expect("backend");
    for _ in 0..400 {
        if b.library().get(&id).is_some() {
            break;
        }
        settle();
    }
    settle();
    b
}

/// Give the backend runtime time to finish work started by the last step.
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(50));
}

fn open_page(cx: &mut TestAppContext, id: GameId) -> (Backend, AnyWindowHandle, Entity<GamePage>) {
    let b = backend();
    let shared = b.clone();
    cx.update(|cx| {
        gpui_kit::init(cx);
        ember_ui::init(shared, cx);
        crate::init(cx);
        // Keep the compatibility panel off the network.
        crate::install_hooks(
            crate::GameHooks::new().with_compatibility(Arc::new(|_, _| {
                gpui_kit::Task::ready(Ok(crate::CompatibilityInfo::default()))
            })),
            cx,
        );
    });
    let (handle, page) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| GamePage::new(id, window, cx))
        })
        .expect("open window")
    });
    settle();
    cx.run_until_parked();
    (b, handle, page)
}

fn primary(cx: &mut TestAppContext, page: &Entity<GamePage>) -> Option<PrimaryAction> {
    cx.update(|cx| page.read(cx).primary_action(cx))
}

#[gpui_kit::test]
fn ctrl_enter_opens_the_install_sheet(cx: &mut TestAppContext) {
    let id = test_game().id();
    let (_b, handle, page) = open_page(cx, id);

    cx.update(|cx| assert_eq!(page.read(cx).title().as_deref(), Some("Test Quest")));
    assert_eq!(primary(cx, &page), Some(PrimaryAction::Install));
    let has_sheet = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, cx| window.has_active_sheet(cx))
            .expect("window")
    };
    assert!(!has_sheet(cx));

    // The key binding runs the same command as the Install button.
    cx.dispatch_keystroke(handle, Keystroke::parse("ctrl-enter").expect("keystroke"));
    settle();
    cx.run_until_parked();
    assert!(has_sheet(cx));

    // An unbound key leaves the page alone.
    cx.update_window(handle, |_, window, cx| window.close_sheet(cx))
        .expect("window");
    cx.dispatch_keystroke(handle, Keystroke::parse("ctrl-shift-z").expect("keystroke"));
    cx.run_until_parked();
    assert!(!has_sheet(cx));
}

#[gpui_kit::test]
fn download_queue_drives_the_primary_action(cx: &mut TestAppContext) {
    let id = test_game().id();
    let (b, _handle, page) = open_page(cx, id.clone());
    assert_eq!(primary(cx, &page), Some(PrimaryAction::Install));

    // A paused queue keeps the install waiting: the page offers Resume.
    b.pause_downloads();
    b.enqueue_install(
        id.clone(),
        "Test Quest".into(),
        InstallRequest::default(),
        0,
    );
    settle();
    cx.run_until_parked();
    assert_eq!(primary(cx, &page), Some(PrimaryAction::Resume));

    // Removing it from the queue returns the page to Install.
    b.cancel_download(&id, true);
    settle();
    cx.run_until_parked();
    assert_eq!(primary(cx, &page), Some(PrimaryAction::Install));
}
