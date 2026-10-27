//! Rockstar Games Launcher: installed titles from the registry.
//!
//! Each installed title has a key under
//! `HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Rockstar Games\<title>` with its
//! folder in `InstallFolder`. The launcher and Social Club have keys there
//! too and are skipped. Titles not in [`TITLES`] are still listed, named after
//! their key.
//!
//! Games launch through the launcher:
//! `Launcher.exe -launchTitleInFolder "<install folder>"`.

use std::path::{Path, PathBuf};

use crate::{
    InstalledGame, LaunchSpec, Launcher, Registry, ScanReport, drive_c, registry, retain_present,
};

/// The registry key holding one subkey per title (both registry views are read).
pub const ROCKSTAR_KEY: &str = r"SOFTWARE\Rockstar Games";

/// The value holding a title's install folder.
pub const INSTALL_FOLDER: &str = "InstallFolder";

/// Subkeys of [`ROCKSTAR_KEY`] that are Rockstar's own software.
const NOT_GAMES: &[&str] = &[
    "Launcher",
    "Rockstar Games Launcher",
    "Rockstar Games Social Club",
    "Social Club",
];

/// A Rockstar title Ember knows by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Title {
    /// Ember's id for the title.
    pub id: &'static str,
    /// The registry key names the title may be installed under.
    pub keys: &'static [&'static str],
    pub title: &'static str,
    /// The title's program, relative to its install folder.
    pub executable: &'static str,
}

/// Known Rockstar titles.
pub const TITLES: &[Title] = &[
    Title {
        id: "gta5",
        keys: &["Grand Theft Auto V"],
        title: "Grand Theft Auto V",
        executable: "PlayGTAV.exe",
    },
    Title {
        id: "gta5-enhanced",
        keys: &["Grand Theft Auto V Enhanced"],
        title: "Grand Theft Auto V Enhanced",
        executable: "PlayGTAV.exe",
    },
    Title {
        id: "rdr2",
        keys: &["Red Dead Redemption 2"],
        title: "Red Dead Redemption 2",
        executable: "RDR2.exe",
    },
    Title {
        id: "rdr",
        keys: &["Red Dead Redemption"],
        title: "Red Dead Redemption",
        executable: "RDR.exe",
    },
    Title {
        id: "gta4",
        keys: &["Grand Theft Auto IV", "GTAIV"],
        title: "Grand Theft Auto IV: The Complete Edition",
        executable: "GTAIV.exe",
    },
    Title {
        id: "lanoire",
        keys: &["L.A. Noire"],
        title: "L.A. Noire",
        executable: "LANoire.exe",
    },
    Title {
        id: "lanoirevr",
        keys: &["L.A. Noire: The VR Case Files", "L.A. Noire VR"],
        title: "L.A. Noire: The VR Case Files",
        executable: "LANoireVR.exe",
    },
    Title {
        id: "mp3",
        keys: &["Max Payne 3"],
        title: "Max Payne 3",
        executable: "MaxPayne3.exe",
    },
    Title {
        id: "bully",
        keys: &["Bully Scholarship Edition", "Bully: Scholarship Edition"],
        title: "Bully: Scholarship Edition",
        executable: "Bully.exe",
    },
    Title {
        id: "gta3",
        keys: &["Grand Theft Auto III"],
        title: "Grand Theft Auto III",
        executable: "gta3.exe",
    },
    Title {
        id: "gtavc",
        keys: &["Grand Theft Auto Vice City", "Grand Theft Auto: Vice City"],
        title: "Grand Theft Auto: Vice City",
        executable: "gta-vc.exe",
    },
    Title {
        id: "gtasa",
        keys: &[
            "Grand Theft Auto San Andreas",
            "Grand Theft Auto: San Andreas",
        ],
        title: "Grand Theft Auto: San Andreas",
        executable: "gta_sa.exe",
    },
];

/// The known title filed under this registry key name (case-insensitive).
pub fn title_for_key(key: &str) -> Option<&'static Title> {
    TITLES
        .iter()
        .find(|t| t.keys.iter().any(|k| k.eq_ignore_ascii_case(key)))
}

/// How the Rockstar Games Launcher starts the title in `install_folder`.
pub fn launch_spec(install_folder: &str) -> LaunchSpec {
    LaunchSpec::LauncherArgs(vec![
        "-launchTitleInFolder".to_string(),
        install_folder.to_string(),
    ])
}

