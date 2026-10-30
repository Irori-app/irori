//! Deals from GOG, GreenManGaming and Humble, with a filter for games that
//! relate to the user's library (other editions, DLCs of owned games).
//!
//! Results are fetched once per session and shared by every Stores page.

use std::collections::HashSet;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::link::Link;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, Global, InteractiveElement as _,
    IntoElement, ObjectFit, ParentElement as _, Render, SharedString, Styled as _,
    StyledImage as _, Subscription, Window, div, rems,
};
use irori_core::sort_key;
use irori_extras::discounts::{self, CatalogLocale, CatalogProduct, DealStore};
use irori_ui::art::{LruImageCache, art_store};
use irori_ui::components::{SectionTitle, art_img, physical_width};
use irori_ui::i18n::{t, t_args};
use irori_ui::models;

/// Deals rendered per "Show more" step.
const PAGE: usize = 40;

/// Loading state of the shared deals list.
#[derive(Clone, Default)]
pub(crate) enum DealsState {
    #[default]
    Idle,
    Loading,
    Loaded {
        products: Arc<Vec<CatalogProduct>>,
        /// Feeds that failed while others loaded.
        failed: Vec<&'static str>,
    },
    /// The request failed outright, with the error.
    Failed(SharedString),
    /// Every feed failed.
    Unavailable,
}

/// Session cache of deals; observed by every deals section.
pub(crate) struct DealsCache {
    pub state: DealsState,
}

struct DealsCacheHandle(Entity<DealsCache>);
impl Global for DealsCacheHandle {}

fn cache(cx: &mut App) -> Entity<DealsCache> {
    if let Some(h) = cx.try_global::<DealsCacheHandle>() {
        return h.0.clone();
    }
    let entity = cx.new(|_| DealsCache {
        state: DealsState::Idle,
    });
    cx.set_global(DealsCacheHandle(entity.clone()));
    entity
}

/// Fetch every feed in parallel; one failing feed does not hide the others.
fn load(entity: Entity<DealsCache>, cx: &mut App) {
    if matches!(entity.read(cx).state, DealsState::Loading) {
        return;
    }
    entity.update(cx, |c, cx| {
        c.state = DealsState::Loading;
        cx.notify();
    });
    let task = irori_ui::run(cx, async move {
        let locale = CatalogLocale::default();
        let (gog, gmg, humble) = futures::join!(
            discounts::gog_discounts(&locale, None),
            discounts::gmg_discounts(None),
            discounts::humble_discounts(),
        );
        Ok((gog, gmg, humble))
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        let state = match result {
            Err(e) => DealsState::Failed(format!("{e:#}").into()),
            Ok((gog, gmg, humble)) => {
                let mut products = Vec::new();
                let mut failed = Vec::new();
                for (name, feed) in [("GOG", gog), ("GreenManGaming", gmg), ("Humble", humble)] {
                    match feed {
                        Ok(list) => products.extend(list),
                        Err(e) => {
                            tracing::warn!("deals from {name}: {e:#}");
                            failed.push(name);
                        }
                    }
                }
                if products.is_empty() && failed.len() == 3 {
                    DealsState::Unavailable
                } else {
                    DealsState::Loaded {
                        products: Arc::new(products),
                        failed,
                    }
                }
            }
        };
        entity.update(cx, |c, cx| {
            c.state = state;
            cx.notify();
        });
    })
    .detach();
}

/// Discount percentage from `"-70%"`.
pub(crate) fn discount_percent(p: &CatalogProduct) -> u32 {
    p.price
        .discount
        .trim()
        .trim_start_matches('-')
        .trim_end_matches('%')
        .trim()
        .parse()
        .unwrap_or(0)
}

/// Whether a deal relates to a library title: same title, or a title that
/// starts with an owned game's name (editions, DLCs, soundtracks).
pub(crate) fn relates_to_library(title: &str, owned: &HashSet<String>) -> bool {
    let key = sort_key(title);
    if owned.contains(&key) {
        return true;
    }
    owned.iter().any(|o| {
        o.chars().count() >= 4
            && key.starts_with(o.as_str())
            && key[o.len()..].starts_with([' ', ':', '-', '—', '–'])
    })
}

/// Indices of deals to show: store, search and library filters applied,
/// biggest discount first.
pub(crate) fn filter_deals(
    products: &[CatalogProduct],
    store: Option<DealStore>,
    query: &str,
    library_only: bool,
    owned: &HashSet<String>,
) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let mut ix: Vec<usize> = products
        .iter()
        .enumerate()
        .filter(|(_, p)| store.is_none_or(|s| p.store == s))
        .filter(|(_, p)| query.is_empty() || p.title.to_lowercase().contains(&query))
        .filter(|(_, p)| !library_only || relates_to_library(&p.title, owned))
        .map(|(i, _)| i)
        .collect();
    ix.sort_by_key(|&i| std::cmp::Reverse(discount_percent(&products[i])));
    ix
}

