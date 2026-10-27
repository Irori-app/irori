//! Reads the local data of official third-party PC launchers so Irori can list
//! the games they installed, without ever touching their accounts.
//!
//! * [`Launcher`]: Battle.net, EA app, Ubisoft Connect and the Rockstar Games
//!   Launcher, with their ids, vendor installer links and where their program
//!   lives inside a Wine prefix.
//! * One module per launcher ([`battlenet`], [`ea`], [`ubisoft`],
//!   [`rockstar`]) holds where its data lives (relative to `drive_c`, which is
//!   also `C:\` on Windows), a parser from that data to [`InstalledGame`]s, and
//!   a scan of a prefix.
//! * [`registry`]: a reader for Wine's `.reg` files, which Ubisoft Connect and
//!   Rockstar use to record installs.
//! * [`scan_prefix`] / [`scan_drive`] scan for every launcher at once. A bad
//!   file never fails a scan: it becomes a warning in the [`ScanReport`].
//!
//! Nothing here knows about Irori: no paths, settings or events. Parsers take
//! bytes or text; only the `scan_*` functions read files.

#![forbid(unsafe_code)]

pub mod battlenet;
pub mod ea;
pub mod registry;
pub mod rockstar;
pub mod ubisoft;
mod yaml;

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use registry::{RegValue, Registry};

/// A third-party launcher whose installed games Irori can list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Launcher {
    #[serde(rename = "battlenet")]
    BattleNet,
    #[serde(rename = "ea")]
    EaApp,
    #[serde(rename = "ubisoft")]
    Ubisoft,
    #[serde(rename = "rockstar")]
    Rockstar,
}

impl Launcher {
    /// Every launcher, in the order Irori shows them.
    pub const ALL: [Launcher; 4] = [
        Launcher::BattleNet,
        Launcher::EaApp,
        Launcher::Ubisoft,
        Launcher::Rockstar,
    ];

    /// Stable id; Irori's app names for this launcher's games are `<id>:<game id>`.
    pub fn id(self) -> &'static str {
        match self {
            Launcher::BattleNet => "battlenet",
            Launcher::EaApp => "ea",
            Launcher::Ubisoft => "ubisoft",
            Launcher::Rockstar => "rockstar",
        }
    }

    /// The launcher for an [`id`](Self::id).
    pub fn from_id(id: &str) -> Option<Launcher> {
        Launcher::ALL.into_iter().find(|l| l.id() == id)
    }

    /// The launcher's own name, never translated.
    pub fn display_name(self) -> &'static str {
        match self {
            Launcher::BattleNet => "Battle.net",
            Launcher::EaApp => "EA app",
            Launcher::Ubisoft => "Ubisoft Connect",
            Launcher::Rockstar => "Rockstar Games Launcher",
        }
    }

    /// The vendor's own download link for the launcher's Windows installer.
    pub fn installer_url(self) -> &'static str {
        match self {
            Launcher::BattleNet => {
                "https://downloader.battle.net/download/getInstallerForGame?os=win&gameProgram=BATTLENET_APP&version=Live"
            }
            Launcher::EaApp => {
                "https://origin-a.akamaihd.net/EA-Desktop-Client-Download/installer-releases/EAappInstaller.exe"
            }
            Launcher::Ubisoft => {
                "https://static3.cdn.ubi.com/orbit/launcher_installer/UbisoftConnectInstaller.exe"
            }
            Launcher::Rockstar => {
                "https://gamedownloads.rockstargames.com/public/installer/Rockstar-Games-Launcher.exe"
            }
        }
    }

    /// Where the installer puts the launcher's program, relative to `drive_c`
    /// (or `C:\`), with `/` separators.
    pub fn exe_path(self) -> &'static str {
        match self {
            Launcher::BattleNet => "Program Files (x86)/Battle.net/Battle.net.exe",
            Launcher::EaApp => "Program Files/Electronic Arts/EA Desktop/EA Desktop/EADesktop.exe",
            Launcher::Ubisoft => {
                "Program Files (x86)/Ubisoft/Ubisoft Game Launcher/UbisoftConnect.exe"
            }
            Launcher::Rockstar => "Program Files/Rockstar Games/Launcher/Launcher.exe",
        }
    }

    /// [`exe_path`](Self::exe_path) as a Windows path (`C:\…`).
    pub fn windows_exe_path(self) -> String {
        format!("C:\\{}", self.exe_path().replace('/', "\\"))
    }

    /// The launcher's program inside the Wine prefix at `prefix`.
    pub fn exe_in_prefix(self, prefix: &Path) -> PathBuf {
        drive_c(prefix).join(self.exe_path())
    }

    /// The games this launcher installed in the Wine prefix at `prefix`.
    pub fn scan_prefix(self, prefix: &Path) -> ScanReport {
        match self {
            Launcher::BattleNet => battlenet::scan_prefix(prefix),
            Launcher::EaApp => ea::scan_prefix(prefix),
            Launcher::Ubisoft => ubisoft::scan_prefix(prefix),
            Launcher::Rockstar => rockstar::scan_prefix(prefix),
        }
    }

    /// The games this launcher installed on a drive (`drive_c`, or `C:\` on
    /// Windows), with `registry` holding `HKEY_LOCAL_MACHINE`.
    pub fn scan_drive(self, drive: &Path, registry: &Registry) -> ScanReport {
        match self {
            Launcher::BattleNet => battlenet::scan_drive(drive),
            Launcher::EaApp => ea::scan_drive(drive),
            Launcher::Ubisoft => ubisoft::scan_drive(drive, registry),
            Launcher::Rockstar => rockstar::scan_drive(drive, registry),
        }
    }
}

