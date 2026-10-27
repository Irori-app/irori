//! Owned games as the entitlement service reports them.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// One owned game.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entitlement {
    /// Entitlement id (needed to download the game).
    pub id: String,
    pub product: Product,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Product {
    /// Product id (`amzn1.adg.product.<uuid>`), Irori's app name.
    pub id: String,
    pub title: Option<String>,
    /// Passed to the game through the Fuel SDK.
    pub sku: Option<String>,
    pub product_detail: ProductDetail,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProductDetail {
    pub icon_url: Option<String>,
    pub details: Details,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Details {
    pub background_url1: Option<String>,
    pub background_url2: Option<String>,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    #[serde(deserialize_with = "strings_or_null")]
    pub genres: Vec<String>,
    pub logo_url: Option<String>,
    pub release_date: Option<String>,
    pub short_description: Option<String>,
}

fn strings_or_null<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(items) => items
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    })
}

/// Drop entries without a product id and repeated products (the first wins).
pub fn dedupe(entitlements: Vec<Entitlement>) -> Vec<Entitlement> {
    let mut seen = std::collections::HashSet::new();
    entitlements
        .into_iter()
        .filter(|e| !e.product.id.is_empty() && seen.insert(e.product.id.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_dedupes_entitlements() {
        let json = r#"[
          {"id": "ent-1", "product": {"id": "amzn1.adg.product.abc", "title": "Foo", "sku": "SKU1",
            "productDetail": {"iconUrl": "https://img/icon.png", "details": {
              "backgroundUrl1": "https://img/bg1.jpg", "genres": ["Action"], "esrbRating": "TEEN"}}}},
          {"id": "ent-2", "product": {"id": "amzn1.adg.product.abc", "title": "Dup"}},
          {"id": "ent-3", "product": {"id": "", "title": "No id"}},
          {"id": "ent-4", "product": {"id": "amzn1.adg.product.def",
            "productDetail": {"details": {"genres": null}}}}
        ]"#;
        let all: Vec<Entitlement> = serde_json::from_str(json).unwrap();
        let list = dedupe(all);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "ent-1");
        assert_eq!(list[0].product.sku.as_deref(), Some("SKU1"));
        assert_eq!(list[0].product.product_detail.details.genres, ["Action"]);
        assert!(list[1].product.product_detail.details.genres.is_empty());
        // Round-trips through Irori's library file.
        let again: Vec<Entitlement> =
            serde_json::from_str(&serde_json::to_string(&list).unwrap()).unwrap();
        assert_eq!(again, list);
    }
}
