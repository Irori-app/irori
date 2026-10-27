use std::path::PathBuf;
use std::sync::Arc;

use ember_core::{AppSettings, GameSettings, LaunchEnvironment, LaunchRequest, OpContext};
use once_cell::sync::Lazy;

use super::*;

static ROOT: Lazy<tempfile::TempDir> = Lazy::new(|| {
    let dir = tempfile::tempdir().expect("temp dir");
    Paths::init(Paths::under(dir.path()));
    dir
});

/// A fresh folder under the test root, with Paths pointing there.
fn test_dir(name: &str) -> PathBuf {
    let root = ROOT.path();
    assert!(Paths::get().config.starts_with(root));
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn touch(path: &std::path::Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn store(dir: &std::path::Path, app: AppSettings) -> Arc<EmulationStore> {
    let app = Arc::new(app);
    EmulationStore::with_dirs(
        EventBus::new(),
        Arc::new(move || app.clone()),
        dir.join("config"),
        dir.join("cache"),
    )
}

fn request(settings: GameSettings, dir: &std::path::Path) -> LaunchRequest {
    LaunchRequest {
        settings,
        environment: LaunchEnvironment::default(),
        option: None,
        extra_args: Vec::new(),
        offline: false,
        skip_version_check: false,
        log_file: dir.join("game.log"),
    }
}

#[tokio::test]
async fn scans_folders_and_remembers_removals() {
    let dir = test_dir("scan");
    let roms = dir.join("roms");
    touch(
        &roms.join("SNES/Super Metroid (Japan, USA) (En,Ja).sfc"),
        b"x",
    );
    touch(&roms.join("GBA/Golden Sun (USA).gba"), b"x");
    let app = AppSettings {
        rom_folders: vec![roms.display().to_string()],
        ..Default::default()
    };
    let s = store(&dir, app.clone());
    s.init().await.unwrap();
    assert!(s.games().is_empty());
    s.refresh(false).await.unwrap();
    let games = s.games();
    assert_eq!(games.len(), 2);
    let metroid = games.iter().find(|g| g.title == "Super Metroid").unwrap();
    assert_eq!(metroid.system.as_deref(), Some("snes"));
    assert_eq!(metroid.tags, ["Japan, USA"]);
    assert!(metroid.is_installed);
    assert_eq!(metroid.runner(), Runner::Emulation);
    let art = metroid.art_cover.as_deref().unwrap();
    assert!(art.starts_with(
        "https://thumbnails.libretro.com/Nintendo%20-%20Super%20Nintendo%20Entertainment%20System/Named_Boxarts/"
    ));
    assert!(s.is_available(&metroid.app_name).await);

    // Removing keeps the file and survives a rescan and a restart.
    s.uninstall(&metroid.app_name, RemoveArgs::default())
        .await
        .unwrap();
    assert!(
        roms.join("SNES/Super Metroid (Japan, USA) (En,Ja).sfc")
            .exists()
    );
    s.refresh(false).await.unwrap();
    assert_eq!(s.games().len(), 1);
    let again = store(&dir, app);
    again.init().await.unwrap();
    assert_eq!(again.games().len(), 1);
}

#[tokio::test]
async fn launches_with_the_chosen_emulator_and_checks_bios() {
    let dir = test_dir("launch");
    let roms = dir.join("roms");
    touch(&roms.join("DOS/Keen/KEEN1.EXE"), b"MZ");
    touch(&roms.join("PS1/Game (USA).chd"), b"x");
    let dosbox = dir.join("bin/dosbox");
    let duck = dir.join("bin/duckstation");
    touch(&dosbox, b"#!/bin/sh");
    touch(&duck, b"#!/bin/sh");
    ember_engines::install::set_custom("dosbox-staging", Some(&dosbox)).unwrap();
    ember_engines::install::set_custom("duckstation", Some(&duck)).unwrap();
    let bios_dir = dir.join("bios");
    std::fs::create_dir_all(&bios_dir).unwrap();
    let app = AppSettings {
        rom_folders: vec![roms.display().to_string()],
        bios_folder: bios_dir.display().to_string(),
        ..Default::default()
    };
    let s = store(&dir, app);
    s.init().await.unwrap();
    s.refresh(false).await.unwrap();
    let keen = s.games().into_iter().find(|g| g.title == "Keen").unwrap();
    let plan = s
        .launch(
            &keen.app_name,
            &request(GameSettings::default(), &dir),
            OpContext::detached(),
        )
        .await
        .unwrap();
    assert_eq!(plan.command.program, dosbox);
    assert!(
        plan.command
            .args
            .iter()
            .any(|a| a.to_string_lossy().ends_with("KEEN1.EXE"))
    );

    // PS1 needs a BIOS: without one the launch says so, typed.
    let ps1 = s.games().into_iter().find(|g| g.title == "Game").unwrap();
    let err = s
        .launch(
            &ps1.app_name,
            &request(GameSettings::default(), &dir),
            OpContext::detached(),
        )
        .await
        .unwrap_err();
    let missing = BiosMissing::of(&err).expect("typed BIOS error");
    assert_eq!(missing.system, "psx");
    assert!(missing.files.contains(&"scph5501.bin".to_string()));

    // A file with the expected name is used even when its digest is unknown.
    touch(&bios_dir.join("scph5501.bin"), b"dump");
    ember_engines::bios::scan(&bios_dir, &ember_engines::bios::BiosIndex::default()).unwrap();
    let plan = s
        .launch(
            &ps1.app_name,
            &request(GameSettings::default(), &dir),
            OpContext::detached(),
        )
        .await
        .unwrap();
    assert_eq!(plan.command.program, duck);

    // A game pinned to a version that is gone fails with a typed error.
    let pinned = GameSettings {
        engine_version: "managed:0.1".into(),
        ..Default::default()
    };
    let err = s
        .launch(&ps1.app_name, &request(pinned, &dir), OpContext::detached())
        .await
        .unwrap_err();
    assert_eq!(
        EngineMissing::of(&err)
            .and_then(|m| m.pinned.clone())
            .as_deref(),
        Some("0.1")
    );
}

#[tokio::test]
async fn adds_games_outside_the_folders() {
    let dir = test_dir("add");
    let rom = dir.join("elsewhere/Kirby's Dream Land (USA, Europe).gb");
    touch(&rom, b"x");
    let keen = dir.join("elsewhere/Keen");
    touch(&keen.join("KEEN4E.EXE"), b"MZ");
    let s = store(&dir, AppSettings::default());
    s.init().await.unwrap();
    let id = s.add_game(&rom, None, None).await.unwrap();
    let game = s.game(&id.app_name).unwrap();
    assert_eq!(game.title, "Kirby's Dream Land");
    assert_eq!(game.system.as_deref(), Some("gb"));
    // Folders need their system; DOS folders get their program.
    assert!(s.add_game(&keen, None, None).await.is_err());
    let keen_id = s
        .add_game(&keen, Some("dos"), Some("Commander Keen 4".into()))
        .await
        .unwrap();
    assert!(
        s.entry(&keen_id.app_name)
            .unwrap()
            .path
            .ends_with("KEEN4E.EXE")
    );
    // Adding twice keeps one entry; a rescan keeps added games.
    s.add_game(&rom, None, None).await.unwrap();
    s.refresh(false).await.unwrap();
    assert_eq!(s.games().len(), 2);
    // Removing an added game forgets it.
    s.uninstall(&id.app_name, RemoveArgs::default())
        .await
        .unwrap();
    assert_eq!(s.games().len(), 1);
    let again = store(&dir, AppSettings::default());
    again.init().await.unwrap();
    assert_eq!(again.games().len(), 1);
}