/// How Irori starts a launcher's game: always through the launcher itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LaunchSpec {
    /// Run the launcher's program ([`Launcher::exe_path`]) with these
    /// arguments, one per element (no shell quoting).
    LauncherArgs(Vec<String>),
    /// Open this link with the launcher. Under Wine, pass it as the launcher
    /// program's only argument; on Windows the shell can open it directly.
    Uri(String),
}

impl LaunchSpec {
    /// The arguments to give the launcher's program to start the game.
    pub fn launcher_args(&self) -> Vec<String> {
        match self {
            LaunchSpec::LauncherArgs(args) => args.clone(),
            LaunchSpec::Uri(uri) => vec![uri.clone()],
        }
    }
}

/// A game a launcher reports as installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledGame {
    pub launcher: Launcher,
    /// The launcher's id for the game (Battle.net product code, EA content id,
    /// Ubisoft install id, Rockstar title id).
    pub id: String,
    pub title: String,
    /// The install folder as the launcher stores it: a Windows path.
    pub install_dir: PathBuf,
    /// The game's own program, as the launcher names it: usually relative to
    /// `install_dir`, sometimes a full Windows path.
    pub executable: Option<String>,
    pub launch: LaunchSpec,
}

impl InstalledGame {
    /// Irori's app name for the game: `<launcher id>:<game id>`.
    pub fn app_name(&self) -> String {
        format!("{}:{}", self.launcher.id(), self.id)
    }
}

/// The games a scan found, and what it couldn't read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub games: Vec<InstalledGame>,
    /// Problems worded for the user, one per unreadable file or entry.
    pub warnings: Vec<String>,
}

impl ScanReport {
    /// Appends another report's games and warnings.
    pub fn merge(&mut self, other: ScanReport) {
        self.games.extend(other.games);
        self.warnings.extend(other.warnings);
    }
}

/// The `drive_c` folder of a Wine prefix.
pub fn drive_c(prefix: &Path) -> PathBuf {
    prefix.join("drive_c")
}

