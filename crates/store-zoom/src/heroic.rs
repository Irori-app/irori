//! Carry a Heroic ZOOM setup over: login token, installed games and the
//! cached library listing.
//!
//! Heroic files read (relative to its config dir):
//! `zoom_store/.zoom.token` (raw token), `zoom_store/config.json`
//! (`username`), `zoom_store/installed.json` (`installed: InstalledInfo[]`,
//! possibly with duplicates) and `store_cache/zoom_library.json`
//! (`games: GameInfo[]`).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use ember_core::{InstallPlatform, json_store};
use serde::Deserialize;
use serde_json::Value;

use crate::api::ZoomGame;
use crate::mapping;
use crate::records::{self, Auth, InstalledRecord, Layout, LibraryCache};

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct HeroicInstalled {
    app_name: String,
    #[serde(rename = "install_path")]
    install_path: String,
    executable: String,
    platform: String,
    is_dosbox: bool,
    dosbox_conf: Option<Vec<String>>,
    #[serde(rename = "install_size")]
    install_size: String,
    version: String,
    language: Option<String>,
    pinned_version: bool,
}

/// Copy the user's Heroic ZOOM login and installs into Ember's store files.
/// Existing Ember installs of the same game are kept.
pub fn import_from_heroic(heroic_config_dir: &Path) -> Result<()> {
    import_into(heroic_config_dir, &Layout::default())
}

pub(crate) fn import_into(heroic: &Path, layout: &Layout) -> Result<()> {
    let store = heroic.join("zoom_store");

    let token = std::fs::read_to_string(store.join(".zoom.token"))
        .map(|t| t.trim().to_string())
        .unwrap_or_default();
    if !token.is_empty() {
        let config: Value = json_store::read(&store.join("config.json"))
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        let username = config
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        records::save_auth(layout, &Auth { token, username })
            .context("saving the imported ZOOM login")?;
    }

    let installed: Value = json_store::read(&store.join("installed.json"))
        .ok()
        .flatten()
        .unwrap_or(Value::Null);
    let entries: Vec<HeroicInstalled> = installed
        .get("installed")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    if !entries.is_empty() {
        let mut ours = records::load_installed(layout);
        for e in entries {
            if e.app_name.is_empty() || e.install_path.is_empty() {
                continue;
            }
            // Later entries win: Heroic appends a new entry on every reinstall.
            if ours.get(&e.app_name).is_some_and(|r| r.installed_at > 0) {
                continue;
            }
            ours.insert(e.app_name.clone(), convert(e));
        }
        records::save_installed(layout, &ours)?;
    }

    let cache_file = heroic.join("store_cache/zoom_library.json");
    if !layout.library_cache().exists() && cache_file.is_file() {
        let v: Value = json_store::read(&cache_file)
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        let games: Vec<ZoomGame> = v
            .get("games")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(game_from_heroic).collect())
            .unwrap_or_default();
        if !games.is_empty() {
            json_store::write(
                &layout.library_cache(),
                &LibraryCache {
                    fetched_at: 0,
                    games,
                },
            )?;
        }
    }
    Ok(())
}

fn relative_to(root: &Path, p: &str) -> String {
    let path = PathBuf::from(p);
    path.strip_prefix(root)
        .map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string())
}

fn convert(e: HeroicInstalled) -> InstalledRecord {
    let root = PathBuf::from(&e.install_path);
    let platform = InstallPlatform::parse(&e.platform).unwrap_or(InstallPlatform::Windows);
    let confs: Vec<String> = e
        .dosbox_conf
        .unwrap_or_default()
        .iter()
        .map(|c| relative_to(&root, c))
        .collect();
    let (executable, dosbox_dir, build_platform) = if e.is_dosbox {
        // Heroic stores `dosbox` (system DOSBox) and copied the configs to
        // the install root; keep the bundled exe as the Wine fallback.
        let bundled = crate::detect::find_dosbox_exe(&root)
            .map(|p| relative_to(&root, &p.to_string_lossy()))
            .unwrap_or_default();
        let dir = confs
            .first()
            .and_then(|c| Path::new(c).parent())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        (bundled, dir, InstallPlatform::Windows)
    } else {
        (relative_to(&root, &e.executable), String::new(), platform)
    };
    InstalledRecord {
        app_name: e.app_name,
        install_path: e.install_path,
        executable,
        platform,
        build_platform,
        is_dosbox: e.is_dosbox,
        dosbox_conf: confs,
        dosbox_dir,
        install_size_bytes: mapping::parse_size(&e.install_size).unwrap_or(0),
        language: e.language.filter(|l| !l.is_empty()),
        version: if e.version.is_empty() {
            "N/A".into()
        } else {
            e.version
        },
        files: Vec::new(),
        pinned_version: e.pinned_version,
        installed_at: 0,
    }
}

