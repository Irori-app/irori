//! Store accounts ([`Route::Accounts`](ember_ui::Route::Accounts)): every
//! account of each store with its game count, log in / log out / refresh
//! per account, and adding another account.

use std::collections::{BTreeMap, BTreeSet};

use ember_core::{Runner, ShownStores, StoreAccount};
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
    Render, SharedString, Styled as _, Subscription, Window, div, rems,
};

use crate::common::{backend_op, card, notify_error, notify_success};
use crate::login::{open_account_login_dialog, open_login_dialog};

/// Stores with a card on the accounts page, in display order. Steam has no
/// login: its card reports what Ember found on this computer.
pub(crate) const ACCOUNT_STORES: [Runner; 6] = [
    Runner::Legendary,
    Runner::Gog,
    Runner::Nile,
    Runner::Zoom,
    Runner::Itch,
    Runner::Steam,
];

/// Icon used for a store across the management screens.
pub(crate) fn store_icon(runner: Runner) -> IconName {
    match runner {
        Runner::Legendary => IconName::Gamepad2,
        Runner::Gog => IconName::Store,
        Runner::Nile => IconName::ShoppingBag,
        Runner::Zoom => IconName::Package,
        Runner::Itch => IconName::Gem,
        Runner::Steam => IconName::Joystick,
        Runner::Sideload => IconName::FolderOpen,
        Runner::Emulation => IconName::Gamepad,
        Runner::Launchers => IconName::Rocket,
    }
}

/// Whether an opt-in store is turned on (stores without a switch always are).
pub(crate) fn store_enabled(app: &ember_core::AppSettings, runner: Runner) -> bool {
    match runner {
        Runner::Zoom => app.zoom_platform,
        Runner::Itch => app.itch_io,
        _ => true,
    }
}

fn set_store_enabled(app: &mut ember_core::AppSettings, runner: Runner, on: bool) {
    match runner {
        Runner::Zoom => app.zoom_platform = on,
        Runner::Itch => app.itch_io = on,
        _ => {}
    }
}

/// What a store card shows; derived from the models when they change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoreCard {
    pub runner: Runner,
    pub user: Option<String>,
    pub games: usize,
    /// The store (ZOOM Platform, itch.io) is turned off in the settings.
    pub disabled: bool,
    /// The user hid the store: its games and pages are left out.
    pub hidden: bool,
    /// The store refused the saved login, with its reason.
    pub expired: Option<String>,
    /// The store reads games installed on this computer; it has no login.
    pub local: bool,
    /// Every account of a store with accounts.
    pub accounts: Vec<StoreAccount>,
    /// Another account can log in beside the signed-in ones.
    pub can_add: bool,
}

/// Cards for the stores this build has, in display order.
pub(crate) fn store_cards(cx: &App) -> Vec<StoreCard> {
    let b = backend(cx);
    let m = models(cx);
    let accounts = m.accounts.read(cx);
    let library = m.library.read(cx);
    let app = &m.settings.read(cx).app;
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
            disabled: !store_enabled(app, runner),
            hidden: !shown.shows(runner),
            expired: accounts.login_expired(runner).map(str::to_string),
            local: !runner.has_account(),
            accounts: accounts.accounts(runner).to_vec(),
            can_add: b.store(runner).is_some_and(|s| s.can_add_account()),
        })
        .collect()
}

/// An account's name, or a stand-in when the store gave none.
fn account_name(account: &StoreAccount) -> String {
    let name = account.display_name.trim();
    if name.is_empty() {
        t("manage.accounts.signed_in").to_string()
    } else {
        name.to_string()
    }
}

/// A store's own card actions use this in place of an account's slot.
const STORE_WIDE: &str = "";

/// One card per store with its account state and actions.
pub struct AccountsView {
    cards: Vec<StoreCard>,
    online: bool,
    /// Accounts being refreshed or logged out, by store and slot.
    refreshing: BTreeSet<(Runner, String)>,
    logging_out: BTreeSet<(Runner, String)>,
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

