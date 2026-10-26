//! Reading Steam's own files: the library folders it knows, the app
//! manifests (`appmanifest_<id>.acf`) of what it installed in them, and the
//! playtime it recorded per user (`localconfig.vdf`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ember_runtime::vdf::{self, Vdf};

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
}

/// Bit of `StateFlags` set once an app is fully installed. It stays set
/// while an update downloads, so those games remain playable.
const STATE_FULLY_INSTALLED: u64 = 4;

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
    if flags & STATE_FULLY_INSTALLED == 0 || is_tool(app_id, name) {
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

/// Minutes played per app ID, from the `localconfig.vdf` of the Steam user
/// who used this computer last (the most recently written one).
pub(crate) fn playtime_minutes(steam_dir: &Path) -> HashMap<String, u64> {
    let Some(config) = latest_local_config(steam_dir) else {
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
        assert_eq!(playtime_minutes(dir.path())["620"], 95);
        assert!(playtime_minutes(&dir.path().join("none")).is_empty());
    }
}
