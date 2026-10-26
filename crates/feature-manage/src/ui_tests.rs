//! UI integration tests: real views in headless windows over a backend with
//! no stores, under a temporary `EMBER_HOME`.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Once};

use anyhow::{Result, bail};
use ember_core::{
    DynStore, ExtraInfo, GameId, GameInfo, InstallInfo, InstallInfoQuery, InstallPlatform,
    InstallRequest, LaunchPlan, LaunchRequest, LoginFlow, MAIN_ACCOUNT, OpContext, Outcome, Paths,
    RemoveArgs, Runner, Store, StoreAccount, UpdateRequest, UserInfo, async_trait,
};
use ember_services::{Backend, DownloadItem, DownloadKind, DownloadQueueSnapshot, DownloadState};
use ember_ui::models;
use gpui_kit::{AppContext as _, Modifiers, TestAppContext, VisualTestContext, WindowOptions};

use crate::onboarding::OnboardingStep;
use crate::{AccountsView, DownloadsView, OnboardingEvent, OnboardingView, should_show_onboarding};

fn setup(cx: &mut TestAppContext) {
    setup_with(cx, Vec::new);
}

fn setup_with(cx: &mut TestAppContext, stores: impl FnOnce() -> Vec<DynStore>) {
    // This integration test receives events from the real Tokio backend.
    cx.executor().allow_parking();
    static PATHS: Once = Once::new();
    PATHS.call_once(|| {
        let dir = tempfile::tempdir().expect("temp dir").keep();
        Paths::init(Paths::under(&dir));
    });
    let backend = Backend::start(|_, _| stores()).expect("backend starts");
    cx.update(|cx| {
        gpui_kit::init(cx);
        ember_ui::init(backend, cx);
    });
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is rendered"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

#[gpui_kit::test]
fn onboarding_walks_to_the_library(cx: &mut TestAppContext) {
    setup(cx);
    let (handle, view) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| OnboardingView::with_heroic(false, window, cx))
        })
        .expect("window opens")
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let _sub = cx.update(|cx| {
        cx.subscribe(&view, move |_, e: &OnboardingEvent, _| {
            sink.borrow_mut().push(*e)
        })
    });
    let mut vcx = VisualTestContext::from_window(handle, cx);
    vcx.run_until_parked();
    assert!(vcx.update(|_, cx| should_show_onboarding(cx)));

    let expected = [
        OnboardingStep::Welcome,
        OnboardingStep::Stores,
        OnboardingStep::InstallFolder,
        OnboardingStep::Done,
    ];
    for step in expected {
        assert_eq!(vcx.update(|_, cx| view.read(cx).step()), step);
        click(&mut vcx, "onboarding-next");
    }
    assert_eq!(*events.borrow(), vec![OnboardingEvent::Finished]);
    assert!(!vcx.update(|_, cx| should_show_onboarding(cx)));
}

fn queued(runner: Runner, app: &str, state: DownloadState) -> DownloadItem {
    DownloadItem {
        id: GameId::new(runner, app),
        title: format!("Game {app}"),
        kind: DownloadKind::Install(InstallRequest::default()),
        state,
        added_at: 0,
        started_at: None,
        finished_at: None,
        download_size: 1 << 30,
        error: None,
        progress: None,
    }
}

