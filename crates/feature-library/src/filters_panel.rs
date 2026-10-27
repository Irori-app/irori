//! The library's view criteria as an observable model, and the Filters
//! popover that edits them.

use ember_core::InstallPlatform;
use ember_ui::i18n::{t, t_args};
use ember_ui::models;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, StyledExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Global, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, WeakEntity, Window, div,
};

use crate::filter::{self, CategoryFilter, HiddenMode, InstallFilter, ReleaseWindow, ViewFilter};

/// Owner of the library's [`ViewFilter`]. Persists every change except the
/// search query, off the UI thread.
pub struct FilterModel {
    filter: ViewFilter,
    _save: Option<Task<()>>,
}

impl FilterModel {
    pub fn new(filter: ViewFilter) -> Self {
        Self {
            filter,
            _save: None,
        }
    }

    pub fn filter(&self) -> &ViewFilter {
        &self.filter
    }

    /// Change the criteria; notifies observers once, and only on a change.
    pub fn update(&mut self, f: impl FnOnce(&mut ViewFilter), cx: &mut Context<Self>) {
        let before = self.filter.clone();
        f(&mut self.filter);
        if before == self.filter {
            return;
        }
        let persisted_changed = ViewFilter {
            query: String::new(),
            ..before
        } != ViewFilter {
            query: String::new(),
            ..self.filter.clone()
        };
        if persisted_changed && !cfg!(test) {
            let snapshot = self.filter.clone();
            self._save = Some(cx.background_spawn(async move {
                if let Err(e) = filter::save(&snapshot) {
                    tracing::warn!("saving library view: {e:#}");
                }
            }));
        }
        cx.notify();
    }
}

struct ActiveFilters(WeakEntity<FilterModel>);
impl Global for ActiveFilters {}

/// Make `model` the one dialogs update (renaming or deleting the selected
/// category). Set by the library view.
pub fn set_active_filters(model: &Entity<FilterModel>, cx: &mut App) {
    cx.set_global(ActiveFilters(model.downgrade()));
}

/// The library's filter model, while a library view exists.
pub fn active_filters(cx: &App) -> Option<Entity<FilterModel>> {
    cx.try_global::<ActiveFilters>()?.0.upgrade()
}

/// Content of the Filters popover. Re-renders when the model changes.
pub struct FiltersPanel {
    model: Entity<FilterModel>,
    _observe: Vec<Subscription>,
}

impl FiltersPanel {
    pub fn new(model: Entity<FilterModel>, cx: &mut Context<Self>) -> Self {
        let library = models(cx).library.clone();
        Self {
            _observe: vec![
                cx.observe(&model, |_, _, cx| cx.notify()),
                cx.observe(&library, |_, _, cx| cx.notify()),
            ],
            model,
        }
    }

    fn set(&self, f: impl FnOnce(&mut ViewFilter) + 'static, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| m.update(f, cx));
    }
}

