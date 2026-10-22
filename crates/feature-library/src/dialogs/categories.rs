//! Managing custom categories: create, rename and delete; choosing the
//! categories of one game; and the small "New category" dialog used from a
//! game's menu.
//!
//! Unlike Heroic, creating a category whose name already exists never wipes
//! its members, renaming onto an existing name is refused instead of
//! overwriting it, names are trimmed, and deleting a selected category only
//! drops it from the library's category filter (the library never switches
//! to "Uncategorized" on its own).

use std::cell::Cell;

use ember_core::GameId;
use ember_ui::i18n::{t, t_args};
use ember_ui::{backend, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div,
};

use super::{dialog_width, text};
use crate::filters_panel::active_filters;

/// Why a category name cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    Empty,
    Exists,
    Unchanged,
}

impl NameError {
    pub fn message(self) -> SharedString {
        match self {
            NameError::Empty => t("library.categories.name_empty"),
            NameError::Exists => t("library.categories.name_exists"),
            NameError::Unchanged => SharedString::default(),
        }
    }
}

/// Validate a new or renamed category name against the existing ones
/// (compared without regard to case). `renaming` is the current name when
/// renaming; changing only its case is allowed.
pub fn validate_name<'a>(
    raw: &str,
    existing: impl IntoIterator<Item = &'a String>,
    renaming: Option<&str>,
) -> Result<String, NameError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if renaming == Some(name) {
        return Err(NameError::Unchanged);
    }
    let lower = name.to_lowercase();
    let taken = existing
        .into_iter()
        .any(|e| e.to_lowercase() == lower && Some(e.as_str()) != renaming);
    if taken {
        return Err(NameError::Exists);
    }
    Ok(name.to_string())
}

fn existing_categories(cx: &App) -> Vec<String> {
    models(cx)
        .library
        .read(cx)
        .user
        .categories
        .keys()
        .cloned()
        .collect()
}

/// Run a user-data change on the backend runtime (it writes a file).
fn userdata(cx: &App, f: impl FnOnce(&ember_services::UserDataService) + Send + 'static) {
    let b = backend(cx);
    ember_ui::state::run_infallible(cx, async move { f(&b.userdata) }).detach();
}

/// The category manager.
pub struct CategoriesDialog {
    new_name: Entity<InputState>,
    editing: Option<(String, Entity<InputState>)>,
    confirm_delete: Option<String>,
    _subscriptions: Vec<Subscription>,
    _editing_subscription: Option<Subscription>,
}

