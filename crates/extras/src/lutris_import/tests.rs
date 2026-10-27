//! Tests for the Lutris import planner, against databases and YAML files
//! built in temporary folders.

use super::paths::ini_value;
use super::*;
use rusqlite::{Connection, params};

/// Lutris's `games` table (`lutris/database/schema.py`), plus the legacy
/// `hidden` column.
const SCHEMA: &str = "
CREATE TABLE games (
    id INTEGER PRIMARY KEY, name TEXT, sortname TEXT, slug TEXT, installer_slug TEXT,
    parent_slug TEXT, platform TEXT, runner TEXT, executable TEXT, directory TEXT,
    updated DATETIME, lastplayed INTEGER, installed INTEGER, installed_at INTEGER,
    year INTEGER, configpath TEXT, has_custom_banner INTEGER, has_custom_icon INTEGER,
    has_custom_coverart_big INTEGER, playtime REAL, service TEXT, service_id TEXT,
    discord_id TEXT, hidden INTEGER
);
CREATE TABLE categories (id INTEGER PRIMARY KEY, name TEXT UNIQUE);
CREATE TABLE games_categories (game_id INTEGER, category_id INTEGER);
";

struct Row<'a> {
    id: i64,
    name: &'a str,
    slug: &'a str,
    runner: &'a str,
    platform: &'a str,
    directory: &'a str,
    configpath: &'a str,
    installed: i64,
    lastplayed: i64,
    playtime: f64,
    service: Option<&'a str>,
    service_id: Option<&'a str>,
}

fn insert(conn: &Connection, r: Row<'_>) {
    conn.execute(
        "INSERT INTO games (id, name, slug, runner, platform, directory, configpath, installed,
             installed_at, lastplayed, playtime, service, service_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1600000000, ?9, ?10, ?11, ?12)",
        params![
            r.id,
            r.name,
            r.slug,
            r.runner,
            r.platform,
            r.directory,
            r.configpath,
            r.installed,
            r.lastplayed,
            r.playtime,
            r.service,
            r.service_id
        ],
    )
    .unwrap();
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn row<'a>(id: i64, slug: &'a str, runner: &'a str, directory: &'a str) -> Row<'a> {
    Row {
        id,
        name: slug,
        slug,
        runner,
        platform: "",
        directory,
        configpath: slug,
        installed: 1,
        lastplayed: 0,
        playtime: 0.0,
        service: None,
        service_id: None,
    }
}

