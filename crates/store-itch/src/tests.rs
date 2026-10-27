//! End-to-end tests against an in-process itch.io server.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Once};

use irori_core::{
    AppSettings, EventBus, InstallPlatform, InstallRequest, LaunchEnvironment, OpContext, Outcome,
    Paths, RemoveArgs, SessionExpired, Store, UpdateRequest, WineKind, WineLaunch,
};
use irori_depot::test_server::{Resp, TestServer, serve_bytes};
use serde_json::json;

use super::*;
use crate::extract::tests::zip_bytes;

const KEY: &str = "Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4zAb7cDe0f";

fn isolate_paths() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let root = std::env::temp_dir().join(format!("irori-itch-{}", std::process::id()));
        Paths::init(Paths::under(&root));
    });
}

struct Fixture {
    _dir: tempfile::TempDir,
    games: PathBuf,
    store: Arc<ItchStore>,
    _server: TestServer,
    /// Bumped to publish a new version of the upload.
    version: Arc<AtomicU8>,
    revoked: Arc<AtomicBool>,
}

fn archive(version: u8) -> Vec<u8> {
    let exe = format!("MZ game v{version}");
    zip_bytes(&[
        ("Game/Game.exe", exe.as_bytes()),
        ("Game/unins000.exe", &[0u8; 64]),
        ("Game/data/level.dat", &[version; 32]),
    ])
}

fn fixture() -> Fixture {
    isolate_paths();
    let dir = tempfile::tempdir().unwrap();
    let games = dir.path().join("games");
    let version = Arc::new(AtomicU8::new(1));
    let revoked = Arc::new(AtomicBool::new(false));
    let (v, r) = (version.clone(), revoked.clone());
    let server = TestServer::start(move |req| {
        let n = v.load(Ordering::SeqCst);
        if req.path.starts_with("/files/") {
            return serve_bytes(req, &archive(n), true);
        }
        if req.header("authorization") != Some(KEY) || r.load(Ordering::SeqCst) {
            return Resp {
                status: 401,
                ..Resp::json(json!({"errors": ["invalid key"]}))
            };
        }
        match req.path.as_str() {
            "/profile" => Resp::json(
                json!({"user": {"id": 42, "username": "player", "display_name": "Player One"}}),
            ),
            p if p.starts_with("/profile/owned-keys?page=1") => Resp::json(json!({"owned_keys": [
                {"id": 500, "game_id": 7, "created_at": "2024-03-01T10:00:00Z", "game": {
                    "id": 7, "title": "Tiny Dungeon", "url": "https://dev.itch.io/tiny-dungeon",
                    "short_text": "A small roguelike", "classification": "game", "type": "default",
                    "cover_url": "https://img.itch.zone/cover.png",
                    "platforms": {"windows": "all", "linux": "all"},
                    "user": {"id": 1, "username": "dev", "display_name": "Dev"}}},
                {"id": 501, "game_id": 8, "game": {"id": 8, "title": "Soundtrack Only",
                    "classification": "soundtrack", "type": "default"}}
            ]})),
            "/games/7/uploads?download_key_id=500" => Resp::json(json!({"uploads": [
                {"id": 3, "storage": "hosted", "filename": "tiny-dungeon-win.zip", "type": "default",
                 "size": archive(n).len(), "platforms": {"windows": "all"},
                 "updated_at": format!("2024-04-0{n}T00:00:00Z")},
                {"id": 4, "storage": "hosted", "filename": "ost.zip", "type": "soundtrack",
                 "size": 10, "platforms": {"windows": "all"}}
            ]})),
            "/uploads/3/download?download_key_id=500" => Resp {
                status: 302,
                headers: vec![(
                    "Location".into(),
                    format!("/files/{n}/tiny-dungeon-win.zip"),
                )],
                ..Default::default()
            },
            _ => Resp::status(404),
        }
    });
    let settings_games = games.clone();
    let settings: SettingsFn = Arc::new(move || {
        Arc::new(AppSettings {
            itch_io: true,
            default_install_path: settings_games.to_string_lossy().into_owned(),
            ..Default::default()
        })
    });
    let store = ItchStore::with_layout(
        EventBus::new(),
        settings,
        Layout {
            config: dir.path().join("config"),
            cache: dir.path().join("cache"),
        },
        &server.base(),
    );
    Fixture {
        _dir: dir,
        games,
        store,
        _server: server,
        version,
        revoked,
    }
}

fn request(f: &Fixture) -> LaunchRequest {
    LaunchRequest {
        settings: Default::default(),
        environment: LaunchEnvironment {
            env: Default::default(),
            wrappers: vec![],
            wine: Some(WineLaunch {
                bin: "/opt/wine/bin/wine".into(),
                build_bin: "/opt/wine/bin/wine".into(),
                kind: WineKind::Wine,
                prefix: f.games.join("prefix"),
                uses_umu: false,
                proton_verb: "waitforexitandrun",
            }),
        },
        option: None,
        extra_args: vec![],
        offline: false,
        skip_version_check: false,
        log_file: f.games.join("launch.log"),
    }
}

