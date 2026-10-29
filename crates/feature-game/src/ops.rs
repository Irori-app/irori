//! Game commands shared by every entry point (game page buttons, menus,
//! keyboard actions, and other features such as library tiles). Each command
//! reports failure to the user through a notification; none only logs.

use std::future::Future;
use std::path::PathBuf;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, PathPromptOptions, SharedString, Task, Window};
use irori_core::{ClientAction, GameId, LaunchOption, UpdateRequest};
use irori_extras::shortcuts::{self, ShortcutTargets};
use irori_services::LaunchParams;
use irori_ui::i18n::{t, t_args};
use irori_ui::{Route, backend, navigate};

/// Run a backend future; on failure show `failure` with the error. Resolves
/// to whether it succeeded, after `on_ok` ran.
pub(crate) fn run_reporting<T, F>(
    window: &Window,
    cx: &mut App,
    failure: impl Into<SharedString>,
    fut: F,
    on_ok: impl FnOnce(T, &mut Window, &mut App) + 'static,
) -> Task<bool>
where
    F: Future<Output = anyhow::Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let failure = failure.into();
    let task = irori_ui::run(cx, fut);
    window.spawn(cx, async move |cx| {
        let result = task.await;
        cx.update(|window, cx| match result {
            Ok(v) => {
                on_ok(v, window, cx);
                true
            }
            Err(e) => {
                notify_error(window, cx, failure, &e);
                false
            }
        })
        .unwrap_or(false)
    })
}

/// Show an error notification titled `title` with the error chain as message.
pub(crate) fn notify_error(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<SharedString>,
    error: &anyhow::Error,
) {
    window.push_notification(
        Notification::error(format!("{error:#}")).title(title.into()),
        cx,
    );
}

pub(crate) fn notify_success(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    window.push_notification(Notification::success(message.into()), cx);
}

fn title_of(id: &GameId, cx: &App) -> String {
    backend(cx)
        .game(id)
        .map(|g| g.title.clone())
        .unwrap_or_else(|| id.app_name.to_string())
}

/// Launch a game with an optional launch option (remembered as the last used).
pub fn play(
    id: &GameId,
    option: Option<LaunchOption>,
    window: &mut Window,
    cx: &mut App,
) -> Task<bool> {
    play_with(
        id,
        LaunchParams {
            option,
            ..Default::default()
        },
        window,
        cx,
    )
}

/// Launch a game with one-off choices: verbose logs for this session, or
/// without the store's update check.
pub fn play_with(
    id: &GameId,
    params: LaunchParams,
    window: &mut Window,
    cx: &mut App,
) -> Task<bool> {
    let b = backend(cx);
    let failure = t_args("game.ops.start_failed", &[("title", &title_of(id, cx))]);
    let id = id.clone();
    let task = {
        let (id, params) = (id.clone(), params.clone());
        irori_ui::run(cx, async move { b.launch_with(&id, params).await })
    };
    window.spawn(cx, async move |cx| {
        let result = task.await;
        cx.update(|window, cx| match result {
            Ok(()) => true,
            Err(e) => {
                // Saves that changed in both places need the user's choice.
                if !irori_ui::save_conflict::resolve_launch_error(&id, params, &e, window, cx) {
                    notify_error(window, cx, failure, &e);
                }
                false
            }
        })
        .unwrap_or(false)
    })
}

/// Stop a running game and every process in its prefix.
pub fn stop(id: &GameId, window: &mut Window, cx: &mut App) -> Task<bool> {
    let b = backend(cx);
    let title = title_of(id, cx);
    let id = id.clone();
    run_reporting(
        window,
        cx,
        t_args("game.ops.stop_failed", &[("title", &title)]),
        async move { b.stop(&id).await },
        |_, _, _| {},
    )
}

/// Queue the pending update of an installed game.
pub fn queue_update(id: &GameId, cx: &mut App) {
    let title = title_of(id, cx);
    backend(cx).enqueue_update(id.clone(), title, UpdateRequest::default());
}

/// Remove a game from the download queue or stop its running download.
/// `keep_files` keeps partial data so a later install resumes.
pub fn cancel_download(id: &GameId, keep_files: bool, cx: &mut App) {
    backend(cx).cancel_download(id, keep_files);
}

