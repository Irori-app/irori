//! Bulk actions on the games picked in selection mode (install, uninstall,
//! add to Steam, categories, favourite and hide), from a bar floating over
//! the library.

use std::sync::Arc;

use ember_core::{GameId, GameInfo, RemoveArgs};
use ember_ui::i18n::{t, t_args};
use ember_ui::{backend, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, ThemeStyled as _,
    WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, Modifiers, ParentElement as _,
    SharedString, Styled as _, Window, div,
};

use super::LibraryView;

/// Titles listed in the uninstall confirmation before "and N more".
const LISTED: usize = 8;

impl LibraryView {
    /// A click on a game: Shift extends the selection, Ctrl (or any click in
    /// selection mode) picks it, a plain click opens its page.
    pub(super) fn click_game(&mut self, id: &GameId, modifiers: Modifiers, cx: &mut Context<Self>) {
        self.focused = Some(id.clone());
        if modifiers.shift {
            self.selection.extend_to(id, &self.frame.games);
        } else if modifiers.secondary() || self.selection.is_active() {
            self.selection.toggle(id);
        } else {
            ember_ui::navigate(ember_ui::Route::Game(id.clone()), cx);
            return;
        }
        cx.notify();
    }

    /// The toolbar's "Select" / "Done".
    pub(super) fn toggle_selection_mode(&mut self, cx: &mut Context<Self>) {
        if self.selection.is_active() {
            self.selection.end();
        } else {
            self.selection.start();
        }
        cx.notify();
    }

