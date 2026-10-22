//! Store deals: GOG's discounted catalog (optionally hiding owned games),
//! GOG wishlist and region, and the GreenManGaming and Humble deal feeds
//! that Heroic publishes. GMG/Humble results are cached on disk for 24 hours.
//!
//! GOG access tokens are passed in by the caller (the GOG store owns the
//! credentials) and are never logged.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context as _, Result};
use futures::future::join_all;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::Mutex;

use crate::util;

const CATALOG_URL: &str = "https://catalog.gog.com/v1/catalog";
const WISHLIST_URL: &str = "https://embed.gog.com/user/wishlist.json";
const USER_DATA_URL: &str = "https://embed.gog.com/userData.json";
const PAGE_LIMIT: u32 = 48;
const MAX_PAGES: u32 = 30;
/// Lifetime of cached GMG/Humble feeds.
pub const FEED_TTL: Duration = Duration::from_secs(24 * 3600);

/// Which shop a deal is from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DealStore {
    #[default]
    Gog,
    Gmg,
    Humble,
}

/// An amount in a currency, as decimal text (`"2.40"`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Money {
    pub amount: String,
    pub currency: String,
}

/// Prices of a deal; the strings are formatted for display.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CatalogPrice {
    #[serde(deserialize_with = "string_or_null")]
    pub r#final: String,
    #[serde(deserialize_with = "string_or_null")]
    pub base: String,
    /// `"-70%"`.
    #[serde(deserialize_with = "string_or_null")]
    pub discount: String,
    pub final_money: Option<Money>,
    pub base_money: Option<Money>,
}

/// A name/slug pair (genre, feature).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tag {
    pub name: String,
    pub slug: String,
}

/// An age rating.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CatalogRating {
    pub name: String,
    pub age_rating: String,
}

/// One discounted product.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CatalogProduct {
    #[serde(deserialize_with = "string_or_number")]
    pub id: String,
    pub title: String,
    pub store: DealStore,
    pub cover_horizontal: Option<String>,
    pub cover_vertical: Option<String>,
    pub price: CatalogPrice,
    pub product_type: String,
    pub store_link: String,
    pub release_date: Option<String>,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub operating_systems: Vec<String>,
    pub genres: Vec<Tag>,
    pub features: Vec<Tag>,
    pub reviews_rating: Option<f64>,
    pub ratings: Vec<CatalogRating>,
}

fn string_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    })
}

fn string_or_null<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

/// Country, locale and currency for the GOG catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogLocale {
    pub country_code: String,
    pub locale: String,
    pub currency_code: String,
}

impl Default for CatalogLocale {
    /// `US` / `en-US` / `USD`.
    fn default() -> Self {
        Self {
            country_code: "US".into(),
            locale: "en-US".into(),
            currency_code: "USD".into(),
        }
    }
}

/// The GOG account's country and currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GogDealsRegion {
    pub country_code: String,
    pub currency_code: String,
}

fn user_agent() -> String {
    format!("Ember/{}", env!("CARGO_PKG_VERSION"))
}

fn catalog_url(locale: &CatalogLocale, page: u32, hide_owned: bool) -> String {
    let mut url = format!(
        "{CATALOG_URL}?limit={PAGE_LIMIT}&order=desc%3Atrending&discounted=eq%3Atrue&productType=in%3Agame%2Cpack%2Cdlc&page={page}&countryCode={}&locale={}&currencyCode={}",
        util::encode_component(&locale.country_code),
        util::encode_component(&locale.locale),
        util::encode_component(&locale.currency_code),
    );
    if hide_owned {
        url.push_str("&hideOwned=true");
    }
    url
}

#[derive(Deserialize)]
struct CatalogPage {
    #[serde(default)]
    pages: u32,
    #[serde(default)]
    products: Vec<CatalogProduct>,
}