fn store_label(store: DealStore) -> &'static str {
    match store {
        DealStore::Gog => "GOG",
        DealStore::Gmg => "GreenManGaming",
        DealStore::Humble => "Humble",
    }
}

const STORE_TABS: [Option<DealStore>; 4] = [
    None,
    Some(DealStore::Gog),
    Some(DealStore::Gmg),
    Some(DealStore::Humble),
];

/// The deals part of the Stores page.
pub(crate) struct DealsSection {
    cache: Entity<DealsCache>,
    search: Entity<InputState>,
    store_tab: usize,
    library_only: bool,
    owned: HashSet<String>,
    shown: usize,
    visible: Vec<usize>,
    /// Language the search placeholder was set in.
    placeholder_language: String,
    images: Entity<LruImageCache>,
    _subscriptions: Vec<Subscription>,
}

impl DealsSection {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cache = cache(cx);
        crate::i18n::register();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t("manage.deals.search")));
        let library = models(cx).library.clone();
        let art = art_store(cx);
        let subscriptions = vec![
            cx.observe(&art, |_, _, cx| cx.notify()),
            cx.observe(&cache, |this, _, cx| {
                this.refilter(cx);
                cx.notify();
            }),
            cx.observe(&library, |this, _, cx| {
                this.owned = owned_titles(cx);
                if this.library_only {
                    this.refilter(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&search, |this, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.shown = PAGE;
                    this.refilter(cx);
                    cx.notify();
                }
            }),
        ];
        if matches!(cache.read(cx).state, DealsState::Idle) {
            load(cache.clone(), cx);
        }
        let mut this = Self {
            cache,
            search,
            store_tab: 0,
            library_only: false,
            owned: owned_titles(cx),
            shown: PAGE,
            visible: Vec::new(),
            placeholder_language: irori_ui::i18n::language(),
            images: LruImageCache::new(64, cx),
            _subscriptions: subscriptions,
        };
        this.refilter(cx);
        this
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let DealsState::Loaded { products, .. } = &self.cache.read(cx).state else {
            self.visible.clear();
            return;
        };
        let query = self.search.read(cx).value();
        self.visible = filter_deals(
            products,
            STORE_TABS[self.store_tab],
            &query,
            self.library_only,
            &self.owned,
        );
    }

    fn render_deal(&self, p: &CatalogProduct, window: &mut Window, cx: &mut App) -> AnyElement {
        let width = rems(6.);
        let art = p
            .cover_horizontal
            .clone()
            .or_else(|| p.cover_vertical.clone())
            .and_then(|url| art_img(&url, physical_width(width, window), false, cx));
        let theme = cx.theme();
        let percent = discount_percent(p);
        h_flex()
            .id(ElementId::Name(format!("deal-{}", p.id).into()))
            .gap_3()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .child(
                div()
                    .w(width)
                    .h(rems(3.4))
                    .flex_shrink_0()
                    .rounded(theme.radius)
                    .overflow_hidden()
                    .bg(theme.muted)
                    .when_some(art, |this, img| {
                        this.child(
                            img.size_full()
                                .rounded(theme.radius)
                                .object_fit(ObjectFit::Cover),
                        )
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        Link::new("open")
                            .when_some(irori_ui::web_url(&p.store_link), |link, url| link.href(url))
                            .child(div().font_medium().truncate().child(p.title.clone())),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        match p.store {
                            DealStore::Gog => t("manage.deals.source.gog"),
                            s => t_args("manage.deals.source.key", &[("store", store_label(s))]),
                        },
                    )),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_shrink_0()
                    .when(percent > 0, |this| {
                        this.child(Tag::success().xsmall().child(t_args(
                            "manage.deals.discount",
                            &[("percent", &percent.to_string())],
                        )))
                    })
                    .when(!p.price.base.is_empty(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .line_through()
                                .text_color(theme.muted_foreground)
                                .child(p.price.base.clone()),
                        )
                    })
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child(p.price.r#final.clone()),
                    ),
            )
            .into_any_element()
    }
}

fn owned_titles(cx: &App) -> HashSet<String> {
    models(cx)
        .library
        .read(cx)
        .snapshot
        .games
        .iter()
        .map(|g| sort_key(&g.title))
        .collect()
}

