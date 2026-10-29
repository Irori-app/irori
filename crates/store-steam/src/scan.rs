//! Reading Steam's own files: the library folders it knows, the app
//! manifests (`appmanifest_<id>.acf`) of what it installed in them, and the
//! playtime it recorded per user (`localconfig.vdf`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use irori_runtime::vdf::{self, Vdf};

/// An app the Steam client installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledApp {
    pub app_id: String,
    pub name: String,
    /// `<library>/steamapps/common/<installdir>`.
    pub install_path: PathBuf,
    pub size_on_disk: u64,
    pub build_id: String,
    /// Unix time of the last update.
    pub last_updated: Option<u64>,
    /// The `appmanifest_<id>.acf` it was read from.
    pub manifest: PathBuf,
}

/// Bits of a manifest's `StateFlags` (Steam's `EAppState`).
pub(crate) mod state {
    /// Set once an app is fully installed. It stays set while an update
    /// downloads, so those games remain playable.
    pub const FULLY_INSTALLED: u64 = 1 << 2;
    pub const APP_RUNNING: u64 = 1 << 6;
    pub const UPDATE_RUNNING: u64 = 1 << 8;
    pub const UPDATE_STARTED: u64 = 1 << 10;
    pub const VALIDATING: u64 = 1 << 14;

    /// States that only occur while Steam prepares or runs a launch. A
    /// pending or paused update is a library state and doesn't count.
    pub const LAUNCHING_OR_RUNNING: u64 =
        APP_RUNNING | UPDATE_RUNNING | UPDATE_STARTED | VALIDATING;
}

/// `StateFlags` of the manifest at `path`, read afresh.
pub(crate) fn manifest_flags(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc = vdf::parse(&text);
    doc.get("AppState")?
        .get("StateFlags")?
        .as_str()?
        .trim()
        .parse()
        .ok()
}

/// Steam tools that are installed like games but are not games: Proton
/// builds, the Steam Linux Runtime containers, the shared redistributables
/// and SteamVR.
const TOOL_APP_IDS: [&str; 6] = [
    "228980",  // Steamworks Common Redistributables
    "250820",  // SteamVR
    "1070560", // Steam Linux Runtime 1.0 (scout)
    "1391110", // Steam Linux Runtime 2.0 (soldier)
    "1628350", // Steam Linux Runtime 3.0 (sniper)
    "1826330", // Proton EasyAntiCheat Runtime
];

/// Name prefixes of tools Valve adds with each release (every Proton
/// version has its own app ID).
const TOOL_NAME_PREFIXES: [&str; 4] = [
    "Proton ",
    "Steam Linux Runtime",
    "Steamworks Common Redistributables",
    "SteamVR",
];

fn is_tool(app_id: &str, name: &str) -> bool {
    TOOL_APP_IDS.contains(&app_id) || TOOL_NAME_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Parse one `appmanifest_<id>.acf` found in `library`. `None` for tools,
/// apps that are not fully installed and unreadable manifests.
pub(crate) fn parse_manifest(text: &str, library: &Path) -> Option<InstalledApp> {
    let doc = vdf::parse(text);
    let state = doc.get("AppState")?;
    let field = |key: &str| state.get(key).and_then(Vdf::as_str).map(str::trim);
    let app_id = field("appid").filter(|id| id.chars().all(|c| c.is_ascii_digit()))?;
    let name = field("name").filter(|n| !n.is_empty())?;
    let install_dir = field("installdir").filter(|d| !d.is_empty())?;
    let flags: u64 = field("StateFlags")?.parse().ok()?;
    if flags & state::FULLY_INSTALLED == 0 || is_tool(app_id, name) {
        return None;
    }
    Some(InstalledApp {
        app_id: app_id.to_string(),
        name: name.to_string(),
        install_path: library.join("steamapps").join("common").join(install_dir),
        size_on_disk: field("SizeOnDisk")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        build_id: field("buildid").unwrap_or_default().to_string(),
        last_updated: field("LastUpdated")
            .and_then(|s| s.parse().ok())
            .filter(|t| *t > 0),
        manifest: library
            .join("steamapps")
            .join(format!("appmanifest_{app_id}.acf")),
    })
}

/// The Steam folder itself plus every library in its
/// `steamapps/libraryfolders.vdf`, existing ones only, without duplicates
/// (`~/.steam/steam` is usually a link to `~/.local/share/Steam`).
pub(crate) fn libraries(steam_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![steam_dir.to_path_buf()];
    let list = steam_dir.join("steamapps").join("libraryfolders.vdf");
    if let Ok(text) = std::fs::read_to_string(&list) {
        candidates.extend(vdf::library_folders(&vdf::parse(&text)));
    }
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|p| p.join("steamapps").is_dir())
        .filter(|p| seen.insert(std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())))
        .collect()
}