async fn fetch_page(url: &str, token: Option<&str>) -> Result<CatalogPage> {
    let mut req = ember_core::http::client()
        .get(url)
        .header(reqwest::header::USER_AGENT, user_agent())
        .timeout(Duration::from_secs(15));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    Ok(req.send().await?.error_for_status()?.json().await?)
}

fn dedupe(products: Vec<CatalogProduct>) -> Vec<CatalogProduct> {
    let mut seen = HashSet::new();
    products
        .into_iter()
        .filter(|p| seen.insert(p.id.clone()))
        .collect()
}

async fn fetch_all(locale: &CatalogLocale, token: Option<&str>) -> Result<Vec<CatalogProduct>> {
    let hide_owned = token.is_some();
    let first = fetch_page(&catalog_url(locale, 1, hide_owned), token).await?;
    let total = first.pages.min(MAX_PAGES);
    let mut all = first.products;
    if total > 1 {
        let pages = join_all((2..=total).map(|n| async move {
            match fetch_page(&catalog_url(locale, n, hide_owned), token).await {
                Ok(p) => p.products,
                Err(e) => {
                    tracing::warn!("Failed to fetch discounts page {n}: {}", e);
                    Vec::new()
                }
            }
        }))
        .await;
        all.extend(pages.into_iter().flatten());
    }
    Ok(dedupe(all))
}

/// GOG's discounted catalog. With `access_token`, owned games are hidden.
/// An empty result for a non-default locale is retried with `US/en-US/USD`.
pub async fn gog_discounts(
    locale: &CatalogLocale,
    access_token: Option<&str>,
) -> Result<Vec<CatalogProduct>> {
    let products = fetch_all(locale, access_token)
        .await
        .context("Failed to fetch GOG discounts")?;
    let fallback = CatalogLocale::default();
    if products.is_empty() && *locale != fallback {
        tracing::info!(
            "no GOG discounts for {}, retrying with US",
            locale.country_code
        );
        return fetch_all(&fallback, access_token)
            .await
            .context("Failed to fetch GOG discounts");
    }
    Ok(products)
}

/// Parse `userData.json` into a region.
pub fn parse_region(json: &serde_json::Value) -> Option<GogDealsRegion> {
    let country = json.get("country")?.as_str()?;
    let currency = json.pointer("/selectedCurrency/code")?.as_str()?;
    Some(GogDealsRegion {
        country_code: country.to_uppercase(),
        currency_code: currency.to_uppercase(),
    })
}

/// The GOG account's region (retried without the token if that fails).
pub async fn gog_deals_region(access_token: Option<&str>) -> Option<GogDealsRegion> {
    let attempt = |token: Option<&str>| {
        let mut req = ember_core::http::client()
            .get(USER_DATA_URL)
            .header(reqwest::header::USER_AGENT, user_agent())
            .timeout(Duration::from_secs(10));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        async move {
            let json: serde_json::Value = req
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?
                .json()
                .await
                .ok()?;
            parse_region(&json)
        }
    };
    if let Some(r) = attempt(access_token).await {
        return Some(r);
    }
    if access_token.is_some() {
        return attempt(None).await;
    }
    None
}

/// Product IDs on the user's GOG wishlist.
pub async fn gog_wishlist(access_token: &str) -> Result<Vec<String>> {
    let json: serde_json::Value = ember_core::http::client()
        .get(WISHLIST_URL)
        .bearer_auth(access_token)
        .header(reqwest::header::USER_AGENT, user_agent())
        .timeout(Duration::from_secs(15))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("reading the GOG wishlist")?;
    Ok(json
        .get("wishlist")
        .and_then(|w| w.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default())
}

/// Currencies GMG publishes feeds for.
pub const GMG_CURRENCIES: [&str; 9] = [
    "GBP", "USD", "EUR", "CAD", "AUD", "BRL", "CNY", "TRL", "KRW",
];

/// One item of an Impact catalog feed.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct FeedItem {
    #[serde(deserialize_with = "opt_string_or_number")]
    catalog_item_id: Option<String>,
    #[serde(deserialize_with = "opt_string_or_number")]
    id: Option<String>,
    name: Option<String>,
    url: Option<String>,
    image_url: Option<String>,
    #[serde(deserialize_with = "opt_string_or_number")]
    current_price: Option<String>,
    #[serde(deserialize_with = "opt_string_or_number")]
    original_price: Option<String>,
    currency: Option<String>,
    text1: Option<String>,
}

