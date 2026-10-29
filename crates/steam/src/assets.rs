//! Store art for many apps at once, from `IStoreBrowseService/GetItems`
//! (no key needed): library covers, heroes, logos and headers, resolved to
//! addresses on Steam's CDN.

use std::collections::HashMap;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::Client;
use crate::library::lenient_u64;

/// Apps asked for per request.
const BATCH: usize = 100;

/// Where the store's art for an app lives, plus its basic store facts.
/// Art Steam doesn't have for the app is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppAssets {
    pub appid: u32,
    pub name: String,
    pub short_description: String,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    /// Library capsule, 600×900.
    pub cover: Option<String>,
    /// Library capsule, 1200×1800.
    pub cover_2x: Option<String>,
    /// Library hero, 1920×620.
    pub hero: Option<String>,
    pub hero_2x: Option<String>,
    /// Library logo, transparent.
    pub logo: Option<String>,
    pub logo_2x: Option<String>,
    /// Store header, 460×215.
    pub header: Option<String>,
    /// Store main capsule, 616×353.
    pub main_capsule: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawItem {
    #[serde(deserialize_with = "lenient_u64")]
    appid: u64,
    name: String,
    assets: Option<RawAssets>,
    basic_info: Option<RawBasicInfo>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawAssets {
    asset_url_format: String,
    library_capsule: String,
    library_capsule_2x: String,
    library_hero: String,
    library_hero_2x: String,
    library_logo: String,
    library_logo_2x: String,
    header: String,
    main_capsule: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawBasicInfo {
    short_description: String,
    publishers: Vec<Named>,
    developers: Vec<Named>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Named {
    name: String,
}

impl Client {
    /// Store art and basic facts of `appids`, keyed by app id. `language`
    /// is a Steam language name (`english`), `country` a two-letter code
    /// (`US`). Apps the store doesn't know are left out.
    pub async fn store_assets(
        &self,
        appids: &[u32],
        language: &str,
        country: &str,
    ) -> Result<HashMap<u32, AppAssets>> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Items {
            store_items: Vec<Value>,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Answer {
            response: Items,
        }
        let url = format!("{}/IStoreBrowseService/GetItems/v1/", self.endpoints().api);
        let mut all = HashMap::new();
        for chunk in appids.chunks(BATCH) {
            let input = json!({
                "ids": chunk.iter().map(|id| json!({"appid": id})).collect::<Vec<_>>(),
                "context": {"language": language, "country_code": country},
                "data_request": {"include_assets": true, "include_basic_info": true},
            });
            let answer: Answer = self
                .get_json(
                    url.clone(),
                    &[("input_json", input.to_string())],
                    "the games' store art",
                )
                .await?;
            all.extend(
                answer
                    .response
                    .store_items
                    .into_iter()
                    // One odd item must not cost the others their art.
                    .filter_map(|v| serde_json::from_value::<RawItem>(v).ok())
                    .filter_map(|item| self.resolve(item))
                    .map(|a| (a.appid, a)),
            );
        }
        Ok(all)
    }

    fn resolve(&self, item: RawItem) -> Option<AppAssets> {
        let appid = u32::try_from(item.appid).ok().filter(|&id| id != 0)?;
        let assets = item.assets.unwrap_or_default();
        let base = &self.endpoints().assets;
        let url = |file: &str| asset_url(base, &assets.asset_url_format, file);
        let basic = item.basic_info.unwrap_or_default();
        let names = |list: Vec<Named>| {
            list.into_iter()
                .map(|n| n.name)
                .filter(|n| !n.is_empty())
                .collect()
        };
        Some(AppAssets {
            appid,
            name: item.name,
            short_description: basic.short_description,
            developers: names(basic.developers),
            publishers: names(basic.publishers),
            cover: url(&assets.library_capsule),
            cover_2x: url(&assets.library_capsule_2x),
            hero: url(&assets.library_hero),
            hero_2x: url(&assets.library_hero_2x),
            logo: url(&assets.library_logo),
            logo_2x: url(&assets.library_logo_2x),
            header: url(&assets.header),
            main_capsule: url(&assets.main_capsule),
        })
    }
}

/// `<base>/<format>` with `${FILENAME}` replaced by `file`; `None` without
/// a file or a format naming where it goes.
fn asset_url(base: &str, format: &str, file: &str) -> Option<String> {
    const SLOT: &str = "${FILENAME}";
    if file.is_empty() || !format.contains(SLOT) {
        return None;
    }
    Some(format!(
        "{base}/{}",
        format.trim_start_matches('/').replace(SLOT, file)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Endpoints;
    use irori_depot::test_server::{Req, Resp, TestServer};

    fn input_json(req: &Req) -> Value {
        let query = req.path.split_once('?').unwrap().1;
        let (_, v) = url::form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == "input_json")
            .unwrap();
        serde_json::from_str(&v).unwrap()
    }

    #[test]
    fn builds_asset_urls() {
        let base = "https://cdn.example/store_item_assets";
        assert_eq!(
            asset_url(base, "steam/apps/220/abc/${FILENAME}?t=123", "x.jpg").as_deref(),
            Some("https://cdn.example/store_item_assets/steam/apps/220/abc/x.jpg?t=123")
        );
        assert_eq!(
            asset_url(base, "/steam/apps/220/${FILENAME}", "y.png").as_deref(),
            Some("https://cdn.example/store_item_assets/steam/apps/220/y.png")
        );
        assert_eq!(asset_url(base, "steam/apps/220/${FILENAME}", ""), None);
        assert_eq!(asset_url(base, "", "x.jpg"), None);
    }

    #[tokio::test]
    async fn fetches_art_in_batches() {
        let server = TestServer::start(|req| {
            if !req.path.starts_with("/IStoreBrowseService/GetItems/v1/?") {
                return Resp::status(404);
            }
            let input = input_json(req);
            let items: Vec<Value> = input["ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| id["appid"].as_u64().unwrap())
                // App 13 is unknown to the store.
                .filter(|&id| id != 13)
                .map(|id| match id {
                    // No art at all.
                    7 => json!({"appid": 7, "name": "Bare", "assets": null}),
                    // A broken item that must not spoil the batch.
                    8 => json!({"appid": 8, "name": ["not", "a", "name"]}),
                    _ => json!({
                        "item_type": 0,
                        "id": id,
                        "success": 1,
                        "appid": id,
                        "name": format!("Game {id}"),
                        "type": 0,
                        "assets": {
                            "asset_url_format": format!("steam/apps/{id}/abc123/${{FILENAME}}?t=1700000000"),
                            "main_capsule": "capsule_616x353.jpg",
                            "header": "header.jpg",
                            "library_capsule": "library_600x900.jpg",
                            "library_capsule_2x": "library_600x900_2x.jpg",
                            "library_hero": "library_hero.jpg",
                            "library_logo": "logo.png"
                        },
                        "basic_info": {
                            "short_description": "Short.",
                            "publishers": [{"name": "Valve", "creator_clan_account_id": 1}],
                            "developers": [{"name": "Valve"}, {"name": ""}]
                        }
                    }),
                })
                .collect();
            Resp::json(json!({"response": {"store_items": items}}))
        });
        let api = Client::with_endpoints(Endpoints {
            api: server.base(),
            store: server.base(),
            assets: format!("{}/store_item_assets/", server.base()),
        })
        .unwrap();

        let ids: Vec<u32> = (1..=150).collect();
        let art = api.store_assets(&ids, "english", "US").await.unwrap();
        assert_eq!(art.len(), 148, "13 is unknown and 8 is broken");
        let game = &art[&150];
        assert_eq!(game.name, "Game 150");
        let cdn = format!("{}/store_item_assets/steam/apps/150/abc123", server.base());
        assert_eq!(
            game.cover.as_deref(),
            Some(format!("{cdn}/library_600x900.jpg?t=1700000000").as_str())
        );
        assert_eq!(
            game.cover_2x.as_deref(),
            Some(format!("{cdn}/library_600x900_2x.jpg?t=1700000000").as_str())
        );
        assert_eq!(
            game.hero.as_deref(),
            Some(format!("{cdn}/library_hero.jpg?t=1700000000").as_str())
        );
        assert_eq!(game.hero_2x, None);
        assert_eq!(
            game.logo.as_deref(),
            Some(format!("{cdn}/logo.png?t=1700000000").as_str())
        );
        assert_eq!(
            game.header.as_deref(),
            Some(format!("{cdn}/header.jpg?t=1700000000").as_str())
        );
        assert_eq!(game.developers, ["Valve"]);
        assert_eq!(game.publishers, ["Valve"]);
        assert_eq!(game.short_description, "Short.");
        assert_eq!(
            art[&7],
            AppAssets {
                appid: 7,
                name: "Bare".into(),
                ..Default::default()
            }
        );

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        let first = input_json(&requests[0]);
        assert_eq!(first["ids"].as_array().unwrap().len(), 100);
        assert_eq!(
            input_json(&requests[1])["ids"].as_array().unwrap().len(),
            50
        );
        assert_eq!(first["context"]["language"], "english");
        assert_eq!(first["context"]["country_code"], "US");
        assert_eq!(first["data_request"]["include_assets"], true);
        assert_eq!(first["data_request"]["include_basic_info"], true);

        assert!(
            api.store_assets(&[], "english", "US")
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(server.requests().len(), 2);
    }
}
