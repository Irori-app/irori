//! Battle.net: installed games from the agent's `product.db`.
//!
//! `product.db` is a protobuf message listing every product the agent manages:
//! its uid, product code (Blizzard's NGDP code: `wow`, `pro`, `fenris`…),
//! settings (install path, play region) and cached state (installed, playable,
//! version). The agent itself (`agent`) and the Battle.net app (`bna`) are in
//! the list too and are skipped.
//!
//! Unlike Lutris, a product counts as installed only when the agent says it is
//! both installed and playable, so products it merely knows about stay out.
//!
//! Games launch through the client: `Battle.net.exe --exec="launch <CODE>"`,
//! where `<CODE>` is the product's launch code (`WoW`, `Pro`, `Fen`…).

use std::path::{Path, PathBuf};

use anyhow::Context;
use prost::Message;

use crate::{
    InstalledGame, LaunchSpec, Launcher, ScanReport, drive_c, folder_name, retain_present,
};

/// The agent's product database, relative to `drive_c` (or `C:\`).
pub const PRODUCT_DB: &str = "ProgramData/Battle.net/Agent/product.db";

/// Product codes that are Blizzard's own software, not games.
const NOT_GAMES: &[&str] = &["agent", "bna", "battle.net"];

/// The hand-written subset of Blizzard's `ProductDb` schema Ember reads.
/// Fields not declared here are skipped by prost.
pub(crate) mod proto {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Database {
        #[prost(message, repeated, tag = "1")]
        pub product_installs: Vec<ProductInstall>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ProductInstall {
        #[prost(string, tag = "1")]
        pub uid: String,
        #[prost(string, tag = "2")]
        pub product_code: String,
        #[prost(message, optional, tag = "3")]
        pub settings: Option<UserSettings>,
        #[prost(message, optional, tag = "4")]
        pub cached_product_state: Option<CachedProductState>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct UserSettings {
        #[prost(string, tag = "1")]
        pub install_path: String,
        #[prost(string, tag = "2")]
        pub play_region: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct CachedProductState {
        #[prost(message, optional, tag = "1")]
        pub base_product_state: Option<BaseProductState>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct BaseProductState {
        #[prost(bool, tag = "1")]
        pub installed: bool,
        #[prost(bool, tag = "2")]
        pub playable: bool,
        #[prost(string, tag = "7")]
        pub current_version_str: String,
    }
}

/// A Blizzard product Ember knows by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Product {
    /// The product code in `product.db`.
    pub code: &'static str,
    /// The uid `product.db` may file it under, when it differs from the code.
    pub uid: Option<&'static str>,
    pub title: &'static str,
    /// What `--exec="launch …"` expects.
    pub launch_code: &'static str,
}

const fn p(
    code: &'static str,
    uid: Option<&'static str>,
    title: &'static str,
    launch_code: &'static str,
) -> Product {
    Product {
        code,
        uid,
        title,
        launch_code,
    }
}

/// Known Battle.net products: product code, uid, title and launch code.
pub const PRODUCTS: &[Product] = &[
    p("wow", None, "World of Warcraft", "WoW"),
    p(
        "wow_classic",
        None,
        "World of Warcraft Classic",
        "WoW_wow_classic",
    ),
    p(
        "wow_classic_era",
        None,
        "World of Warcraft Classic Era",
        "WoW_wow_classic_era",
    ),
    p("d3", Some("diablo3"), "Diablo III", "D3"),
    p("d3cn", None, "Diablo III", "D3CN"),
    p("fenris", None, "Diablo IV", "Fen"),
    p("osi", None, "Diablo II: Resurrected", "OSI"),
    p("anbs", None, "Diablo Immortal", "ANBS"),
    p("pro", Some("prometheus"), "Overwatch 2", "Pro"),
    p("hsb", Some("hs_beta"), "Hearthstone", "WTCG"),
    p("hero", Some("heroes"), "Heroes of the Storm", "Hero"),
    p("s1", None, "StarCraft: Remastered", "S1"),
    p("s2", None, "StarCraft II", "S2"),
    p("w1", None, "Warcraft: Orcs & Humans", "W1"),
    p("w1r", None, "Warcraft I: Remastered", "W1R"),
    p("w2bn", None, "Warcraft II: Battle.net Edition", "W2BN"),
    p("w2r", None, "Warcraft II: Remastered", "W2R"),
    p("w3", None, "Warcraft III: Reforged", "W3"),
    p("gryphon", None, "Warcraft Rumble", "GRY"),
    p("viper", None, "Call of Duty: Black Ops 4", "VIPR"),
    p("odin", None, "Call of Duty: Modern Warfare", "ODIN"),
    p(
        "lazarus",
        None,
        "Call of Duty: Modern Warfare 2 Campaign Remastered",
        "LAZR",
    ),
    p("zeus", None, "Call of Duty: Black Ops Cold War", "ZEUS"),
    p("fore", None, "Call of Duty: Vanguard", "FORE"),
    p("auks", None, "Call of Duty: Modern Warfare II", "AUKS"),
    p("codhq", None, "Call of Duty HQ", "CODHQ"),
    p("rtro", None, "Blizzard Arcade Collection", "RTRO"),
    p("wlby", None, "Crash Bandicoot 4: It's About Time", "WLBY"),
];

/// The known product with this product code or uid (case-insensitive).
pub fn product(code_or_uid: &str) -> Option<&'static Product> {
    PRODUCTS
        .iter()
        .find(|p| p.code.eq_ignore_ascii_case(code_or_uid))
        .or_else(|| {
            PRODUCTS
                .iter()
                .find(|p| p.uid.is_some_and(|u| u.eq_ignore_ascii_case(code_or_uid)))
        })
}

/// How Battle.net launches a product: `--exec="launch <CODE>"`, as one argument.
pub fn launch_spec(launch_code: &str) -> LaunchSpec {
    LaunchSpec::LauncherArgs(vec![format!("--exec=launch {launch_code}")])
}

/// The installed, playable games in a `product.db`.
pub fn parse_product_db(bytes: &[u8]) -> anyhow::Result<Vec<InstalledGame>> {
    let db = proto::Database::decode(bytes)
        .context("Battle.net's product database isn't in the expected format")?;
    let mut games: Vec<InstalledGame> = Vec::new();
    for install in db.product_installs {
        let code = install.product_code.trim();
        if code.is_empty() || NOT_GAMES.iter().any(|n| n.eq_ignore_ascii_case(code)) {
            continue;
        }
        let state = install
            .cached_product_state
            .and_then(|s| s.base_product_state)
            .unwrap_or_default();
        if !state.installed || !state.playable {
            continue;
        }
        let install_path = install.settings.map(|s| s.install_path).unwrap_or_default();
        if install_path.is_empty() || games.iter().any(|g| g.id == code) {
            continue;
        }
        let known = product(code).or_else(|| product(&install.uid));
        let title = known
            .map(|p| p.title.to_string())
            .or_else(|| folder_name(&install_path).map(str::to_string))
            .unwrap_or_else(|| code.to_string());
        // Battle.net also accepts the product code for products whose launch
        // code Ember doesn't know.
        let launch_code = known.map_or(code, |p| p.launch_code);
        games.push(InstalledGame {
            launcher: Launcher::BattleNet,
            id: code.to_string(),
            title,
            install_dir: PathBuf::from(&install_path),
            executable: None,
            launch: launch_spec(launch_code),
        });
    }
    Ok(games)
}

/// The Battle.net games installed in the Wine prefix at `prefix`.
pub fn scan_prefix(prefix: &Path) -> ScanReport {
    scan_drive(&drive_c(prefix))
}

/// The Battle.net games installed on `drive` (`drive_c`, or `C:\`).
pub fn scan_drive(drive: &Path) -> ScanReport {
    let mut report = ScanReport::default();
    let path = drive.join(PRODUCT_DB);
    if !path.is_file() {
        return report;
    }
    let result = std::fs::read(&path)
        .with_context(|| format!("Couldn't read {}", path.display()))
        .and_then(|bytes| parse_product_db(&bytes));
    match result {
        Ok(games) => report.games = games,
        Err(e) => report.warnings.push(format!("{e:#}")),
    }
    retain_present(drive, &mut report);
    report
}

#[cfg(test)]
pub(crate) mod tests {
    use super::proto::*;
    use super::*;