/// Every game installed in the Steam libraries of `steam_dir`, sorted by
/// app ID. An app installed in two libraries is listed once.
pub(crate) fn installed_apps(steam_dir: &Path) -> Vec<InstalledApp> {
    let mut apps: HashMap<String, InstalledApp> = HashMap::new();
    for library in libraries(steam_dir) {
        let Ok(entries) = std::fs::read_dir(library.join("steamapps")) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                tracing::debug!("unreadable Steam manifest {}", entry.path().display());
                continue;
            };
            if let Some(app) = parse_manifest(&text, &library) {
                apps.entry(app.app_id.clone()).or_insert(app);
            }
        }
    }
    let mut apps: Vec<InstalledApp> = apps.into_values().collect();
    apps.sort_by(|a, b| a.app_id.cmp(&b.app_id));
    apps
}

/// Steam's offset between a SteamID64 and the account ID that names the
/// user's `userdata` folder.
const STEAMID64_BASE: u64 = 76_561_197_960_265_728;

/// Minutes played per app ID, from the `localconfig.vdf` of `steamid` (the
/// signed-in account) when Steam has one for it, else of the Steam user who
/// used this computer last (the most recently written one).
pub(crate) fn playtime_minutes(steam_dir: &Path, steamid: Option<&str>) -> HashMap<String, u64> {
    let own = steamid
        .and_then(|id| id.parse::<u64>().ok())
        .and_then(|id| id.checked_sub(STEAMID64_BASE))
        .map(|account| {
            steam_dir
                .join("userdata")
                .join(account.to_string())
                .join("config")
                .join("localconfig.vdf")
        })
        .filter(|p| p.is_file());
    let Some(config) = own.or_else(|| latest_local_config(steam_dir)) else {
        return HashMap::new();
    };
    match std::fs::read_to_string(&config) {
        Ok(text) => parse_playtime(&text),
        Err(e) => {
            tracing::debug!("reading {}: {e}", config.display());
            HashMap::new()
        }
    }
}

fn latest_local_config(steam_dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(steam_dir.join("userdata"))
        .ok()?
        .flatten()
        .map(|user| user.path().join("config").join("localconfig.vdf"))
        .filter_map(|p| {
            let modified = std::fs::metadata(&p).ok()?.modified().ok()?;
            Some((modified, p))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, p)| p)
}

/// `UserLocalConfigStore/Software/Valve/Steam/apps/<id>/Playtime`, in
/// minutes. Steam's key case varies between versions; lookups ignore it.
pub(crate) fn parse_playtime(text: &str) -> HashMap<String, u64> {
    let doc = vdf::parse(text);
    let apps = ["UserLocalConfigStore", "Software", "Valve", "Steam", "apps"]
        .iter()
        .try_fold(&doc, |node, key| node.get(key));
    let Some(apps) = apps else {
        return HashMap::new();
    };
    apps.entries()
        .iter()
        .filter_map(|(id, app)| {
            let minutes: u64 = app.get("Playtime")?.as_str()?.trim().parse().ok()?;
            (minutes > 0).then(|| (id.clone(), minutes))
        })
        .collect()
}

/// The name `steamid` shows as in Steam, from the accounts Steam remembers
/// on this computer (`config/loginusers.vdf`).
pub(crate) fn persona_name(steam_dir: &Path, steamid: &str) -> Option<String> {
    let text = std::fs::read_to_string(steam_dir.join("config").join("loginusers.vdf")).ok()?;
    let doc = vdf::parse(&text);
    let name = doc
        .get("users")?
        .get(steamid)?
        .get("PersonaName")?
        .as_str()?
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
}

/// Art the Steam client keeps for an app in `appcache/librarycache/<id>`:
/// directly in that folder for older apps, in hashed subfolders (one per
/// version of the image) for newer ones. Paths of existing files only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LocalArt {
    pub cover: Option<PathBuf>,
    pub hero: Option<PathBuf>,
    pub logo: Option<PathBuf>,
    pub header: Option<PathBuf>,
}

