//! Carry a Heroic GOG setup over to Irori: session tokens, account, installed
//! games (gogdl manifests become build records), support files, private
//! branch passwords, save
//! sync timestamps, queued playtime and the cached library.
//!
//! Existing Irori data wins over Heroic's where both exist. Token contents are
//! never logged.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::Value;

use irori_core::{CloudSaveLocation, ExtraInfo, GameInfo, InstallPlatform, Runner, json_store};

use crate::auth::{CLIENT_ID, UserData};
use crate::installed::{InstalledFile, InstalledRecord};
use crate::library::REDIST_APP;
use crate::paths::GogPaths;
use crate::playtime::{PlaytimeQueue, QueuedSession};

/// Import from a Heroic config dir (`~/.config/heroic`) into Irori's layout.
pub fn import_from_heroic(heroic_config_dir: &Path) -> Result<()> {
    import_into(heroic_config_dir, &GogPaths::from_irori())
}

/// Heroic's GOG library entry (subset).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct HeroicGame {
    app_name: String,
    title: String,
    developer: Option<String>,
    art_cover: Option<String>,
    art_square: Option<String>,
    art_background: Option<String>,
    art_icon: Option<String>,
    art_logo: Option<String>,
    folder_name: Option<String>,
    is_linux_native: bool,
    is_mac_native: bool,
    cloud_save_enabled: bool,
    gog_save_location: Option<Vec<CloudSaveLocation>>,
    extra: Option<Value>,
    install: Option<Value>,
}