/// A native Lutris setup under `home` with one game per interesting case.
fn fixture(home: &Path) -> LutrisDirs {
    let data = home.join(".local/share/lutris");
    let config = home.join(".config/lutris");
    let games = home.join("Games");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&config).unwrap();

    // Install folders and files.
    write(&games.join("witcher/bin/witcher.exe"), "");
    write(&games.join("supertux/run.sh"), "");
    write(&games.join("roms/chrono.sfc"), "");
    write(&games.join("dos/keen/KEEN.EXE"), "");
    std::fs::create_dir_all(games.join("prefixes/witcher")).unwrap();

    let conn = Connection::open(data.join("pga.db")).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    let dir = |d: &str| games.join(d).to_string_lossy().into_owned();
    let (witcher, supertux, roms, keen, nothing) = (
        dir("witcher"),
        dir("supertux"),
        dir("roms"),
        dir("dos/keen"),
        dir("nothing"),
    );
    insert(
        &conn,
        Row {
            name: "The Witcher",
            lastplayed: 1_700_000_000,
            playtime: 12.5,
            ..row(1, "the-witcher", "wine", &witcher)
        },
    );
    insert(
        &conn,
        Row {
            name: "SuperTux",
            playtime: 0.0083,
            ..row(2, "supertux", "linux", &supertux)
        },
    );
    insert(
        &conn,
        Row {
            name: "Chrono Trigger",
            platform: "Nintendo SNES",
            ..row(3, "chrono-trigger", "libretro", &roms)
        },
    );
    insert(&conn, row(4, "keen", "dosbox", &keen));
    insert(
        &conn,
        Row {
            service: Some("steam"),
            service_id: Some("220"),
            ..row(5, "half-life-2", "steam", "")
        },
    );
    insert(
        &conn,
        Row {
            service: Some("gog"),
            service_id: Some("1207658924"),
            ..row(6, "unreal", "wine", &nothing)
        },
    );
    insert(&conn, row(7, "zx-game", "fuse", &nothing));
    insert(
        &conn,
        Row {
            installed: 0,
            ..row(8, "gone", "", "")
        },
    );
    insert(&conn, row(9, "lost", "linux", &nothing));
    insert(&conn, row(10, "no-config", "linux", &nothing));

    conn.execute_batch(
        "INSERT INTO categories (id, name) VALUES (1, 'favorite'), (2, '.hidden'), (3, 'RPG');
         INSERT INTO games_categories (game_id, category_id) VALUES (1, 1), (1, 3), (3, 3), (2, 2);",
    )
    .unwrap();
    drop(conn);

    write(
        &config.join("system.yml"),
        "system:\n  env:\n    DXVK_HUD: fps\n    SHARED: system\n",
    );
    write(
        &config.join("runners/wine.yml"),
        "wine:\n  version: wine-ge-8-26-x86_64\n  dxvk: true\nsystem:\n  env:\n    SHARED: runner\n    WINE_ONLY: '1'\n",
    );
    write(
        &config.join("games/the-witcher.yml"),
        "game:\n  exe: bin/witcher.exe\n  args: -nointro\n  prefix: ~/Games/prefixes/witcher\n  arch: win64\nsystem:\n  env:\n    GAME_ONLY: true\n    DXVK_HUD: ''\n",
    );
    write(
        &config.join("games/supertux.yml"),
        &format!(
            "game:\n  exe: {}\n  working_dir: .\n",
            games.join("supertux/run.sh").display()
        ),
    );
    write(
        &config.join("games/chrono-trigger.yml"),
        "game:\n  core: snes9x\n  main_file: chrono.sfc\n",
    );
    write(
        &config.join("games/keen.yml"),
        "game:\n  main_file: KEEN.EXE\n",
    );
    write(
        &config.join("games/half-life-2.yml"),
        "game:\n  appid: 220\n",
    );
    write(
        &config.join("games/lost.yml"),
        "game:\n  exe: lost.x86_64\n",
    );
    write(&data.join("coverart/the-witcher.jpg"), "jpg");
    write(&data.join("banners/supertux.png"), "png");
    write(
        &home.join(".cache/lutris/coverart/chrono-trigger.webp"),
        "webp",
    );

    let mut found = LutrisDirs::from_home(home);
    assert_eq!(found.len(), 1);
    found.remove(0)
}

fn by_slug<'a>(plan: &'a LutrisPlan, slug: &str) -> &'a PlannedGame {
    plan.games
        .iter()
        .find(|g| g.slug == slug)
        .unwrap_or_else(|| panic!("{slug} not in plan"))
}

