//! The updates page ([`Route::Updates`](irori_ui::Route::Updates)): one place
//! for what can be updated: games, the components they run on, and Irori.
//!
//! Game updates go through the download queue, so progress, pause and retry
//! stay on the downloads page. Components update in place here. Observes the
//! library, component, download and Irori update models.

use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Window, div,
};
use irori_core::GameId;
use irori_core::util::unix_now;
use irori_services::{ComponentKind, ComponentUpdate};
use irori_ui::components::{EmptyState, PageHeader, SectionTitle};
use irori_ui::i18n::{t, t_args};
use irori_ui::state::run_infallible;
use irori_ui::{backend, models};

use crate::common::{card, notify_error};
use crate::format;

/// Game rows rendered per "Show more" step, as a library can hold hundreds.
const GAMES_PAGE: usize = 50;

/// Text shown for a component's kind.
fn kind_label(kind: ComponentKind) -> SharedString {
    match kind {
        ComponentKind::Wine => t("manage.updates.kind.wine"),
        ComponentKind::Translation => t("manage.updates.kind.translation"),
        ComponentKind::Emulator => t("manage.updates.kind.emulator"),
    }
}

/// The update count in a sentence.
fn summary(count: usize) -> SharedString {
    match count {
        0 => t("manage.updates.up_to_date"),
        1 => t_args("manage.updates.summary_one", &[("count", "1")]),
        n => t_args("manage.updates.summary_other", &[("count", &n.to_string())]),
    }
}

/// Games with an update, by title.
fn sorted_games(ids: impl Iterator<Item = GameId>, cx: &App) -> Vec<(GameId, String)> {
    let b = backend(cx);
    let mut games: Vec<(GameId, String)> = ids
        .map(|id| {
            let title = b
                .game(&id)
                .map(|g| g.title.clone())
                .unwrap_or_else(|| id.app_name.to_string());
            (id, title)
        })
        .collect();
    games.sort_by_key(|(_, title)| title.to_lowercase());
    games
}

pub struct UpdatesView {
    games_shown: usize,
    checking: bool,
    _subscriptions: Vec<Subscription>,
}