fn opt_string_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => Some(s),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct Feed {
    items: Vec<FeedItem>,
}

static SKU_PLATFORM: Lazy<Regex> = Lazy::new(|| Regex::new(r" - ([A-Za-z0-9 +()]+)$").unwrap());

fn platform_from_sku(sku: &str) -> Vec<String> {
    let Some(c) = SKU_PLATFORM.captures(sku) else {
        return Vec::new();
    };
    let suffix = c[1].to_lowercase();
    let os = if suffix.starts_with("xbox") {
        "xbox"
    } else if suffix == "mac" {
        "osx"
    } else if suffix == "pc" || suffix.starts_with("windows") {
        "windows"
    } else {
        return Vec::new();
    };
    vec![os.to_string()]
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Format an amount for display (en-US style: symbol, grouping).
pub fn format_money(amount: f64, currency: &str) -> String {
    let (symbol, decimals) = match currency {
        "USD" => ("$", 2),
        "GBP" => ("£", 2),
        "EUR" => ("€", 2),
        "CAD" => ("CA$", 2),
        "AUD" => ("A$", 2),
        "BRL" => ("R$", 2),
        "CNY" => ("CN¥", 2),
        "JPY" => ("¥", 0),
        "KRW" => ("₩", 0),
        other => return format!("{other} {amount:.2}"),
    };
    let text = format!("{amount:.decimals$}");
    let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
    let mut grouped = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    if frac.is_empty() {
        format!("{symbol}{grouped}")
    } else {
        format!("{symbol}{grouped}.{frac}")
    }
}

/// Turn a feed into deals: only items with a valid lower current price are
/// kept; duplicates by ID are dropped.
fn normalize_feed(feed: Feed, store: DealStore) -> Vec<CatalogProduct> {
    let prefix = match store {
        DealStore::Gmg => "gmg-",
        DealStore::Humble => "humble-",
        DealStore::Gog => "",
    };
    let items = feed.items.into_iter().filter_map(|item| {
        let id = item.catalog_item_id.clone().or(item.id)?;
        let title = item.name?;
        let link = item.url?;
        let current: f64 = item.current_price?.trim().parse().ok()?;
        if !current.is_finite() || current < 0.0 {
            return None;
        }
        let base = item
            .original_price
            .and_then(|p| p.trim().parse::<f64>().ok())
            .filter(|b| b.is_finite() && *b > 0.0)
            .unwrap_or(current);
        if base <= current {
            return None;
        }
        let pct = ((1.0 - current / base) * 100.0).round();
        let currency = item.currency.unwrap_or_else(|| "USD".into());
        let (operating_systems, features) = match store {
            DealStore::Humble => (vec!["windows".to_string()], Vec::new()),
            _ => (
                platform_from_sku(item.catalog_item_id.as_deref().unwrap_or("")),
                item.text1
                    .filter(|t| !t.is_empty())
                    .map(|t| {
                        vec![Tag {
                            name: capitalize(&t),
                            slug: t.to_lowercase(),
                        }]
                    })
                    .unwrap_or_default(),
            ),
        };
        Some(CatalogProduct {
            id: format!("{prefix}{id}"),
            title,
            store,
            cover_horizontal: item.image_url,
            price: CatalogPrice {
                r#final: format_money(current, &currency),
                base: format_money(base, &currency),
                discount: format!("-{pct}%"),
                final_money: Some(Money {
                    amount: format!("{current:.2}"),
                    currency: currency.clone(),
                }),
                base_money: Some(Money {
                    amount: format!("{base:.2}"),
                    currency,
                }),
            },
            product_type: "game".into(),
            store_link: link,
            operating_systems,
            features,
            ..Default::default()
        })
    });
    dedupe(items.collect())
}

static GMG_LOCK: Mutex<()> = Mutex::const_new(());
static HUMBLE_LOCK: Mutex<()> = Mutex::const_new(());

async fn feed_deals(store: DealStore, currency: &str) -> Result<Vec<CatalogProduct>> {
    let (lock, url, name) = match store {
        DealStore::Gmg => (
            &GMG_LOCK,
            format!(
                "https://raw.githubusercontent.com/Heroic-Games-Launcher/deals-listing/gmg-feed/gmg-discounts-{currency}.json"
            ),
            "GMG",
        ),
        _ => (
            &HUMBLE_LOCK,
            format!(
                "https://raw.githubusercontent.com/Heroic-Games-Launcher/deals-listing/humble-feed/humble-discounts-{currency}.json"
            ),
            "Humble",
        ),
    };
    let _guard = lock.lock().await;
    let cache =
        util::cache_dir("discounts").join(format!("{}-{currency}.json", name.to_lowercase()));
    if let Some(deals) = util::cache_read::<Vec<CatalogProduct>>(&cache, FEED_TTL).await {
        return Ok(deals);
    }
    let resp = ember_core::http::client()
        .get(&url)
        .header(reqwest::header::USER_AGENT, user_agent())
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("Failed to fetch {name} discounts ({currency})"))?;
    let feed: Feed = resp
        .json()
        .await
        .with_context(|| format!("Failed to read {name} discounts ({currency})"))?;
    let deals = normalize_feed(feed, store);
    if let Err(e) = util::cache_write(&cache, &deals).await {
        tracing::warn!("caching {name} deals: {e:#}");
    }
    Ok(deals)
}