#[test]
fn plans_every_kind_of_game() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dirs = fixture(home);
    let games = home.join("Games");
    let plan = plan(&dirs).unwrap();
    assert_eq!(plan.games.len(), 10);
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);

    let witcher = by_slug(&plan, "the-witcher");
    assert_eq!(witcher.name, "The Witcher");
    assert!(witcher.favourite);
    assert!(!witcher.hidden);
    assert_eq!(witcher.categories, vec!["RPG".to_string()]);
    assert_eq!(witcher.playtime_minutes, 750);
    assert_eq!(witcher.last_played.as_deref(), Some("2023-11-14T22:13:20Z"));
    assert_eq!(
        witcher.cover,
        Some(dirs.data_dir.join("coverart/the-witcher.jpg"))
    );
    let ImportKind::Windows {
        exe,
        args,
        working_dir,
        prefix,
        wine_version,
        arch,
        env,
    } = &witcher.kind
    else {
        panic!("{:?}", witcher.kind)
    };
    assert_eq!(exe, &games.join("witcher/bin/witcher.exe"));
    assert_eq!(args.as_deref(), Some("-nointro"));
    assert_eq!(working_dir, &None);
    assert_eq!(prefix, &Some(home.join("Games/prefixes/witcher")));
    assert_eq!(wine_version.as_deref(), Some("wine-ge-8-26-x86_64"));
    assert_eq!(arch.as_deref(), Some("win64"));
    // Merged key by key across the three levels; the empty override drops
    // DXVK_HUD and booleans read as text.
    let expected: BTreeMap<String, String> = [
        ("GAME_ONLY", "true"),
        ("SHARED", "runner"),
        ("WINE_ONLY", "1"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(env, &expected);

    let supertux = by_slug(&plan, "supertux");
    assert!(supertux.hidden);
    assert!(!supertux.favourite);
    assert!(supertux.categories.is_empty());
    assert_eq!(supertux.playtime_minutes, 0);
    assert_eq!(supertux.last_played, None);
    assert_eq!(
        supertux.cover,
        Some(dirs.data_dir.join("banners/supertux.png"))
    );
    let ImportKind::Native {
        exe,
        working_dir,
        env,
        ..
    } = &supertux.kind
    else {
        panic!("{:?}", supertux.kind)
    };
    assert_eq!(exe, &games.join("supertux/run.sh"));
    assert_eq!(working_dir, &Some(games.join("supertux/.")));
    // Only system.yml applies to a Linux game.
    assert_eq!(env.get("SHARED").map(String::as_str), Some("system"));
    assert_eq!(env.get("WINE_ONLY"), None);

    let chrono = by_slug(&plan, "chrono-trigger");
    assert_eq!(chrono.categories, vec!["RPG".to_string()]);
    assert_eq!(
        chrono.cover,
        Some(home.join(".cache/lutris/coverart/chrono-trigger.webp"))
    );
    assert_eq!(
        chrono.kind,
        ImportKind::Emulated {
            system: "snes".into(),
            engine: "retroarch:snes9x".into(),
            rom: games.join("roms/chrono.sfc"),
            game_id: None,
        }
    );

    assert_eq!(
        by_slug(&plan, "keen").kind,
        ImportKind::Emulated {
            system: "dos".into(),
            engine: "dosbox-staging".into(),
            rom: games.join("dos/keen/KEEN.EXE"),
            game_id: None,
        }
    );
    assert_eq!(
        by_slug(&plan, "half-life-2").kind,
        ImportKind::Steam {
            app_id: "220".into()
        }
    );
    assert_eq!(
        by_slug(&plan, "unreal").kind,
        ImportKind::StoreGame {
            service: "gog".into(),
            service_id: "1207658924".into()
        }
    );
    let skipped = |slug: &str| match &by_slug(&plan, slug).kind {
        ImportKind::Skipped { reason } => reason.clone(),
        other => panic!("{slug}: {other:?}"),
    };
    assert_eq!(skipped("zx-game"), SkipReason::UnknownRunner("fuse".into()));
    assert_eq!(skipped("gone"), SkipReason::NotInstalled);
    assert_eq!(
        skipped("lost"),
        SkipReason::FileMissing(games.join("nothing/lost.x86_64"))
    );
    assert_eq!(skipped("no-config"), SkipReason::MissingEntryPoint);
    assert_eq!(SkipReason::NotInstalled.id(), "notInstalled");
}

#[test]
fn never_modifies_the_database() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = fixture(tmp.path());
    let before = std::fs::read(&dirs.pga_path).unwrap();
    let mtime = std::fs::metadata(&dirs.pga_path)
        .unwrap()
        .modified()
        .unwrap();
    let siblings = |dir: &Path| -> Vec<_> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let files = siblings(&dirs.data_dir);

    plan(&dirs).unwrap();
    plan(&dirs).unwrap();

    assert_eq!(std::fs::read(&dirs.pga_path).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&dirs.pga_path)
            .unwrap()
            .modified()
            .unwrap(),
        mtime
    );
    // No journal, WAL or shared-memory file appeared.
    assert_eq!(siblings(&dirs.data_dir), files);
}