impl Render for DealsSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let language = irori_ui::i18n::language();
        if language != self.placeholder_language {
            self.placeholder_language = language;
            self.search.update(cx, |s, cx| {
                s.set_placeholder(t("manage.deals.search"), window, cx)
            });
        }
        let state = self.cache.read(cx).state.clone();
        let muted = cx.theme().muted_foreground;
        let reload = Button::new("deals-reload")
            .ghost()
            .small()
            .icon(IconName::RefreshCw)
            .label(t("manage.deals.reload"))
            .disabled(matches!(state, DealsState::Loading))
            .on_click(cx.listener(|this, _, _, cx| load(this.cache.clone(), cx)));
        let body: AnyElement = match &state {
            DealsState::Idle | DealsState::Loading => h_flex()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Spinner::new().small())
                .child(t("manage.deals.loading"))
                .into_any_element(),
            DealsState::Failed(message) => Alert::error("deals-error", message.clone())
                .title(t("manage.deals.failed"))
                .into_any_element(),
            DealsState::Unavailable => {
                Alert::error("deals-error", t("manage.deals.unavailable")).into_any_element()
            }
            DealsState::Loaded { products, failed } => {
                let total = self.visible.len();
                let rows: Vec<AnyElement> = self
                    .visible
                    .iter()
                    .take(self.shown)
                    .map(|&i| self.render_deal(&products[i], window, cx))
                    .collect();
                v_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_4()
                            .flex_wrap()
                            .child(
                                TabBar::new("deal-stores")
                                    .segmented()
                                    .small()
                                    .selected_index(self.store_tab)
                                    .children(STORE_TABS.iter().map(|s| {
                                        Tab::new().label(
                                            s.map(|s| SharedString::from(store_label(s)))
                                                .unwrap_or_else(|| t("manage.deals.all_stores")),
                                        )
                                    }))
                                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                        this.store_tab = *ix;
                                        this.shown = PAGE;
                                        this.refilter(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(div().w(rems(16.)).child(Input::new(&self.search).small()))
                            .child(
                                Switch::new("deals-library")
                                    .label(t("manage.deals.library_only"))
                                    .checked(self.library_only)
                                    .on_change(cx.listener(|this, on: &bool, _, cx| {
                                        this.library_only = *on;
                                        this.shown = PAGE;
                                        this.refilter(cx);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when(!failed.is_empty(), |this| {
                        this.child(Alert::warning(
                            "deals-partial",
                            t_args("manage.deals.partial", &[("stores", &failed.join(", "))]),
                        ))
                    })
                    .child(if rows.is_empty() {
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(if products.is_empty() {
                                t("manage.deals.none")
                            } else {
                                t("manage.deals.no_match")
                            })
                            .into_any_element()
                    } else {
                        v_flex().gap_1().children(rows).into_any_element()
                    })
                    .when(total > self.shown, |this| {
                        this.child(
                            div().child(
                                Button::new("deals-more")
                                    .ghost()
                                    .small()
                                    .label(t_args(
                                        "manage.deals.show_more",
                                        &[("count", &(total - self.shown).to_string())],
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.shown += PAGE;
                                        cx.notify();
                                    })),
                            ),
                        )
                    })
                    .into_any_element()
            }
        };
        v_flex()
            .gap_3()
            .child(SectionTitle::new(t("manage.deals.title")).trailing(reload))
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .child(Icon::new(IconName::Info).xsmall())
                    .child(div().flex_1().min_w_0().child(t("manage.deals.info"))),
            )
            .child(div().image_cache(self.images.clone()).child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_extras::discounts::CatalogPrice;

    fn deal(title: &str, store: DealStore, discount: &str) -> CatalogProduct {
        CatalogProduct {
            id: title.into(),
            title: title.into(),
            store,
            price: CatalogPrice {
                discount: discount.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn filters_and_sorts_deals() {
        let products = vec![
            deal(
                "The Witcher 3: Wild Hunt - Hearts of Stone",
                DealStore::Gog,
                "-50%",
            ),
            deal("Hades", DealStore::Gmg, "-70%"),
            deal("Unrelated Game", DealStore::Humble, "-90%"),
            deal("Witchery", DealStore::Gog, "-10%"),
        ];
        let owned: HashSet<String> = ["witcher 3: wild hunt".to_string(), "hades".to_string()]
            .into_iter()
            .collect();
        assert_eq!(
            filter_deals(&products, None, "", false, &owned),
            vec![2, 1, 0, 3]
        );
        assert_eq!(filter_deals(&products, None, "", true, &owned), vec![1, 0]);
        assert_eq!(
            filter_deals(&products, Some(DealStore::Gog), "", false, &owned),
            vec![0, 3]
        );
        assert_eq!(filter_deals(&products, None, "HAD", false, &owned), vec![1]);
        assert_eq!(discount_percent(&products[1]), 70);
    }
}