impl CategoriesDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_name = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t("library.categories.new_placeholder"))
        });
        let library = models(cx).library.clone();
        let subscriptions = vec![
            cx.observe(&library, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &new_name,
                window,
                |this, _, event, window, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => this.create(window, cx),
                    _ => {}
                },
            ),
        ];
        Self {
            new_name,
            editing: None,
            confirm_delete: None,
            _subscriptions: subscriptions,
            _editing_subscription: None,
        }
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(name) = validate_name(&text(&self.new_name, cx), &existing_categories(cx), None)
        else {
            return;
        };
        userdata(cx, move |u| u.create_category(&name));
        self.new_name
            .update(cx, |s, cx| s.set_value(SharedString::default(), window, cx));
        cx.notify();
    }

    fn start_rename(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name.clone()));
        input.update(cx, |s, cx| s.focus(window, cx));
        self._editing_subscription =
            Some(
                cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => this.commit_rename(window, cx),
                    _ => {}
                }),
            );
        self.editing = Some((name, input));
        self.confirm_delete = None;
        cx.notify();
    }

    fn rename_result(&self, cx: &App) -> Option<(String, Result<String, NameError>)> {
        let (from, input) = self.editing.as_ref()?;
        Some((
            from.clone(),
            validate_name(&text(input, cx), &existing_categories(cx), Some(from)),
        ))
    }

    fn commit_rename(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some((from, Ok(to))) = self.rename_result(cx) else {
            return;
        };
        {
            let (from, to) = (from.clone(), to.clone());
            userdata(cx, move |u| u.rename_category(&from, &to));
        }
        if let Some(filters) = active_filters(cx) {
            filters.update(cx, |m, cx| m.update(|f| f.rename_category(&from, &to), cx));
        }
        self.editing = None;
        self._editing_subscription = None;
        cx.notify();
    }

    fn delete(&mut self, name: String, cx: &mut Context<Self>) {
        {
            let name = name.clone();
            userdata(cx, move |u| u.delete_category(&name));
        }
        if let Some(filters) = active_filters(cx) {
            filters.update(cx, |m, cx| m.update(|f| f.forget_category(&name), cx));
        }
        self.confirm_delete = None;
        cx.notify();
    }

    fn render_row(&self, name: &str, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let row_id = SharedString::from(format!("category-{name}"));
        let base = h_flex()
            .id(row_id)
            .w_full()
            .gap_2()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border);

        if let Some((from, input)) = self.editing.as_ref().filter(|(n, _)| n == name) {
            let result = validate_name(
                &text(input, cx),
                &existing_categories(cx),
                Some(from.as_str()),
            );
            let error = match result {
                Err(e @ (NameError::Exists | NameError::Empty)) => Some(e.message()),
                _ => None,
            };
            return base.child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(Input::new(input).small()))
                            .child(
                                Button::new("rename-save")
                                    .small()
                                    .primary()
                                    .label(t("library.categories.rename"))
                                    .disabled(result.is_err())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.commit_rename(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("rename-cancel")
                                    .small()
                                    .ghost()
                                    .label(t("library.common.cancel"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.editing = None;
                                        this._editing_subscription = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when_some(error, |this, e| {
                        this.child(div().text_xs().text_color(cx.theme().danger).child(e))
                    }),
            );
        }

        if self.confirm_delete.as_deref() == Some(name) {
            let delete_name = name.to_string();
            return base.child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .child(div().min_w_0().text_sm().child(t_args(
                        "library.categories.delete_confirm",
                        &[("name", name)],
                    )))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_shrink_0()
                            .child(
                                Button::new("delete-cancel")
                                    .small()
                                    .ghost()
                                    .label(t("library.common.cancel"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_delete = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("delete-confirm")
                                    .small()
                                    .danger()
                                    .label(t("library.categories.delete"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete(delete_name.clone(), cx)
                                    })),
                            ),
                    ),
            );
        }

        let rename_name = name.to_string();
        let delete_name = name.to_string();
        base.justify_between()
            .child(
                v_flex()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .truncate()
                            .child(name.to_string()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(match count {
                                1 => t("library.common.games_one"),
                                n => t_args(
                                    "library.common.games_other",
                                    &[("count", &n.to_string())],
                                ),
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .flex_shrink_0()
                    .child(
                        Button::new("rename")
                            .small()
                            .ghost()
                            .icon(IconName::Pencil)
                            .tooltip(t("library.categories.rename"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_rename(rename_name.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new("delete")
                            .small()
                            .ghost()
                            .icon(IconName::Trash)
                            .tooltip(t("library.categories.delete"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm_delete = Some(delete_name.clone());
                                this.editing = None;
                                cx.notify();
                            })),
                    ),
            )
    }
}

impl Render for CategoriesDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let categories: Vec<(String, usize)> = models(cx)
            .library
            .read(cx)
            .user
            .categories
            .iter()
            .map(|(n, s)| (n.clone(), s.len()))
            .collect();
        let candidate = validate_name(
            &text(&self.new_name, cx),
            categories.iter().map(|(n, _)| n),
            None,
        );
        let exists = candidate == Err(NameError::Exists);
        let mut rows = Vec::with_capacity(categories.len());
        for (name, count) in &categories {
            rows.push(self.render_row(name, *count, cx).into_any_element());
        }

        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(Input::new(&self.new_name)))
                            .child(
                                Button::new("create-category")
                                    .icon(IconName::Plus)
                                    .label(t("library.categories.create"))
                                    .disabled(candidate.is_err())
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.create(window, cx)),
                                    ),
                            ),
                    )
                    .when(exists, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(NameError::Exists.message()),
                        )
                    }),
            )
            .child(
                v_flex()
                    .id("category-list")
                    .max_h_96()
                    .overflow_y_scroll()
                    .when(categories.is_empty(), |this| {
                        this.child(
                            div()
                                .py_4()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(t("library.categories.empty_manager")),
                        )
                    })
                    .children(rows),
            )
    }
}

