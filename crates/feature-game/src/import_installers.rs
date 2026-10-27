//! "Import installers": pick a folder of GOG offline installers and install
//! games from them instead of downloading. Each installer is matched to an
//! owned game (by the product id inside it, else by its file name); an
//! unrecognised one can be assigned by hand.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, rems,
};
use irori_core::{GameId, InstallRequest, Runner};
use irori_gog::FoundInstaller;
use irori_ui::i18n::{t, t_args};
use irori_ui::{backend, models};

use crate::hooks::gog_store;
use crate::i18n::platform_label;
use crate::ops::{notify_error, pick_path};

/// Ask for a folder, look for GOG installers in it and list them.
pub fn open_import_installers_dialog(window: &mut Window, cx: &mut App) {
    let Some(gog) = gog_store(cx) else {
        return;
    };
    let pick = pick_path(window, cx, true, t("game.import.pick_folder"));
    window
        .spawn(cx, async move |cx| {
            let Some(folder) = pick.await else { return };
            let scan_folder = folder.clone();
            let task = cx.update(|_, cx| {
                irori_ui::run(cx, async move { gog.scan_installers(&scan_folder).await })
            });
            let Ok(task) = task else { return };
            let result = task.await;
            let _ = cx.update(|window, cx| match result {
                Ok(found) => show(folder, found, window, cx),
                Err(e) => notify_error(window, cx, t("game.import.scan_failed"), &e),
            });
        })
        .detach();
}

fn show(folder: PathBuf, found: Vec<FoundInstaller>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|_| ImportInstallers {
        folder,
        rows: found
            .into_iter()
            .map(|f| Row {
                choice: f.app_name.clone(),
                found: f,
            })
            .collect(),
    });
    let width = window.rem_size() * 44.0;
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t("game.import.title"))
            .w(width)
            .child(view.clone())
    });
}

struct Row {
    found: FoundInstaller,
    /// The game it installs: the match, or the user's choice.
    choice: Option<String>,
}

struct ImportInstallers {
    folder: PathBuf,
    rows: Vec<Row>,
}

/// Whether a game can be installed from an installer right now.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Readiness {
    Ready,
    NoGame,
    MissingTool(&'static str),
    Installed,
    Queued,
}

impl ImportInstallers {
    fn readiness(row: &Row, cx: &App) -> Readiness {
        if let Some(tool) = row.found.missing_tool {
            return Readiness::MissingTool(tool);
        }
        let Some(app) = &row.choice else {
            return Readiness::NoGame;
        };
        let id = GameId::new(Runner::Gog, app.as_str());
        if backend(cx).is_queued(&id) {
            Readiness::Queued
        } else if models(cx)
            .library
            .read(cx)
            .snapshot
            .get(&id)
            .is_some_and(|g| g.is_installed)
        {
            Readiness::Installed
        } else {
            Readiness::Ready
        }
    }

    /// Queue the install of row `ix`.
    fn install(&self, ix: usize, cx: &mut App) {
        let Some(row) = self.rows.get(ix) else { return };
        let Some(app) = row.choice.clone() else {
            return;
        };
        let id = GameId::new(Runner::Gog, app.as_str());
        let title = title_of(&id, cx);
        let base = PathBuf::from(&models(cx).settings.read(cx).app.default_install_path);
        let request = InstallRequest {
            path: base,
            platform: row.found.platform,
            installer: Some(row.found.path.clone()),
            ..Default::default()
        };
        backend(cx).enqueue_install(id, title, request, 0);
    }