    /// Refresh one account (`slot`), or the whole store for [`STORE_WIDE`].
    fn refresh(
        &mut self,
        runner: Runner,
        slot: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = (runner, slot.clone());
        if !self.refreshing.insert(key.clone()) {
            return;
        }
        cx.notify();
        let b = backend(cx);
        let task = ember_ui::run(cx, async move {
            if slot == STORE_WIDE {
                b.refresh_store(runner, true).await?;
            } else {
                b.refresh_account(runner, &slot, true).await?;
            }
            b.refresh_updates().await;
            Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.refreshing.remove(&key);
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

    /// Ask before logging out of `account`; with several accounts, name it.
    fn confirm_logout(
        &mut self,
        runner: Runner,
        account: StoreAccount,
        several: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        let store = runner.store_name();
        let name = account_name(&account);
        let title = if several {
            t_args(
                "manage.accounts.logout_account_confirm.title",
                &[("name", &name), ("store", store)],
            )
        } else {
            t_args("manage.accounts.logout_confirm.title", &[("store", store)])
        };
        let description = match account.installed {
            0 => t("manage.accounts.logout_confirm.description"),
            1 if several => t("manage.accounts.logout_account_confirm.installed_one"),
            n if several => t_args(
                "manage.accounts.logout_account_confirm.installed_other",
                &[("count", &n.to_string())],
            ),
            _ => t("manage.accounts.logout_confirm.description"),
        };
        let slot = account.slot.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            let slot = slot.clone();
            let name = name.clone();
            alert
                .confirm()
                .title(title.clone())
                .description(description.clone())
                .ok_text(t("manage.accounts.logout_confirm.ok"))
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let (slot, name) = (slot.clone(), several.then(|| name.clone()));
                    let _ = view.update(cx, |this, cx| this.logout(runner, slot, name, window, cx));
                    true
                })
        });
    }

    /// Log out of the account in `slot`; `name` names it in the outcome.
    fn logout(
        &mut self,
        runner: Runner,
        slot: String,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = backend(cx).store(runner) else {
            return;
        };
        let key = (runner, slot.clone());
        if !self.logging_out.insert(key.clone()) {
            return;
        }
        cx.notify();
        let task = ember_ui::run(cx, async move { store.account_logout(&slot).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.logging_out.remove(&key);
                let store = runner.store_name();
                match result {
                    Ok(()) => {
                        backend(cx).forget_expired_login(runner);
                        let message = match &name {
                            Some(name) => t_args(
                                "manage.accounts.logged_out_account",
                                &[("name", name), ("store", store)],
                            ),
                            None => t_args("manage.accounts.logged_out", &[("store", store)]),
                        };
                        notify_success(message, window, cx);
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
        let any_expired = card_data.accounts.iter().any(|a| a.expired.is_some());
        let state_tag = if card_data.hidden {
            Tag::secondary()
                .small()
                .child(t("manage.accounts.state.hidden"))
        } else if card_data.disabled {
            Tag::secondary()
                .small()
                .child(t("manage.accounts.state.turned_off"))
        } else if card_data.expired.is_some() || any_expired {
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
        let detail: AnyElement = if card_data.hidden {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t("manage.accounts.hidden_detail"))
                .into_any_element()
        } else if card_data.disabled {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t_args(
                    "manage.accounts.turned_off",
                    &[("store", runner.store_name())],
                ))
                .into_any_element()
        } else if !card_data.accounts.is_empty() {
            let several = card_data.accounts.len() > 1;
            v_flex()
                .gap_3()
                .children(
                    card_data
                        .accounts
                        .iter()
                        .map(|account| self.render_account(runner, account, several, cx)),
                )
                .into_any_element()
        } else if let Some(reason) = &card_data.expired {
            v_flex()
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
                .into_any_element()
        } else {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t_args(
                    "manage.accounts.login_hint",
                    &[("store", runner.short_name())],
                ))
                .into_any_element()
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
        let signed_in = card_data.accounts.iter().any(|a| a.signed_in);
        let actions: AnyElement = if card_data.hidden {
            Button::new("show")
                .icon(IconName::Eye)
                .label(t("manage.accounts.show"))
                .on_click(set_shown(true))
                .into_any_element()
        } else if card_data.disabled {
            Button::new("enable")
                .label(t("manage.accounts.turn_on"))
                .on_click(move |_, _, cx| {
                    backend_op(cx, move |b| {
                        b.settings
                            .update_app(|s| set_store_enabled(s, runner, true))
                    })
                })
                .into_any_element()
        } else if signed_in {
            h_flex()
                .flex_wrap()
                .gap_2()
                .when(card_data.can_add, |row| {
                    row.child(
                        Button::new("add-account")
                            .debug_selector(|| "add-account".into())
                            .outline()
                            .icon(IconName::Plus)
                            .label(t("manage.accounts.add_account"))
                            .disabled(!self.online)
                            .on_click(move |_, window, cx| {
                                open_account_login_dialog(runner, None, window, cx)
                            }),
                    )
                })
                .children(hide)
                .into_any_element()
        } else {
            let label = if card_data.expired.is_some() {
                t("manage.accounts.login_again")
            } else {
                t("manage.accounts.login")
            };
            h_flex()
                .flex_wrap()
                .gap_2()
                .child(
                    Button::new("login")
                        .primary()
                        .icon(IconName::LogIn)
                        .label(label)
                        .disabled(!self.online)
                        .on_click(move |_, window, cx| open_login_dialog(runner, window, cx)),
                )
                .children(hide)
                .into_any_element()
        };
        Self::card_frame(runner, state_tag, detail, actions, cx)
    }

    /// One account of a store: its name, what it holds and its actions.
    fn render_account(
        &self,
        runner: Runner,
        account: &StoreAccount,
        several: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let key = (runner, account.slot.clone());
        let refreshing = self.refreshing.contains(&key);
        let logging_out = self.logging_out.contains(&key);
        let name = account_name(account);
        let (detail, warn) = if account.expired.is_some() {
            (t("manage.accounts.account_expired"), true)
        } else if account.signed_in {
            let games = match account.games {
                1 => t("manage.accounts.games_one"),
                n => t_args("manage.accounts.games_other", &[("count", &n.to_string())]),
            };
            (games, false)
        } else {
            let installed = match account.installed {
                1 => t("manage.accounts.signed_out_installed_one"),
                n => t_args(
                    "manage.accounts.signed_out_installed_other",
                    &[("count", &n.to_string())],
                ),
            };
            (installed, false)
        };
        let login = |id: &'static str, label: SharedString, primary: bool| {
            let slot = account.slot.clone();
            Button::new(id)
                .small()
                .when(primary, |b| b.primary())
                .icon(IconName::LogIn)
                .label(label)
                .disabled(!self.online)
                .on_click(move |_, window, cx| {
                    open_account_login_dialog(runner, Some(slot.clone()), window, cx)
                })
        };
        let logout = {
            let account = account.clone();
            Button::new("logout")
                .small()
                .ghost()
                .icon(IconName::LogOut)
                .tooltip(t("manage.accounts.logout"))
                .loading(logging_out)
                .disabled(logging_out)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.confirm_logout(runner, account.clone(), several, window, cx)
                }))
        };
        let actions = if account.expired.is_some() {
            h_flex()
                .gap_1()
                .child(login("login-again", t("manage.accounts.login_again"), true))
                .child(logout)
        } else if account.signed_in {
            let slot = account.slot.clone();
            h_flex()
                .gap_1()
                .child(
                    Button::new("refresh")
                        .small()
                        .ghost()
                        .icon(IconName::RefreshCw)
                        .tooltip(t("manage.accounts.refresh"))
                        .loading(refreshing)
                        .disabled(!self.online || refreshing)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.refresh(runner, slot.clone(), window, cx)
                        })),
                )
                .child(logout)
        } else {
            h_flex()
                .gap_1()
                .child(login("login", t("manage.accounts.login"), false))
        };
        let id = format!("account-{}-{}", runner.id(), account.slot);
        h_flex()
            .id(ElementId::Name(id.clone().into()))
            .debug_selector(move || id.clone())
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconName::CircleUser)
                    .text_color(theme.muted_foreground)
                    .flex_shrink_0(),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_medium().truncate().child(name))
                    .child(
                        div()
                            .text_xs()
                            .text_color(if warn {
                                theme.warning
                            } else {
                                theme.muted_foreground
                            })
                            .child(detail),
                    ),
            )
            .child(actions.flex_shrink_0())
            .into_any_element()
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
        let refreshing = self.refreshing.contains(&(runner, STORE_WIDE.to_string()));
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
        let actions = if card_data.hidden {
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
                            this.refresh(runner, STORE_WIDE.to_string(), window, cx)
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
