//! Store metadata of the account's apps ([`GameRecord`]) and its mapping to
//! [`GameInfo`].
//!
//! The catalogue is kept as [`CatalogEntry`] values: the install-independent
//! part of a game plus the few metadata fields that depend on the installed
//! platform. [`compose`] combines an entry with the install state.

use std::collections::{HashMap, HashSet};

use irori_core::{DlcInfo, ExtraInfo, GameInfo, InstallPlatform, Runner};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use irori_epic_api::library::GameRecord;

use crate::state::InstalledMap;

/// Latest build per platform: `platform → app_name → build_version`.
pub(crate) type Assets = HashMap<String, HashMap<String, String>>;

/// The latest builds of every app.
pub(crate) fn assets_of<'a>(records: impl IntoIterator<Item = &'a GameRecord>) -> Assets {
    let mut out = Assets::new();
    for r in records {
        for (platform, a) in &r.asset_infos {
            out.entry(platform.clone())
                .or_default()
                .insert(r.app_name.clone(), a.build_version.clone());
        }
    }
    out
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct GameMetadata {
    pub app_name: String,
    pub app_title: String,
    pub asset_infos: HashMap<String, Value>,
    pub metadata: MetadataInner,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct MetadataInner {
    pub namespace: String,
    pub categories: Vec<Category>,
    pub description: String,
    pub short_description: Option<String>,
    pub key_images: Vec<KeyImage>,
    pub title: String,
    pub developer: Option<String>,
    pub dlc_item_list: Vec<DlcItem>,
    pub release_info: Vec<ReleaseInfo>,
    pub custom_attributes: HashMap<String, Value>,
    pub main_game_item: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Category {
    pub path: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct KeyImage {
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct DlcItem {
    pub title: String,
    pub release_info: Vec<ReleaseInfo>,
    pub custom_attributes: HashMap<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ReleaseInfo {
    pub app_id: String,
    pub platform: Option<Vec<String>>,
}

fn attr<'a>(attrs: &'a HashMap<String, Value>, name: &str) -> Option<&'a str> {
    attrs
        .get(name)?
        .get("value")?
        .as_str()
        .filter(|s| !s.is_empty())
}

/// The install-independent part of a library entry, cached on disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct CatalogEntry {
    pub game: GameInfo,
    pub save_folder_win: String,
    pub save_folder_mac: String,
    /// Platforms of the first release info (`Windows`, `Win32`, `Mac`…).
    pub metadata_platforms: Vec<String>,
    /// `AdditionalCommandLine` attribute (used for DLC launch options).
    pub additional_command_line: Option<String>,
}

/// `https://www.epicgames.com/store/product/<clean title>`.
///
/// Heroic's character class also kept `(`, `)` and `|`, producing broken
/// URLs; those are removed here.
pub(crate) fn store_url(title: &str) -> String {
    static CLEAN: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"('[A-Za-z0-9_])|(\\[A-Za-z0-9_]{5})|(\\"(\\.|[^"])*")|[^0-9a-zA-Z\s]"#)
            .expect("regex")
    });
    let cleaned = CLEAN
        .replace_all(title, "")
        .replace(' ', "-")
        .to_lowercase();
    let slug = cleaned.split("--definitive").next().unwrap_or_default();
    format!("https://www.epicgames.com/store/product/{slug}")
}

fn is_mobile_only(release_info: &[ReleaseInfo]) -> bool {
    release_info.iter().all(|r| {
        r.platform
            .as_ref()
            .is_some_and(|p| p.iter().all(|x| x == "Android" || x == "iOS"))
    })
}

fn platform_of(name: &str) -> Option<InstallPlatform> {
    match name {
        "Windows" | "Win32" => Some(InstallPlatform::Windows),
        "Mac" => Some(InstallPlatform::Mac),
        _ => None,
    }
}

/// Build the catalogue entry of `app_name`, or `None` for entries Heroic
/// hides: Unreal Engine assets, mods and mobile-only titles.
pub(crate) fn build_entry(app_name: &str, meta: &GameMetadata) -> Option<CatalogEntry> {
    let m = &meta.metadata;
    let categories: Vec<&str> = m.categories.iter().map(|c| c.path.as_str()).collect();
    let is_ue_asset = m.namespace == "ue"
        || categories
            .iter()
            .any(|c| ["assets", "asset-format", "plugins", "projects"].contains(c));
    if is_ue_asset || categories.contains(&"mods") || is_mobile_only(&m.release_info) {
        return None;
    }
    let attrs = &m.custom_attributes;
    let third_party = attr(attrs, "ThirdPartyManagedApp")
        .or_else(|| attr(attrs, "ThirdPartyManagedProvider"))
        .map(str::to_string);
    let tp_lower = third_party.as_deref().map(str::to_lowercase);

    let image = |kinds: &[&str]| {
        m.key_images
            .iter()
            .find(|i| kinds.contains(&i.kind.as_str()) && !i.url.is_empty())
            .map(|i| i.url.clone())
    };
    let wide = image(&["DieselGameBox", "OfferImageWide"]);
    let tall = image(&["DieselGameBoxTall", "OfferImageTall"]);
    let tall_front = image(&["DieselStoreFrontTall"]);

    // Every release counts: a port added later (Cyberpunk 2077 on Mac) can
    // arrive as a release of its own beside the original Windows one.
    let mut metadata_platforms: Vec<String> = Vec::new();
    let listed = m
        .release_info
        .iter()
        .filter_map(|r| r.platform.clone())
        .flatten()
        .chain(
            m.main_game_item
                .as_ref()
                .and_then(|main| main.get("releaseInfo")?.as_array().cloned())
                .into_iter()
                .flatten()
                .filter_map(|r| {
                    serde_json::from_value::<Vec<String>>(r.get("platform")?.clone()).ok()
                })
                .flatten(),
        );
    for p in listed {
        if !metadata_platforms.contains(&p) {
            metadata_platforms.push(p);
        }
    }

    let mut platforms: Vec<InstallPlatform> = Vec::new();
    let names: Vec<&String> = if meta.asset_infos.is_empty() {
        metadata_platforms.iter().collect()
    } else {
        let mut keys: Vec<&String> = meta.asset_infos.keys().collect();
        keys.sort();
        keys
    };
    for p in names.into_iter().filter_map(|n| platform_of(n)) {
        if !platforms.contains(&p) {
            platforms.push(p);
        }
    }

    let mut seen = HashSet::new();
    let dlcs = m
        .dlc_item_list
        .iter()
        .filter_map(|d| {
            let app = d.release_info.first()?.app_id.clone();
            (!app.is_empty() && seen.insert(app.clone())).then(|| DlcInfo {
                app_name: app,
                title: d.title.clone(),
                ..Default::default()
            })
        })
        .collect();

    let title = if m.title.is_empty() {
        meta.app_title.clone()
    } else {
        m.title.clone()
    };
    let url = store_url(&title);
    let short = m.short_description.clone().unwrap_or_default();
    let game = GameInfo {
        runner: Some(Runner::Legendary),
        app_name: app_name.to_string(),
        title,
        developer: m.developer.clone().filter(|d| !d.is_empty()),
        description: Some(m.description.clone()).filter(|d| !d.is_empty()),
        namespace: Some(m.namespace.clone()).filter(|n| !n.is_empty()),
        store_url: Some(url.clone()),
        art_cover: tall.clone().or(tall_front).or(wide.clone()),
        art_square: wide.clone().or(tall),
        art_logo: image(&["DieselGameBoxLogo"]),
        art_background: wide,
        installable: !meta.asset_infos.is_empty() || third_party.is_some(),
        can_run_offline: attr(attrs, "CanRunOffline") == Some("true"),
        folder_name: Some(attr(attrs, "FolderName").unwrap_or(app_name).to_string()),
        is_ea_managed: tp_lower
            .as_deref()
            .is_some_and(|t| t == "origin" || t == "the ea app"),
        is_ubisoft_managed: tp_lower.as_deref() == Some("ubisoftconnect"),
        third_party_managed_app: third_party,
        platforms,
        dlcs,
        is_dlc: m.main_game_item.is_some(),
        extra: Some(ExtraInfo {
            description: Some(m.description.clone()),
            short_description: Some(short),
            store_url: Some(url),
            ..Default::default()
        }),
        ..Default::default()
    };
    Some(CatalogEntry {
        game,
        save_folder_win: attr(attrs, "CloudSaveFolder")
            .unwrap_or_default()
            .to_string(),
        save_folder_mac: attr(attrs, "CloudSaveFolder_MAC")
            .unwrap_or_default()
            .to_string(),
        metadata_platforms,
        additional_command_line: attr(attrs, "AdditionalCommandLine").map(str::to_string),
    })
}

/// The catalogue entry of a library record (see [`build_entry`]).
pub(crate) fn entry_of(rec: &GameRecord) -> Option<CatalogEntry> {
    let meta = GameMetadata {
        app_name: rec.app_name.clone(),
        app_title: rec.app_title.clone(),
        asset_infos: rec
            .asset_infos
            .keys()
            .map(|k| (k.clone(), Value::Null))
            .collect(),
        metadata: match serde_json::from_value(rec.metadata.clone()) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Could not read the Epic metadata of {}: {e}", rec.app_name);
                return None;
            }
        },
    };
    build_entry(&rec.app_name, &meta)
}