/// The titles the registry (`HKEY_LOCAL_MACHINE`) records as installed.
pub fn installed_games(registry: &Registry) -> Vec<InstalledGame> {
    let mut games: Vec<InstalledGame> = Vec::new();
    for key in registry.machine_subkeys(ROCKSTAR_KEY) {
        if NOT_GAMES.iter().any(|n| n.eq_ignore_ascii_case(&key)) {
            continue;
        }
        let Some(folder) = registry
            .machine_string(&format!(r"{ROCKSTAR_KEY}\{key}"), INSTALL_FOLDER)
            .map(str::trim)
            .filter(|f| !f.is_empty())
        else {
            continue;
        };
        let known = title_for_key(&key);
        let id = known.map_or_else(|| slug(&key), |t| t.id.to_string());
        if id.is_empty() || games.iter().any(|g| g.id == id) {
            continue;
        }
        games.push(InstalledGame {
            launcher: Launcher::Rockstar,
            id,
            title: known.map_or_else(|| key.clone(), |t| t.title.to_string()),
            install_dir: PathBuf::from(folder),
            executable: known.map(|t| t.executable.to_string()),
            launch: launch_spec(folder),
        });
    }
    games
}

/// `Some Title: Edition` → `some-title-edition`, the id of titles Ember doesn't know.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The Rockstar titles installed in the Wine prefix at `prefix`.
pub fn scan_prefix(prefix: &Path) -> ScanReport {
    let mut warnings = Vec::new();
    let registry = registry::load_machine(prefix, &mut warnings);
    let mut report = scan_drive(&drive_c(prefix), &registry);
    report.warnings.splice(0..0, warnings);
    report
}

/// The Rockstar titles installed on `drive` (`drive_c`, or `C:\`), with
/// `registry` holding `HKEY_LOCAL_MACHINE`.
pub fn scan_drive(drive: &Path, registry: &Registry) -> ScanReport {
    let mut report = ScanReport {
        games: installed_games(registry),
        warnings: Vec::new(),
    };
    retain_present(drive, &mut report);
    report
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const SYSTEM_REG: &str = r#"WINE REGISTRY Version 2
;; All keys relative to \\Machine

[Software\\Wow6432Node\\Rockstar Games\\Grand Theft Auto V] 1700000000
#time=1da0000000000000
"InstallFolder"="C:\\Program Files\\Rockstar Games\\Grand Theft Auto V"
"Language"="en-US"
"Telemetry"=hex:00,01,02,\
  03

[Software\\Wow6432Node\\Rockstar Games\\Launcher] 1700000000
"InstallFolder"="C:\\Program Files\\Rockstar Games\\Launcher"

[Software\\Wow6432Node\\Rockstar Games\\Rockstar Games Social Club] 1700000000
"InstallFolder"="C:\\Program Files\\Rockstar Games\\Social Club"

[Software\\Rockstar Games\\Some New Game: Deluxe] 1700000000
"InstallFolder"="D:\\Rockstar\\Some New Game"

[Software\\Wow6432Node\\Rockstar Games\\Uninstalled Thing] 1700000000
"Version"="1.0"
"#;

    #[test]
    fn lists_titles_with_an_install_folder() {
        let games = installed_games(&Registry::parse(SYSTEM_REG));
        assert_eq!(games.len(), 2, "{games:?}");

        let gta = &games[0];
        assert_eq!(gta.app_name(), "rockstar:gta5");
        assert_eq!(gta.title, "Grand Theft Auto V");
        assert_eq!(
            gta.install_dir,
            PathBuf::from(r"C:\Program Files\Rockstar Games\Grand Theft Auto V")
        );
        assert_eq!(gta.executable.as_deref(), Some("PlayGTAV.exe"));
        assert_eq!(
            gta.launch.launcher_args(),
            [
                "-launchTitleInFolder",
                r"C:\Program Files\Rockstar Games\Grand Theft Auto V"
            ]
        );

        let other = &games[1];
        assert_eq!(other.id, "some-new-game-deluxe");
        assert_eq!(other.title, "Some New Game: Deluxe");
        assert_eq!(other.executable, None);
    }
}