impl UpdatesView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let m = models(cx).clone();
        Self {
            games_shown: GAMES_PAGE,
            checking: false,
            _subscriptions: vec![
                cx.observe(&m.library, |_, _, cx| cx.notify()),
                cx.observe(&m.components, |_, _, cx| cx.notify()),
                cx.observe(&m.downloads, |_, _, cx| cx.notify()),
                cx.observe(&m.app_update, |_, _, cx| cx.notify()),
            ],
        }
    }

    /// Ask the stores, the component sources and GitHub now.
    fn check_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        self.checking = true;
        cx.notify();
        let b = backend(cx);
        let task = irori_ui::run(cx, async move {
            b.refresh_updates().await;
            b.check_components().await;
            if b.app_update().kind.checks() {
                b.check_app_update().await?;
            }
            Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.checking = false;
                if let Err(e) = result {
                    notify_error(t("manage.updates.check_failed"), &e, window, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn update_all(&self, cx: &mut Context<Self>) {
        let b = backend(cx);
        run_infallible(cx, async move {
            b.enqueue_all_updates().await;
            b.update_all_components().await;
        })
        .detach();
    }

    fn update_game(&self, id: GameId, title: String, cx: &mut Context<Self>) {
        backend(cx).enqueue_update(id, title, Default::default());
    }

    fn ignore_game(&self, id: &GameId, title: &str, window: &mut Window, cx: &mut Context<Self>) {
        backend(cx).ignore_game_updates(id);
        window.push_notification(
            Notification::info(t_args("manage.updates.ignored", &[("title", title)])),
            cx,
        );
    }

    fn update_component(&self, id: String, cx: &mut Context<Self>) {
        let b = backend(cx);
        // Failures reach the user as a notice and on the component's row.
        run_infallible(cx, async move {
            let _ = b.update_component(&id).await;
        })
        .detach();
    }

    fn skip_component(&self, id: String, skip: bool, cx: &mut Context<Self>) {
        let b = backend(cx);
        run_infallible(cx, async move { b.skip_component_update(&id, skip).await }).detach();
    }

    /// A row: what it is, on the left, and its actions.
    fn row(
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        error: Option<String>,
        actions: Vec<AnyElement>,
        cx: &App,
    ) -> gpui_kit::Div {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .gap_3()
            .items_center()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().truncate().child(title.into()))
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(detail.into()),
                    )
                    .when_some(error, |this, e| {
                        this.child(div().text_sm().text_color(theme.danger).child(e))
                    }),
            )
            .child(h_flex().flex_shrink_0().gap_2().children(actions))
    }

    fn render_game(&self, id: GameId, title: String, cx: &mut Context<Self>) -> impl IntoElement {
        let queued = models(cx)
            .downloads
            .read(cx)
            .queue
            .active
            .iter()
            .any(|i| i.id == id);
        let store = id.runner.store_name();
        let key = id.key();
        let actions: Vec<AnyElement> = if queued {
            vec![
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t("manage.updates.in_queue"))
                    .into_any_element(),
            ]
        } else {
            let (update_id, update_title) = (id.clone(), title.clone());
            let (ignore_id, ignore_title) = (id, title.clone());
            vec![
                Button::new(ElementId::Name(format!("ignore-{key}").into()))
                    .ghost()
                    .small()
                    .icon(IconName::Ban)
                    .tooltip(t("manage.updates.ignore"))
                    .accessibility_label(t("manage.updates.ignore"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.ignore_game(&ignore_id, &ignore_title, window, cx)
                    }))
                    .into_any_element(),
                Button::new(ElementId::Name(format!("queue-update-{key}").into()))
                    .small()
                    .label(t("manage.updates.update"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update_game(update_id.clone(), update_title.clone(), cx)
                    }))
                    .into_any_element(),
            ]
        };
        Self::row(title, store, None, actions, cx).debug_selector(move || format!("update-{key}"))
    }

    fn render_component(
        &self,
        update: &ComponentUpdate,
        running: bool,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = update.id.clone();
        let element = |name: &str| ElementId::Name(format!("{name}-{id}").into());
        let mut actions: Vec<AnyElement> = Vec::new();
        if let Some(url) = update.notes_url.clone() {
            actions.push(
                Button::new(element("notes"))
                    .ghost()
                    .small()
                    .icon(IconName::ExternalLink)
                    .tooltip(t("manage.updates.release_notes"))
                    .accessibility_label(t("manage.updates.release_notes"))
                    .on_click(move |_, _, cx| {
                        if let Some(url) = irori_ui::web_url(&url) {
                            cx.open_url(&url);
                        }
                    })
                    .into_any_element(),
            );
        }
        if running {
            let target = id.clone();
            actions.push(
                Button::new(element("cancel"))
                    .ghost()
                    .small()
                    .label(t("manage.updates.cancel"))
                    .on_click(move |_, _, cx| backend(cx).cancel_component_update(&target))
                    .into_any_element(),
            );
            actions.push(
                Button::new(element("updating"))
                    .small()
                    .loading(true)
                    .label(t("manage.updates.updating"))
                    .into_any_element(),
            );
        } else if update.skipped {
            let target = id.clone();
            actions.push(
                Button::new(element("unskip"))
                    .ghost()
                    .small()
                    .label(t("manage.updates.unskip"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.skip_component(target.clone(), false, cx)
                    }))
                    .into_any_element(),
            );
        } else {
            let (skip, install) = (id.clone(), id.clone());
            actions.push(
                Button::new(element("skip"))
                    .ghost()
                    .small()
                    .label(t("manage.updates.skip"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.skip_component(skip.clone(), true, cx)
                    }))
                    .into_any_element(),
            );
            actions.push(
                Button::new(element("update"))
                    .small()
                    .label(t("manage.updates.update"))
                    .on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.update_component(install.clone(), cx)
                        }),
                    )
                    .into_any_element(),
            );
        }
        let mut detail = format!(
            "{} · {} → {}",
            kind_label(update.kind),
            update.installed,
            update.latest
        );
        if update.skipped {
            detail = format!("{detail} · {}", t("manage.updates.skipped"));
        }
        Self::row(update.name.clone(), detail, error, actions, cx)
    }

    fn render_irori(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let status = models(cx).app_update.read(cx).status.clone();
        let release = status.available?;
        let url = release.url;
        let detail: SharedString = match status.kind.update_command() {
            Some(command) => command.into(),
            None => t_args(
                "manage.updates.irori_running",
                &[("current", &status.current)],
            ),
        };
        let action = Button::new("irori-release")
            .small()
            .icon(IconName::ExternalLink)
            .label(t("manage.updates.view_release"))
            .on_click(move |_, _, cx| cx.open_url(&url))
            .into_any_element();
        let row = Self::row(
            t_args(
                "manage.updates.irori_available",
                &[("version", &release.version)],
            ),
            detail,
            None,
            vec![action],
            cx,
        );
        Some(card(cx).child(row).into_any_element())
    }
}