/// The games every launcher installed in the Wine prefix at `prefix`.
pub fn scan_prefix(prefix: &Path) -> ScanReport {
    let mut report = ScanReport::default();
    let registry = registry::load_machine(prefix, &mut report.warnings);
    report.merge(scan_drive(&drive_c(prefix), &registry));
    report
}

/// The games every launcher installed on `drive` (`drive_c`, or `C:\` on
/// Windows), with `registry` holding `HKEY_LOCAL_MACHINE`.
pub fn scan_drive(drive: &Path, registry: &Registry) -> ScanReport {
    let mut report = ScanReport::default();
    for launcher in Launcher::ALL {
        report.merge(launcher.scan_drive(drive, registry));
    }
    report
}

/// Maps a Windows path on drive C (`C:\Games\X` or `C:/Games/X`) into `drive`,
/// matching each component case-insensitively the way Windows does. Returns
/// `None` for other drives, relative paths and paths that climb with `..`.
pub fn resolve_in_drive(drive: &Path, windows_path: &str) -> Option<PathBuf> {
    let rest = windows_path
        .strip_prefix("C:")
        .or_else(|| windows_path.strip_prefix("c:"))?;
    let mut path = drive.to_path_buf();
    for part in rest.split(['\\', '/']) {
        match part {
            "" | "." => continue,
            ".." => return None,
            _ => {}
        }
        // A component like `C:` or `/` would replace the whole path on join.
        if !matches!(
            Path::new(part).components().next(),
            Some(Component::Normal(_))
        ) {
            return None;
        }
        path = join_case_insensitive(&path, part);
    }
    Some(path)
}