impl FiltersPanel {
    /// Category checkboxes: games in any checked category are shown.
    fn render_categories(
        &self,
        f: &ViewFilter,
        categories: &[String],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut entries = Vec::with_capacity(categories.len() + 1);
        if !categories.is_empty() {
            entries.push(CategoryFilter::Uncategorized);
        }
        entries.extend(categories.iter().cloned().map(CategoryFilter::Named));
        let boxes = entries.into_iter().enumerate().map(|(ix, entry)| {
            let checked = f.categories.contains(&entry);
            Checkbox::new(SharedString::from(format!("filter-category-{ix}")))
                .label(entry.label())
                .checked(checked)
                .on_change(cx.listener(move |this, _: &bool, _, cx| {
                    let entry = entry.clone();
                    this.set(move |f| f.toggle_category(entry), cx)
                }))
        });
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .child(section(t("library.filters.section.categories")))
                    .child(
                        h_flex()
                            .gap_1()
                            .when(!f.categories.is_empty(), |el| {
                                el.child(
                                    Button::new("filter-categories-clear")
                                        .xsmall()
                                        .ghost()
                                        .label(t("library.filters.categories.show_all"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set(|f| f.categories.clear(), cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("filter-categories-manage")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Settings2)
                                    .label(t("library.filters.categories.manage"))
                                    .on_click(|_, window, cx| {
                                        crate::open_categories_dialog(None, window, cx)
                                    }),
                            ),
                    ),
            )
            .when(categories.is_empty(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(t("library.filters.categories.empty")),
                )
            })
            .child(
                v_flex()
                    .id("filter-categories")
                    .gap_2()
                    .max_h_40()
                    .overflow_y_scroll()
                    .children(boxes),
            )
    }
}

fn section(title: SharedString) -> impl IntoElement {
    div().text_xs().font_medium().child(title)
}

impl FiltersPanel {
    /// The system chips, for libraries with emulated games.
    fn render_systems(&self, f: &ViewFilter, cx: &mut Context<Self>) -> Option<AnyElement> {
        let mut present: Vec<String> = models(cx)
            .library
            .read(cx)
            .snapshot
            .games
            .iter()
            .filter_map(|g| g.system.clone())
            .collect();
        present.sort_by_key(|id| {
            ember_engines::systems::all()
                .iter()
                .position(|s| &s.id == id)
                .unwrap_or(usize::MAX)
        });
        present.dedup();
        if present.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .gap_2()
                .child(section(t("library.filters.section.system")))
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .children(present.into_iter().map(|id| {
                            let label = ember_engines::systems::get(&id)
                                .map_or(id.clone(), |s| s.short.clone());
                            let selected = f.systems.contains(&id);
                            Button::new(SharedString::from(format!("system-{id}")))
                                .xsmall()
                                .outline()
                                .label(label)
                                .selected(selected)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let id = id.clone();
                                    this.set(
                                        move |f| {
                                            if !f.systems.remove(&id) {
                                                f.systems.insert(id);
                                            }
                                        },
                                        cx,
                                    )
                                }))
                        })),
                )
                .into_any_element(),
        )
    }
}