    fn render_row(&self, ix: usize, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let file = file_name(&row.found.path);
        let readiness = Self::readiness(row, cx);
        let game: AnyElement = match &row.choice {
            Some(app) => div()
                .truncate()
                .child(title_of(&GameId::new(Runner::Gog, app.as_str()), cx))
                .into_any_element(),
            None => {
                let choices = uninstalled_games(cx);
                let view = cx.entity().downgrade();
                Button::new(("import-choose", ix))
                    .outline()
                    .xsmall()
                    .label(t("game.import.choose_game"))
                    .dropdown_menu(move |mut menu, _, _| {
                        for (app, title) in &choices {
                            let (view, app) = (view.clone(), app.clone());
                            menu = menu.item(PopupMenuItem::new(title.clone()).on_click(
                                move |_, _, cx| {
                                    let app = app.clone();
                                    let _ = view.update(cx, |this, cx| {
                                        if let Some(row) = this.rows.get_mut(ix) {
                                            row.choice = Some(app);
                                        }
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        menu
                    })
                    .into_any_element()
            }
        };
        let status: AnyElement = match readiness {
            Readiness::Ready => Button::new(("import-install", ix))
                .small()
                .primary()
                .icon(IconName::Download)
                .label(t("game.import.install"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.install(ix, cx);
                    cx.notify();
                }))
                .into_any_element(),
            Readiness::Installed => note(t("game.import.installed"), muted),
            Readiness::Queued => note(t("game.import.queued"), muted),
            Readiness::NoGame => note(t("game.import.not_recognised"), muted),
            Readiness::MissingTool(tool) => note(
                t_args("game.import.needs_tool", &[("tool", tool)]),
                theme.warning,
            ),
        };
        h_flex()
            .id(ElementId::Name(format!("import-row-{ix}").into()))
            .w_full()
            .gap_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().font_medium().truncate().child(game))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(format!("{} · {file}", platform_label(row.found.platform))),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .justify_end()
                    .w(rems(14.))
                    .child(status),
            )
            .into_any_element()
    }
}

impl Render for ImportInstallers {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let ready: Vec<usize> = (0..self.rows.len())
            .filter(|ix| Self::readiness(&self.rows[*ix], cx) == Readiness::Ready)
            .collect();
        let rows: Vec<AnyElement> = (0..self.rows.len())
            .map(|ix| {
                let row = &self.rows[ix];
                self.render_row(ix, row, cx)
            })
            .collect();
        v_flex()
            .gap_3()
            .child(div().text_sm().text_color(muted).child(t_args(
                "game.import.found_in",
                &[("folder", &self.folder.display().to_string())],
            )))
            .map(|d| {
                if rows.is_empty() {
                    d.child(div().text_sm().py_4().child(t("game.import.none_found")))
                } else {
                    d.child(
                        v_flex()
                            .id("import-rows")
                            .max_h(rems(26.))
                            .overflow_y_scroll()
                            .children(rows),
                    )
                }
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("import-close")
                            .ghost()
                            .label(t("game.import.close"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("import-all")
                            .primary()
                            .disabled(ready.len() < 2)
                            .label(t_args(
                                "game.import.install_all",
                                &[("count", &ready.len().to_string())],
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                for ix in &ready {
                                    this.install(*ix, cx);
                                }
                                cx.notify();
                            })),
                    ),
            )
    }
}

fn note(text: SharedString, color: gpui_kit::Hsla) -> AnyElement {
    div()
        .text_xs()
        .text_color(color)
        .text_right()
        .child(text)
        .into_any_element()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn title_of(id: &GameId, cx: &App) -> String {
    models(cx)
        .library
        .read(cx)
        .snapshot
        .get(id)
        .map(|g| g.title.clone())
        .unwrap_or_else(|| id.app_name.to_string())
}

/// Owned GOG games that are not installed, by title, for assigning an
/// unrecognised installer.
fn uninstalled_games(cx: &App) -> Vec<(String, String)> {
    let library = models(cx).library.read(cx).snapshot.clone();
    let mut games: Vec<(String, String)> = library
        .games
        .iter()
        .filter(|g| g.runner() == Runner::Gog && !g.is_installed)
        .map(|g| (g.app_name.clone(), g.title.clone()))
        .collect();
    games.sort_by_key(|g| g.1.to_lowercase());
    games
}