/// Pause the download queue (the running download stops and resumes later).
pub fn pause_downloads(cx: &mut App) {
    backend(cx).pause_downloads();
}

pub fn resume_downloads(cx: &mut App) {
    backend(cx).resume_downloads();
}

/// Verify installed files and redownload broken ones.
pub fn verify(id: &GameId, window: &mut Window, cx: &mut App) -> Task<bool> {
    let b = backend(cx);
    let title = title_of(id, cx);
    let id = id.clone();
    run_reporting(
        window,
        cx,
        t_args("game.ops.verify_failed", &[("title", &title)]),
        async move {
            if !b.is_online() {
                anyhow::bail!(t("game.ops.verify_offline"));
            }
            b.repair(&id).await
        },
        |_, _, _| {},
    )
}

pub fn set_favourite(id: &GameId, on: bool, cx: &mut App) {
    backend(cx).userdata.set_favourite(id, on);
}

pub fn set_hidden(id: &GameId, on: bool, cx: &mut App) {
    backend(cx).userdata.set_hidden(id, on);
}

/// Open the per-game settings page on `section`.
pub fn open_settings(id: &GameId, section: &'static str, cx: &mut App) {
    navigate(Route::GameSettings(id.clone(), section.into()), cx);
}

/// Open a folder or file with the desktop's default application.
pub(crate) fn open_path(path: PathBuf, window: &mut Window, cx: &mut App) {
    if let Err(e) = open::that_detached(&path) {
        let e = anyhow::Error::from(e).context(t_args(
            "game.ops.opening_path",
            &[("path", &path.display().to_string())],
        ));
        notify_error(window, cx, t("game.ops.open_failed"), &e);
    }
}

/// Open the install folder in the file manager.
pub fn open_install_folder(id: &GameId, window: &mut Window, cx: &mut App) {
    match backend(cx).game(id).and_then(|g| g.install_path()) {
        Some(path) => open_path(path, window, cx),
        None => notify_error(
            window,
            cx,
            t("game.ops.open_install_folder_failed"),
            &anyhow::anyhow!(t("game.ops.no_install_location")),
        ),
    }
}

/// Open the Wine prefix of a game in the file manager.
pub fn open_prefix_folder(id: &GameId, window: &mut Window, cx: &mut App) {
    let b = backend(cx);
    let Some(game) = b.game(id) else { return };
    let prefix = irori_runtime::prefix_path(&b.settings.app(), &b.settings.game(id), &game);
    open_path(prefix, window, cx);
}

/// Create desktop and application-menu shortcuts (`steam` adds a Steam
/// shortcut instead).
pub fn add_shortcuts(id: &GameId, steam: bool, window: &mut Window, cx: &mut App) -> Task<bool> {
    let b = backend(cx);
    let Some(game) = b.game(id) else {
        return Task::ready(false);
    };
    let targets = ShortcutTargets {
        desktop: !steam,
        app_menu: !steam,
        steam,
    };
    let title = game.title.clone();
    let done = if steam {
        "game.ops.steam_shortcut_added"
    } else {
        "game.ops.shortcuts_added"
    };
    run_reporting(
        window,
        cx,
        t_args("game.ops.shortcuts_add_failed", &[("title", &title)]),
        async move { shortcuts::add(&game, &b.settings.app(), targets).await },
        move |_, window, cx| notify_success(window, cx, t_args(done, &[("title", &title)])),
    )
}

/// Remove every shortcut Irori created for the game.
pub fn remove_shortcuts(id: &GameId, window: &mut Window, cx: &mut App) -> Task<bool> {
    let b = backend(cx);
    let Some(game) = b.game(id) else {
        return Task::ready(false);
    };
    let title = game.title.clone();
    run_reporting(
        window,
        cx,
        t_args("game.ops.shortcuts_remove_failed", &[("title", &title)]),
        async move { shortcuts::remove(&game, &b.settings.app()).await },
        move |_, window, cx| {
            notify_success(
                window,
                cx,
                t_args("game.ops.shortcuts_removed", &[("title", &title)]),
            )
        },
    )
}