/// Open the category manager (`None`), or the categories of one game.
pub fn open_categories_dialog(game: Option<&GameId>, window: &mut Window, cx: &mut App) {
    match game {
        Some(id) => open_game_categories_dialog(id.clone(), window, cx),
        None => open_manager(window, cx),
    }
}

fn open_manager(window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| CategoriesDialog::new(window, cx));
    let width = dialog_width(window, 30.);
    let focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if focus.replace(false) {
            let input = view.read(cx).new_name.clone();
            input.focus_handle(cx).focus(window, cx);
        }
        dialog
            .title(t("library.categories.title"))
            .w(width)
            .child(view.clone())
    });
}

/// The categories of one game: a checkbox per category, plus creating a new
/// category the game joins right away.
pub struct GameCategoriesDialog {
    game: GameId,
    new_name: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl GameCategoriesDialog {
    fn new(game: GameId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_name = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t("library.categories.new_placeholder"))
        });
        let library = models(cx).library.clone();
        let subscriptions = vec![
            cx.observe(&library, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &new_name,
                window,
                |this, _, event, window, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => this.create(window, cx),
                    _ => {}
                },
            ),
        ];
        Self {
            game,
            new_name,
            _subscriptions: subscriptions,
        }
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(name) = validate_name(&text(&self.new_name, cx), &existing_categories(cx), None)
        else {
            return;
        };
        let game = self.game.clone();
        userdata(cx, move |u| {
            u.create_category(&name);
            u.set_category(&game, &name, true);
        });
        self.new_name
            .update(cx, |s, cx| s.set_value(SharedString::default(), window, cx));
        cx.notify();
    }
}

impl Render for GameCategoriesDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let key = self.game.key();
        let entries: Vec<(String, bool)> = models(cx)
            .library
            .read(cx)
            .user
            .categories
            .iter()
            .map(|(name, set)| (name.clone(), set.contains(&key)))
            .collect();
        let candidate = validate_name(
            &text(&self.new_name, cx),
            entries.iter().map(|(n, _)| n),
            None,
        );
        let muted = cx.theme().muted_foreground;
        let empty = entries.is_empty();
        let boxes = entries.into_iter().enumerate().map(|(ix, (name, member))| {
            let game = self.game.clone();
            Checkbox::new(SharedString::from(format!("game-category-{ix}")))
                .label(name.clone())
                .checked(member)
                .on_change(move |on: &bool, _, cx| {
                    let (game, name, on) = (game.clone(), name.clone(), *on);
                    userdata(cx, move |u| u.set_category(&game, &name, on));
                })
        });
        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .id("game-category-list")
                    .gap_2()
                    .max_h_80()
                    .overflow_y_scroll()
                    .when(empty, |el| {
                        el.child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child(t("library.categories.empty_game")),
                        )
                    })
                    .children(boxes),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(Input::new(&self.new_name)))
                            .child(
                                Button::new("game-category-create")
                                    .icon(IconName::Plus)
                                    .label(t("library.categories.create"))
                                    .disabled(candidate.is_err())
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.create(window, cx)),
                                    ),
                            ),
                    )
                    .when(candidate == Err(NameError::Exists), |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(NameError::Exists.message()),
                        )
                    }),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        Button::new("game-category-manage")
                            .ghost()
                            .icon(IconName::Settings2)
                            .label(t("library.categories.manage"))
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                                open_manager(window, cx);
                            }),
                    )
                    .child(
                        Button::new("game-category-done")
                            .primary()
                            .label(t("library.categories.done"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
    }
}

/// Open the categories of one game.
fn open_game_categories_dialog(game: GameId, window: &mut Window, cx: &mut App) {
    let title = backend(cx)
        .game(&game)
        .map(|g| t_args("library.categories.game_title", &[("title", &g.title)]))
        .unwrap_or_else(|| t("library.categories.title"));
    let view = cx.new(|cx| GameCategoriesDialog::new(game, window, cx));
    let width = dialog_width(window, 26.);
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.title(title.clone()).w(width).child(view.clone())
    });
}

