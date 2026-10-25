//! UI integration tests: real views in headless windows against a backend
//! whose files live in a temporary directory.

use std::sync::OnceLock;

use ember_core::{GameId, Paths, Runner};
use ember_services::Backend;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Entity, Focusable as _, TestAppContext, WindowOptions,
};
use serde_json::Value;

use crate::form::{FormTarget, SettingsForm};
use crate::view::{SettingsSection, SettingsView};

/// A fresh backend per test (its event bus wakes only that test's tasks);
/// all of them share one temporary settings directory.
fn test_backend() -> Backend {
    static ROOT: OnceLock<std::path::PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = tempfile::tempdir().expect("temp dir").keep();
        Paths::init(Paths::under(&root));
        root
    });
    Backend::start(|_, _| Vec::new()).expect("backend")
}

fn open<V: gpui_kit::Render>(
    cx: &mut TestAppContext,
    build: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::Context<V>) -> V + 'static,
) -> (AnyWindowHandle, Entity<V>) {
    let backend = test_backend();
    cx.update(|cx| {
        gpui_kit::init(cx);
        ember_ui::init(backend, cx);
        crate::init(cx);
    });
    let opened = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| build(window, cx))
        })
        .expect("open window")
    });
    cx.run_until_parked();
    opened
}

#[gpui_kit::test]
fn typing_in_search_filters_fields_across_sections(cx: &mut TestAppContext) {
    let (handle, view) = open(cx, |window, cx| {
        SettingsView::new("general".into(), window, cx)
    });
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.section(), SettingsSection::General);
        assert!(v.app_form().read(cx).visible_count() > 10);
    });

    // Focus the search field and type through the window's input path.
    cx.update_window(handle, |_, window, cx| {
        let search = view.read(cx).search_input();
        search.read(cx).focus_handle(cx).focus(window, cx);
    })
    .unwrap();
    cx.simulate_input(handle, "fsync");
    cx.run_until_parked();

    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.query(), "fsync");
        assert_eq!(v.app_form().read(cx).visible_count(), 0);
        // Fsync lives in the game defaults form, not the current section.
        assert_eq!(
            v.defaults_form().read(cx).visible_count(),
            usize::from(cfg!(target_os = "linux"))
        );
    });

    // Opening a section clears the query and restores the section filter.
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |v, cx| v.set_section("appearance", window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.query(), "");
        assert_eq!(v.section(), SettingsSection::Appearance);
        let form = v.app_form().read(cx);
        assert!(form.visible_count() >= 5);
    });
}

#[gpui_kit::test]
fn per_game_override_marks_and_resets(cx: &mut TestAppContext) {
    let id = GameId::new(Runner::Sideload, "ui-test-game");
    let target = FormTarget::Game(id.clone());
    let (handle, form) = open(cx, move |window, cx| SettingsForm::new(target, window, cx));
    let backend = cx.update(|cx| ember_ui::backend(cx));
    cx.update(|cx| assert_eq!(form.read(cx).overridden_count(), 0));

    cx.update_window(handle, |_, window, cx| {
        form.update(cx, |f, cx| {
            f.write_for_test("showFps", Value::Bool(true), window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert!(backend.settings.overrides(&id).is_overridden("showFps"));
    assert!(!backend.settings.app().game.show_fps);
    cx.update(|cx| assert_eq!(form.read(cx).overridden_count(), 1));

    // Nested gamescope field overrides the parent key only while it differs.
    cx.update_window(handle, |_, window, cx| {
        form.update(cx, |f, cx| {
            f.write_for_test("gamescope.fpsLimiter", Value::from("60"), window, cx)
        });
    })
    .unwrap();
    assert!(backend.settings.overrides(&id).is_overridden("gamescope"));
    assert_eq!(backend.settings.game(&id).gamescope.fps_limiter, "60");

    cx.update_window(handle, |_, window, cx| {
        form.update(cx, |f, cx| {
            f.reset_for_test("gamescope.fpsLimiter", window, cx);
            f.reset_for_test("showFps", window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert!(backend.settings.overrides(&id).0.is_empty());
    cx.update(|cx| assert_eq!(form.read(cx).overridden_count(), 0));
}

#[gpui_kit::test]
fn store_and_platform_specific_rows_follow_their_context(cx: &mut TestAppContext) {
    use crate::registry::FieldSection;

    // A game that isn't installed natively: no Steam runtime choice, but the
    // manual save sync row is there.
    let id = GameId::new(Runner::Sideload, "ui-test-visibility");
    let target = FormTarget::Game(id);
    let (_, form) = open(cx, move |window, cx| SettingsForm::new(target, window, cx));
    cx.update(|cx| {
        form.update(cx, |f, cx| f.set_query("steam linux runtime", cx));
        assert_eq!(form.read(cx).visible_count(), 0);
        form.update(cx, |f, cx| f.set_query("sync saves now", cx));
        assert_eq!(form.read(cx).visible_count(), 1);
    });

    // Without the Epic store handle the Epic group stays hidden, while the
    // controller rows are always offered.
    let (_, app_form) = open(cx, |window, cx| {
        SettingsForm::new(FormTarget::App, window, cx)
    });
    cx.update(|cx| {
        app_form.update(cx, |f, cx| f.set_query("eos overlay", cx));
        assert_eq!(app_form.read(cx).visible_count(), 0);
        app_form.update(cx, |f, cx| f.set_sections(vec![FieldSection::General], cx));
        let general = app_form.read(cx).visible_count();
        app_form.update(cx, |f, cx| f.set_query("controller", cx));
        assert_eq!(app_form.read(cx).visible_count(), 4);
        assert!(general > 20);
    });
}