/// Ask for a folder or file. Resolves to `None` when cancelled; errors are reported.
pub(crate) fn pick_path(
    window: &mut Window,
    cx: &mut App,
    directories: bool,
    prompt: SharedString,
) -> Task<Option<PathBuf>> {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: !directories,
        directories,
        multiple: false,
        prompt: Some(prompt),
    });
    window.spawn(cx, async move |cx| match rx.await {
        Ok(Ok(Some(mut paths))) => paths.pop(),
        Ok(Ok(None)) | Err(_) => None,
        Ok(Err(e)) => {
            let _ = cx.update(|window, cx| {
                notify_error(window, cx, t("game.ops.file_chooser_failed"), &e)
            });
            None
        }
    })
}

/// Pick a Windows program and run it inside the game's prefix.
pub fn run_exe_in_prefix(id: &GameId, window: &mut Window, cx: &mut App) {
    let id = id.clone();
    let pick = pick_path(window, cx, false, t("game.ops.pick_run"));
    window
        .spawn(cx, async move |cx| {
            let Some(exe) = pick.await else { return };
            let _ = cx.update(|window, cx| {
                let b = backend(cx);
                let argv = vec![exe.to_string_lossy().into_owned()];
                let id = id.clone();
                run_reporting(
                    window,
                    cx,
                    t_args(
                        "game.ops.run_failed",
                        &[("path", &exe.display().to_string())],
                    ),
                    async move { b.run_in_prefix(&id, argv).await.map(|_| ()) },
                    |_, _, _| {},
                )
                .detach();
            });
        })
        .detach();
}

/// Open `winecfg` for the game's prefix.
pub fn winecfg(id: &GameId, window: &mut Window, cx: &mut App) -> Task<bool> {
    let b = backend(cx);
    let id = id.clone();
    run_reporting(
        window,
        cx,
        t("game.ops.winecfg_failed"),
        async move {
            b.run_in_prefix(&id, vec!["winecfg".into()])
                .await
                .map(|_| ())
        },
        |_, _, _| {},
    )
}

/// Open the Winetricks GUI for the game's prefix.
pub fn winetricks(id: &GameId, window: &mut Window, cx: &mut App) -> Task<bool> {
    let b = backend(cx);
    let id = id.clone();
    run_reporting(
        window,
        cx,
        t("game.ops.winetricks_failed"),
        async move { b.winetricks(&id, Vec::new()).await.map(|_| ()) },
        |_, _, _| {},
    )
}

/// Open protontricks on the Proton prefix of a Steam game (Linux).
pub fn protontricks(id: &GameId, window: &mut Window, cx: &mut App) -> Task<bool> {
    let b = backend(cx);
    let id = id.clone();
    run_reporting(
        window,
        cx,
        t("game.ops.protontricks_failed"),
        async move { b.protontricks(&id, Vec::new()).await.map(|_| ()) },
        |_, _, _| {},
    )
}

/// Ask the client of a store that installs its own games (Steam) to install,
/// remove, verify or show `id`; the client opens its own window.
pub fn client_action(id: &GameId, action: ClientAction, window: &mut Window, cx: &mut App) {
    let b = backend(cx);
    let store = id.runner.store_name();
    let id = id.clone();
    run_reporting(
        window,
        cx,
        t_args("game.ops.client_failed", &[("store", store)]),
        async move { b.client_action(&id, action).await },
        |_, _, _| {},
    )
    .detach();
}

/// Point the install record at another folder without moving files (for
/// installs whose drive moved or that were copied elsewhere).
pub fn change_install_location(id: &GameId, window: &mut Window, cx: &mut App) {
    let id = id.clone();
    let pick = pick_path(window, cx, true, t("game.ops.pick_use_folder"));
    window
        .spawn(cx, async move |cx| {
            let Some(path) = pick.await else { return };
            let _ = cx.update(|window, cx| {
                let b = backend(cx);
                let title = title_of(&id, cx);
                let id = id.clone();
                run_reporting(
                    window,
                    cx,
                    t_args("game.ops.change_location_failed", &[("title", &title)]),
                    async move { b.set_install_path(&id, &path).await },
                    |_, _, _| {},
                )
                .detach();
            });
        })
        .detach();
}