#[test]
fn tolerates_old_schemas() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let data = home.join(".local/share/lutris");
    std::fs::create_dir_all(&data).unwrap();
    write(&home.join("game/run"), "");
    let conn = Connection::open(data.join("pga.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE games (id INTEGER PRIMARY KEY, name TEXT, slug TEXT, runner TEXT,
             directory TEXT, installed INTEGER, configpath TEXT, playtime TEXT);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO games VALUES (1, 'Old', 'old', 'linux', ?1, 1, 'old', '1.5')",
        params![home.join("game").to_string_lossy()],
    )
    .unwrap();
    drop(conn);
    // No ~/.config/lutris: the YAML lives in the data folder.
    write(&data.join("games/old.yml"), "game:\n  exe: run\n");

    let dirs = LutrisDirs::from_home(home).remove(0);
    assert_eq!(dirs.config_dir, data);
    let plan = plan(&dirs).unwrap();
    let old = &plan.games[0];
    assert_eq!(old.playtime_minutes, 90);
    assert!(!old.favourite && !old.hidden);
    assert!(matches!(&old.kind, ImportKind::Native { exe, .. } if *exe == home.join("game/run")));
}

#[test]
fn reads_pga_path_from_lutris_conf() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let config = home.join(".config/lutris");
    write(
        &config.join("lutris.conf"),
        "[services]\npga_path = /wrong\n\n[lutris]\n# comment\nwidth = 800\npga_path = ~/elsewhere/games.db\n",
    );
    assert!(LutrisDirs::from_home(home).is_empty(), "no database yet");
    let db = home.join("elsewhere/games.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    Connection::open(&db)
        .unwrap()
        .execute_batch(SCHEMA)
        .unwrap();

    let found = LutrisDirs::from_home(home);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].pga_path, db);
    assert_eq!(found[0].config_dir, config);
    assert!(!found[0].flatpak);
    assert_eq!(plan(&found[0]).unwrap().games, Vec::new());

    assert_eq!(
        ini_value("[lutris]\npga_path:  /x/y.db \n", "lutris", "pga_path").as_deref(),
        Some("/x/y.db")
    );
    assert_eq!(
        ini_value("[lutris]\npga_path =\n", "lutris", "pga_path"),
        None
    );
}

#[test]
fn finds_native_and_flatpak_installations() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let native = home.join(".local/share/lutris/pga.db");
    let sandbox = home.join(".var/app/net.lutris.Lutris");
    let flatpak = sandbox.join("data/lutris/pga.db");
    for db in [&native, &flatpak] {
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        Connection::open(db).unwrap().execute_batch(SCHEMA).unwrap();
    }
    std::fs::create_dir_all(sandbox.join("config/lutris")).unwrap();

    let found = LutrisDirs::from_home(home);
    assert_eq!(found.len(), 2);
    assert!(!found[0].flatpak);
    assert_eq!(found[0].pga_path, native);
    assert_eq!(found[0].config_dir, home.join(".local/share/lutris"));
    assert!(found[1].flatpak);
    assert_eq!(found[1].pga_path, flatpak);
    assert_eq!(found[1].config_dir, sandbox.join("config/lutris"));
    assert_eq!(found[1].cache_dir, sandbox.join("cache/lutris"));
    assert_eq!(
        found[1].yaml_dirs(),
        vec![
            sandbox.join("config/lutris").as_path(),
            sandbox.join("data/lutris").as_path()
        ]
    );
}

