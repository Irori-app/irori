#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
//! Ember: a fast, native game launcher for Linux.

mod actions;
mod cli;
mod headless;
mod import;
mod instance;
mod pages;
mod palette;
mod playing;
mod routing;
mod shell;
mod sidebar;
mod stores;
mod strings;
#[cfg(target_os = "linux")]
mod tray;
#[cfg(not(target_os = "linux"))]
#[path = "tray_native.rs"]
mod tray;

use std::sync::Arc;

use anyhow::Result;
use ember_core::{GameId, Paths};
use ember_services::Backend;
use ember_ui::{Route, models::models, nav};
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::component::{Root, TitleBar};
use gpui_kit::{
    App, AppContext as _, Bounds, Focusable as _, QuitMode, WindowBounds, WindowDecorations,
    WindowHandle, WindowOptions, px, size,
};
use parking_lot::Mutex;

use crate::cli::Cli;
use crate::shell::AppShell;

/// Requests from outside the GPUI thread (tray, second instances).
#[derive(Debug, Clone)]
pub enum ShellCommand {
    Show,
    Quit,
    Launch(GameId),
    BigPicture,
    Args(Vec<String>),
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = Cli::parse(args.clone());
    if cli.help {
        println!("{}\n\n{}", cli::HELP, headless::HELP);
        return Ok(());
    }
    // A bare word is a command; never fall through to opening the window
    // for a mistyped one.
    if let Some(first) = args
        .first()
        .filter(|a| !a.starts_with('-') && !a.contains("://"))
        && !headless::COMMANDS.contains(&first.as_str())
    {
        eprintln!("ember: unknown command '{first}'\n\n{}", headless::HELP);
        std::process::exit(2);
    }
    if args
        .first()
        .is_some_and(|a| headless::COMMANDS.contains(&a.as_str()))
    {
        init_logging(false);
        let code = headless::run(&args)?;
        std::process::exit(code);
    }
    if cli.version {
        println!("ember {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let Some(listener) = instance::acquire(&args)? else {
        // Forwarded to the running instance.
        return Ok(());
    };
    init_logging(true);
    tracing::info!("Ember {} starting", env!("CARGO_PKG_VERSION"));
    limit_malloc_arenas();

    let backend = Backend::start(stores::build)?;
    let (tx, rx) = mpsc::unbounded::<ShellCommand>();
    {
        let tx = tx.clone();
        instance::serve(listener, move |args| {
            let _ = tx.unbounded_send(ShellCommand::Args(args));
        })?;
    }

    let application = gpui_kit::application().with_assets(gpui_kit::assets::AllAssets);
    let url_tx = tx.clone();
    application.on_open_urls(move |urls| {
        let _ = url_tx.unbounded_send(ShellCommand::Args(urls));
    });
    let reopen_tx = tx.clone();
    application.on_reopen(move |_| {
        let _ = reopen_tx.unbounded_send(ShellCommand::Show);
    });
    application.run(move |cx| {
        gpui_kit::init(cx);
        ember_ui::init(backend.clone(), cx);
        ember_ui::i18n::register(strings::CATALOGS);
        ember_ui::hooks::install(stores::hooks(), cx);
        ember_bigpicture::init(cx);
        ember_library::init(cx);
        ember_manage::init(cx);
        ember_settings::init(cx);
        ember_game::init(cx);
        ember_game::install_hooks(stores::game_hooks(), cx);
        ember_settings::install_store_handles(stores::EPIC.get().cloned(), cx);
        ember_game::install_store_handles(
            stores::EPIC.get().cloned(),
            stores::GOG.get().cloned(),
            cx,
        );
        if let Some(sideload) = stores::SIDELOAD.get() {
            ember_library::install_sideload_store(sideload.clone(), cx);
        }
        actions::bind_keys(cx);
        cx.set_quit_mode(QuitMode::Explicit);
        let prefs = backend.settings.app().ui.clone();
        ember_ui::theme::apply(&prefs, None, cx);

        let app = Arc::new(Mutex::new(AppHandles::default()));
        start_tray(&backend, tx.clone(), app.clone(), cx);

        let start_hidden = (cli.minimized || backend.settings.app().start_in_tray)
            && app.lock().tray.is_some()
            && !cli.has_request();
        if !start_hidden {
            open_main_window(&app, cx);
            if backend.settings.app().start_in_console_mode {
                nav::navigate(Route::BigPicture, cx);
            }
        }
        handle_cli(&cli, &app, cx);
        listen(rx, app.clone(), cx);
        install_quit_on_close(app, cx);
    });
    instance::release();
    Ok(())
}

/// glibc gives every thread that allocates concurrently its own arena (up to
/// 8 × cores) and rarely returns their memory; image decoding on the worker
/// threads would otherwise leave tens of MiB stranded in each.
fn limit_malloc_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: mallopt only adjusts allocator tunables; called before other threads allocate.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
    }
}