#[tokio::test]
async fn logs_in_installs_updates_and_uninstalls() {
    let f = fixture();
    let s = &f.store;
    s.init().await.unwrap();
    assert!(s.user().is_none());
    assert!(s.login("not a key").await.is_err());
    let wrong = s
        .login("Zz3dEf6hIj9kLm2nOp5qRs8tUv1wXy4zAb7cDe0f")
        .await
        .unwrap_err();
    assert!(wrong.to_string().contains("did not accept"), "{wrong:#}");
    let user = s.login(&format!("key: {KEY} View")).await.unwrap();
    assert_eq!(user.display_name, "Player One");

    s.refresh(false).await.unwrap();
    let games = s.games();
    assert_eq!(games.len(), 1, "the soundtrack is not a game");
    let game = s.game("7").unwrap();
    assert_eq!(game.title, "Tiny Dungeon");
    assert_eq!(game.developer.as_deref(), Some("Dev"));
    assert!(game.installable && !game.is_installed);
    assert!(game.platforms.contains(&InstallPlatform::Linux));
    assert!(
        s.install_info("7", InstallPlatform::Mac, &Default::default())
            .await
            .is_err()
    );
    let info = s
        .install_info("7", InstallPlatform::Windows, &Default::default())
        .await
        .unwrap();
    assert_eq!(info.download_size, archive(1).len() as u64);

    let outcome = s
        .install(
            "7",
            &InstallRequest {
                platform: InstallPlatform::Windows,
                ..Default::default()
            },
            OpContext::detached(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    let dir = f.games.join("Tiny Dungeon");
    assert_eq!(
        std::fs::read(dir.join("Game/Game.exe")).unwrap(),
        b"MZ game v1"
    );
    assert!(!f.games.join(".itch-download").join("7").exists());
    let installed = s.game("7").unwrap();
    assert!(installed.is_installed);
    let install = installed.install.as_ref().unwrap();
    assert_eq!(
        install.executable,
        Path::new("Game").join("Game.exe").to_string_lossy()
    );
    assert_eq!(install.version, "2024-04-01");
    assert!(s.updatable_games().await.unwrap().is_empty());

    let plan = s
        .launch("7", &request(&f), OpContext::detached())
        .await
        .unwrap();
    if !cfg!(windows) {
        assert_eq!(plan.command.program, PathBuf::from("/opt/wine/bin/wine"));
    }

    // A new upload date is an update; pinning hides it.
    f.version.store(2, Ordering::SeqCst);
    assert_eq!(s.updatable_games().await.unwrap(), ["7"]);
    s.set_version_pinned("7", true).await.unwrap();
    assert!(s.updatable_games().await.unwrap().is_empty());
    s.set_version_pinned("7", false).await.unwrap();
    let outcome = s
        .update("7", &UpdateRequest::default(), OpContext::detached())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(
        std::fs::read(dir.join("Game/Game.exe")).unwrap(),
        b"MZ game v2"
    );
    assert_eq!(
        std::fs::read(dir.join("Game/data/level.dat")).unwrap(),
        [2; 32]
    );
    assert!(!f.games.join(".Tiny Dungeon.itch-new").exists());
    assert!(!f.games.join(".Tiny Dungeon.itch-old").exists());
    assert!(s.updatable_games().await.unwrap().is_empty());

    // A restarted store knows the install and the library without network.
    let again = ItchStore::with_layout(
        EventBus::new(),
        s.settings.clone(),
        s.layout.clone(),
        "http://127.0.0.1:1",
    );
    again.init().await.unwrap();
    assert!(again.is_logged_in());
    assert!(again.game("7").unwrap().is_installed);

    s.uninstall(
        "7",
        RemoveArgs {
            delete_files: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(!dir.exists());
    assert!(!s.game("7").unwrap().is_installed);

    s.logout().await.unwrap();
    assert!(s.user().is_none());
    assert!(s.games().is_empty());
}

#[tokio::test]
async fn a_revoked_key_asks_to_log_in_again() {
    let f = fixture();
    f.store.init().await.unwrap();
    f.store.login(KEY).await.unwrap();
    f.revoked.store(true, Ordering::SeqCst);
    let e = f.store.refresh(false).await.unwrap_err();
    assert_eq!(SessionExpired::of(&e).unwrap().runner, Runner::Itch);
}

#[tokio::test]
async fn refuses_to_install_into_a_folder_with_files() {
    let f = fixture();
    f.store.init().await.unwrap();
    f.store.login(KEY).await.unwrap();
    f.store.refresh(false).await.unwrap();
    let dir = f.games.join("Tiny Dungeon");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("keep.txt"), b"mine").unwrap();
    let outcome = f
        .store
        .install(
            "7",
            &InstallRequest {
                platform: InstallPlatform::Windows,
                ..Default::default()
            },
            OpContext::detached(),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, Outcome::Error(e) if e.contains("not empty")));
    assert_eq!(std::fs::read(dir.join("keep.txt")).unwrap(), b"mine");
}

#[tokio::test]
async fn turned_off_means_no_login_or_install() {
    isolate_paths();
    let dir = tempfile::tempdir().unwrap();
    let store = ItchStore::with_layout(
        EventBus::new(),
        Arc::new(|| Arc::new(AppSettings::default())),
        Layout {
            config: dir.path().join("c"),
            cache: dir.path().join("k"),
        },
        "http://127.0.0.1:1",
    );
    store.init().await.unwrap();
    assert!(store.login(KEY).await.is_err());
    store.refresh(false).await.unwrap();
    assert!(store.games().is_empty());
    assert!(store.user().is_none());
}
