//! Command palette (Ctrl+K): jump to any game, play installed games, pick a
//! random one, and run window commands, all from the keyboard.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use ember_core::{GameId, GameInfo};
use ember_ui::i18n::{t, t_args};
use ember_ui::{Route, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme as _, IndexPath, WindowExt as _};
use gpui_kit::{App, AppContext as _, Entity, ParentElement as _, Styled as _, Window, px};

/// What confirming a palette row does.
#[derive(Clone)]
enum PaletteTarget {
    Open(GameId),
    Play(GameId),
    Route(Route),
    Refresh,
    Random,
    SetUpEmulation,
    LutrisScript,
}

/// Retained palette state, owned by the shell.
pub struct Palette {
    state: Entity<CommandState>,
    targets: Rc<std::cell::RefCell<Vec<PaletteTarget>>>,
    open: Rc<Cell<bool>>,
}

impl Palette {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {
            state: cx.new(|cx| CommandState::new(window, cx)),
            targets: Rc::default(),
            open: Rc::default(),
        }
    }

    fn items(games: &[Arc<GameInfo>], targets: &mut Vec<PaletteTarget>) -> Vec<CommandItem> {
        let mut items = Vec::with_capacity(games.len() * 2 + 8);
        let mut push =
            |item: CommandItem, target: PaletteTarget, targets: &mut Vec<PaletteTarget>| {
                items.push(item);
                targets.push(target);
            };
        for (label, icon, route) in [
            ("shell.nav.library", IconName::LibraryBig, Route::Library),
            ("shell.nav.downloads", IconName::Download, Route::Downloads),
            ("shell.nav.stores", IconName::Store, Route::Stores),
            ("shell.nav.stats", IconName::ChartColumn, Route::Stats),
            (
                if cfg!(target_os = "macos") {
                    "manage.wine.title"
                } else {
                    "shell.nav.wine"
                },
                IconName::Wine,
                Route::WineManager,
            ),
            ("shell.nav.emulators", IconName::Joystick, Route::Emulators),
            ("shell.nav.accounts", IconName::CircleUser, Route::Accounts),
            (
                "shell.nav.settings",
                IconName::Settings,
                Route::Settings("general".into()),
            ),
            (
                "shell.nav.big_picture",
                IconName::Gamepad2,
                Route::BigPicture,
            ),
        ] {
            if cfg!(windows) && route == Route::WineManager {
                continue;
            }
            push(
                CommandItem::new()
                    .label(t_args("shell.palette.go_to", &[("place", &t(label))]))
                    .keywords([t(label)])
                    .icon(icon),
                PaletteTarget::Route(route),
                targets,
            );
        }
        push(
            CommandItem::new()
                .label(t("shell.palette.set_up_emulation"))
                .keywords([t("shell.palette.set_up_emulation_keywords")])
                .icon(IconName::Joystick),
            PaletteTarget::SetUpEmulation,
            targets,
        );
        push(
            CommandItem::new()
                .label(t("shell.palette.lutris_script"))
                .keywords([t("shell.palette.lutris_script_keywords")])
                .icon(IconName::ScrollText),
            PaletteTarget::LutrisScript,
            targets,
        );
        push(
            CommandItem::new()
                .label(t("shell.palette.refresh"))
                .icon(IconName::RefreshCw),
            PaletteTarget::Refresh,
            targets,
        );
        push(
            CommandItem::new()
                .label(t("shell.palette.random"))
                .keywords([t("shell.palette.random_keywords")])
                .icon(IconName::Dices),
            PaletteTarget::Random,
            targets,
        );
        for g in games.iter().filter(|g| g.is_installed) {
            push(
                CommandItem::new()
                    .label(t_args("shell.palette.play", &[("title", &g.title)]))
                    .icon(IconName::Play)
                    .keywords([g.runner().short_name()]),
                PaletteTarget::Play(g.id()),
                targets,
            );
        }
        for g in games {
            push(
                CommandItem::new()
                    .label(g.title.clone())
                    .icon(IconName::Gamepad2)
                    .keywords([g.runner().short_name(), g.runner().store_name()]),
                PaletteTarget::Open(g.id()),
                targets,
            );
        }
        items
    }

    /// Open the palette as a dialog in `window`.
    pub fn open(&self, window: &mut Window, cx: &mut App) {
        if self.open.get() {
            self.state.update(cx, |s, cx| s.focus(window, cx));
            return;
        }
        self.open.set(true);
        let games = models(cx).library.read(cx).snapshot.games.clone();
        let mut targets = Vec::new();
        let items = Rc::new(Self::items(&games, &mut targets));
        *self.targets.borrow_mut() = targets;
        self.state.update(cx, |s, cx| s.set_query("", window, cx));

        let state = self.state.clone();
        let targets = self.targets.clone();
        let open = self.open.clone();
        let focus_on_mount = Rc::new(Cell::new(true));
        window.open_dialog(cx, move |dialog, _, _| {
            let state = state.clone();
            let targets = targets.clone();
            let items = items.clone();
            let open = open.clone();
            let focus_on_mount = focus_on_mount.clone();
            dialog
                .close_button(false)
                .p_0()
                .on_close(move |_, _, _| open.set(false))
                .content(move |content, window, cx| {
                    if focus_on_mount.replace(false) {
                        let state = state.clone();
                        window.defer(cx, move |window, cx| {
                            state.update(cx, |s, cx| s.focus(window, cx))
                        });
                    }
                    let targets = targets.clone();
                    content.child(
                        Command::new(&state)
                            .bordered(false)
                            .rounded(cx.theme().radius_lg)
                            .placeholder(t("shell.search.placeholder"))
                            .max_h(px(440.))
                            .items(items.iter().cloned())
                            .on_confirm(move |ix: IndexPath, window, cx| {
                                let target = targets.borrow().get(ix.row).cloned();
                                window.close_dialog(cx);
                                if let Some(t) = target {
                                    run_target(t, window, cx);
                                }
                            }),
                    )
                })
        });
    }
}

fn run_target(target: PaletteTarget, window: &mut Window, cx: &mut App) {
    match target {
        PaletteTarget::Open(id) => ember_ui::navigate(Route::Game(id), cx),
        PaletteTarget::Route(r) => ember_ui::navigate(r, cx),
        PaletteTarget::Refresh => ember_ui::models::refresh_library(false, cx),
        PaletteTarget::Random => ember_library::open_random_game(window, cx),
        PaletteTarget::SetUpEmulation => ember_manage::open_emulation_setup(window, cx),
        PaletteTarget::LutrisScript => ember_library::open_lutris_installer(None, window, cx),
        PaletteTarget::Play(id) => crate::shell::launch_game(id, window, cx),
    }
}