fn map_heroic_game(h: HeroicGame) -> GameInfo {
    let description = h
        .extra
        .as_ref()
        .and_then(|e| e.pointer("/about/description"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let genres: Vec<String> = h
        .extra
        .as_ref()
        .and_then(|e| e.get("genres"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|g| g.as_str().map(str::to_string))
        .collect();
    let mut platforms = vec![InstallPlatform::Windows];
    if h.is_linux_native {
        platforms.push(InstallPlatform::Linux);
    }
    if h.is_mac_native {
        platforms.push(InstallPlatform::Mac);
    }
    GameInfo {
        runner: Some(Runner::Gog),
        app_name: h.app_name,
        title: h.title,
        developer: h.developer.filter(|d| !d.is_empty()),
        description: description.clone(),
        // Heroic's "cover" is landscape and its "square" is the vertical cover.
        art_cover: h
            .art_square
            .clone()
            .filter(|s| !s.is_empty())
            .or(h.art_cover.clone()),
        art_square: h.art_cover.filter(|s| !s.is_empty()),
        art_background: h.art_background.filter(|s| !s.is_empty()),
        art_icon: h.art_icon.filter(|s| !s.is_empty()),
        art_logo: h.art_logo.filter(|s| !s.is_empty()),
        folder_name: h.folder_name.filter(|s| !s.is_empty()),
        is_linux_native: h.is_linux_native,
        is_mac_native: h.is_mac_native,
        platforms,
        cloud_save_enabled: h.cloud_save_enabled,
        gog_save_locations: h.gog_save_location.unwrap_or_default(),
        installable: true,
        can_run_offline: true,
        is_dlc: h
            .install
            .as_ref()
            .and_then(|i| i.get("is_dlc"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        genres: genres.clone(),
        extra: Some(ExtraInfo {
            description,
            genres,
            ..Default::default()
        }),
        ..Default::default()
    }
    .finalize()
}

fn read_value(path: &Path) -> Option<Value> {
    json_store::read::<Value>(path).ok().flatten()
}

fn merge_string_map(src: &Path, dst: &Path) -> Result<()> {
    let Some(Value::Object(theirs)) = read_value(src) else {
        return Ok(());
    };
    let mut ours: serde_json::Map<String, Value> = match read_value(dst) {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    for (k, v) in theirs {
        if k.starts_with("__") {
            continue;
        }
        ours.entry(k).or_insert(v);
    }
    json_store::write(dst, &Value::Object(ours))
}

pub(crate) fn import_into(heroic: &Path, dst: &GogPaths) -> Result<()> {
    let store = heroic.join("gog_store");
    let gogdl = heroic.join("gogdlConfig").join("heroic_gogdl");
    std::fs::create_dir_all(&dst.config)
        .with_context(|| format!("creating {}", dst.config.display()))?;

    // Session.
    let auth = read_value(&store.join("auth.json"));
    let has_session = auth
        .as_ref()
        .and_then(|a| a.get(CLIENT_ID))
        .and_then(|c| c.get("refresh_token"))
        .and_then(Value::as_str)
        .is_some_and(|t| !t.is_empty());
    let config = read_value(&store.join("config.json"));
    let logged_in = config
        .as_ref()
        .and_then(|c| c.get("isLoggedIn"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if has_session && logged_in && !dst.auth_file().exists() {
        if let Some(a) = &auth {
            json_store::write(&dst.auth_file(), a).context("saving the imported GOG session")?;
        }
        let user = config.as_ref().and_then(|c| c.get("userData"));
        let data = UserData {
            id: user
                .and_then(|u| u.get("id"))
                .map(crate::api::id_string)
                .unwrap_or_default(),
            username: user
                .and_then(|u| u.get("username"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            avatar: user
                .and_then(|u| u.pointer("/avatar/medium"))
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        json_store::write(&dst.user_file(), &data)?;
    }

    // Installed games.
    let theirs: InstalledFile = json_store::read(&store.join("installed.json"))
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut ours: InstalledFile = json_store::read_or_default(&dst.installed_file());
    let mut imported_apps: Vec<String> = Vec::new();
    for rec in theirs.into_records() {
        if !ours.installed.iter().any(|r| r.app_name == rec.app_name) {
            imported_apps.push(rec.app_name.clone());
            ours.installed.push(rec);
        }
    }
    ours.installed
        .sort_by(|a: &InstalledRecord, b| a.app_name.cmp(&b.app_name));
    json_store::write(&dst.installed_file(), &ours)?;

    // Build records and support files of the imported games.
    crate::migrate::convert_gogdl_dir(&gogdl, dst, Some(&imported_apps))?;
    let imported: Vec<InstalledRecord> = ours
        .installed
        .iter()
        .filter(|r| imported_apps.contains(&r.app_name))
        .cloned()
        .collect();
    crate::migrate::convert_linux_installs(dst, &imported)?;

    merge_string_map(
        &store.join("privateBranches.json"),
        &dst.private_branches_file(),
    )?;
    merge_string_map(
        &store.join("saveTimestamps.json"),
        &dst.save_timestamps_file(),
    )?;

    // Queued playtime.
    if let Some(Value::Object(q)) = read_value(
        &heroic
            .join("store_cache")
            .join("gog_playtime_sync_queue.json"),
    ) {
        let mut queue: PlaytimeQueue = json_store::read_or_default(&dst.playtime_queue_file());
        let mut changed = false;
        for (user, sessions) in q {
            if user == "lock" || user.starts_with("__") {
                continue;
            }
            let list: Vec<QueuedSession> = serde_json::from_value(sessions).unwrap_or_default();
            let entry = queue.entry(user).or_default();
            for s in list {
                if !entry.contains(&s) {
                    entry.push(s);
                    changed = true;
                }
            }
        }
        if changed {
            json_store::write(&dst.playtime_queue_file(), &queue)?;
        }
    }

    // Library snapshot, so the catalogue shows before the first refresh.
    if !dst.library_cache().exists()
        && let Some(lib) = read_value(&heroic.join("store_cache").join("gog_library.json"))
    {
        let games: Vec<GameInfo> = lib
            .get("games")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|g| serde_json::from_value::<HeroicGame>(g.clone()).ok())
            .filter(|g| !g.app_name.is_empty() && g.app_name != REDIST_APP)
            .map(map_heroic_game)
            .collect();
        let map: HashMap<String, GameInfo> =
            games.into_iter().map(|g| (g.app_name.clone(), g)).collect();
        let mut list: Vec<GameInfo> = map.into_values().collect();
        list.sort_by(|a, b| a.app_name.cmp(&b.app_name));
        json_store::write(&dst.library_cache(), &list)?;
    }
    tracing::info!(
        "imported GOG data from Heroic ({} installed games)",
        imported_apps.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_heroic_layout() {
        let h = tempfile::tempdir().unwrap();
        let e = tempfile::tempdir().unwrap();
        let store = h.path().join("gog_store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            store.join("auth.json"),
            format!(r#"{{"{CLIENT_ID}": {{"access_token": "REDACTED_A", "refresh_token": "REDACTED_R", "user_id": "7", "expires_in": 3600, "loginTime": 1.0}}}}"#),
        )
        .unwrap();
        std::fs::write(
            store.join("config.json"),
            r#"{"isLoggedIn": true, "userData": {"id": "7", "username": "player", "email": "x@y", "avatar": {"medium": "https://a"}}}"#,
        )
        .unwrap();
        std::fs::write(
            store.join("installed.json"),
            r#"{"installed": [{"platform": "windows", "executable": "", "install_path": "/g/CP", "install_size": "1 GiB",
               "is_dlc": false, "version": "2.5", "appName": "1423049311", "installedDLCs": [], "pinnedVersion": false}]}"#,
        )
        .unwrap();
        std::fs::write(
            store.join("privateBranches.json"),
            r#"{"1423049311": "pw"}"#,
        )
        .unwrap();
        let gogdl = h.path().join("gogdlConfig/heroic_gogdl");
        std::fs::create_dir_all(gogdl.join("manifests")).unwrap();
        std::fs::write(gogdl.join("manifests/1423049311"), r#"{"version": 2}"#).unwrap();
        std::fs::create_dir_all(gogdl.join("gog-support/1423049311/1423049311")).unwrap();
        std::fs::write(
            gogdl.join("gog-support/1423049311/1423049311/setup.exe"),
            b"MZ",
        )
        .unwrap();
        let cache = h.path().join("store_cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join("gog_playtime_sync_queue.json"),
            r#"{"7": [{"appName": "1423049311", "session_date": 100, "time": 5}], "lock": [], "__timestamp": {}}"#,
        )
        .unwrap();
        std::fs::write(
            cache.join("gog_library.json"),
            r#"{"games": [{"app_name": "gog-redist", "title": "R"},
                {"runner": "gog", "app_name": "1423049311", "title": "Cyberpunk 2077", "art_cover": "land", "art_square": "vert",
                 "folder_name": "Cyberpunk 2077", "cloud_save_enabled": true, "canRunOffline": true,
                 "gog_save_location": [{"name": "saves", "location": "<?SAVED_GAMES?>/CP"}],
                 "extra": {"about": {"description": "RPG"}, "genres": ["RPG"]}, "install": {"is_dlc": false}}]}"#,
        )
        .unwrap();

        let dst = GogPaths::under(e.path());
        import_into(h.path(), &dst).unwrap();

        let auth: Value = json_store::read(&dst.auth_file()).unwrap().unwrap();
        assert_eq!(auth[CLIENT_ID]["user_id"], "7");
        let user: UserData = json_store::read(&dst.user_file()).unwrap().unwrap();
        assert_eq!(user.username, "player");
        let inst: InstalledFile = json_store::read(&dst.installed_file()).unwrap().unwrap();
        assert_eq!(inst.installed[0].app_name, "1423049311");
        assert!(dst.build_file("1423049311").is_file());
        assert!(
            dst.support_dir("1423049311")
                .join("1423049311/setup.exe")
                .is_file()
        );
        let q: PlaytimeQueue = json_store::read(&dst.playtime_queue_file())
            .unwrap()
            .unwrap();
        assert_eq!(q["7"][0].time, 5);
        let lib: Vec<GameInfo> = json_store::read(&dst.library_cache()).unwrap().unwrap();
        assert_eq!(lib.len(), 1);
        assert_eq!(lib[0].art_cover.as_deref(), Some("vert"));
        assert_eq!(lib[0].art_square.as_deref(), Some("land"));
        assert_eq!(lib[0].gog_save_locations[0].name, "saves");
        let pb: HashMap<String, String> = json_store::read(&dst.private_branches_file())
            .unwrap()
            .unwrap();
        assert_eq!(pb["1423049311"], "pw");

        // A second import keeps Irori's data and does not duplicate.
        import_into(h.path(), &dst).unwrap();
        let q: PlaytimeQueue = json_store::read(&dst.playtime_queue_file())
            .unwrap()
            .unwrap();
        assert_eq!(q["7"].len(), 1);
    }
}