/// GreenManGaming deals in a currency (unsupported currencies use USD).
pub async fn gmg_discounts(currency: Option<&str>) -> Result<Vec<CatalogProduct>> {
    let cur = currency.unwrap_or("USD").to_uppercase();
    let cur = if GMG_CURRENCIES.contains(&cur.as_str()) {
        cur
    } else {
        "USD".to_string()
    };
    feed_deals(DealStore::Gmg, &cur).await
}

/// Humble deals (USD only).
pub async fn humble_discounts() -> Result<Vec<CatalogProduct>> {
    feed_deals(DealStore::Humble, "USD").await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gog_catalog_product() {
        let json = r#"{"pages": 3, "products": [{"id": "1207658924", "slug": "x", "title": "Witcher",
            "coverHorizontal": "https://a/h.jpg", "price": {"final": "$4.99", "base": "$9.99", "discount": "-50%",
            "finalMoney": {"amount": "4.99", "currency": "USD", "discount": "5.00"},
            "baseMoney": {"amount": "9.99", "currency": "USD"}}, "productType": "game",
            "storeLink": "https://www.gog.com/en/game/x", "developers": ["CDPR"], "operatingSystems": ["windows"],
            "genres": [{"name": "RPG", "slug": "rpg"}], "reviewsRating": 45, "ratings": [{"name": "PEGI", "ageRating": "18"}]},
            {"id": 5, "title": "N", "price": {"final": "$1", "base": "$1", "discount": null}}]}"#;
        let page: CatalogPage = serde_json::from_str(json).unwrap();
        assert_eq!(page.pages, 3);
        let p = &page.products[0];
        assert_eq!(p.store, DealStore::Gog);
        assert_eq!(p.price.discount, "-50%");
        assert_eq!(p.price.final_money.as_ref().unwrap().amount, "4.99");
        assert_eq!(p.ratings[0].age_rating, "18");
        assert_eq!(page.products[1].id, "5");
        assert_eq!(page.products[1].price.discount, "");
    }

    #[test]
    fn catalog_url_params() {
        let url = catalog_url(&CatalogLocale::default(), 2, true);
        assert!(url.ends_with(
            "limit=48&order=desc%3Atrending&discounted=eq%3Atrue&productType=in%3Agame%2Cpack%2Cdlc&page=2&countryCode=US&locale=en-US&currencyCode=USD&hideOwned=true"
        ));
    }

    #[test]
    fn region() {
        let j = serde_json::json!({"country": "de", "selectedCurrency": {"code": "eur"}});
        assert_eq!(
            parse_region(&j),
            Some(GogDealsRegion {
                country_code: "DE".into(),
                currency_code: "EUR".into()
            })
        );
        assert_eq!(parse_region(&serde_json::json!({"country": "de"})), None);
    }

    #[test]
    fn gmg_feed_normalization() {
        let feed: Feed = serde_json::from_str(r#"{"Items": [
            {"CatalogItemId": "Mega Sudoku - PC", "Name": "Mega Sudoku", "Url": "https://gmg/x",
             "ImageUrl": "https://img", "CurrentPrice": "2.40", "OriginalPrice": "7.99", "Currency": "USD", "Text1": "steam"},
            {"CatalogItemId": "Mega Sudoku - PC", "Name": "Dup", "Url": "u", "CurrentPrice": "1", "OriginalPrice": "2"},
            {"Id": 7, "Name": "Full price", "Url": "u", "CurrentPrice": "5", "OriginalPrice": "5"},
            {"Id": 8, "Name": "No url", "CurrentPrice": "1", "OriginalPrice": "5"},
            {"Id": 9, "Name": "Big", "Url": "u", "CurrentPrice": "1500", "OriginalPrice": 3000, "Currency": "KRW"},
            {"CatalogItemId": "Thing - Xbox One", "Name": "X", "Url": "u", "CurrentPrice": "1", "OriginalPrice": "4", "Currency": "TRL"}
        ]}"#).unwrap();
        let deals = normalize_feed(feed, DealStore::Gmg);
        assert_eq!(deals.len(), 3);
        let d = &deals[0];
        assert_eq!(d.id, "gmg-Mega Sudoku - PC");
        assert_eq!(d.price.r#final, "$2.40");
        assert_eq!(d.price.base, "$7.99");
        assert_eq!(d.price.discount, "-70%");
        assert_eq!(d.operating_systems, ["windows"]);
        assert_eq!(
            d.features,
            [Tag {
                name: "Steam".into(),
                slug: "steam".into()
            }]
        );
        assert_eq!(deals[1].price.r#final, "₩1,500");
        assert_eq!(
            deals[1].price.base_money.as_ref().unwrap().amount,
            "3000.00"
        );
        assert_eq!(deals[2].operating_systems, ["xbox"]);
        assert_eq!(deals[2].price.r#final, "TRL 1.00");
    }

    #[test]
    fn humble_feed_normalization() {
        let feed: Feed = serde_json::from_str(
            r#"{"Items": [
            {"CatalogItemId": "blackbook_storefront", "Name": "Black Book", "Url": "u",
             "CurrentPrice": "7.49", "OriginalPrice": "24.99", "Text1": "steam"}]}"#,
        )
        .unwrap();
        let deals = normalize_feed(feed, DealStore::Humble);
        assert_eq!(deals[0].id, "humble-blackbook_storefront");
        assert_eq!(deals[0].store, DealStore::Humble);
        assert_eq!(deals[0].operating_systems, ["windows"]);
        assert!(deals[0].features.is_empty());
        assert_eq!(deals[0].price.discount, "-70%");
    }

    #[test]
    fn money_formatting() {
        assert_eq!(format_money(1234567.5, "USD"), "$1,234,567.50");
        assert_eq!(format_money(0.5, "EUR"), "€0.50");
        assert_eq!(format_money(999.0, "GBP"), "£999.00");
        assert_eq!(format_money(3.0, "XYZ"), "XYZ 3.00");
    }
}