fn game_from_heroic(v: &Value) -> Option<ZoomGame> {
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let id = s("app_name");
    if id.is_empty() {
        return None;
    }
    let mut os = vec!["windows".to_string()];
    if v.get("is_linux_native").and_then(Value::as_bool) == Some(true) {
        os.push("linux".into());
    }
    if v.get("is_mac_native").and_then(Value::as_bool) == Some(true) {
        os.push("osx".into());
    }
    let split = |k: &str| {
        s(k).split(", ")
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    Some(ZoomGame {
        id,
        slug: s("folder_name"),
        name: s("title"),
        poster_url: s("art_cover"),
        description: v
            .pointer("/extra/about/description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        store_url: s("store_url"),
        operating_systems: os,
        developers: split("developer"),
        publishers: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_heroic_zoom_state() {
        let d = tempfile::tempdir().unwrap();
        let heroic = d.path().join("heroic");
        let game_dir = d.path().join("Games/some-game");
        std::fs::create_dir_all(game_dir.join("DOSBOX")).unwrap();
        std::fs::write(game_dir.join("DOSBOX/DOSBox.exe"), b"MZ").unwrap();
        std::fs::create_dir_all(heroic.join("zoom_store")).unwrap();
        std::fs::create_dir_all(heroic.join("store_cache")).unwrap();
        std::fs::write(heroic.join("zoom_store/.zoom.token"), "tok-REDACTED\n").unwrap();
        std::fs::write(
            heroic.join("zoom_store/config.json"),
            "{\n\t\"isLoggedIn\": true,\n\t\"username\": \"player\"\n}",
        )
        .unwrap();
        let gd = game_dir.to_string_lossy();
        let installed = serde_json::json!({"installed": [
            {"platform":"windows","executable":"Old.exe","install_path":gd,"install_size":"1 GiB",
             "is_dlc":false,"version":"1.0","appName":"7","installedDLCs":[]},
            {"platform":"linux","executable":"dosbox","install_path":gd,"isDosbox":true,
             "dosboxConf":[format!("{gd}/game.conf"), format!("{gd}/game_single.conf")],
             "install_size":"1.5 GiB","is_dlc":false,"version":"1.0","appName":"7","pinnedVersion":true},
            {"platform":"linux","executable":"/opt/x/start.sh","install_path":"/opt/x",
             "install_size":"10 MiB","is_dlc":false,"version":"1.0","appName":"8"}
        ]});
        std::fs::write(
            heroic.join("zoom_store/installed.json"),
            serde_json::to_string_pretty(&installed).unwrap(),
        )
        .unwrap();
        std::fs::write(
            heroic.join("store_cache/zoom_library.json"),
            serde_json::json!({"games":[{"runner":"zoom","app_name":"7","title":"Some Game",
                "folder_name":"some-game","art_cover":"https://p","store_url":"https://s",
                "is_linux_native":true,"developer":"A, B","extra":{"about":{"description":"D"}}}],
                "__timestamp":{"games":"Thu Oct 22 2026"}})
            .to_string(),
        )
        .unwrap();

        let layout = Layout {
            config_dir: d.path().join("ember/config"),
            cache_dir: d.path().join("ember/cache"),
        };
        import_into(&heroic, &layout).unwrap();

        let auth = records::load_auth(&layout).unwrap();
        assert_eq!(auth.token, "tok-REDACTED");
        assert_eq!(auth.username, "player");

        let inst = records::load_installed(&layout);
        let g7 = &inst["7"];
        assert!(g7.is_dosbox && g7.pinned_version);
        assert_eq!(g7.dosbox_conf, vec!["game.conf", "game_single.conf"]);
        assert_eq!(g7.dosbox_dir, "");
        assert_eq!(Path::new(&g7.executable), Path::new("DOSBOX/DOSBox.exe"));
        assert_eq!(g7.platform, InstallPlatform::Linux);
        assert_eq!(g7.build_platform, InstallPlatform::Windows);
        assert_eq!(g7.install_size_bytes, 1536 * 1024 * 1024);
        let g8 = &inst["8"];
        assert_eq!(g8.executable, "start.sh");
        assert_eq!(g8.platform, InstallPlatform::Linux);

        let lib: LibraryCache = json_store::read(&layout.library_cache()).unwrap().unwrap();
        assert_eq!(lib.games[0].name, "Some Game");
        assert_eq!(lib.games[0].slug, "some-game");
        assert_eq!(lib.games[0].developers, vec!["A", "B"]);
        assert!(
            lib.games[0]
                .operating_systems
                .contains(&"linux".to_string())
        );
    }

    #[test]
    fn missing_heroic_data_is_fine() {
        let d = tempfile::tempdir().unwrap();
        let layout = Layout {
            config_dir: d.path().join("c"),
            cache_dir: d.path().join("k"),
        };
        import_into(&d.path().join("nope"), &layout).unwrap();
        assert!(records::load_auth(&layout).is_none());
    }
}
