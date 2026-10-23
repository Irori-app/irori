//! UI integration tests: real views in headless windows over a backend with
//! no stores, under a temporary `EMBER_HOME`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Once;

use ember_core::{GameId, InstallRequest, Paths, Runner};
use ember_services::{Backend, DownloadItem, DownloadKind, DownloadQueueSnapshot, DownloadState};
use ember_ui::models;
use gpui_kit::{AppContext as _, Modifiers, TestAppContext, VisualTestContext, WindowOptions};

use crate::onboarding::OnboardingStep;
use crate::{DownloadsView, OnboardingEvent, OnboardingView, should_show_onboarding};

fn setup(cx: &mut TestAppContext) {
    static PATHS: Once = Once::new();
    PATHS.call_once(|| {
        let dir = tempfile::tempdir().expect("temp dir").keep();
        Paths::init(Paths::under(&dir));
    });
    let backend = Backend::start(|_, _| Vec::new()).expect("backend starts");
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
