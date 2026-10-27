//! The account's games: builds per platform plus store metadata.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::{Asset, Client, Session, Sidecar};

/// One app of the account. Irori's own catalogue format.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameRecord {
    pub app_name: String,
    pub app_title: String,
    /// Build per platform (`Windows`, `Win32`, `Mac`); empty for games
    /// installed through another store (EA app, Ubisoft Connect).
    pub asset_infos: BTreeMap<String, Asset>,
    /// Catalog item as the store API returns it.
    pub metadata: Value,
    pub sidecar: Option<Sidecar>,
}

impl GameRecord {
    fn attr(&self, name: &str) -> Option<&str> {
        self.metadata
            .get("customAttributes")?
            .get(name)?
            .get("value")?
            .as_str()
    }

    pub fn namespace(&self) -> &str {
        self.metadata
            .get("namespace")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    pub fn catalog_item_id(&self) -> &str {
        self.metadata
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    pub fn is_dlc(&self) -> bool {
        self.metadata.get("mainGameItem").is_some()
    }

    /// Catalog id of the base game of a DLC.
    pub fn main_game_id(&self) -> Option<&str> {
        self.metadata.get("mainGameItem")?.get("id")?.as_str()
    }

    /// App name of the base game of a DLC.
    pub fn main_game_app(&self) -> Option<&str> {
        self.metadata
            .get("mainGameItem")?
            .get("releaseInfo")?
            .get(0)?
            .get("appId")?
            .as_str()
    }

    pub fn is_launchable_addon(&self) -> bool {
        self.metadata
            .get("categories")
            .and_then(Value::as_array)
            .is_some_and(|c| {
                c.iter()
                    .any(|c| c.get("path").and_then(Value::as_str) == Some("addons/launchable"))
            })
    }

    pub fn third_party_store(&self) -> Option<&str> {
        self.attr("ThirdPartyManagedApp")
    }

    pub fn is_origin_game(&self) -> bool {
        self.third_party_store().is_some_and(|s| {
            s.eq_ignore_ascii_case("origin") || s.eq_ignore_ascii_case("the ea app")
        })
    }

    pub fn partner_link_type(&self) -> Option<&str> {
        self.attr("partnerLinkType")
    }

    pub fn can_run_offline(&self) -> bool {
        self.attr("CanRunOffline").unwrap_or("true") == "true"
    }

    pub fn requires_ownership_token(&self) -> bool {
        self.attr("OwnershipToken")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }

    pub fn folder_name(&self) -> &str {
        self.attr("FolderName")
            .filter(|f| !f.trim().is_empty())
            .unwrap_or(&self.app_name)
    }

    pub fn additional_command_line(&self) -> Option<&str> {
        self.attr("AdditionalCommandLine")
            .filter(|s| !s.trim().is_empty())
    }

    /// Cloud-save folder template for the platform (`Mac` or Windows).
    pub fn cloud_save_folder(&self, platform: &str) -> Option<&str> {
        let key = if platform == "Mac" {
            "CloudSaveFolder_MAC"
        } else {
            "CloudSaveFolder"
        };
        self.attr(key).filter(|s| !s.is_empty())
    }

    /// Include/exclude patterns of cloud saves.
    pub fn cloud_filters(&self) -> (Option<Vec<String>>, Option<Vec<String>>) {
        let split = |s: &str| s.split(',').map(|p| p.trim().to_string()).collect();
        (
            self.attr("CloudIncludeList").map(split),
            self.attr("CloudExcludeList").map(split),
        )
    }

    /// `extraLaunchOption_NNN_Name/Args` pairs, sorted by name.
    pub fn launch_options(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for i in 1.. {
            let (Some(name), Some(args)) = (
                self.attr(&format!("extraLaunchOption_{i:03}_Name")),
                self.attr(&format!("extraLaunchOption_{i:03}_Args")),
            ) else {
                break;
            };
            out.push((name.to_string(), args.to_string()));
        }
        out.sort();
        out
    }

    /// DLCs of the catalog entry: (title, entitlement name, app name if it
    /// has its own build).
    pub fn dlc_items(&self) -> Vec<(String, String, Option<String>)> {
        self.metadata
            .get("dlcItemList")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|d| {
                let s = |k: &str| {
                    d.get(k)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                let app = d
                    .get("releaseInfo")
                    .and_then(|r| r.get(0))
                    .and_then(|r| r.get("appId"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                (s("title"), s("entitlementName"), app)
            })
            .collect()
    }

    pub fn version(&self, platform: &str) -> Option<&str> {
        self.asset_infos
            .get(platform)
            .map(|a| a.build_version.as_str())
    }

    /// The sidecar's `deploymentId` (EOS deployment passed to games).
    pub fn deployment_id(&self) -> Option<&str> {
        self.sidecar.as_ref()?.config.get("deploymentId")?.as_str()
    }
}

/// Bring `known` (app name → record) up to date: fetch the builds for
/// `platforms` and the metadata of new or changed apps, plus the library
/// entries without builds. Returns the full list for the account.
pub async fn sync(
    client: &Client,
    session: &Session,
    platforms: &BTreeSet<String>,
    known: &HashMap<String, GameRecord>,
    force: bool,
) -> Result<Vec<GameRecord>> {
    let mut assets: BTreeMap<String, BTreeMap<String, Asset>> = BTreeMap::new();
    for platform in platforms {
        for asset in client.assets(session, platform).await? {
            assets
                .entry(asset.app_name.clone())
                .or_default()
                .insert(platform.clone(), asset);
        }
    }

    let mut out: Vec<GameRecord> = Vec::new();
    let mut fetch: Vec<(String, BTreeMap<String, Asset>, bool)> = Vec::new();
    for (app, infos) in assets {
        if infos.values().any(|a| a.namespace == "ue") {
            continue;
        }
        let windows_rvn = infos.get("Windows").map_or(0, |a| a.sidecar_rvn);
        match known.get(&app) {
            Some(rec) if !force => {
                let build_changed = infos
                    .iter()
                    .any(|(p, a)| rec.version(p) != Some(a.build_version.as_str()));
                let sidecar_changed =
                    windows_rvn > 0 && rec.sidecar.as_ref().map(|s| s.rev) != Some(windows_rvn);
                if build_changed || sidecar_changed || rec.metadata.is_null() {
                    fetch.push((app, infos, sidecar_changed));
                } else {
                    out.push(GameRecord {
                        asset_infos: infos,
                        ..rec.clone()
                    });
                }
            }
            _ => fetch.push((app, infos, windows_rvn > 0)),
        }
    }

    let fetched: Vec<Result<GameRecord>> = futures::stream::iter(fetch)
        .map(|(app, infos, sidecar)| async move {
            let first = infos.values().next().cloned().unwrap_or_default();
            let metadata = client
                .catalog_item(session, &first.namespace, &first.catalog_item_id)
                .await?
                .unwrap_or_else(|| serde_json::json!({"title": "Unknown"}));
            let sidecar = if sidecar {
                client
                    .manifest_info(
                        session,
                        &first.namespace,
                        &first.catalog_item_id,
                        &app,
                        "Windows",
                    )
                    .await
                    .ok()
                    .and_then(|i| i.sidecar)
            } else {
                known.get(&app).and_then(|r| r.sidecar.clone())
            };
            Ok(GameRecord {
                app_title: metadata
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                app_name: app,
                asset_infos: infos,
                metadata,
                sidecar,
            })
        })
        .buffer_unordered(16)
        .collect()
        .await;
    for r in fetched {
        out.push(r?);
    }

    // Games owned through Epic but installed through another store.
    let with_builds: BTreeSet<String> = out.iter().map(|r| r.app_name.clone()).collect();
    let items = client.library_items(session).await?;
    let extra: Vec<(String, String, String)> = items
        .into_iter()
        .filter(|i| i.namespace != "ue" && i.sandbox_type != "PRIVATE")
        .filter_map(|i| Some((i.app_name?, i.namespace, i.catalog_item_id)))
        .filter(|(app, _, _)| app != "1" && !with_builds.contains(app))
        .collect();
    let fetched: Vec<Result<Option<GameRecord>>> = futures::stream::iter(extra)
        .map(|(app, ns, cat)| async move {
            if let Some(rec) = known.get(&app).filter(|r| !force && !r.metadata.is_null()) {
                return Ok(Some(GameRecord {
                    asset_infos: BTreeMap::new(),
                    ..rec.clone()
                }));
            }
            let Some(metadata) = client.catalog_item(session, &ns, &cat).await? else {
                return Ok(None);
            };
            Ok(Some(GameRecord {
                app_title: metadata
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                app_name: app,
                asset_infos: BTreeMap::new(),
                metadata,
                sidecar: None,
            }))
        })
        .buffer_unordered(16)
        .collect()
        .await;
    for r in fetched {
        out.extend(r?);
    }
    out.sort_by(|a, b| a.app_name.cmp(&b.app_name));
    out.dedup_by(|a, b| a.app_name == b.app_name);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use irori_depot::test_server::{Resp, TestServer};
    use serde_json::json;

    use super::*;
    use crate::api::tests::{endpoints, session};

    #[test]
    fn reads_custom_attributes() {
        let rec = GameRecord {
            app_name: "Sugar".into(),
            metadata: json!({
                "id": "cat", "namespace": "ns",
                "categories": [{"path": "addons/launchable"}],
                "mainGameItem": {"id": "base", "releaseInfo": [{"appId": "Base"}]},
                "customAttributes": {
                    "CanRunOffline": {"value": "false"},
                    "OwnershipToken": {"value": "True"},
                    "FolderName": {"value": "SugarDir"},
                    "CloudSaveFolder": {"value": "{AppData}/S"},
                    "CloudIncludeList": {"value": "a.sav, b/"},
                    "extraLaunchOption_001_Name": {"value": "Zed"},
                    "extraLaunchOption_001_Args": {"value": "-z"},
                    "extraLaunchOption_002_Name": {"value": "Alpha"},
                    "extraLaunchOption_002_Args": {"value": "-a"},
                    "ThirdPartyManagedApp": {"value": "Origin"}
                },
                "dlcItemList": [{"title": "D", "entitlementName": "E", "releaseInfo": [{"appId": "DlcApp"}]},
                                {"title": "Cosmetic", "entitlementName": "C"}]
            }),
            sidecar: Some(Sidecar {
                config: json!({"deploymentId": "dep"}),
                rev: 1,
            }),
            ..Default::default()
        };
        assert_eq!((rec.namespace(), rec.catalog_item_id()), ("ns", "cat"));
        assert!(rec.is_dlc() && rec.is_launchable_addon() && rec.is_origin_game());
        assert_eq!(rec.main_game_app(), Some("Base"));
        assert!(!rec.can_run_offline() && rec.requires_ownership_token());
        assert_eq!(rec.folder_name(), "SugarDir");
        assert_eq!(rec.cloud_save_folder("Windows"), Some("{AppData}/S"));
        assert_eq!(rec.cloud_save_folder("Mac"), None);
        assert_eq!(rec.cloud_filters().0.unwrap(), ["a.sav", "b/"]);
        assert_eq!(
            rec.launch_options(),
            [("Alpha".into(), "-a".into()), ("Zed".into(), "-z".into())]
        );
        assert_eq!(
            rec.dlc_items()[0],
            ("D".into(), "E".into(), Some("DlcApp".into()))
        );
        assert_eq!(rec.dlc_items()[1].2, None);
        assert_eq!(rec.deployment_id(), Some("dep"));
    }

    #[tokio::test]
    async fn syncs_the_library() {
        let server = TestServer::start(|req| {
            let p = req.path.as_str();
            if p.starts_with("/launcher/api/public/assets/Windows") {
                Resp::json(json!([
                    {"appName": "Sugar", "buildVersion": "2", "catalogItemId": "c1", "namespace": "ns"},
                    {"appName": "Same", "buildVersion": "1", "catalogItemId": "c2", "namespace": "ns"},
                    {"appName": "Asset", "buildVersion": "1", "catalogItemId": "c3", "namespace": "ue"}]))
            } else if p.starts_with("/catalog/api/shared/namespace/ns/bulk/items?id=c1") {
                Resp::json(json!({"c1": {"id": "c1", "title": "Sugar"}}))
            } else if p.starts_with("/catalog/api/shared/namespace/ea/bulk/items?id=c9") {
                Resp::json(json!({"c9": {"id": "c9", "title": "EA Game"}}))
            } else if p.starts_with("/library/api/public/items") {
                Resp::json(json!({"records": [
                    {"appName": "EaGame", "namespace": "ea", "catalogItemId": "c9", "sandboxType": "PUBLIC"},
                    {"appName": "Sugar", "namespace": "ns", "catalogItemId": "c1"},
                    {"appName": "Hidden", "namespace": "x", "catalogItemId": "c0", "sandboxType": "PRIVATE"}
                ], "responseMetadata": {}}))
            } else {
                Resp::status(404)
            }
        });
        let client =
            Client::with_endpoints(crate::Identity::default(), endpoints(&server)).unwrap();
        let mut known = HashMap::new();
        let mut same = GameRecord {
            app_name: "Same".into(),
            app_title: "Cached".into(),
            metadata: json!({"title": "Cached"}),
            ..Default::default()
        };
        same.asset_infos.insert(
            "Windows".into(),
            Asset {
                build_version: "1".into(),
                ..Default::default()
            },
        );
        known.insert("Same".to_string(), same);
        let platforms: BTreeSet<String> = ["Windows".to_string()].into();
        let list = sync(&client, &session(), &platforms, &known, false)
            .await
            .unwrap();
        let names: Vec<&str> = list.iter().map(|r| r.app_name.as_str()).collect();
        assert_eq!(names, ["EaGame", "Same", "Sugar"]);
        assert_eq!(list[1].app_title, "Cached");
        assert_eq!(list[2].app_title, "Sugar");
        assert_eq!(list[2].version("Windows"), Some("2"));
        assert!(list[0].asset_infos.is_empty());
        let catalog_calls = server
            .requests()
            .iter()
            .filter(|r| r.path.contains("/catalog/"))
            .count();
        assert_eq!(catalog_calls, 2, "unchanged apps keep their metadata");
    }
}