impl Render for UpdatesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = models(cx).clone();
        let report = m.components.read(cx).report.clone();
        let updates = m.library.read(cx).updates.clone();
        let games = sorted_games(updates.iter().cloned(), cx);
        let failed_stores = backend(cx).update_check_failures();
        let pending_components = report.pending().count();
        let count = games.len() + pending_components;
        let checking = self.checking || report.checking;

        let checked = report
            .checked_at
            .map(|at| format::relative_time(at, unix_now()));
        let mut description = summary(count).to_string();
        if let Some(when) = checked {
            description.push_str(" · ");
            description.push_str(&t_args("manage.updates.checked", &[("when", &when)]));
        }
        let mut header = PageHeader::new(t("manage.updates.title"))
            .description(description)
            .action(
                Button::new("updates-check")
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .label(t("manage.updates.check_now"))
                    .loading(checking)
                    .on_click(cx.listener(|this, _, window, cx| this.check_now(window, cx))),
            );
        if count > 0 {
            header = header.action(
                Button::new("updates-all")
                    .primary()
                    .label(t_args(
                        "manage.updates.update_all",
                        &[("count", &count.to_string())],
                    ))
                    .on_click(cx.listener(|this, _, _, cx| this.update_all(cx))),
            );
        }

        let alerts = failed_stores.iter().enumerate().map(|(ix, runner)| {
            Alert::warning(
                ElementId::Name(format!("update-check-{ix}").into()),
                t_args(
                    "manage.updates.store_check_failed",
                    &[("store", runner.store_name())],
                ),
            )
        });
        let component_alert = report.check_errors.first().map(|e| {
            Alert::warning("component-check", e.clone())
                .title(t("manage.updates.component_check_failed"))
        });

        let shown = games.len().min(self.games_shown);
        let more = games.len() > shown;
        let game_rows: Vec<AnyElement> = games
            .into_iter()
            .take(shown)
            .map(|(id, title)| self.render_game(id, title, cx).into_any_element())
            .collect();
        let component_rows: Vec<AnyElement> = report
            .updates
            .iter()
            .map(|u| {
                let running = report.running.contains(&u.id);
                let error = report.failed.get(&u.id).cloned();
                self.render_component(u, running, error, cx)
                    .into_any_element()
            })
            .collect();
        let irori = self.render_irori(cx);
        let muted = cx.theme().muted_foreground;

        let nothing = game_rows.is_empty() && component_rows.is_empty() && irori.is_none();
        let body: AnyElement = if nothing {
            EmptyState::new(IconName::CircleCheck, t("manage.updates.up_to_date"))
                .description(t("manage.updates.empty_description"))
                .into_any_element()
        } else {
            v_flex()
                .gap_6()
                .children(irori)
                .when(!game_rows.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .gap_3()
                            .child(SectionTitle::new(t("manage.updates.section.games")))
                            .child(card(cx).children(game_rows))
                            .when(more, |this| {
                                this.child(
                                    h_flex().justify_center().child(
                                        Button::new("updates-more")
                                            .ghost()
                                            .small()
                                            .label(t("manage.updates.show_more"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.games_shown += GAMES_PAGE;
                                                cx.notify();
                                            })),
                                    ),
                                )
                            }),
                    )
                })
                .when(!component_rows.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .gap_3()
                            .child(SectionTitle::new(t("manage.updates.section.components")))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(t("manage.updates.components_note")),
                            )
                            .child(card(cx).children(component_rows)),
                    )
                })
                .into_any_element()
        };

        v_flex()
            .size_full()
            .min_h_0()
            .child(div().px_6().pt_6().pb_4().child(header))
            .child(
                div()
                    .id("updates-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .px_6()
                            .pb_6()
                            .gap_4()
                            .children(alerts)
                            .children(component_alert)
                            .child(body),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_count_updates() {
        crate::i18n::register();
        assert_eq!(summary(0), "Everything is up to date");
        assert_eq!(summary(1), "1 update available");
        assert_eq!(summary(3), "3 updates available");
    }
}