/// Combine a catalogue entry with the current install state.
pub(crate) fn compose(
    entry: &CatalogEntry,
    installed: &InstalledMap,
    owned: &HashSet<String>,
    assets: &Assets,
) -> GameInfo {
    let mut g = entry.game.clone();
    let info = installed.get(&g.app_name);
    let on_mac = info.is_some_and(|i| i.platform == "Mac");
    let folder = if on_mac {
        &entry.save_folder_mac
    } else {
        &entry.save_folder_win
    };
    g.cloud_save_enabled = !folder.is_empty();
    g.save_folder = Some(folder.clone()).filter(|f| !f.is_empty());
    g.is_installed = info.is_some();
    g.install = info.map(|i| i.to_installed_info());
    g.save_path = info
        .and_then(|i| i.save_path.clone())
        .filter(|p| !p.is_empty());
    // Not installed: native when Epic offers a Mac build, whether its
    // catalogue says so or only the Mac asset list does.
    g.is_mac_native = match info {
        Some(i) => i.platform == "Mac",
        None => {
            entry.metadata_platforms.iter().any(|p| p == "Mac")
                || g.platforms.contains(&InstallPlatform::Mac)
                || assets
                    .get("Mac")
                    .is_some_and(|m| m.contains_key(&g.app_name))
        }
    };
    g.is_linux_native = false;
    for d in &mut g.dlcs {
        d.is_installed = installed.contains_key(&d.app_name);
        d.owned = owned.contains(&d.app_name);
    }
    let platform = info.map(|i| i.platform_arg()).unwrap_or("Windows");
    g.version = assets
        .get(platform)
        .and_then(|m| m.get(&g.app_name))
        .or_else(|| assets.values().find_map(|m| m.get(&g.app_name)))
        .cloned();
    g.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_installed(text: &str) -> InstalledMap {
        serde_json::from_str(text).unwrap()
    }

    fn parse_assets(text: &str) -> Assets {
        serde_json::from_str(text).unwrap()
    }

    const META: &str = r#"{
      "app_name": "Sugar",
      "app_title": "Sugar Game",
      "asset_infos": {"Windows": {"app_name":"Sugar","build_version":"1.0","namespace":"ns1"}},
      "base_urls": [],
      "metadata": {
        "namespace": "ns1",
        "title": "Sugar: The Game™",
        "developer": "Dev Co",
        "description": "Sweet",
        "categories": [{"path":"games"},{"path":"applications"}],
        "keyImages": [
          {"type":"Thumbnail","url":"https://img/thumb"},
          {"type":"DieselGameBoxTall","url":"https://img/tall"},
          {"type":"DieselGameBox","url":"https://img/wide"},
          {"type":"DieselGameBoxLogo","url":"https://img/logo"}
        ],
        "customAttributes": {
          "CanRunOffline": {"type":"STRING","value":"true"},
          "CloudSaveFolder": {"type":"STRING","value":"{AppData}/Sugar/Saved"},
          "CloudSaveFolder_MAC": {"type":"STRING","value":"{AppData}/SugarMac"},
          "FolderName": {"type":"STRING","value":"SugarGame"}
        },
        "releaseInfo": [{"appId":"Sugar","id":"r1","platform":["Windows","Mac"]}],
        "dlcItemList": [
          {"title":"Sugar DLC","releaseInfo":[{"appId":"SugarDlc","id":"r2"}]},
          {"title":"No install DLC"}
        ]
      }
    }"#;

    fn entry(json: &str) -> Option<CatalogEntry> {
        let meta: GameMetadata = serde_json::from_str(json).unwrap();
        build_entry(&meta.app_name.clone(), &meta)
    }

    #[test]
    fn maps_metadata_to_game_info() {
        let e = entry(META).unwrap();
        let g = &e.game;
        assert_eq!(g.title, "Sugar: The Game™");
        assert_eq!(g.art_cover.as_deref(), Some("https://img/tall"));
        assert_eq!(g.art_square.as_deref(), Some("https://img/wide"));
        assert_eq!(g.art_logo.as_deref(), Some("https://img/logo"));
        assert!(g.can_run_offline);
        assert_eq!(g.folder_name.as_deref(), Some("SugarGame"));
        assert_eq!(g.platforms, vec![InstallPlatform::Windows]);
        assert_eq!(g.dlcs.len(), 1);
        assert_eq!(g.dlcs[0].app_name, "SugarDlc");
        assert!(!g.is_dlc);
        assert!(g.installable);
        assert_eq!(
            g.store_url.as_deref(),
            Some("https://www.epicgames.com/store/product/sugar-the-game")
        );
    }

    #[test]
    fn composes_install_state() {
        let e = entry(META).unwrap();
        let installed = parse_installed(
            r#"{"Sugar":{"install_path":"/g/SugarGame","install_size":1048576,"platform":"Mac",
                          "version":"0.9","save_path":"/saves"},
                "SugarDlc":{"platform":"Mac"}}"#,
        );
        let assets = parse_assets(r#"{"Mac":{"Sugar":"1.1"}}"#);
        let owned: HashSet<String> = ["Sugar".to_string()].into();
        let g = compose(&e, &installed, &owned, &assets);
        assert!(g.is_installed);
        assert!(g.is_mac_native);
        assert_eq!(g.save_folder.as_deref(), Some("{AppData}/SugarMac"));
        assert!(g.cloud_save_enabled);
        assert_eq!(g.save_path.as_deref(), Some("/saves"));
        assert_eq!(g.version.as_deref(), Some("1.1"));
        assert_eq!(g.install.as_ref().unwrap().install_size, "1.0 MiB");
        assert!(g.dlcs[0].is_installed);
        assert!(!g.dlcs[0].owned);
        assert_eq!(g.sort_title, "sugar: the game™");

        let not_installed = compose(&e, &InstalledMap::new(), &owned, &assets);
        assert!(!not_installed.is_installed);
        assert!(not_installed.is_mac_native);
        assert_eq!(
            not_installed.save_folder.as_deref(),
            Some("{AppData}/Sugar/Saved")
        );
    }

    #[test]
    fn a_mac_port_released_separately_is_native() {
        // The Windows release first, the Mac port added as its own release.
        let json = META.replace(
            r#""releaseInfo": [{"appId":"Sugar","id":"r1","platform":["Windows","Mac"]}]"#,
            r#""releaseInfo": [{"appId":"Sugar","id":"r1","platform":["Windows"]},
                              {"appId":"Sugar","id":"r3","platform":["Mac"]}]"#,
        );
        let e = entry(&json).unwrap();
        let owned: HashSet<String> = ["Sugar".to_string()].into();
        let g = compose(&e, &InstalledMap::new(), &owned, &Assets::default());
        assert!(g.is_mac_native);
    }

    #[test]
    fn a_mac_asset_alone_makes_it_native() {
        let json = META.replace(r#","Mac"]"#, "]");
        let e = entry(&json).unwrap();
        let owned: HashSet<String> = ["Sugar".to_string()].into();
        let none = compose(&e, &InstalledMap::new(), &owned, &Assets::default());
        assert!(!none.is_mac_native);
        let assets = parse_assets(r#"{"Mac":{"Sugar":"1.1"}}"#);
        let g = compose(&e, &InstalledMap::new(), &owned, &assets);
        assert!(g.is_mac_native);
    }

    #[test]
    fn filters_assets_mods_and_mobile() {
        let ue = r#"{"app_name":"a","metadata":{"namespace":"ue","releaseInfo":[{"appId":"a"}]}}"#;
        assert!(entry(ue).is_none());
        let plugin = r#"{"app_name":"a","metadata":{"namespace":"x","categories":[{"path":"plugins"}],"releaseInfo":[{"appId":"a"}]}}"#;
        assert!(entry(plugin).is_none());
        let mods = r#"{"app_name":"a","metadata":{"categories":[{"path":"mods"}],"releaseInfo":[{"appId":"a"}]}}"#;
        assert!(entry(mods).is_none());
        let mobile = r#"{"app_name":"a","metadata":{"releaseInfo":[{"appId":"a","platform":["Android","iOS"]}]}}"#;
        assert!(entry(mobile).is_none());
        let empty = r#"{"app_name":"a","metadata":{"releaseInfo":[]}}"#;
        assert!(entry(empty).is_none());
        let pc = r#"{"app_name":"a","metadata":{"releaseInfo":[{"appId":"a","platform":["Android","Windows"]}]}}"#;
        assert!(entry(pc).is_some());
    }

    #[test]
    fn third_party_flags_and_dlc() {
        let ea = r#"{"app_name":"ea","metadata":{"title":"EA Game","releaseInfo":[{"appId":"ea"}],
            "customAttributes":{"ThirdPartyManagedApp":{"type":"STRING","value":"The EA App"}},
            "mainGameItem":{"id":"x"}}}"#;
        let g = entry(ea).unwrap().game;
        assert!(g.is_ea_managed);
        assert!(!g.is_ubisoft_managed);
        assert!(g.is_dlc);
        assert!(g.installable);
        let ubi = r#"{"app_name":"u","metadata":{"releaseInfo":[{"appId":"u"}],
            "customAttributes":{"ThirdPartyManagedProvider":{"type":"STRING","value":"UbisoftConnect"}}}}"#;
        assert!(entry(ubi).unwrap().game.is_ubisoft_managed);
    }

    #[test]
    fn store_urls() {
        assert_eq!(
            store_url("Assassin's Creed® Unity"),
            "https://www.epicgames.com/store/product/assassin-creed-unity"
        );
        assert_eq!(
            store_url("Death Stranding Director's Cut"),
            "https://www.epicgames.com/store/product/death-stranding-director-cut"
        );
        assert_eq!(
            store_url("Game (Beta)"),
            "https://www.epicgames.com/store/product/game-beta"
        );
    }
}