fn join_case_insensitive(dir: &Path, name: &str) -> PathBuf {
    let exact = dir.join(name);
    if exact.exists() {
        return exact;
    }
    let found = fs::read_dir(dir).ok().and_then(|entries| {
        entries
            .flatten()
            .map(|e| e.file_name())
            .find(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
    });
    match found {
        Some(n) => dir.join(n),
        None => exact,
    }
}

/// Drops games whose install folder is on drive C but no longer exists, so a
/// launcher's stale record doesn't show a game that was deleted by hand.
fn retain_present(drive: &Path, report: &mut ScanReport) {
    let mut kept = Vec::with_capacity(report.games.len());
    for game in report.games.drain(..) {
        let dir = game.install_dir.to_string_lossy();
        match resolve_in_drive(drive, &dir) {
            Some(path) if !path.is_dir() => report.warnings.push(format!(
                "{} lists “{}” in {}, but that folder is missing.",
                game.launcher.display_name(),
                game.title,
                dir
            )),
            _ => kept.push(game),
        }
    }
    report.games = kept;
}

/// The last component of a Windows path, for titles of games the tables don't know.
fn folder_name(windows_path: &str) -> Option<&str> {
    windows_path
        .trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty() && !s.ends_with(':'))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn launcher_ids_and_paths() {
        for launcher in Launcher::ALL {
            assert_eq!(Launcher::from_id(launcher.id()), Some(launcher));
            let json = serde_json::to_string(&launcher).expect("serialize");
            assert_eq!(json, format!("\"{}\"", launcher.id()));
            assert!(launcher.installer_url().starts_with("https://"));
        }
        assert_eq!(Launcher::from_id("steam"), None);
        assert_eq!(
            Launcher::BattleNet.windows_exe_path(),
            r"C:\Program Files (x86)\Battle.net\Battle.net.exe"
        );
        assert_eq!(
            Launcher::Rockstar.exe_in_prefix(Path::new("/p")),
            Path::new("/p/drive_c/Program Files/Rockstar Games/Launcher/Launcher.exe")
        );
    }

    #[test]
    fn resolves_only_safe_paths_on_drive_c() {
        let drive = Path::new("/nonexistent/drive_c");
        assert_eq!(
            resolve_in_drive(drive, r"C:\Games\X\"),
            Some(drive.join("Games").join("X"))
        );
        assert_eq!(
            resolve_in_drive(drive, "c:/Games/./X"),
            Some(drive.join("Games").join("X"))
        );
        assert_eq!(resolve_in_drive(drive, r"D:\Games\X"), None);
        assert_eq!(resolve_in_drive(drive, r"C:\Games\..\..\etc"), None);
        assert_eq!(resolve_in_drive(drive, r"Games\X"), None);
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, bytes).expect("write");
    }

    #[test]
    fn scans_every_launcher_in_a_prefix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prefix = tmp.path();
        let drive = drive_c(prefix);

        // Battle.net: WoW's folder differs in case, Overwatch's exists, the
        // unknown game's folder is gone.
        write(
            &drive.join(battlenet::PRODUCT_DB),
            &battlenet::tests::sample_db(),
        );
        fs::create_dir_all(drive.join("Program Files (x86)/World Of Warcraft")).expect("mkdir");
        fs::create_dir_all(drive.join("Program Files (x86)/Overwatch")).expect("mkdir");

        // Ubisoft Connect: an old empty cache in the install folder and a
        // newer one in the user's folder, which is the one read.
        write(&drive.join(ubisoft::CONFIGURATIONS), b"");
        let old = fs::File::options()
            .write(true)
            .open(drive.join(ubisoft::CONFIGURATIONS))
            .expect("open");
        old.set_modified(std::time::SystemTime::UNIX_EPOCH)
            .expect("set mtime");
        write(
            &drive
                .join("users/steamuser")
                .join(ubisoft::USER_CONFIGURATIONS),
            &ubisoft::tests::sample_configurations(),
        );
        fs::create_dir_all(
            drive.join("Program Files (x86)/Ubisoft/Ubisoft Game Launcher/games/Far Cry 5"),
        )
        .expect("mkdir");

        // EA app: one good manifest, one broken, one folder without a manifest.
        let ea_games = drive.join(ea::GAME_DIRS[0]);
        write(
            &ea_games.join("Mass Effect LE").join(ea::INSTALLER_DATA),
            ea::tests::SAMPLE.as_bytes(),
        );
        write(
            &ea_games.join("Broken").join(ea::INSTALLER_DATA),
            b"<DiPManifest><contentIDs>",
        );
        fs::create_dir_all(ea_games.join("Leftovers")).expect("mkdir");

        // Rockstar and Ubisoft installs in the registry.
        let reg = format!(
            "{}\n[Software\\\\Wow6432Node\\\\Ubisoft\\\\Launcher\\\\Installs\\\\635] 1\n\
             \"InstallDir\"=\"C:/Program Files (x86)/Ubisoft/Ubisoft Game Launcher/games/Far Cry 5/\"\n",
            rockstar::tests::SYSTEM_REG
        );
        write(&prefix.join(registry::SYSTEM_REG), reg.as_bytes());
        fs::create_dir_all(drive.join("Program Files/Rockstar Games/Grand Theft Auto V"))
            .expect("mkdir");

        let report = scan_prefix(prefix);
        let names: Vec<String> = report.games.iter().map(InstalledGame::app_name).collect();
        assert_eq!(
            names,
            [
                "battlenet:wow",
                "battlenet:pro",
                "ea:71171",
                "ubisoft:635",
                "rockstar:gta5",
                // On drive D:, which a prefix scan can't check.
                "rockstar:some-new-game-deluxe",
            ]
        );
        assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
        assert!(report.warnings[0].contains("Brand New Game"));
        assert!(report.warnings[1].contains("Broken"));

        let single = Launcher::EaApp.scan_prefix(prefix);
        assert_eq!(single.games.len(), 1);
        assert_eq!(
            single.games[0].install_dir,
            PathBuf::from(r"C:\Program Files\EA Games\Mass Effect LE")
        );
    }

    #[test]
    fn an_empty_prefix_has_no_games_and_no_warnings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(scan_prefix(tmp.path()), ScanReport::default());
    }
}