#[gpui_kit::test]
fn downloads_show_items_of_every_store_in_queue_order(cx: &mut TestAppContext) {
    setup(cx);
    let (handle, view) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| DownloadsView::new(window, cx))
        })
        .expect("window opens")
    });
    let mut vcx = VisualTestContext::from_window(handle, cx);
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("queue-zoom:b").is_none());

    vcx.update(|_, cx| {
        let downloads = models(cx).downloads.clone();
        downloads.update(cx, |d, cx| {
            d.queue = DownloadQueueSnapshot {
                active: vec![
                    queued(Runner::Gog, "a", DownloadState::Running),
                    queued(Runner::Zoom, "b", DownloadState::Queued),
                    queued(Runner::Sideload, "c", DownloadState::Queued),
                ],
                finished: vec![queued(Runner::Nile, "d", DownloadState::Failed)],
                paused: false,
            };
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let current = vcx.update(|_, cx| {
        view.read(cx)
            .queue_for_tests()
            .current
            .as_ref()
            .map(|c| c.item.id.clone())
    });
    assert_eq!(current, Some(GameId::new(Runner::Gog, "a")));
    // ZOOM and manually added items render (Heroic hid them), in queue order.
    let zoom = vcx.debug_bounds("queue-zoom:b").expect("ZOOM row rendered");
    let other = vcx
        .debug_bounds("queue-sideload:c")
        .expect("sideload row rendered");
    assert!(zoom.top() < other.top());
    // The running item is the hero, not a queue row.
    assert!(vcx.debug_bounds("queue-gog:a").is_none());
}

/// A GOG with two accounts: one signed in, one logged out that still has a
/// game installed with it.
struct TwoAccounts;

#[async_trait]
impl Store for TwoAccounts {
    fn runner(&self) -> Runner {
        Runner::Gog
    }
    async fn init(&self) -> Result<()> {
        Ok(())
    }
    fn user(&self) -> Option<UserInfo> {
        Some(UserInfo {
            account_id: "1".into(),
            display_name: "Alice".into(),
        })
    }
    fn accounts(&self) -> Vec<StoreAccount> {
        vec![
            StoreAccount {
                slot: MAIN_ACCOUNT.into(),
                account_id: "1".into(),
                display_name: "Alice".into(),
                signed_in: true,
                games: 12,
                ..Default::default()
            },
            StoreAccount {
                slot: "2".into(),
                account_id: "2".into(),
                display_name: "Bob".into(),
                installed: 1,
                ..Default::default()
            },
        ]
    }
    fn can_add_account(&self) -> bool {
        true
    }
    fn login_flow(&self) -> LoginFlow {
        LoginFlow::None
    }
    async fn login(&self, _: &str) -> Result<UserInfo> {
        bail!("unused")
    }
    async fn logout(&self) -> Result<()> {
        Ok(())
    }
    async fn refresh(&self, _: bool) -> Result<()> {
        Ok(())
    }
    fn games(&self) -> Vec<Arc<GameInfo>> {
        Vec::new()
    }
    fn game(&self, _: &str) -> Option<Arc<GameInfo>> {
        None
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
    async fn install(&self, _: &str, _: &InstallRequest, _: OpContext) -> Result<Outcome> {
        bail!("unused")
    }
    async fn update(&self, _: &str, _: &UpdateRequest, _: OpContext) -> Result<Outcome> {
        bail!("unused")
    }
    async fn repair(&self, _: &str, _: OpContext) -> Result<Outcome> {
        bail!("unused")
    }
    async fn uninstall(&self, _: &str, _: RemoveArgs) -> Result<()> {
        bail!("unused")
    }
    async fn import(&self, _: &str, _: &Path, _: InstallPlatform) -> Result<()> {
        bail!("unused")
    }
    async fn set_install_path(&self, _: &str, _: &Path) -> Result<()> {
        bail!("unused")
    }
    async fn launch(&self, _: &str, _: &LaunchRequest, _: OpContext) -> Result<LaunchPlan> {
        bail!("unused")
    }
}

#[gpui_kit::test]
fn accounts_page_lists_every_account_of_a_store(cx: &mut TestAppContext) {
    setup_with(cx, || vec![Arc::new(TwoAccounts) as DynStore]);
    let (handle, _view) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| AccountsView::new(window, cx))
        })
        .expect("window opens")
    });
    let mut vcx = VisualTestContext::from_window(handle, cx);
    vcx.run_until_parked();
    let alice = vcx
        .debug_bounds("account-gog-main")
        .expect("the signed-in account is listed");
    let bob = vcx
        .debug_bounds("account-gog-2")
        .expect("the logged-out account with an install is listed");
    assert!(alice.top() < bob.top(), "in the store's order");
    assert!(vcx.debug_bounds("add-account").is_some());
}
