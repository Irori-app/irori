//! The game's "More actions" menu. The same builder serves the game page
//! dropdown and other features' context menus, so every entry point offers
//! the same commands with the same labels and enabled states.

use ember_core::{GameId, GameInfo, InstallPlatform, Runner};
use ember_services::LaunchParams;
use ember_ui::i18n::t;
use ember_ui::{backend, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{App, WeakEntity, Window};

use super::{GamePage, GamePageEvent};
use crate::state::{ActionInputs, PrimaryAction};
use crate::{install, manage, ops, redmod};

/// Append every game command for `id` to `menu`. Use it as the builder of a
/// `DropdownMenu` or `ContextMenu` anywhere a game is shown.
pub fn game_menu(menu: PopupMenu, id: &GameId, window: &mut Window, cx: &mut App) -> PopupMenu {
    build_menu(menu, id, None, None, None, window, cx)
}

/// One-off launch variants offered next to Play: label key, icon, params.
pub(super) const PLAY_VARIANTS: [(&str, IconName, LaunchParamsFn); 2] = [
    ("game.menu.play_verbose", IconName::ScrollText, verbose),
    (
        "game.menu.play_skip_update",
        IconName::FastForward,
        skip_update_check,
    ),
];

pub(super) type LaunchParamsFn = fn(LaunchParams) -> LaunchParams;

fn verbose(p: LaunchParams) -> LaunchParams {
    LaunchParams { verbose: true, ..p }
}

fn skip_update_check(p: LaunchParams) -> LaunchParams {
    LaunchParams {
        skip_version_check: true,
        ..p
    }
}

/// Launch through the page when there is one (so it shows "Launching…"),
/// else directly.
fn launch(
    page: Option<&WeakEntity<GamePage>>,
    id: &GameId,
    params: LaunchParams,
    window: &mut Window,
    cx: &mut App,
) {
    match page.and_then(|p| p.upgrade()) {
        Some(page) => page.update(cx, |p, cx| p.play_with(params, window, cx)),
        None => ops::play_with(id, params, window, cx).detach(),
    }
}

fn idle(game: &GameInfo, cx: &App) -> bool {
    let m = models(cx);
    let status = m.statuses.read(cx).kind(&game.id());
    let queue = &m.downloads.read(cx).queue;
    let inputs = ActionInputs::from_game(game, status, queue, false);
    PrimaryAction::from_inputs(&inputs).is_idle()
}

/// `page` is the game page the menu belongs to; page-only entries (edit,
/// categories) need it.
pub(super) fn build_menu(
    mut menu: PopupMenu,
    id: &GameId,
    shortcuts_exist: Option<bool>,
    store_url: Option<String>,
    page: Option<WeakEntity<GamePage>>,
    _: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    let Some(game) = backend(cx).game(id) else {
        return menu;
    };
    let idle = idle(&game, cx);
    let sideload = id.runner == Runner::Sideload;
    let hidden = models(cx).library.read(cx).is_hidden(id);
    let item = |key: &str, icon: IconName| PopupMenuItem::new(t(key)).icon(icon);

    if !game.is_installed {
        if !sideload {
            let i = id.clone();
            menu = menu.item(
                item("game.common.install_ellipsis", IconName::Download)
                    .disabled(!idle)
                    .on_click(move |_, window, cx| {
                        install::open_install_sheet(i.clone(), window, cx)
                    }),
            );
            let i = id.clone();
            menu = menu.item(
                item("game.menu.import", IconName::FolderInput)
                    .disabled(!idle)
                    .on_click(move |_, window, cx| {
                        manage::open_import_dialog(i.clone(), window, cx)
                    }),
            );
            // Steam starts Ember for the shortcut, which offers the install.
            if game.installable {
                let i = id.clone();
                menu = menu.separator().item(
                    item("game.menu.add_to_steam", IconName::Gamepad2).on_click(
                        move |_, window, cx| ops::add_shortcuts(&i, true, window, cx).detach(),
                    ),
                );
            }
        }
    } else {
        let playable = matches!(
            PrimaryAction::from_inputs(&ActionInputs::from_game(
                &game,
                models(cx).statuses.read(cx).kind(id),
                &models(cx).downloads.read(cx).queue,
                false,
            )),
            PrimaryAction::Play | PrimaryAction::Update
        );
        // Steam starts its games itself: no Ember logs or update check.
        let variants = if id.runner.manages_installs() {
            &PLAY_VARIANTS[..]
        } else {
            &[]
        };
        for &(label, icon, params) in variants {
            let i = id.clone();
            let page = page.clone();
            menu = menu.item(item(label, icon).disabled(!playable).on_click(
                move |_, window, cx| {
                    launch(
                        page.as_ref(),
                        &i,
                        params(LaunchParams::default()),
                        window,
                        cx,
                    )
                },
            ));
        }
        menu = menu.separator();
        let store_managed =
            !sideload && game.third_party_managed_app.is_none() && id.runner.manages_installs();
        if store_managed {
            let i = id.clone();
            menu = menu.item(
                item("game.menu.verify", IconName::ShieldCheck)
                    .disabled(!idle)
                    .on_click(move |_, window, cx| ops::verify(&i, window, cx).detach()),
            );
            let i = id.clone();
            menu = menu.item(
                item("game.menu.update_now", IconName::CloudDownload)
                    .disabled(!idle || id.runner == Runner::Zoom)
                    .on_click(move |_, _, cx| ops::queue_update(&i, cx)),
            );
            if matches!(id.runner, Runner::Legendary | Runner::Gog) {
                let i = id.clone();
                menu = menu.item(
                    item("game.menu.modify", IconName::Layers)
                        .disabled(!idle)
                        .on_click(move |_, window, cx| {
                            install::open_modify_sheet(i.clone(), window, cx)
                        }),
                );
            }
            if redmod::applies(&game, cx) {
                let i = id.clone();
                menu = menu.item(
                    item("game.menu.redmod", IconName::Puzzle)
                        .disabled(!idle)
                        .on_click(move |_, window, cx| {
                            redmod::open_redmod_sheet(i.clone(), window, cx)
                        }),
                );
            }
            let i = id.clone();
            menu = menu.item(
                item("game.menu.move", IconName::FolderInput)
                    .disabled(!idle || id.runner == Runner::Zoom)
                    .on_click(move |_, window, cx| manage::open_move_dialog(i.clone(), window, cx)),
            );
            let i = id.clone();
            menu = menu.item(
                item("game.menu.change_location", IconName::FolderSearch)
                    .disabled(!idle)
                    .on_click(move |_, window, cx| ops::change_install_location(&i, window, cx)),
            );
            menu = menu.separator();
        }
        let i = id.clone();
        menu = menu.item(
            item("game.menu.open_install_folder", IconName::FolderOpen)
                .disabled(game.install_path().is_none())
                .on_click(move |_, window, cx| ops::open_install_folder(&i, window, cx)),
        );
        if id.runner.manages_installs() {
            let i = id.clone();
            menu = menu.item(
                item("game.menu.open_log", IconName::ScrollText)
                    .on_click(move |_, window, cx| crate::open_log_sheet(i.clone(), window, cx)),
            );
        }
        let uses_wine = game
            .installed_platform()
            .unwrap_or(InstallPlatform::Windows)
            .needs_wine();
        if uses_wine {
            menu = menu.separator();
            let i = id.clone();
            menu = menu.item(
                item("game.menu.open_prefix", IconName::Folder)
                    .on_click(move |_, window, cx| ops::open_prefix_folder(&i, window, cx)),
            );
            let i = id.clone();
            menu = menu.item(
                item("game.menu.run_exe", IconName::Terminal)
                    .disabled(!idle)
                    .on_click(move |_, window, cx| ops::run_exe_in_prefix(&i, window, cx)),
            );
            let i = id.clone();
            menu = menu.item(
                item("game.menu.winecfg", IconName::Wine)
                    .disabled(!idle)
                    .on_click(move |_, window, cx| ops::winecfg(&i, window, cx).detach()),
            );
            let i = id.clone();
            menu = menu.item(
                PopupMenuItem::new("Winetricks")
                    .icon(IconName::Wrench)
                    .disabled(!idle)
                    .on_click(move |_, window, cx| ops::winetricks(&i, window, cx).detach()),
            );
        }
        menu = menu.separator();
        if shortcuts_exist != Some(true) {
            let i = id.clone();
            menu =
                menu.item(item("game.menu.add_shortcuts", IconName::Link).on_click(
                    move |_, window, cx| ops::add_shortcuts(&i, false, window, cx).detach(),
                ));
        }
        if shortcuts_exist != Some(false) {
            let i = id.clone();
            menu = menu.item(
                item("game.menu.remove_shortcuts", IconName::Link2Off)
                    .on_click(move |_, window, cx| ops::remove_shortcuts(&i, window, cx).detach()),
            );
        }
        if id.runner != Runner::Steam {
            let i = id.clone();
            menu =
                menu.item(item("game.menu.add_to_steam", IconName::Gamepad2).on_click(
                    move |_, window, cx| ops::add_shortcuts(&i, true, window, cx).detach(),
                ));
        }
    }

    if let Some(page) = page.clone() {
        menu = menu.separator();
        let i = id.clone();
        let p = page.clone();
        menu = menu.item(
            item("game.menu.edit", IconName::Pencil).on_click(move |_, _, cx| {
                let _ = p.update(cx, |_, cx| cx.emit(GamePageEvent::RequestEdit(i.clone())));
            }),
        );
        menu = menu.item(
            item("game.menu.categories", IconName::Tags).on_click(move |_, _, cx| {
                let _ = page.update(cx, |_, cx| cx.emit(GamePageEvent::RequestCategories));
            }),
        );
    }
    let i = id.clone();
    menu = menu.item(
        item(
            if hidden {
                "game.menu.show_in_library"
            } else {
                "game.menu.hide_from_library"
            },
            if hidden {
                IconName::Eye
            } else {
                IconName::EyeOff
            },
        )
        .on_click(move |_, _, cx| ops::set_hidden(&i, !hidden, cx)),
    );
    if let Some(url) = store_url.filter(|u| !u.is_empty()) {
        menu = menu
            .item(PopupMenuItem::link(t("game.menu.store_page"), url).icon(IconName::ExternalLink));
    }
    if game.is_installed && id.runner.manages_installs() {
        menu = menu.separator();
        let i = id.clone();
        menu = menu.item(
            item("game.menu.uninstall", IconName::Trash)
                .disabled(!idle)
                .on_click(move |_, window, cx| {
                    manage::open_uninstall_dialog(i.clone(), window, cx)
                }),
        );
    }
    menu
}
