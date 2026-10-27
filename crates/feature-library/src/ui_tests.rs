//! UI integration tests: the real `LibraryView` in a headless window over a
//! real backend with an in-memory store.

use std::path::Path;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use gpui_kit::{
    AppContext as _, Bounds, Focusable as _, TestAppContext, WindowBounds, WindowOptions, px, size,
};
use irori_core::{
    DynStore, ExtraInfo, GameId, GameInfo, InstallInfo, InstallInfoQuery, InstallPlatform,
    InstallRequest, LaunchPlan, LaunchRequest, LoginFlow, OpContext, Outcome, Paths, RemoveArgs,
    Runner, Store, UpdateRequest, UserInfo, async_trait,
};
use irori_services::Backend;
use irori_ui::models;

use crate::filter::InstallFilter;
use crate::view::LibraryView;

struct MemoryStore {
    games: Vec<Arc<GameInfo>>,
}

impl MemoryStore {
    fn new() -> Arc<Self> {
        let game = |app: &str, title: &str, installed: bool| {
            // Hades is installed in a folder that does not exist.
            let install = (app == "3").then(|| irori_core::InstalledInfo {
                install_path: "/nonexistent/irori-library-test/hades".into(),
                ..Default::default()
            });
            Arc::new(
                GameInfo {
                    runner: Some(Runner::Gog),
                    app_name: app.into(),
                    title: title.into(),
                    is_installed: installed,
                    installable: true,
                    install,
                    ..Default::default()
                }
                .finalize(),
            )
        };
        Arc::new(Self {
            games: vec![
                game("1", "Alan Wake", false),
                game("2", "Celeste", true),
                game("3", "Hades", true),
                game("4", "Inside", false),
                game("5", "Outer Wilds", true),
            ],
        })
    }
}

#[async_trait]
impl Store for MemoryStore {
    fn runner(&self) -> Runner {
        Runner::Gog
    }
    async fn init(&self) -> Result<()> {
        Ok(())
    }
    fn user(&self) -> Option<UserInfo> {
        Some(UserInfo {
            account_id: "1".into(),
            display_name: "Tester".into(),
        })
    }
    fn login_flow(&self) -> LoginFlow {
        LoginFlow::None
    }
    async fn login(&self, _: &str) -> Result<UserInfo> {
        bail!("not supported")
    }
    async fn logout(&self) -> Result<()> {
        Ok(())
    }
    async fn refresh(&self, _: bool) -> Result<()> {
        Ok(())
    }
    fn games(&self) -> Vec<Arc<GameInfo>> {
        self.games.clone()
    }
    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        self.games.iter().find(|g| g.app_name == app_name).cloned()
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
        bail!("not supported")
    }
    async fn updatable_games(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    async fn install(&self, _: &str, _: &InstallRequest, _: OpContext) -> Result<Outcome> {
        bail!("not supported")
    }
    async fn update(&self, _: &str, _: &UpdateRequest, _: OpContext) -> Result<Outcome> {
        bail!("not supported")
    }
    async fn repair(&self, _: &str, _: OpContext) -> Result<Outcome> {
        bail!("not supported")
    }
    async fn uninstall(&self, _: &str, _: RemoveArgs) -> Result<()> {
        bail!("not supported")
    }
    async fn import(&self, _: &str, _: &Path, _: InstallPlatform) -> Result<()> {
        bail!("not supported")
    }
    async fn set_install_path(&self, _: &str, _: &Path) -> Result<()> {
        bail!("not supported")
    }
    async fn launch(&self, _: &str, _: &LaunchRequest, _: OpContext) -> Result<LaunchPlan> {
        bail!("not supported")
    }
}

/// Keep every file the backend writes in a private temporary directory.
fn isolate_paths() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let root = std::env::temp_dir().join(format!("irori-library-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Paths::init(Paths::under(&root));
    });
}