impl Render for FiltersPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let f = self.model.read(cx).filter().clone();
        let systems = self.render_systems(&f, cx);
        let categories: Vec<String> = models(cx)
            .library
            .read(cx)
            .user
            .categories
            .keys()
            .cloned()
            .collect();
        let muted = cx.theme().muted_foreground;

        let platforms: [(Option<InstallPlatform>, SharedString); 5] = [
            (None, t("library.filters.platform.any")),
            (
                Some(InstallPlatform::Windows),
                InstallPlatform::Windows.label().into(),
            ),
            (
                Some(InstallPlatform::Linux),
                InstallPlatform::Linux.label().into(),
            ),
            (
                Some(InstallPlatform::Mac),
                InstallPlatform::Mac.label().into(),
            ),
            (
                Some(InstallPlatform::Browser),
                t("library.platform.browser"),
            ),
        ];
        let installs = [
            (InstallFilter::Any, t("library.filters.install.all")),
            (
                InstallFilter::Installed,
                t("library.filters.install.installed"),
            ),
            (
                InstallFilter::NotInstalled,
                t("library.filters.install.not_installed"),
            ),
        ];
        let released = [
            ReleaseWindow::Any,
            ReleaseWindow::ThisYear,
            ReleaseWindow::LastFiveYears,
            ReleaseWindow::Older,
        ];
        let hidden = [
            (HiddenMode::Hide, t("library.filters.hidden.hide")),
            (HiddenMode::Show, t("library.filters.hidden.show")),
            (HiddenMode::Only, t("library.filters.hidden.only")),
        ];
        let platform_values = platforms.clone().map(|(p, _)| p);
        let install_values = installs.clone().map(|(v, _)| v);
        let hidden_values = hidden.clone().map(|(v, _)| v);

        v_flex()
            .id("library-filters-panel")
            .w_96()
            .gap_4()
            .when_some(systems, |el, systems| el.child(systems))
            .child(
                v_flex()
                    .gap_2()
                    .child(section(t("library.filters.section.platform")))
                    .child(
                        ButtonGroup::new("filter-platform")
                            .small()
                            .outline()
                            .children(platforms.into_iter().enumerate().map(|(ix, (p, label))| {
                                Button::new(SharedString::from(format!("platform-{ix}")))
                                    .label(label)
                                    .selected(f.platform == p)
                            }))
                            .on_click(cx.listener(move |this, clicked: &Vec<usize>, _, cx| {
                                if let Some(&ix) = clicked.first() {
                                    let p = platform_values[ix];
                                    this.set(move |f| f.platform = p, cx);
                                }
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(section(t("library.filters.section.install")))
                    .child(
                        ButtonGroup::new("filter-install")
                            .small()
                            .outline()
                            .children(installs.into_iter().enumerate().map(|(ix, (v, label))| {
                                Button::new(SharedString::from(format!("install-{ix}")))
                                    .label(label)
                                    .selected(f.install == v)
                            }))
                            .on_click(cx.listener(move |this, clicked: &Vec<usize>, _, cx| {
                                if let Some(&ix) = clicked.first() {
                                    let v = install_values[ix];
                                    this.set(move |f| f.install = v, cx);
                                }
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(section(t("library.filters.section.released")))
                    .child(
                        ButtonGroup::new("filter-released")
                            .small()
                            .outline()
                            .children(released.iter().enumerate().map(|(ix, w)| {
                                Button::new(SharedString::from(format!("released-{ix}")))
                                    .label(w.label())
                                    .selected(f.released == *w)
                            }))
                            .on_click(cx.listener(move |this, clicked: &Vec<usize>, _, cx| {
                                if let Some(&ix) = clicked.first() {
                                    let w = released[ix];
                                    this.set(move |f| f.released = w, cx);
                                }
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(section(t("library.filters.section.hidden")))
                    .child(
                        ButtonGroup::new("filter-hidden")
                            .small()
                            .outline()
                            .children(hidden.into_iter().enumerate().map(|(ix, (v, label))| {
                                Button::new(SharedString::from(format!("hidden-{ix}")))
                                    .label(label)
                                    .selected(f.hidden == v)
                            }))
                            .on_click(cx.listener(move |this, clicked: &Vec<usize>, _, cx| {
                                if let Some(&ix) = clicked.first() {
                                    let v = hidden_values[ix];
                                    this.set(move |f| f.hidden = v, cx);
                                }
                            })),
                    ),
            )
            .child(self.render_categories(&f, &categories, cx))
            .child(
                v_flex()
                    .gap_2()
                    .child(section(t("library.filters.section.show_only")))
                    .child(
                        Checkbox::new("filter-favourites")
                            .label(t("library.filters.favourites"))
                            .checked(f.favourites_only)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                let v = *v;
                                this.set(move |f| f.favourites_only = v, cx)
                            })),
                    )
                    .child(
                        Checkbox::new("filter-updates")
                            .label(t("library.filters.updates"))
                            .checked(f.updates_only)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                let v = *v;
                                this.set(move |f| f.updates_only = v, cx)
                            })),
                    )
                    .child(
                        Checkbox::new("filter-offline")
                            .label(t("library.filters.offline"))
                            .checked(f.offline_only)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                let v = *v;
                                this.set(move |f| f.offline_only = v, cx)
                            })),
                    )
                    .child(
                        Checkbox::new("filter-unavailable")
                            .label(t("library.filters.hide_missing"))
                            .checked(f.hide_unavailable)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                let v = *v;
                                this.set(move |f| f.hide_unavailable = v, cx)
                            })),
                    )
                    .child(
                        Checkbox::new("filter-third-party")
                            .label(t("library.filters.hide_third_party"))
                            .checked(f.hide_third_party)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                let v = *v;
                                this.set(move |f| f.hide_third_party = v, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t("library.filters.third_party_hint")),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(div().text_xs().text_color(muted).min_w_0().child(
                        match f.active_count() {
                            0 => t("library.filters.active_none"),
                            1 => t("library.filters.active_one"),
                            n => {
                                t_args("library.filters.active_other", &[("count", &n.to_string())])
                            }
                        },
                    ))
                    .child(
                        Button::new("filter-reset")
                            .small()
                            .ghost()
                            .label(t("library.filters.reset"))
                            .disabled(f.active_count() == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set(ViewFilter::reset_filters, cx)
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_part_ignores_query() {
        let a = ViewFilter {
            query: "a".into(),
            ..Default::default()
        };
        let b = ViewFilter {
            query: "b".into(),
            ..Default::default()
        };
        assert_eq!(
            ViewFilter {
                query: String::new(),
                ..a
            },
            ViewFilter {
                query: String::new(),
                ..b
            }
        );
    }
}