/// File names Steam uses for each image, best first.
const COVER_NAMES: [&str; 3] = [
    "library_600x900_2x.jpg",
    "library_600x900.jpg",
    "library_capsule.jpg",
];
const HERO_NAMES: [&str; 1] = ["library_hero.jpg"];
const LOGO_NAMES: [&str; 1] = ["logo.png"];
const HEADER_NAMES: [&str; 2] = ["library_header.jpg", "header.jpg"];

/// The cached art of `app_id`. Blocking: one folder listing per app.
pub(crate) fn local_art(steam_dir: &Path, app_id: &str) -> LocalArt {
    let root = steam_dir.join("appcache").join("librarycache").join(app_id);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return LocalArt::default();
    };
    // (name, modified, path) of every image in the folder and one level down.
    let mut files: Vec<(String, std::time::SystemTime, PathBuf)> = Vec::new();
    let mut add = |path: PathBuf| {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            return;
        };
        let modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        files.push((name, modified, path));
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            for inner in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                add(inner.path());
            }
        } else {
            add(path);
        }
    }
    // The newest file of the best-ranked name: Steam leaves older versions
    // of an image behind when it fetches a new one.
    let pick = |names: &[&str]| {
        names.iter().find_map(|wanted| {
            files
                .iter()
                .filter(|(name, _, _)| name == wanted)
                .max_by_key(|(_, modified, _)| *modified)
                .map(|(_, _, path)| path.clone())
        })
    };
    LocalArt {
        cover: pick(&COVER_NAMES),
        hero: pick(&HERO_NAMES),
        logo: pick(&LOGO_NAMES),
        header: pick(&HEADER_NAMES),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str, name: &str, dir: &str, flags: u32) -> String {
        format!(
            "\"AppState\"\n{{\n\t\"appid\"\t\t\"{id}\"\n\t\"universe\"\t\t\"1\"\n\t\"name\"\t\t\"{name}\"\n\t\"StateFlags\"\t\t\"{flags}\"\n\t\"installdir\"\t\t\"{dir}\"\n\t\"LastUpdated\"\t\t\"1700000000\"\n\t\"SizeOnDisk\"\t\t\"123456\"\n\t\"buildid\"\t\t\"987\"\n\t\"UserConfig\"\n\t{{\n\t\t\"language\"\t\t\"english\"\n\t}}\n}}\n"
        )
    }

    #[test]
    fn reads_an_installed_game() {
        let app = parse_manifest(
            &manifest("620", "Portal 2", "Portal 2", 4),
            Path::new("/lib"),
        )
        .unwrap();
        assert_eq!(app.app_id, "620");
        assert_eq!(app.name, "Portal 2");
        assert_eq!(
            app.install_path,
            Path::new("/lib/steamapps/common/Portal 2")
        );
        assert_eq!(app.size_on_disk, 123_456);
        assert_eq!(app.build_id, "987");
        assert_eq!(app.last_updated, Some(1_700_000_000));
        // Installed with an update waiting (4 | 2) still counts.
        assert!(parse_manifest(&manifest("620", "Portal 2", "P", 6), Path::new("/l")).is_some());
    }

    #[test]
    fn skips_tools_partial_installs_and_garbage() {
        let lib = Path::new("/lib");
        assert!(parse_manifest(&manifest("1493710", "Proton Experimental", "P", 4), lib).is_none());
        assert!(parse_manifest(&manifest("1628350", "Sniper", "S", 4), lib).is_none());
        assert!(
            parse_manifest(
                &manifest("1", "Steam Linux Runtime 4.0", "SteamLinuxRuntime_4", 4),
                lib
            )
            .is_none()
        );
        // Still downloading for the first time.
        assert!(parse_manifest(&manifest("70", "Half-Life", "Half-Life", 1026), lib).is_none());
        assert!(parse_manifest("", lib).is_none());
        assert!(parse_manifest("\"AppState\" { \"appid\" \"x1\" }", lib).is_none());
    }

    #[test]
    fn scans_every_library_once() {
        let dir = tempfile::tempdir().unwrap();
        let steam = dir.path().join("Steam");
        let second = dir.path().join("Second Drive");
        std::fs::create_dir_all(steam.join("steamapps")).unwrap();
        std::fs::create_dir_all(second.join("steamapps")).unwrap();
        let escaped = |p: &Path| p.to_string_lossy().replace('\\', "\\\\");
        std::fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} \"1\" {{ \"path\" \"{}\" }} \"2\" {{ \"path\" \"{}\" }} }}",
                escaped(&steam),
                escaped(&second),
                escaped(&dir.path().join("unplugged"))
            ),
        )
        .unwrap();
        let write = |lib: &Path, id: &str, name: &str| {
            std::fs::write(
                lib.join(format!("steamapps/appmanifest_{id}.acf")),
                manifest(id, name, name, 4),
            )
            .unwrap();
        };
        write(&steam, "620", "Portal 2");
        write(&second, "400", "Portal");
        write(&second, "620", "Portal 2");
        write(&second, "228980", "Steamworks Common Redistributables");
        std::fs::write(second.join("steamapps/notes.txt"), "x").unwrap();

        assert_eq!(libraries(&steam), vec![steam.clone(), second.clone()]);
        let apps = installed_apps(&steam);
        let ids: Vec<&str> = apps.iter().map(|a| a.app_id.as_str()).collect();
        assert_eq!(ids, ["400", "620"]);
        assert_eq!(apps[0].install_path, second.join("steamapps/common/Portal"));
        assert!(installed_apps(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn reads_playtime_of_the_latest_user() {
        let text = r#"
"UserLocalConfigStore"
{
	"Software"
	{
		"valve"
		{
			"Steam"
			{
				"Apps"
				{
					"620" { "LastPlayed" "1700000000" "Playtime" "95" "Playtime2wks" "3" }
					"400" { "LastPlayed" "1" }
					"70" { "Playtime" "0" }
				}
			}
		}
	}
}"#;
        let minutes = parse_playtime(text);
        assert_eq!(minutes.len(), 1);
        assert_eq!(minutes["620"], 95);
        assert!(parse_playtime("garbage {").is_empty());

        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("userdata/1234/config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("localconfig.vdf"), text).unwrap();
        assert_eq!(playtime_minutes(dir.path(), None)["620"], 95);
        assert!(playtime_minutes(&dir.path().join("none"), None).is_empty());
        // The signed-in account's own file wins over the latest one.
        let own = dir.path().join("userdata/7/config");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(
            own.join("localconfig.vdf"),
            "\"UserLocalConfigStore\" { \"Software\" { \"Valve\" { \"Steam\" { \"apps\" { \"620\" { \"Playtime\" \"5\" } } } } } }",
        )
        .unwrap();
        let steamid = (STEAMID64_BASE + 7).to_string();
        assert_eq!(playtime_minutes(dir.path(), Some(&steamid))["620"], 5);
    }

    #[test]
    fn finds_cached_art_in_both_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("appcache/librarycache");
        // Older layout: files directly in the app's folder.
        std::fs::create_dir_all(cache.join("10")).unwrap();
        for name in [
            "library_600x900.jpg",
            "library_hero.jpg",
            "logo.png",
            "header.jpg",
        ] {
            std::fs::write(cache.join("10").join(name), "x").unwrap();
        }
        let old = local_art(dir.path(), "10");
        assert!(old.cover.unwrap().ends_with("10/library_600x900.jpg"));
        assert!(old.header.unwrap().ends_with("10/header.jpg"));
        assert!(old.hero.is_some() && old.logo.is_some());

        // Newer layout: one hashed folder per image.
        let app = cache.join("3393110");
        for (hash, name) in [
            ("145d", "library_capsule.jpg"),
            ("0f3e", "library_hero.jpg"),
            ("7d14", "library_header.jpg"),
            ("a2c3", "logo.png"),
        ] {
            std::fs::create_dir_all(app.join(hash)).unwrap();
            std::fs::write(app.join(hash).join(name), "x").unwrap();
        }
        let new = local_art(dir.path(), "3393110");
        assert!(new.cover.unwrap().ends_with("145d/library_capsule.jpg"));
        assert!(new.header.unwrap().ends_with("7d14/library_header.jpg"));
        assert!(new.hero.unwrap().ends_with("0f3e/library_hero.jpg"));
        assert!(new.logo.unwrap().ends_with("a2c3/logo.png"));

        assert_eq!(local_art(dir.path(), "1"), LocalArt::default());
    }

    #[test]
    fn reads_the_persona_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            dir.path().join("config/loginusers.vdf"),
            "\"users\" { \"76561197960265729\" { \"AccountName\" \"player\" \"PersonaName\" \"Player One\" } }",
        )
        .unwrap();
        assert_eq!(
            persona_name(dir.path(), "76561197960265729").as_deref(),
            Some("Player One")
        );
        assert!(persona_name(dir.path(), "1").is_none());
    }
}
