//! Store websites ([`Route::Stores`](ember_ui::Route::Stores)): links that
//! open each store in the browser, and deals on games the user owns or
//! wishes for.

use ember_core::Runner;
use ember_ui::components::{PageHeader, SectionTitle};
use ember_ui::i18n::t;
use ember_ui::models;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::link::Link;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div, rems,
};

use crate::accounts::{ACCOUNT_STORES, StoreCard, store_cards, store_icon};
use crate::common::card;
use crate::deals::DealsSection;
use crate::login::open_login_dialog;

/// A store's website.
pub(crate) struct StoreSite {
    pub url: &'static str,
    pub host: &'static str,
    pub blurb: SharedString,
}

pub(crate) fn store_site(runner: Runner) -> Option<StoreSite> {
    Some(match runner {
        Runner::Legendary => StoreSite {
            url: "https://store.epicgames.com/",
            host: "store.epicgames.com",
            blurb: t("manage.stores.blurb.epic"),
        },
        Runner::Gog => StoreSite {
            url: "https://www.gog.com/",
            host: "gog.com",
            blurb: t("manage.stores.blurb.gog"),
        },
        Runner::Nile => StoreSite {
            url: "https://gaming.amazon.com/",
            host: "gaming.amazon.com",
            blurb: t("manage.stores.blurb.amazon"),
        },
        Runner::Zoom => StoreSite {
            url: "https://www.zoom-platform.com/",
            host: "zoom-platform.com",
            blurb: t("manage.stores.blurb.zoom"),
        },
        Runner::Itch => StoreSite {
            url: "https://itch.io/",
            host: "itch.io",
            blurb: t("manage.stores.blurb.itch"),
        },
        // Steam's games come from its own client; Ember has nothing to
        // offer there beyond the library.
        Runner::Steam | Runner::Sideload | Runner::Emulation => return None,
    })
}

/// Store links plus deals.
pub struct StoresView {
    cards: Vec<StoreCard>,
    deals: Entity<DealsSection>,
    _subscriptions: Vec<Subscription>,
}

impl StoresView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::i18n::register();
        let m = models(cx).clone();
        let subscriptions = vec![
            cx.observe(&m.accounts, |this, _, cx| this.rebuild(cx)),
            cx.observe(&m.settings, |this, _, cx| this.rebuild(cx)),
        ];
        let deals = cx.new(|cx| DealsSection::new(window, cx));
        Self {
            cards: store_cards(cx),
            deals,
            _subscriptions: subscriptions,
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let cards = store_cards(cx);
        if cards != self.cards {
            self.cards = cards;
            cx.notify();
        }
    }

    fn render_store(&self, data: &StoreCard, cx: &mut Context<Self>) -> Option<AnyElement> {
        let site = store_site(data.runner)?;
        let theme = cx.theme();
        let runner = data.runner;
        Some(
            card(cx)
                .id(ElementId::Name(format!("store-{}", runner.id()).into()))
                .w(rems(17.))
                .child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(store_icon(runner)).text_color(theme.muted_foreground))
                        .child(div().font_semibold().child(runner.store_name())),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(site.blurb),
                )
                .child(
                    Link::new("site").href(site.url).child(
                        h_flex()
                            .gap_1()
                            .text_sm()
                            .child(site.host)
                            .child(Icon::new(IconName::ExternalLink).xsmall()),
                    ),
                )
                .when(data.user.is_none() && !data.disabled, |this| {
                    this.child(
                        h_flex()
                            .gap_2()
                            .justify_between()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(t("manage.stores.login_hint")),
                            )
                            .child(
                                Button::new("login")
                                    .small()
                                    .label(t("manage.stores.login"))
                                    .on_click(move |_, window, cx| {
                                        open_login_dialog(runner, window, cx)
                                    }),
                            ),
                    )
                })
                .into_any_element(),
        )
    }
}

impl Render for StoresView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cards: Vec<AnyElement> = self
            .cards
            .clone()
            .iter()
            .filter(|c| ACCOUNT_STORES.contains(&c.runner) && !c.hidden)
            .filter_map(|c| self.render_store(c, cx))
            .collect();
        v_flex()
            .id("stores-page")
            .size_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .px_6()
                    .py_6()
                    .gap_6()
                    .child(
                        PageHeader::new(t("manage.stores.title"))
                            .description(t("manage.stores.description")),
                    )
                    .child(
                        v_flex()
                            .gap_3()
                            .child(SectionTitle::new(t("manage.stores.websites")))
                            .child(h_flex().flex_wrap().items_start().gap_4().children(cards)),
                    )
                    .child(self.deals.clone()),
            )
    }
}