    pub(super) fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selection.select_all(&self.frame.games);
        cx.notify();
    }

    pub(super) fn end_selection(&mut self, cx: &mut Context<Self>) {
        if self.selection.is_active() {
            self.selection.end();
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    fn picked(&self) -> Vec<Arc<GameInfo>> {
        self.selection.games(&self.frame.games)
    }

    /// Queue every picked game that can be installed, with default options.
    fn install_picked(
        &mut self,
        games: Vec<Arc<GameInfo>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let b = backend(cx);
        let task = ember_ui::run(cx, async move {
            let mut failed = Vec::new();
            let mut queued = 0usize;
            for g in games {
                match b.install_with_defaults(&g.id()).await {
                    Ok(()) => queued += 1,
                    Err(e) => failed.push((g.title.clone(), format!("{e:#}"))),
                }
            }
            Ok((queued, failed))
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok((queued, failed)) = task.await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if queued > 0 {
                    window.push_notification(
                        Notification::success(t_args(
                            "library.bulk.install_queued",
                            &[("count", &queued.to_string())],
                        )),
                        cx,
                    );
                }
                for (title, error) in failed {
                    window.push_notification(
                        Notification::error(error)
                            .title(t_args("library.bulk.install_failed", &[("title", &title)])),
                        cx,
                    );
                }
                this.selection.end();
                cx.notify();
            });
        })
        .detach();
    }

    /// Add every picked game to Steam in one pass, installed or not.
    fn add_picked_to_steam(
        &mut self,
        games: Vec<Arc<GameInfo>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = backend(cx).settings.app();
        let task = ember_ui::run(cx, async move {
            ember_extras::shortcuts::add_to_steam(&games, &app).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(added) => window.push_notification(
                        Notification::success(t_args(
                            "library.bulk.steam_added",
                            &[("count", &added.to_string())],
                        )),
                        cx,
                    ),
                    Err(e) => crate::notify_error(t("library.bulk.steam_failed"), &e, window, cx),
                }
                this.selection.end();
                cx.notify();
            });
        })
        .detach();
    }

    /// Ask, then uninstall every picked installed game one after another.
    fn confirm_uninstall(
        &mut self,
        games: Vec<Arc<GameInfo>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = games.len();
        let heading = if count == 1 {
            t_args(
                "library.bulk.uninstall_title_one",
                &[("title", &games[0].title)],
            )
        } else {
            t_args(
                "library.bulk.uninstall_title_other",
                &[("count", &count.to_string())],
            )
        };
        let mut lines: Vec<String> = games.iter().take(LISTED).map(|g| g.title.clone()).collect();
        if count > LISTED {
            lines.push(
                t_args(
                    "library.bulk.and_more",
                    &[("count", &(count - LISTED).to_string())],
                )
                .to_string(),
            );
        }
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            let (games, view) = (games.clone(), view.clone());
            dialog.title(heading.clone()).child(
                v_flex()
                    .gap_4()
                    .child(
                        div()
                            .text_sm()
                            .child(t("library.bulk.uninstall_description")),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .text_sm()
                            .text_color(muted)
                            .children(lines.iter().map(|l| div().truncate().child(l.clone()))),
                    )
                    .child(
                        DialogFooter::new()
                            .child(
                                Button::new("bulk-uninstall-cancel")
                                    .ghost()
                                    .label(t("library.bulk.cancel"))
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("bulk-uninstall-confirm")
                                    .danger()
                                    .label(t("library.bulk.uninstall_confirm"))
                                    .on_click(move |_, window, cx| {
                                        window.close_dialog(cx);
                                        let games = games.clone();
                                        let _ = view.update(cx, |this, cx| {
                                            this.selection.end();
                                            cx.notify();
                                        });
                                        uninstall_all(games, window, cx);
                                    }),
                            ),
                    ),
            )
        });
    }

    /// The bar over the library while selection mode is on.
    pub(super) fn render_bulk_bar(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.selection.is_active() {
            return None;
        }
        let picked = self.picked();
        let user = models(cx).library.read(cx).user.clone();
        let states: Vec<_> = picked
            .iter()
            .map(|g| self.state_for(g, false, cx))
            .collect();
        let installable: Vec<Arc<GameInfo>> = picked
            .iter()
            .zip(&states)
            .filter(|(g, s)| {
                !s.installed
                    && s.installable
                    && !s.is_queued()
                    && !s.is_busy()
                    && !s.browser
                    && g.third_party_managed_app.is_none()
            })
            .map(|(g, _)| g.clone())
            .collect();
        let removable: Vec<Arc<GameInfo>> = picked
            .iter()
            .zip(&states)
            .filter(|(_, s)| s.installed && !s.is_playing() && !s.is_busy())
            .map(|(g, _)| g.clone())
            .collect();
        let for_steam: Vec<Arc<GameInfo>> = picked
            .iter()
            .zip(&states)
            .filter(|(_, s)| !s.browser && (s.installed || s.installable))
            .map(|(g, _)| g.clone())
            .collect();
        let ids: Vec<GameId> = picked.iter().map(|g| g.id()).collect();
        let keys: Vec<String> = ids.iter().map(GameId::key).collect();
        let all_favourite = !keys.is_empty() && keys.iter().all(|k| user.favourites.contains(k));
        let all_hidden = !keys.is_empty() && keys.iter().all(|k| user.hidden.contains(k));
        let categories: Vec<(String, bool)> = user
            .categories
            .iter()
            .map(|(name, games)| {
                (
                    name.clone(),
                    !keys.is_empty() && keys.iter().all(|k| games.contains(k)),
                )
            })
            .collect();

        let theme = cx.theme();
        let count = picked.len();
        let status: SharedString = match count {
            0 => t("library.bulk.none"),
            1 => t("library.bulk.count_one"),
            n => t_args("library.bulk.count_other", &[("count", &n.to_string())]),
        };
        let nothing = count == 0;

        let install = {
            let games = installable.clone();
            Button::new("bulk-install")
                .small()
                .primary()
                .icon(IconName::Download)
                .label(t_args(
                    "library.bulk.install",
                    &[("count", &installable.len().to_string())],
                ))
                .disabled(installable.is_empty())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.install_picked(games.clone(), window, cx)
                }))
        };
        let uninstall = {
            let games = removable.clone();
            Button::new("bulk-uninstall")
                .small()
                .outline()
                .icon(IconName::Trash)
                .label(t_args(
                    "library.bulk.uninstall",
                    &[("count", &removable.len().to_string())],
                ))
                .disabled(removable.is_empty())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.confirm_uninstall(games.clone(), window, cx)
                }))
        };
        let steam = {
            let games = for_steam.clone();
            Button::new("bulk-steam")
                .small()
                .outline()
                .icon(IconName::Gamepad2)
                .label(t_args(
                    "library.bulk.add_to_steam",
                    &[("count", &for_steam.len().to_string())],
                ))
                .disabled(for_steam.is_empty())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.add_picked_to_steam(games.clone(), window, cx)
                }))
        };
        let category_ids = ids.clone();
        let view = cx.entity().downgrade();
        let categories_button = Button::new("bulk-categories")
            .small()
            .outline()
            .icon(IconName::Tags)
            .label(t("library.bulk.categories"))
            .disabled(nothing)
            .dropdown_menu(move |mut menu, _, _| {
                for (name, all_in) in &categories {
                    let (ids, name, add) = (category_ids.clone(), name.clone(), !*all_in);
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(*all_in).on_click(
                        move |_, _, cx| backend(cx).userdata.set_category_many(&ids, &name, add),
                    ));
                }
                if !categories.is_empty() {
                    menu = menu.separator();
                }
                let view = view.clone();
                menu.item(
                    PopupMenuItem::new(t("library.bulk.manage_categories")).on_click(
                        move |_, window, cx| {
                            let _ = view.update(cx, |_, cx| cx.notify());
                            crate::dialogs::categories::open_categories_dialog(None, window, cx)
                        },
                    ),
                )
            });
        let favourite = {
            let ids = ids.clone();
            Button::new("bulk-favourite")
                .small()
                .ghost()
                .icon(if all_favourite {
                    IconName::StarOff
                } else {
                    IconName::Star
                })
                .label(if all_favourite {
                    t("library.bulk.unfavourite")
                } else {
                    t("library.bulk.favourite")
                })
                .disabled(nothing)
                .on_click(move |_, _, cx| backend(cx).userdata.set_favourites(&ids, !all_favourite))
        };
        let hide = Button::new("bulk-hide")
            .small()
            .ghost()
            .icon(if all_hidden {
                IconName::Eye
            } else {
                IconName::EyeOff
            })
            .label(if all_hidden {
                t("library.bulk.unhide")
            } else {
                t("library.bulk.hide")
            })
            .disabled(nothing)
            .on_click(move |_, _, cx| backend(cx).userdata.set_hidden_many(&ids, !all_hidden));

        Some(
            div()
                .absolute()
                .bottom_6()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .px_4()
                .child(
                    h_flex()
                        .id("library-bulk-bar")
                        .gap_2()
                        .px_3()
                        .py_2()
                        .flex_wrap()
                        .max_w_full()
                        .popover_style(cx)
                        .rounded(theme.radius_lg)
                        .child(div().px_1().text_sm().font_semibold().child(status))
                        .when(!nothing, |bar| {
                            bar.child(install)
                                .child(uninstall)
                                .child(steam)
                                .child(categories_button)
                                .child(favourite)
                                .child(hide)
                        })
                        .child(
                            Button::new("bulk-select-all")
                                .small()
                                .ghost()
                                .label(t("library.bulk.select_all"))
                                .on_click(cx.listener(|this, _, _, cx| this.select_all(cx))),
                        )
                        .child(
                            Button::new("bulk-done")
                                .small()
                                .ghost()
                                .label(t("library.bulk.done"))
                                .on_click(cx.listener(|this, _, _, cx| this.end_selection(cx))),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// Uninstall `games` one after another, reporting each failure.
fn uninstall_all(games: Vec<Arc<GameInfo>>, window: &mut Window, cx: &mut gpui_kit::App) {
    let b = backend(cx);
    let args = RemoveArgs {
        remove_prefix: false,
        delete_files: true,
        remove_settings: false,
    };
    let task = ember_ui::run(cx, async move {
        let mut failed = Vec::new();
        for g in games {
            if let Err(e) = b.uninstall(&g.id(), args).await {
                failed.push((g.title.clone(), format!("{e:#}")));
            }
        }
        Ok(failed)
    });
    window
        .spawn(cx, async move |cx| {
            let Ok(failed) = task.await else { return };
            let _ = cx.update(|window, cx| {
                for (title, error) in failed {
                    window.push_notification(
                        Notification::error(error).title(t_args(
                            "library.bulk.uninstall_failed",
                            &[("title", &title)],
                        )),
                        cx,
                    );
                }
            });
        })
        .detach();
}