/// Creates a category and adds one game to it.
pub struct NewCategoryDialog {
    game: GameId,
    name: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl NewCategoryDialog {
    fn new(game: GameId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t("library.categories.name_placeholder"))
        });
        let subscriptions = vec![
            cx.subscribe_in(&name, window, |this, _, event, window, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                _ => {}
            }),
        ];
        Self {
            game,
            name,
            _subscriptions: subscriptions,
        }
    }

    /// The category to add the game to, and whether it must be created.
    fn target(&self, cx: &App) -> Option<(String, bool)> {
        let existing = existing_categories(cx);
        let raw = text(&self.name, cx);
        match validate_name(&raw, &existing, None) {
            Ok(name) => Some((name, true)),
            Err(NameError::Exists) => existing
                .into_iter()
                .find(|e| e.to_lowercase() == raw.to_lowercase())
                .map(|e| (e, false)),
            Err(_) => None,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((name, create)) = self.target(cx) else {
            return;
        };
        let game = self.game.clone();
        userdata(cx, move |u| {
            if create {
                u.create_category(&name);
            }
            u.set_category(&game, &name, true);
        });
        window.close_dialog(cx);
    }
}

impl Render for NewCategoryDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.target(cx);
        let label = match &target {
            Some((name, false)) => t_args("library.categories.add_to", &[("name", name)]),
            _ => t("library.categories.create"),
        };
        v_flex()
            .gap_3()
            .child(Input::new(&self.name))
            .when(matches!(target, Some((_, false))), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t("library.categories.exists_hint")),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("new-category-cancel")
                            .ghost()
                            .label(t("library.common.cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("new-category-create")
                            .primary()
                            .label(label)
                            .disabled(target.is_none())
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

/// Ask for a category name, create it if needed and add `game` to it.
pub fn open_new_category_dialog(game: GameId, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| NewCategoryDialog::new(game, window, cx));
    let width = dialog_width(window, 24.);
    let focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if focus.replace(false) {
            let input = view.read(cx).name.clone();
            input.focus_handle(cx).focus(window, cx);
        }
        dialog
            .title(t("library.categories.new_title"))
            .w(width)
            .child(view.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names() {
        let existing = vec!["RPG".to_string(), "Co-op".to_string()];
        assert_eq!(
            validate_name("  Shooters ", &existing, None),
            Ok("Shooters".into())
        );
        assert_eq!(validate_name("   ", &existing, None), Err(NameError::Empty));
        // Creating an existing name is refused (Heroic wiped its members).
        assert_eq!(
            validate_name("rpg", &existing, None),
            Err(NameError::Exists)
        );
        // Renaming onto another existing name is refused.
        assert_eq!(
            validate_name("co-op", &existing, Some("RPG")),
            Err(NameError::Exists)
        );
        assert_eq!(
            validate_name("RPG", &existing, Some("RPG")),
            Err(NameError::Unchanged)
        );
        // Changing only the case of the category being renamed is fine.
        assert_eq!(
            validate_name("rpg", &existing, Some("RPG")),
            Ok("rpg".into())
        );
    }
}