#[derive(Default)]
struct AppHandles {
    window: Option<WindowHandle<Root>>,
    tray: Option<tray::TrayHandle>,
}

/// Log to stderr and the launcher log file. Headless commands (`gui = false`)
/// default to warnings only so their output stays readable.
fn init_logging(gui: bool) {
    use tracing_subscriber::prelude::*;
    let default = if gui { "info" } else { "warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_env("EMBER_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let file = gui
        .then(|| {
            let path = Paths::get().launcher_log_file();
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            // Keep the previous session's log for bug reports.
            let _ = std::fs::rename(&path, path.with_extension("log.old"));
            std::fs::File::create(&path).ok()
        })
        .flatten();
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    match file {
        Some(f) => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(std::sync::Mutex::new(f)),
            )
            .init(),
        None => registry.init(),
    }
}

fn open_main_window(app: &Arc<Mutex<AppHandles>>, cx: &mut App) {
    let existing = app.lock().window;
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let prefs = models(cx).settings.read(cx).app.ui.clone();
    let bounds = Bounds::centered(
        None,
        size(
            px(prefs.window_width.max(960.)),
            px(prefs.window_height.max(600.)),
        ),
        cx,
    );
    let options = WindowOptions {
        window_bounds: Some(if prefs.window_maximized {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        }),
        window_min_size: Some(size(px(800.), px(540.))),
        app_id: Some("ember".into()),
        // Ember draws its own title bar; ask Wayland compositors not to add one.
        window_decorations: Some(WindowDecorations::Client),
        ..TitleBar::window_options()
    };
    let route = shell::initial_route(cx);
    nav::navigator(cx).update(cx, |n, cx| n.replace(route, cx));
    match gpui_kit::open_window(options, cx, |window, cx| {
        let shell = cx.new(|cx| AppShell::new(window, cx));
        let focus = shell.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        shell
    }) {
        Ok((handle, _)) => {
            app.lock().window = handle.downcast::<Root>();
        }
        Err(e) => tracing::error!("opening window: {e:#}"),
    }
}

fn handle_cli(cli: &Cli, app: &Arc<Mutex<AppHandles>>, cx: &mut App) {
    if let Some(id) = &cli.open {
        open_main_window(app, cx);
        nav::navigate(Route::Game(id.clone()), cx);
    }
    if let Some(route) = &cli.route {
        open_main_window(app, cx);
        nav::navigate(route.clone(), cx);
    }
    if cli.big_picture {
        open_main_window(app, cx);
        nav::navigate(Route::BigPicture, cx);
    }
    if let Some(id) = &cli.launch {
        launch_from_outside(id.clone(), app, cx);
    }
}