#[test]
fn rejects_a_missing_or_foreign_database() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let data = home.join(".local/share/lutris");
    let dirs = LutrisDirs {
        flatpak: false,
        home: home.to_path_buf(),
        data_dir: data.clone(),
        config_dir: data.clone(),
        cache_dir: home.join(".cache/lutris"),
        pga_path: data.join("pga.db"),
    };
    assert!(plan(&dirs).is_err());
    std::fs::create_dir_all(&data).unwrap();
    Connection::open(&dirs.pga_path)
        .unwrap()
        .execute_batch("CREATE TABLE other (x INTEGER);")
        .unwrap();
    let err = plan(&dirs).unwrap_err();
    assert!(format!("{err:#}").contains("not a Lutris game database"));
}

#[test]
fn converts_playtime_hours_to_minutes() {
    assert_eq!(hours_to_minutes(0.0), 0);
    assert_eq!(hours_to_minutes(1.0), 60);
    assert_eq!(hours_to_minutes(0.0083), 0);
    assert_eq!(hours_to_minutes(0.0084), 1);
    assert_eq!(hours_to_minutes(2.25), 135);
    assert_eq!(hours_to_minutes(-3.0), 0);
    assert_eq!(hours_to_minutes(f64::NAN), 0);
}

#[test]
fn maps_emulators_to_systems_and_engines() {
    use emulators::{Hints, map};
    let target = |runner: &str, rom: &str, core: Option<&str>, platform: Option<&str>| {
        map(
            runner,
            &Hints {
                rom: Path::new(rom),
                platform,
                dolphin_platform: None,
                core,
            },
        )
        .map(|t| (t.system, t.engine))
    };
    let ok = |system: &'static str, engine: &str| Ok((system, engine.to_string()));

    assert_eq!(
        target("libretro", "a.sfc", Some("bsnes"), None),
        ok("snes", "retroarch:bsnes")
    );
    assert_eq!(
        target("libretro", "a.nes", Some("fceumm"), None),
        ok("nes", "retroarch:fceumm")
    );
    assert_eq!(
        target("libretro", "a.z64", Some("mupen64plus_next"), None),
        ok("n64", "retroarch:mupen64plus_next")
    );
    assert_eq!(
        target("libretro", "a.gbc", Some("gambatte"), None),
        ok("gbc", "retroarch:gambatte")
    );
    assert_eq!(
        target("libretro", "a.gb", Some("gambatte"), None),
        ok("gb", "retroarch:gambatte")
    );
    assert_eq!(
        target("libretro", "a.gba", Some("mgba"), None),
        ok("gba", "retroarch:mgba")
    );
    assert_eq!(
        target("libretro", "a.gb", Some("mgba"), None),
        ok("gb", "retroarch:mgba")
    );
    assert_eq!(
        target("libretro", "a.nds", Some("melondsds"), None),
        ok("nds", "retroarch:melondsds")
    );
    assert_eq!(
        target("libretro", "a.md", Some("genesis_plus_gx"), None),
        ok("genesis", "retroarch:genesis_plus_gx")
    );
    assert_eq!(
        target("libretro", "a.sms", Some("picodrive"), None),
        ok("sms", "retroarch:picodrive")
    );
    assert_eq!(
        target("libretro", "a.gg", Some("genesis_plus_gx"), None),
        ok("gamegear", "retroarch:genesis_plus_gx")
    );
    assert_eq!(
        target("libretro", "a.cue", Some("genesis_plus_gx"), None),
        ok("segacd", "retroarch:genesis_plus_gx")
    );
    assert_eq!(
        target("libretro", "a.pce", Some("mednafen_pce_fast"), None),
        ok("pce", "retroarch:mednafen_pce_fast")
    );
    assert_eq!(
        target("libretro", "a.cue", Some("pcsx_rearmed"), None),
        ok("psx", "retroarch:pcsx_rearmed")
    );
    assert_eq!(
        target("libretro", "a.iso", Some("ppsspp"), None),
        ok("psp", "retroarch:ppsspp")
    );
    assert_eq!(
        target("libretro", "a.bin", None, None),
        Err(SkipReason::MissingCore)
    );
    assert_eq!(
        target("libretro", "a.bin", Some("vice_x64"), None),
        Err(SkipReason::UnknownCore("vice_x64".into()))
    );

    assert_eq!(
        target("duckstation", "a.chd", None, None),
        ok("psx", "duckstation")
    );
    assert_eq!(target("ppsspp", "a.cso", None, None), ok("psp", "ppsspp"));
    assert_eq!(
        target("dolphin", "a.iso", None, Some("Nintendo Wii")),
        ok("wii", "dolphin")
    );
    assert_eq!(
        target("dolphin", "a.wbfs", None, None),
        ok("wii", "dolphin")
    );
    assert_eq!(
        target("dolphin", "a.rvz", None, Some("Nintendo GameCube")),
        ok("gamecube", "dolphin")
    );
    assert_eq!(
        target("dolphin", "a.iso", None, None),
        ok("gamecube", "dolphin")
    );
    assert_eq!(
        target("scummvm", "/games/monkey", None, None),
        ok("scummvm", "scummvm")
    );
    assert_eq!(target("pcsx2", "a.iso", None, None), ok("ps2", "pcsx2"));
    assert_eq!(target("mgba", "a.gbc", None, None), ok("gbc", "mgba"));
    assert_eq!(target("melonds", "a.nds", None, None), ok("nds", "melonds"));
    assert_eq!(target("azahar", "a.3ds", None, None), ok("3ds", "azahar"));
    assert_eq!(target("citra", "a.cia", None, None), ok("3ds", "azahar"));
    assert_eq!(target("mame", "pacman", None, None), ok("arcade", "mame"));

    let dolphin_wii = map(
        "dolphin",
        &Hints {
            rom: Path::new("a.iso"),
            platform: Some("Nintendo GameCube"),
            dolphin_platform: Some("1"),
            core: None,
        },
    );
    assert_eq!(dolphin_wii.map(|t| t.system), Ok("wii"));
}

