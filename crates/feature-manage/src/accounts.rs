//! Store accounts ([`Route::Accounts`](ember_ui::Route::Accounts)): login
//! state, user, game count, log in / log out / refresh per store.

use std::collections::{BTreeMap, BTreeSet};

use ember_core::{Runner, ShownStores};
use ember_ui::components::PageHeader;
use ember_ui::i18n::{t, t_args};
use ember_ui::{backend, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Styled as _, Subscription, Window, div, rems,
};

use crate::common::{backend_op, card, notify_error, notify_success};
use crate::login::open_login_dialog;

/// Stores with a card on the accounts page, in display order. Steam has no
/// login: its card reports what Ember found on this computer.
pub(crate) const ACCOUNT_STORES: [Runner; 5] = [
    Runner::Legendary,
    Runner::Gog,
    Runner::Nile,
    Runner::Zoom,
    Runner::Steam,
];

/// Icon used for a store across the management screens.
pub(crate) fn store_icon(runner: Runner) -> IconName {
    match runner {
        Runner::Legendary => IconName::Gamepad2,
        Runner::Gog => IconName::Store,
        Runner::Nile => IconName::ShoppingBag,
        Runner::Zoom => IconName::Package,
        Runner::Steam => IconName::Joystick,
        Runner::Sideload => IconName::FolderOpen,
    }
}

/// What a store card shows; derived from the models when they change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoreCard {
    pub runner: Runner,
    pub user: Option<String>,
    pub games: usize,
    /// ZOOM support is turned off in the settings.
    pub disabled: bool,
    /// The user hid the store: its games and pages are left out.
    pub hidden: bool,
    /// The store refused the saved login, with its reason.
    pub expired: Option<String>,
    /// The store reads games installed on this computer; it has no login.
    pub local: bool,
}

/// Cards for the stores this build has, in display order.
pub(crate) fn store_cards(cx: &App) -> Vec<StoreCard> {
    let b = backend(cx);
    let m = models(cx);
    let accounts = m.accounts.read(cx);
    let library = m.library.read(cx);
    let app = &m.settings.read(cx).app;
    let zoom_enabled = app.zoom_platform;
    let shown = app.ui.stores;
    let mut counts: BTreeMap<Runner, usize> = BTreeMap::new();
    for g in &library.snapshot.games {
        *counts.entry(g.runner()).or_default() += 1;
    }
    ACCOUNT_STORES
        .iter()
        .filter(|r| b.store(**r).is_some())
        .map(|&runner| StoreCard {
            runner,
            user: accounts.user(runner).map(|u| {
                let name = u.display_name.trim();
                if name.is_empty() {
                    t("manage.accounts.signed_in").to_string()
                } else {
                    name.to_string()
                }
            }),
            games: counts.get(&runner).copied().unwrap_or(0),
            disabled: runner == Runner::Zoom && !zoom_enabled,
            hidden: !shown.shows(runner),
            expired: accounts.login_expired(runner).map(str::to_string),
            local: !runner.has_account(),
        })
        .collect()
}

/// One card per store with its account state and actions.
pub struct AccountsView {
    cards: Vec<StoreCard>,
    online: bool,
    refreshing: BTreeSet<Runner>,
    logging_out: BTreeSet<Runner>,
    _subscriptions: Vec<Subscription>,
}