    pub(crate) fn install(
        uid: &str,
        code: &str,
        path: &str,
        installed: bool,
        playable: bool,
    ) -> ProductInstall {
        ProductInstall {
            uid: uid.into(),
            product_code: code.into(),
            settings: Some(UserSettings {
                install_path: path.into(),
                play_region: "eu".into(),
            }),
            cached_product_state: Some(CachedProductState {
                base_product_state: Some(BaseProductState {
                    installed,
                    playable,
                    current_version_str: "1.0.0.1".into(),
                }),
            }),
        }
    }

    pub(crate) fn sample_db() -> Vec<u8> {
        Database {
            product_installs: vec![
                install(
                    "battle.net",
                    "bna",
                    "C:/Program Files (x86)/Battle.net",
                    true,
                    true,
                ),
                install(
                    "agent",
                    "agent",
                    "C:/ProgramData/Battle.net/Agent",
                    true,
                    true,
                ),
                install(
                    "wow",
                    "wow",
                    "C:/Program Files (x86)/World of Warcraft",
                    true,
                    true,
                ),
                install(
                    "prometheus",
                    "pro",
                    "C:/Program Files (x86)/Overwatch",
                    true,
                    true,
                ),
                install(
                    "hs_beta",
                    "hsb",
                    "C:/Program Files (x86)/Hearthstone",
                    true,
                    false,
                ),
                install(
                    "diablo3",
                    "d3",
                    "C:/Program Files (x86)/Diablo III",
                    false,
                    false,
                ),
                install("newgame", "zzz", "C:/Games/Brand New Game/", true, true),
            ],
        }
        .encode_to_vec()
    }

    #[test]
    fn lists_installed_playable_games_only() {
        let games = parse_product_db(&sample_db()).expect("parse");
        let ids: Vec<&str> = games.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["wow", "pro", "zzz"]);

        let wow = &games[0];
        assert_eq!(wow.title, "World of Warcraft");
        assert_eq!(wow.app_name(), "battlenet:wow");
        assert_eq!(
            wow.install_dir,
            PathBuf::from("C:/Program Files (x86)/World of Warcraft")
        );
        assert_eq!(wow.launch.launcher_args(), ["--exec=launch WoW"]);

        assert_eq!(games[1].title, "Overwatch 2");
        assert_eq!(games[1].launch.launcher_args(), ["--exec=launch Pro"]);

        // Unknown products keep their folder name and launch by product code.
        assert_eq!(games[2].title, "Brand New Game");
        assert_eq!(games[2].launch.launcher_args(), ["--exec=launch zzz"]);
    }

    #[test]
    fn finds_products_by_uid_too() {
        assert_eq!(product("hs_beta").map(|p| p.launch_code), Some("WTCG"));
        assert_eq!(product("FENRIS").map(|p| p.launch_code), Some("Fen"));
        assert!(product("nope").is_none());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_product_db(&[0xff, 0xff, 0xff]).is_err());
        assert!(parse_product_db(&[]).expect("empty is valid").is_empty());
    }
}
