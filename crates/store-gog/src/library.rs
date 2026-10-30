//! Catalogue: Galaxy library + GamesDB (with ETag cache and certificates),
//! product API fallback, and the mapping to [`GameInfo`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use irori_core::{
    CloudSaveLocation, DlcInfo, Event, ExtraInfo, GameInfo, InstallPlatform, InstalledInfo, Runner,
    json_store, util,
};

use crate::GogStore;
use crate::api::{self, GamesDb, id_string};

/// App name of the Galaxy common redistributables pseudo game.
pub const REDIST_APP: &str = "gog-redist";
/// GamesDB answers are reused without a request for this long.
const GAMESDB_FRESH_SECS: u64 = 6 * 3600;
const GAMESDB_RETRIES: u32 = 5;
const CONCURRENCY: usize = 8;

/// `{formatter}`/`{ext}` template of a GamesDB image.
fn image(v: Option<&Value>, ext: &str) -> Option<String> {
    let f = v?.get("url_format")?.as_str()?;
    (!f.is_empty()).then(|| f.replace("{formatter}", "").replace("{ext}", ext))
}

fn localized(v: Option<&Value>) -> Option<String> {
    let v = v?;
    v.get("*")
        .or_else(|| v.get("en-US"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn names(v: Option<&Value>) -> Option<String> {
    let list: Vec<&str> = v?
        .as_array()?
        .iter()
        .filter_map(|d| d.get("name").and_then(Value::as_str))
        .collect();
    (!list.is_empty()).then(|| list.join(", "))
}

/// A GamesDB release mapped to a library entry.
#[derive(Debug, Clone)]
pub struct MappedRelease {
    pub info: GameInfo,
    /// GamesDB release id of this entry.
    pub release_id: String,
    /// Release id of the base game (DLCs only).
    pub parent_release_id: Option<String>,
}

/// GamesDB's `aggregated_rating` (a float, 0–100) as a whole score.
fn rating(v: Option<&Value>) -> Option<u8> {
    let score = v?.as_f64()?;
    (score > 0.0).then(|| score.round().clamp(0.0, 100.0) as u8)
}

/// Map a GamesDB `external_releases` record. `None` for types that are not
/// shown (packs, hidden games).
pub fn gamesdb_to_game_info(v: &Value) -> Option<MappedRelease> {
    let game = v.get("game")?;
    let kind = game
        .get("type")
        .or_else(|| v.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let is_dlc = kind == "dlc";
    let visible = game
        .get("visible_in_library")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !is_dlc && !(matches!(kind, "game" | "mod") && visible) {
        return None;
    }
    let app_name = v
        .get("external_id")
        .map(id_string)
        // Ids name files and folders; GOG's are numbers.
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()))?;
    let title = localized(v.get("title"))
        .or_else(|| localized(game.get("title")))
        .unwrap_or_default();
    let slugs: Vec<&str> = v
        .get("supported_operating_systems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|o| o.get("slug").and_then(Value::as_str))
        .collect();
    let mut platforms = Vec::new();
    for (slug, p) in [
        ("windows", InstallPlatform::Windows),
        ("linux", InstallPlatform::Linux),
        ("osx", InstallPlatform::Mac),
    ] {
        if slugs.contains(&slug) {
            platforms.push(p);
        }
    }
    let background = image(game.get("background"), "webp");
    let landscape = image(game.get("logo"), "jpg")
        .or_else(|| image(game.get("horizontal_artwork"), "jpg"))
        .or_else(|| background.clone());
    let cover = image(game.get("vertical_cover"), "jpg")
        .or_else(|| image(game.get("cover"), "jpg"))
        .or_else(|| landscape.clone());
    let genres: Vec<String> = game
        .get("genres")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|g| localized(g.get("name")))
        .collect();
    let description = localized(v.get("summary")).or_else(|| localized(game.get("summary")));
    let release_date = v
        .get("first_release_date")
        .and_then(Value::as_str)
        .or_else(|| game.get("first_release_date").and_then(Value::as_str))
        .map(|s| s.chars().take(10).collect());
    let info = GameInfo {
        runner: Some(Runner::Gog),
        app_name,
        title,
        developer: names(game.get("developers")),
        publisher: names(game.get("publishers")),
        description: description.clone(),
        art_cover: cover,
        art_square: landscape,
        art_background: background,
        art_icon: image(game.get("square_icon").or_else(|| game.get("icon")), "jpg"),
        installable: true,
        can_run_offline: true,
        is_linux_native: slugs.contains(&"linux"),
        is_mac_native: slugs.contains(&"osx"),
        platforms,
        is_dlc,
        genres: genres.clone(),
        release_date,
        rating: rating(game.get("aggregated_rating")),
        extra: Some(ExtraInfo {
            description,
            genres,
            ..Default::default()
        }),
        ..Default::default()
    };
    Some(MappedRelease {
        info: info.finalize(),
        release_id: v.get("id").map(id_string).unwrap_or_default(),
        parent_release_id: v.get("parent_id").map(id_string).filter(|s| !s.is_empty()),
    })
}

fn normalize_url(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    if s.is_empty() {
        None
    } else if let Some(rest) = s.strip_prefix("//") {
        Some(format!("https://{rest}"))
    } else {
        Some(s.to_string())
    }
}

/// Map a product API answer (fallback when GamesDB is unavailable).
pub fn product_to_game_info(p: &Value, app_name: &str) -> GameInfo {
    let images = p.get("images");
    let img = |k: &str| normalize_url(images.and_then(|i| i.get(k)));
    let cover = img("logo").or_else(|| img("icon"));
    let square = img("icon")
        .or_else(|| img("sidebarIcon"))
        .or_else(|| img("logo"))
        .or_else(|| cover.clone());
    let desc = p.get("description");
    let full = desc
        .and_then(|d| d.get("full"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let lead = desc
        .and_then(|d| d.get("lead"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let compat = p.get("content_system_compatibility");
    let flag = |k: &str| {
        compat
            .and_then(|c| c.get(k))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let mut platforms = Vec::new();
    for (k, pl) in [
        ("windows", InstallPlatform::Windows),
        ("linux", InstallPlatform::Linux),
        ("osx", InstallPlatform::Mac),
    ] {
        if flag(k) {
            platforms.push(pl);
        }
    }
    let description = full.or(lead).map(str::to_string);
    GameInfo {
        runner: Some(Runner::Gog),
        app_name: app_name.to_string(),
        title: p
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        description: description.clone(),
        art_cover: cover,
        art_square: square,
        art_logo: img("logo"),
        art_background: img("background"),
        art_icon: img("icon"),
        installable: p
            .get("is_installable")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        can_run_offline: true,
        is_mac_native: flag("osx"),
        is_linux_native: flag("linux"),
        platforms,
        is_dlc: p.get("game_type").and_then(Value::as_str).unwrap_or("game") != "game",
        store_url: p
            .pointer("/links/product_card")
            .and_then(Value::as_str)
            .map(str::to_string),
        extra: Some(ExtraInfo {
            description,
            short_description: lead.map(str::to_string),
            ..Default::default()
        }),
        ..Default::default()
    }
    .finalize()
}

/// The pseudo game that owns the Galaxy redistributables. Hidden (DLC flag).
pub fn redist_game_info(redist_dir: &Path) -> GameInfo {
    GameInfo {
        runner: Some(Runner::Gog),
        app_name: REDIST_APP.into(),
        title: "Galaxy Common Redistributables".into(),
        art_cover: Some("https://images.gog-statics.com/516af877f6a03199526d1ce5a76358b8f85f6b828764cf46c820f77ae8832fc5.jpg".into()),
        can_run_offline: true,
        installable: true,
        is_installed: true,
        is_dlc: true,
        install: Some(InstalledInfo {
            install_path: redist_dir.to_string_lossy().into_owned(),
            is_dlc: true,
            ..Default::default()
        }),
        ..Default::default()
    }
    .finalize()
}

/// Attach owned DLCs to their base games (by GamesDB parent release id).
pub fn link_dlcs(entries: &mut [MappedRelease]) {
    let mut by_release: HashMap<String, usize> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if !e.info.is_dlc && !e.release_id.is_empty() {
            by_release.insert(e.release_id.clone(), i);
        }
    }
    let mut attach: Vec<(usize, DlcInfo)> = Vec::new();
    for e in entries.iter().filter(|e| e.info.is_dlc) {
        if let Some(&base) = e.parent_release_id.as_ref().and_then(|p| by_release.get(p)) {
            attach.push((
                base,
                DlcInfo {
                    app_name: e.info.app_name.clone(),
                    title: e.info.title.clone(),
                    is_installed: false,
                    owned: true,
                },
            ));
        }
    }
    for (base, dlc) in attach {
        let dlcs = &mut entries[base].info.dlcs;
        if !dlcs.iter().any(|d| d.app_name == dlc.app_name) {
            dlcs.push(dlc);
        }
    }
}

/// One cached GamesDB answer.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct CachedGamesDb {
    etag: Option<String>,
    fetched_at: u64,
    data: Value,
}

fn gamesdb_cache_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{}.json", irori_core::paths::sanitize(id)))
}

/// Look up one release: cached GamesDB, conditional request with retries,
/// then the product API.
async fn fetch_release(
    item: api::GalaxyItem,
    cache_dir: PathBuf,
    token: String,
    locale: String,
    full: bool,
) -> Option<MappedRelease> {
    let id = id_string(&item.external_id);
    if id.is_empty() {
        return None;
    }
    let file = gamesdb_cache_file(&cache_dir, &id);
    let cached: Option<CachedGamesDb> = {
        let f = file.clone();
        tokio::task::spawn_blocking(move || json_store::read(&f).ok().flatten())
            .await
            .ok()
            .flatten()
    };
    let now = util::unix_now();
    if let Some(c) = &cached
        && !full
        && now.saturating_sub(c.fetched_at) < GAMESDB_FRESH_SECS
        && !c.data.is_null()
    {
        return gamesdb_to_game_info(&c.data);
    }
    let etag = cached.as_ref().and_then(|c| c.etag.clone());
    let mut unavailable = true;
    for attempt in 0..GAMESDB_RETRIES {
        match api::gamesdb(
            &id,
            item.certificate.as_deref(),
            Some(&token),
            etag.as_deref(),
        )
        .await
        {
            Ok(GamesDb::NotModified) => {
                if let Some(mut c) = cached {
                    c.fetched_at = now;
                    let mapped = gamesdb_to_game_info(&c.data);
                    let _ = tokio::task::spawn_blocking(move || json_store::write(&file, &c)).await;
                    return mapped;
                }
                break;
            }
            Ok(GamesDb::NotFound) => break,
            Ok(GamesDb::Data { json, etag }) => {
                let mapped = gamesdb_to_game_info(&json);
                let c = CachedGamesDb {
                    etag,
                    fetched_at: now,
                    data: json,
                };
                let _ = tokio::task::spawn_blocking(move || json_store::write(&file, &c)).await;
                if mapped.is_some() {
                    return mapped;
                }
                unavailable = false;
                break;
            }
            Err(e) => {
                tracing::warn!(
                    "GamesDB lookup of {id} failed (attempt {}): {e:#}",
                    attempt + 1
                );
                if attempt + 1 < GAMESDB_RETRIES {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }
    if !unavailable {
        return None;
    }
    match api::product(&id, &["description"], &locale, Some(&token)).await {
        Ok(Some(p)) => {
            tracing::warn!("using product API metadata for GOG game {id}");
            Some(MappedRelease {
                info: product_to_game_info(&p, &id),
                release_id: String::new(),
                parent_release_id: None,
            })
        }
        Ok(None) => None,
        Err(e) => {
            if let Some(c) = cached {
                return gamesdb_to_game_info(&c.data);
            }
            tracing::error!("no metadata for GOG game {id}: {e:#}");
            None
        }
    }
}

impl GogStore {
    /// Locale for GOG web APIs from the UI language.
    pub(crate) fn api_locale(&self) -> String {
        let lang = (self.settings)().ui_language.clone();
        if lang.is_empty() {
            "en-US".into()
        } else {
            lang
        }
    }

    /// Re-fetch the owned catalogue and publish it.
    pub(crate) async fn refresh_library(&self, full: bool) -> Result<()> {
        let _guard = self.refresh_lock.lock().await;
        if self.user_data().is_none() {
            return Ok(());
        }
        let creds = self.auth.credentials().await?;
        let items: Vec<api::GalaxyItem> = api::galaxy_library(&creds)
            .await?
            .into_iter()
            .filter(|i| i.platform_id == "gog")
            .collect();
        tracing::info!("GOG library: {} owned releases", items.len());
        let added: HashMap<String, String> = items
            .iter()
            .filter_map(|i| Some((api::id_string(&i.external_id), i.date_added()?)))
            .collect();
        // Games hidden on gog.com; when that cannot be read, the previous
        // answer stands.
        let hidden = match api::hidden_products(&creds).await {
            Ok(set) => Some(set),
            Err(e) => {
                tracing::warn!("GOG hidden games: {e:#}");
                None
            }
        };

        let cache_dir = self.paths.gamesdb_cache_dir();
        let locale = self.api_locale();
        let mut entries: Vec<MappedRelease> = futures::stream::iter(items)
            .map(|item| {
                fetch_release(
                    item,
                    cache_dir.clone(),
                    creds.access_token.clone(),
                    locale.clone(),
                    full,
                )
            })
            .buffer_unordered(CONCURRENCY)
            .filter_map(|m| async move { m })
            .collect()
            .await;
        link_dlcs(&mut entries);

        let previous = self.state.read().catalogue.clone();
        let mut seen = HashSet::new();
        let mut catalogue: HashMap<String, Arc<GameInfo>> = HashMap::new();
        for e in entries {
            let mut info = e.info;
            if !seen.insert(info.app_name.clone()) {
                continue;
            }
            info.date_added = added.get(&info.app_name).cloned();
            let old = previous.get(&info.app_name);
            info.store_hidden = match &hidden {
                Some(set) => set.contains(&info.app_name),
                None => old.is_some_and(|o| o.store_hidden),
            };
            if let Some(old) = old {
                info.folder_name = old.folder_name.clone();
                info.cloud_save_enabled = old.cloud_save_enabled;
                info.gog_save_locations = old.gog_save_locations.clone();
            }
            catalogue.insert(info.app_name.clone(), Arc::new(info));
        }

        self.update_cloud_save_locations(&mut catalogue).await;
        self.reconcile_installs().await;

        self.save_catalogue(&catalogue).await?;
        self.state.write().catalogue = catalogue;
        self.rebuild_view();
        self.bus.emit(Event::LibraryChanged(Runner::Gog));
        Ok(())
    }

    /// Query remote-config for installed Windows/macOS games.
    async fn update_cloud_save_locations(&self, catalogue: &mut HashMap<String, Arc<GameInfo>>) {
        let installed: Vec<_> = self
            .state
            .read()
            .installed
            .values()
            .filter(|r| r.platform != InstallPlatform::Linux)
            .cloned()
            .collect();
        let results: Vec<(String, Result<Option<Vec<CloudSaveLocation>>>)> =
            futures::stream::iter(installed)
                .map(|rec| async move {
                    let locs = self.save_locations_for(&rec).await;
                    (rec.app_name, locs)
                })
                .buffer_unordered(CONCURRENCY)
                .collect()
                .await;
        for (app, locs) in results {
            let Some(entry) = catalogue.get_mut(&app) else {
                continue;
            };
            let mut info = (**entry).clone();
            match locs {
                Ok(Some(l)) => {
                    info.cloud_save_enabled = true;
                    info.gog_save_locations = l;
                }
                Ok(None) => {
                    info.cloud_save_enabled = false;
                    info.gog_save_locations.clear();
                }
                Err(e) => {
                    tracing::warn!("cloud save locations of {app}: {e:#}");
                    continue;
                }
            }
            *entry = Arc::new(info);
        }
    }

    async fn save_catalogue(&self, catalogue: &HashMap<String, Arc<GameInfo>>) -> Result<()> {
        let mut list: Vec<GameInfo> = catalogue.values().map(|g| (**g).clone()).collect();
        list.sort_by(|a, b| a.app_name.cmp(&b.app_name));
        let path = self.paths.library_cache();
        tokio::task::spawn_blocking(move || json_store::write(&path, &list))
            .await
            .context("saving the GOG library")??;
        Ok(())
    }

    /// Persist the catalogue after a single entry changed (folder name, saves).
    pub(crate) async fn update_catalogue_entry(
        &self,
        app_name: &str,
        f: impl FnOnce(&mut GameInfo),
    ) {
        let catalogue = {
            let mut st = self.state.write();
            let Some(entry) = st.catalogue.get(app_name) else {
                return;
            };
            let mut info = (**entry).clone();
            f(&mut info);
            st.catalogue.insert(app_name.to_string(), Arc::new(info));
            st.catalogue.clone()
        };
        self.rebuild_view();
        if let Err(e) = self.save_catalogue(&catalogue).await {
            tracing::warn!("{e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cyberpunk() -> Value {
        serde_json::json!({
          "id": "51811581085313589", "external_id": "1423049311", "parent_id": null, "type": "game",
          "supported_operating_systems": [{"slug": "windows"}, {"slug": "osx"}],
          "title": {"*": "Cyberpunk 2077 "}, "summary": {"*": "RPG"},
          "first_release_date": "2020-12-10T00:00:00+0000",
          "game": {
            "type": "game", "visible_in_library": true, "title": {"*": "x"},
            "aggregated_rating": 75.2381,
            "developers": [{"name": "CD Projekt RED"}], "publishers": [{"name": "CD PROJEKT"}],
            "genres": [{"name": {"*": "Role-playing (RPG)"}}],
            "background": {"url_format": "https://i/bg{formatter}.{ext}?namespace=gamesdb"},
            "vertical_cover": {"url_format": "https://i/vc{formatter}.{ext}"},
            "logo": {"url_format": "https://i/logo{formatter}.{ext}"},
            "square_icon": {"url_format": "https://i/sq{formatter}.{ext}"}
          }
        })
    }

    #[test]
    fn maps_gamesdb_game() {
        let m = gamesdb_to_game_info(&cyberpunk()).unwrap();
        let g = m.info;
        assert_eq!(g.app_name, "1423049311");
        assert_eq!(g.title, "Cyberpunk 2077");
        assert_eq!(g.developer.as_deref(), Some("CD Projekt RED"));
        assert_eq!(
            g.art_background.as_deref(),
            Some("https://i/bg.webp?namespace=gamesdb")
        );
        assert_eq!(g.art_cover.as_deref(), Some("https://i/vc.jpg"));
        assert_eq!(g.art_square.as_deref(), Some("https://i/logo.jpg"));
        assert_eq!(g.art_icon.as_deref(), Some("https://i/sq.jpg"));
        assert!(g.is_mac_native && !g.is_linux_native);
        assert_eq!(
            g.platforms,
            vec![InstallPlatform::Windows, InstallPlatform::Mac]
        );
        assert_eq!(g.genres, vec!["Role-playing (RPG)"]);
        assert_eq!(g.release_date.as_deref(), Some("2020-12-10"));
        assert_eq!(g.rating, Some(75));
        assert!(!g.is_dlc && g.can_run_offline);
        assert_eq!(m.release_id, "51811581085313589");
    }

    #[test]
    fn hidden_games_are_skipped_and_dlcs_linked() {
        let mut hidden = cyberpunk();
        hidden["game"]["visible_in_library"] = Value::Bool(false);
        assert!(gamesdb_to_game_info(&hidden).is_none());

        let dlc = serde_json::json!({
            "id": "55889890102136005", "external_id": 1256837418, "parent_id": "51811581085313589",
            "supported_operating_systems": [{"slug": "windows"}, {"slug": "linux"}],
            "title": {"*": "Phantom Liberty"},
            "game": {"type": "dlc", "visible_in_library": false}
        });
        let mut entries = vec![
            gamesdb_to_game_info(&cyberpunk()).unwrap(),
            gamesdb_to_game_info(&dlc).unwrap(),
        ];
        assert!(entries[1].info.is_dlc && entries[1].info.is_linux_native);
        link_dlcs(&mut entries);
        assert_eq!(entries[0].info.dlcs.len(), 1);
        assert_eq!(entries[0].info.dlcs[0].app_name, "1256837418");
        assert!(entries[0].info.dlcs[0].owned);
    }

    #[test]
    fn maps_product_api() {
        let p = serde_json::json!({
            "id": 1207658924, "title": " Unreal Gold ", "game_type": "game", "is_installable": true,
            "content_system_compatibility": {"windows": true, "osx": false, "linux": true},
            "images": {"logo": "//images/logo.jpg", "icon": "", "background": "//images/bg.jpg"},
            "description": {"lead": "Lead", "full": ""},
            "links": {"product_card": "https://www.gog.com/game/unreal_gold"}
        });
        let g = product_to_game_info(&p, "1207658924");
        assert_eq!(g.title, "Unreal Gold");
        assert_eq!(g.art_cover.as_deref(), Some("https://images/logo.jpg"));
        assert_eq!(g.art_square.as_deref(), Some("https://images/logo.jpg"));
        assert_eq!(g.description.as_deref(), Some("Lead"));
        assert!(g.is_linux_native && !g.is_dlc && g.installable);
        assert_eq!(
            g.platforms,
            vec![InstallPlatform::Windows, InstallPlatform::Linux]
        );
    }

    #[test]
    fn redist_entry_is_hidden() {
        let g = redist_game_info(Path::new("/r"));
        assert!(g.is_dlc && g.is_installed);
        assert_eq!(g.app_name, REDIST_APP);
    }

    #[test]
    fn galaxy_items_carry_the_date_they_joined_the_account() {
        let item: api::GalaxyItem = serde_json::from_value(serde_json::json!({
            "platform_id": "gog", "external_id": 1423049311, "date_created": "1590589063"
        }))
        .unwrap();
        assert_eq!(item.date_added().as_deref(), Some("2020-05-27T14:17:43Z"));
        let owned: api::GalaxyItem = serde_json::from_value(serde_json::json!({
            "platform_id": "gog", "external_id": "1", "owned_since": 1600000000, "date_created": 1
        }))
        .unwrap();
        assert_eq!(owned.date_added().as_deref(), Some("2020-09-13T12:26:40Z"));
        let none: api::GalaxyItem =
            serde_json::from_value(serde_json::json!({"platform_id": "gog", "external_id": "1"}))
                .unwrap();
        assert_eq!(none.date_added(), None);
    }
}