/// Launch requested by a URL, shortcut or the tray.
fn launch_from_outside(id: GameId, app: &Arc<Mutex<AppHandles>>, cx: &mut App) {
    let hide = ember_ui::backend(cx)
        .settings
        .app()
        .hide_window_on_protocol_launch;
    let window = app.lock().window;
    match window {
        Some(handle) if !hide => {
            let _ = handle.update(cx, |_, window, cx| shell::launch_game(id, window, cx));
        }
        _ => {
            let b = ember_ui::backend(cx);
            let task = ember_ui::run(cx, async move { b.launch(&id, None, Vec::new()).await });
            cx.spawn(async move |_| {
                if let Err(e) = task.await {
                    tracing::error!("launch failed: {e:#}");
                }
            })
            .detach();
        }
    }
}

fn listen(
    mut rx: mpsc::UnboundedReceiver<ShellCommand>,
    app: Arc<Mutex<AppHandles>>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        while let Some(cmd) = rx.next().await {
            let app = app.clone();
            cx.update(|cx| match cmd {
                ShellCommand::Show => open_main_window(&app, cx),
                ShellCommand::Quit => cx.quit(),
                ShellCommand::Launch(id) => launch_from_outside(id, &app, cx),
                ShellCommand::BigPicture => {
                    open_main_window(&app, cx);
                    nav::navigate(Route::BigPicture, cx);
                }
                ShellCommand::Args(args) => {
                    let cli = Cli::parse(args);
                    if !cli.has_request() {
                        open_main_window(&app, cx);
                    }
                    handle_cli(&cli, &app, cx);
                }
            });
        }
    })
    .detach();
}

#[cfg(target_os = "linux")]
fn start_tray(
    backend: &Backend,
    tx: mpsc::UnboundedSender<ShellCommand>,
    app: Arc<Mutex<AppHandles>>,
    cx: &mut App,
) {
    let settings = backend.settings.app();
    if settings.no_tray_icon {
        return;
    }
    let dark = settings.dark_tray_icon;
    let task = ember_ui::state::run_infallible(cx, async move { tray::start(tx, dark).await });
    cx.spawn(async move |cx| {
        let Some(Some(handle)) = task.await else {
            return;
        };
        app.lock().tray = Some(handle.clone());
        cx.update(|cx| watch_recent_for_tray(handle, cx));
    })
    .detach();
}

#[cfg(not(target_os = "linux"))]
fn start_tray(
    backend: &Backend,
    tx: mpsc::UnboundedSender<ShellCommand>,
    app: Arc<Mutex<AppHandles>>,
    cx: &mut App,
) {
    let settings = backend.settings.app();
    if settings.no_tray_icon {
        return;
    }
    if let Some(handle) = tray::start(tx, settings.dark_tray_icon) {
        app.lock().tray = Some(handle.clone());
        watch_recent_for_tray(handle, cx);
    }
}

/// Keep the tray's "Recently played" menu in step with the library model.
fn watch_recent_for_tray(handle: tray::TrayHandle, cx: &mut App) {
    let library = models(cx).library.clone();
    let last: std::rc::Rc<std::cell::RefCell<Vec<GameId>>> = Default::default();
    let push = move |cx: &mut App| {
        let b = ember_ui::backend(cx);
        let recent: Vec<(GameId, String)> = b
            .recent_games(true)
            .iter()
            .map(|g| (g.id(), g.title.clone()))
            .collect();
        let ids: Vec<GameId> = recent.iter().map(|(id, _)| id.clone()).collect();
        if *last.borrow() == ids {
            return;
        }
        *last.borrow_mut() = ids;
        let h = handle.clone();
        #[cfg(target_os = "linux")]
        b.spawn(async move { tray::set_recent(&h, recent).await });
        #[cfg(not(target_os = "linux"))]
        tray::set_recent(&h, recent);
    };
    push(cx);
    cx.observe(&library, move |_, cx| push(cx)).detach();
}

/// Quit when the main window closes, unless the user keeps Ember in the tray.
fn install_quit_on_close(app: Arc<Mutex<AppHandles>>, cx: &mut App) {
    cx.on_window_closed(move |cx, _| {
        let keep = {
            let handles = app.lock();
            handles.tray.is_some() && ember_ui::backend(cx).settings.app().exit_to_tray
        };
        app.lock().window = None;
        if !keep && cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}