#[test]
fn plans_scummvm_and_mame_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let data = home.join(".local/share/lutris");
    std::fs::create_dir_all(home.join("Games/monkey")).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let conn = Connection::open(data.join("pga.db")).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    insert(&conn, row(1, "monkey", "scummvm", ""));
    insert(&conn, row(2, "pacman", "mame", ""));
    drop(conn);
    write(
        &data.join("games/monkey.yml"),
        "game:\n  path: ~/Games/monkey\n  game_id: monkey\n",
    );
    write(
        &data.join("games/pacman.yml"),
        "game:\n  main_file: pacman\n",
    );

    let dirs = LutrisDirs::from_home(home).remove(0);
    let plan = plan(&dirs).unwrap();
    assert_eq!(
        plan.games[0].kind,
        ImportKind::Emulated {
            system: "scummvm".into(),
            engine: "scummvm".into(),
            rom: home.join("Games/monkey"),
            game_id: Some("monkey".into()),
        }
    );
    assert_eq!(
        plan.games[1].kind,
        ImportKind::Emulated {
            system: "arcade".into(),
            engine: "mame".into(),
            rom: PathBuf::from("pacman"),
            game_id: None,
        }
    );
}

#[test]
fn reports_invalid_yaml_as_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let data = home.join(".local/share/lutris");
    std::fs::create_dir_all(&data).unwrap();
    let conn = Connection::open(data.join("pga.db")).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    insert(&conn, row(1, "broken", "linux", ""));
    drop(conn);
    write(&data.join("games/broken.yml"), "game: [unclosed\n");

    let plan = plan(&LutrisDirs::from_home(home).remove(0)).unwrap();
    assert_eq!(
        plan.games[0].kind,
        ImportKind::Skipped {
            reason: SkipReason::MissingEntryPoint
        }
    );
    assert_eq!(plan.warnings.len(), 1);
    assert!(plan.warnings[0].contains("not valid YAML"));
}