/// Start a backend over the in-memory store, install the UI layer and wait
/// until the library model holds the store's games.
fn start(cx: &mut TestAppContext) -> Backend {
    isolate_paths();
    cx.executor().allow_parking();
    let store: DynStore = MemoryStore::new();
    let backend = Backend::start(move |_, _| vec![store]).expect("backend starts");
    cx.update(|cx| {
        gpui_kit::init(cx);
        irori_ui::init(backend.clone(), cx);
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        let len = cx.read(|cx| models(cx).library.read(cx).snapshot.len());
        if len == 5 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "library never loaded ({len} games)"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    backend
}

#[gpui_kit::test]
fn keyboard_navigation_filters_and_search(cx: &mut TestAppContext) {
    let _backend = start(cx);
    let (window, view) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(1280.), px(800.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| LibraryView::new(window, cx)),
            )
        })
        .expect("window opens");
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    draw(cx);

    let titles = |cx: &mut TestAppContext| -> Vec<String> {
        cx.read(|cx| {
            view.read(cx)
                .visible_games()
                .iter()
                .map(|g| g.title.clone())
                .collect()
        })
    };
    assert_eq!(
        titles(cx),
        vec!["Alan Wake", "Celeste", "Hades", "Inside", "Outer Wilds"]
    );

    // Focus the grid and move with the arrow keys: the first key focuses the
    // first game, Right moves along the row, End jumps to the last game.
    cx.update_window(window, |_, window, cx| {
        let grid = view.read(cx).grid_focus_handle();
        grid.focus(window, cx);
    })
    .unwrap();
    draw(cx);
    cx.simulate_keystrokes(window, "down right");
    draw(cx);
    let focused = |cx: &mut TestAppContext| cx.read(|cx| view.read(cx).focused_game().cloned());
    assert_eq!(focused(cx), Some(GameId::new(Runner::Gog, "2")));
    cx.simulate_keystrokes(window, "end");
    assert_eq!(focused(cx), Some(GameId::new(Runner::Gog, "5")));

    // "/" moves focus to the search field; typing filters instantly.
    cx.simulate_keystrokes(window, "/");
    draw(cx);
    let search_focused = cx
        .update_window(window, |_, window, cx| {
            view.read(cx).search_focus_handle(cx).is_focused(window)
        })
        .unwrap();
    assert!(search_focused, "search field has focus after /");
    cx.simulate_input(window, "hades");
    draw(cx);
    assert_eq!(titles(cx), vec!["Hades"]);

    // Escape clears the search; filters still apply and are counted.
    cx.simulate_keystrokes(window, "escape");
    draw(cx);
    assert_eq!(titles(cx).len(), 5);
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.update_filter(|f| f.install = InstallFilter::Installed, cx)
        })
    });
    draw(cx);
    assert_eq!(titles(cx), vec!["Celeste", "Hades", "Outer Wilds"]);
    assert_eq!(cx.read(|cx| view.read(cx).filter(cx).active_count()), 1);

    // Hiding games with missing files drops Hades once the background
    // availability check finished.
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.update_filter(|f| f.hide_unavailable = true, cx)
        })
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while titles(cx).len() != 2 {
        assert!(Instant::now() < deadline, "availability never resolved");
        std::thread::sleep(Duration::from_millis(10));
        draw(cx);
    }
    assert_eq!(titles(cx), vec!["Celeste", "Outer Wilds"]);
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.update_filter(|f| f.hide_unavailable = false, cx)
        })
    });
    draw(cx);

    // Enter in the search field moves into the results.
    cx.simulate_keystrokes(window, "ctrl-f");
    cx.simulate_input(window, "wilds");
    cx.simulate_keystrokes(window, "enter");
    draw(cx);
    assert_eq!(focused(cx), Some(GameId::new(Runner::Gog, "5")));
    let grid_focused = cx
        .update_window(window, |_, window, cx| {
            view.read(cx).grid_focus_handle().is_focused(window)
        })
        .unwrap();
    assert!(grid_focused);

    // The view's root is focusable for the shell.
    let _ = cx.read(|cx| view.read(cx).focus_handle(cx));
}

#[gpui_kit::test]
fn picking_games_for_bulk_actions(cx: &mut TestAppContext) {
    let _backend = start(cx);
    let (window, view) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(1280.), px(800.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| LibraryView::new(window, cx)),
            )
        })
        .expect("window opens");
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    draw(cx);
    let state = |cx: &mut TestAppContext| cx.read(|cx| view.read(cx).selection_state());
    let game = |app: &str| GameId::new(Runner::Gog, app);
    let ctrl = gpui_kit::Modifiers {
        control: !cfg!(target_os = "macos"),
        platform: cfg!(target_os = "macos"),
        ..Default::default()
    };
    let shift = gpui_kit::Modifiers {
        shift: true,
        ..Default::default()
    };
    let click = |cx: &mut TestAppContext, app: &str, m: gpui_kit::Modifiers| {
        cx.update(|cx| view.update(cx, |v, cx| v.click_for_test(&game(app), m, cx)));
    };

    // Ctrl-click starts selection mode; Shift-click picks the range.
    click(cx, "2", ctrl);
    click(cx, "4", shift);
    draw(cx);
    assert_eq!(
        state(cx),
        (
            vec!["Celeste".into(), "Hades".into(), "Inside".into()],
            true
        )
    );
    // In selection mode a plain click toggles instead of opening the game.
    click(cx, "3", Default::default());
    assert_eq!(state(cx).0, ["Celeste", "Inside"]);

    // Keyboard (focus follows the last click): Home moves to the first game,
    // Space picks it, Ctrl+A picks all, Escape ends.
    cx.update_window(window, |_, window, cx| {
        view.read(cx).grid_focus_handle().focus(window, cx);
    })
    .unwrap();
    draw(cx);
    cx.simulate_keystrokes(window, "home space");
    draw(cx);
    assert_eq!(state(cx).0, ["Alan Wake", "Celeste", "Inside"]);
    cx.simulate_keystrokes(window, "ctrl-a");
    draw(cx);
    assert_eq!(state(cx).0.len(), 5);

    // A filter hiding picked games drops them from the selection.
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.update_filter(|f| f.install = InstallFilter::Installed, cx)
        })
    });
    draw(cx);
    assert_eq!(state(cx).0, ["Celeste", "Hades", "Outer Wilds"]);

    cx.simulate_keystrokes(window, "escape");
    draw(cx);
    assert_eq!(state(cx), (Vec::new(), false));
}