impl AccountsView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::i18n::register();
        let m = models(cx).clone();
        let subscriptions = vec![
            cx.observe(&m.accounts, |this, _, cx| this.rebuild(cx)),
            cx.observe(&m.library, |this, _, cx| this.rebuild(cx)),
            cx.observe(&m.settings, |this, _, cx| this.rebuild(cx)),
        ];
        let mut this = Self {
            cards: Vec::new(),
            online: true,
            refreshing: BTreeSet::new(),
            logging_out: BTreeSet::new(),
            _subscriptions: subscriptions,
        };
        this.rebuild(cx);
        this
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let cards = store_cards(cx);
        let online = models(cx).accounts.read(cx).online;
        if cards != self.cards || online != self.online {
            self.cards = cards;
            self.online = online;
            cx.notify();
        }
    }

    fn refresh(&mut self, runner: Runner, window: &mut Window, cx: &mut Context<Self>) {
        if !self.refreshing.insert(runner) {
            return;
        }
        cx.notify();
        let b = backend(cx);
        let task = ember_ui::run(cx, async move {
            b.refresh_store(runner, true).await?;
            b.refresh_updates().await;
            Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.refreshing.remove(&runner);
                if let Err(e) = result {
                    notify_error(
                        t_args(
                            "manage.accounts.refresh_failed",
                            &[("store", runner.store_name())],
                        ),
                        &e,
                        window,
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm_logout(&mut self, runner: Runner, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            alert
                .confirm()
                .title(t_args(
                    "manage.accounts.logout_confirm.title",
                    &[("store", runner.store_name())],
                ))
                .description(t("manage.accounts.logout_confirm.description"))
                .ok_text(t("manage.accounts.logout_confirm.ok"))
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| this.logout(runner, window, cx));
                    true
                })
        });
    }

    fn logout(&mut self, runner: Runner, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = backend(cx).store(runner) else {
            return;
        };
        if !self.logging_out.insert(runner) {
            return;
        }
        cx.notify();
        let task = ember_ui::run(cx, async move { store.logout().await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.logging_out.remove(&runner);
                match result {
                    Ok(()) => {
                        backend(cx).forget_expired_login(runner);
                        notify_success(
                            t_args(
                                "manage.accounts.logged_out",
                                &[("store", runner.store_name())],
                            ),
                            window,
                            cx,
                        );
                        ember_ui::models::refresh_library(false, cx);
                    }
                    Err(e) => notify_error(
                        t_args(
                            "manage.accounts.logout_failed",
                            &[("store", runner.store_name())],
                        ),
                        &e,
                        window,
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_card(&self, card_data: &StoreCard, cx: &mut Context<Self>) -> AnyElement {
        if card_data.local {
            return self.render_local_card(card_data, cx);
        }
        let theme = cx.theme();
        let runner = card_data.runner;
        let refreshing = self.refreshing.contains(&runner);
        let logging_out = self.logging_out.contains(&runner);
        let state_tag = if card_data.hidden {
            Tag::secondary()
                .small()
                .child(t("manage.accounts.state.hidden"))
        } else if card_data.disabled {
            Tag::secondary()
                .small()
                .child(t("manage.accounts.state.turned_off"))
        } else if card_data.expired.is_some() {
            Tag::warning()
                .small()
                .child(t("manage.accounts.state.expired"))
        } else if card_data.user.is_some() {
            Tag::success()
                .small()
                .child(t("manage.accounts.state.signed_in"))
        } else {
            Tag::secondary()
                .small()
                .child(t("manage.accounts.state.signed_out"))
        };
        let detail: AnyElement = match (&card_data.user, card_data.disabled) {
            _ if card_data.hidden => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t("manage.accounts.hidden_detail"))
                .into_any_element(),
            (_, false) if let Some(reason) = &card_data.expired => v_flex()
                .gap_0p5()
                .child(div().text_sm().child(t_args(
                    "manage.accounts.expired_hint",
                    &[("store", runner.store_name())],
                )))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(reason.clone()),
                )
                .into_any_element(),
            (_, true) => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t("manage.accounts.zoom_turned_off"))
                .into_any_element(),
            (Some(user), _) => v_flex()
                .gap_0p5()
                .child(div().font_medium().truncate().child(user.clone()))
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    match card_data.games {
                        1 => t("manage.accounts.games_one"),
                        n => t_args("manage.accounts.games_other", &[("count", &n.to_string())]),
                    },
                ))
                .into_any_element(),
            (None, _) => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t_args(
                    "manage.accounts.login_hint",
                    &[("store", runner.short_name())],
                ))
                .into_any_element(),
        };
        let set_shown = move |shown: bool| {
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                backend_op(cx, move |b| {
                    b.settings.update_app(|s| s.ui.stores.set(runner, shown))
                })
            }
        };
        let hide = ShownStores::can_hide(runner).then(|| {
            Button::new("hide")
                .ghost()
                .icon(IconName::EyeOff)
                .label(t("manage.accounts.hide"))
                .on_click(set_shown(false))
        });
        let actions: AnyElement =
            if card_data.hidden {
                Button::new("show")
                    .icon(IconName::Eye)
                    .label(t("manage.accounts.show"))
                    .on_click(set_shown(true))
                    .into_any_element()
            } else if !card_data.disabled && card_data.expired.is_some() {
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("login-again")
                            .primary()
                            .icon(IconName::LogIn)
                            .label(t("manage.accounts.login_again"))
                            .disabled(!self.online)
                            .on_click(move |_, window, cx| open_login_dialog(runner, window, cx)),
                    )
                    .when(card_data.user.is_some(), |row| {
                        row.child(
                            Button::new("logout")
                                .ghost()
                                .icon(IconName::LogOut)
                                .label(t("manage.accounts.logout"))
                                .loading(logging_out)
                                .disabled(logging_out)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_logout(runner, window, cx)
                                })),
                        )
                    })
                    .children(hide)
                    .into_any_element()
            } else if card_data.disabled {
                Button::new("enable")
                    .label(t("manage.accounts.turn_on"))
                    .on_click(|_, _, cx| {
                        backend_op(cx, |b| b.settings.update_app(|s| s.zoom_platform = true))
                    })
                    .into_any_element()
            } else if card_data.user.is_some() {
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("refresh")
                            .icon(IconName::RefreshCw)
                            .label(t("manage.accounts.refresh"))
                            .loading(refreshing)
                            .disabled(!self.online || refreshing)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.refresh(runner, window, cx)
                            })),
                    )
                    .child(
                        Button::new("logout")
                            .ghost()
                            .icon(IconName::LogOut)
                            .label(t("manage.accounts.logout"))
                            .loading(logging_out)
                            .disabled(logging_out)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_logout(runner, window, cx)
                            })),
                    )
                    .children(hide)
                    .into_any_element()
            } else {
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("login")
                            .primary()
                            .icon(IconName::LogIn)
                            .label(t("manage.accounts.login"))
                            .disabled(!self.online)
                            .on_click(move |_, window, cx| open_login_dialog(runner, window, cx)),
                    )
                    .children(hide)
                    .into_any_element()
            };
        Self::card_frame(runner, state_tag, detail, actions, cx)
    }

    fn card_frame(
        runner: Runner,
        state_tag: Tag,
        detail: AnyElement,
        actions: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        card(cx)
            .id(ElementId::Name(format!("account-{}", runner.id()).into()))
            .w(rems(20.))
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(store_icon(runner)).text_color(theme.muted_foreground))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_semibold()
                            .truncate()
                            .child(runner.store_name()),
                    )
                    .child(state_tag),
            )
            .child(div().min_h_10().child(detail))
            .child(actions)
            .into_any_element()
    }

    /// A store without login (Steam): what Ember found on this computer,
    /// a rescan and the hide switch. Rescanning needs no connection.
    fn render_local_card(&self, card_data: &StoreCard, cx: &mut Context<Self>) -> AnyElement {
        let runner = card_data.runner;
        let found = card_data.user.is_some();
        let refreshing = self.refreshing.contains(&runner);
        let muted = cx.theme().muted_foreground;
        let state_tag = if card_data.hidden {
            Tag::secondary()
                .small()
                .child(t("manage.accounts.state.hidden"))
        } else if found {
            Tag::success()
                .small()
                .child(t("manage.accounts.state.local"))
        } else {
            Tag::secondary()
                .small()
                .child(t("manage.accounts.state.not_found"))
        };
        let detail = if card_data.hidden {
            div()
                .text_sm()
                .text_color(muted)
                .child(t("manage.accounts.local_hidden_detail"))
                .into_any_element()
        } else if found {
            v_flex()
                .gap_0p5()
                .child(div().font_medium().child(match card_data.games {
                    1 => t("manage.accounts.installed_one"),
                    n => t_args(
                        "manage.accounts.installed_other",
                        &[("count", &n.to_string())],
                    ),
                }))
                .child(div().text_sm().text_color(muted).child(t_args(
                    "manage.accounts.local_hint",
                    &[("store", runner.store_name())],
                )))
                .into_any_element()
        } else {
            div()
                .text_sm()
                .text_color(muted)
                .child(t_args(
                    "manage.accounts.local_missing",
                    &[("store", runner.store_name())],
                ))
                .into_any_element()
        };
        let actions =
            if card_data.hidden {
                Button::new("show")
                    .icon(IconName::Eye)
                    .label(t("manage.accounts.show"))
                    .on_click(move |_, _, cx| {
                        backend_op(cx, move |b| {
                            b.settings.update_app(|s| s.ui.stores.set(runner, true))
                        })
                    })
                    .into_any_element()
            } else {
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("rescan")
                            .icon(IconName::RefreshCw)
                            .label(t("manage.accounts.rescan"))
                            .loading(refreshing)
                            .disabled(refreshing)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.refresh(runner, window, cx)
                            })),
                    )
                    .child(
                        Button::new("hide")
                            .ghost()
                            .icon(IconName::EyeOff)
                            .label(t("manage.accounts.hide"))
                            .on_click(move |_, _, cx| {
                                backend_op(cx, move |b| {
                                    b.settings.update_app(|s| s.ui.stores.set(runner, false))
                                })
                            }),
                    )
                    .into_any_element()
            };
        Self::card_frame(runner, state_tag, detail, actions, cx)
    }
}

impl Render for AccountsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cards: Vec<AnyElement> = self
            .cards
            .clone()
            .iter()
            .map(|c| self.render_card(c, cx))
            .collect();
        v_flex()
            .id("accounts-page")
            .size_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .px_6()
                    .py_6()
                    .gap_6()
                    .child(
                        PageHeader::new(t("manage.accounts.title"))
                            .description(t("manage.accounts.description")),
                    )
                    .when(!self.online, |this| {
                        this.child(Alert::warning(
                            "accounts-offline",
                            t("manage.accounts.offline"),
                        ))
                    })
                    .child(h_flex().flex_wrap().items_start().gap_4().children(cards)),
            )
    }
}
