//! Store-level tests against an in-process fake of the ZOOM API.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ember_core::process::CancellationToken;
use ember_core::{
    AppSettings, EventBus, InstallPlatform, InstallRequest, LaunchEnvironment, LaunchRequest,
    OpContext, Outcome, RemoveArgs, Store,
};
use serde_json::json;

use super::*;
use crate::extract::tests::make_tar_gz;
use crate::test_server::{Req, Resp, TestServer, serve_bytes};

struct Fixture {
    _dir: tempfile::TempDir,
    games_dir: PathBuf,
    server: TestServer,
    store: Arc<ZoomStore>,
    new_build: Arc<AtomicBool>,
    slow: Arc<AtomicBool>,
    archive_len: usize,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("src.tar.gz");
    // Incompressible payload so the archive is large enough to interrupt.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let big: Vec<u8> = (0..600_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    make_tar_gz(
        &archive_path,
        &[
            ("Game/start.sh", b"#!/bin/sh\necho hi\n", 0o644),
            ("Game/data.bin", &big, 0o644),
        ],
    );
    let archive = Arc::new(std::fs::read(&archive_path).unwrap());
    let archive_len = archive.len();
    let md5 = hex::encode(md5::compute(&*archive).0);
    let new_build = Arc::new(AtomicBool::new(false));
    let slow = Arc::new(AtomicBool::new(false));
    let (nb, sl) = (new_build.clone(), slow.clone());
    let server = TestServer::start(move |req: &Req| {
        let authed = req.header("authorization") == Some("Bearer good-token-123456");
        let host = req.header("host").unwrap_or_default().to_string();
        match req.path.as_str() {
            p if p.starts_with("/li/") && !authed => Resp::status(401),
            "/li/loggedin" => Resp::json(json!({"name": "player"})),
            "/li/games" => Resp::json(json!({"current_page": 1, "total_pages": 2, "games": [
                {"id": 1, "slug": "first-game", "name": "First Game", "poster_url": "https://p/1.jpg",
                 "description": "One", "store_url": "https://s/1", "operating_systems": ["linux", "windows"],
                 "developers": ["Dev"], "publishers": ["Pub"]}
            ]})),
            "/li/games?page=2" => {
                Resp::json(json!({"current_page": 2, "total_pages": 2, "games": [
                    {"id": 2, "slug": "second", "name": "Second", "poster_url": null, "description": null,
                     "store_url": "", "operating_systems": ["windows"], "developers": [], "publishers": []}
                ]}))
            }
            "/li/game/1/files" => {
                let id = if nb.load(Ordering::SeqCst) {
                    "f2"
                } else {
                    "f1"
                };
                Resp::json(
                    json!({"manual": [], "misc": [], "soundtrack": [], "windows": [],
                    "linux": [{"id": id, "name": "first_game_1.0.2.tar.gz",
                               "size": format!("{archive_len} bytes"), "filename": "first_game_1.0.2.tar.gz",
                               "total_size": archive_len, "md5": md5}]}),
                )
            }
            p if p.starts_with("/li/download/") => {
                Resp::json(json!({"url": format!("http://{host}/bin/game.tar.gz")}))
            }
            "/bin/game.tar.gz" => {
                let mut r = serve_bytes(req, &archive, true);
                if sl.load(Ordering::SeqCst) {
                    r.chunk_delay = Some(Duration::from_millis(15));
                }
                r
            }
            _ => Resp::status(404),
        }
    });
    let games_dir = dir.path().join("Games");
    let app = Arc::new(AppSettings {
        default_install_path: games_dir.to_string_lossy().into_owned(),
        ..Default::default()
    });
    let settings: SettingsFn = Arc::new(move || app.clone());
    let opts = ZoomOptions {
        api_base: server.base(),
        layout: Layout {
            config_dir: dir.path().join("config"),
            cache_dir: dir.path().join("cache"),
        },
        page_delay: Duration::from_millis(1),
    };
    let store = ZoomStore::with_options(EventBus::new(), settings, opts);
    Fixture {
        _dir: dir,
        games_dir,
        server,
        store,
        new_build,
        slow,
        archive_len,
    }
}

fn ctx_with(cancel: CancellationToken, progress: ember_core::ProgressSink) -> OpContext {
    OpContext {
        progress,
        cancel,
        step: Arc::new(|_| {}),
        log_file: None,
        settings: None,
    }
}

fn linux_request(f: &Fixture) -> InstallRequest {
    InstallRequest {
        path: f.games_dir.clone(),
        platform: InstallPlatform::Linux,
        ..Default::default()
    }
}

#[test]
fn deletion_guard() {
    let protected = [PathBuf::from("/home/u/Games/Ember")];
    assert!(is_safe_to_delete(
        Path::new("/home/u/Games/Ember/game"),
        &protected
    ));
    assert!(!is_safe_to_delete(
        Path::new("/home/u/Games/Ember"),
        &protected
    ));
    assert!(!is_safe_to_delete(Path::new("/home"), &protected));
    assert!(!is_safe_to_delete(Path::new("relative/x/y"), &protected));
}

#[test]
fn extracts_login_token() {
    assert_eq!(
        extract_token("https://www.zoom-platform.com/x?foo=1&li_token=abc%2Bdef&z=2").as_deref(),
        Some("abc+def")
    );
    assert_eq!(
        extract_token("  https://zoom/#li_token=TOKEN123  ").as_deref(),
        Some("TOKEN123")
    );
    assert_eq!(
        extract_token("abcdef0123456789abcdef").as_deref(),
        Some("abcdef0123456789abcdef")
    );
    assert_eq!(extract_token("https://www.zoom-platform.com/login"), None);
    assert_eq!(extract_token("short"), None);
}

#[test]
fn sign_in_window_waits_for_the_redirect() {
    // The window's pattern is matched against every address it visits,
    // starting with the login page, which asks for `return_li_token=true`.
    let re = regex::Regex::new(LI_TOKEN_CAPTURE).unwrap();
    assert!(!re.is_match(api::LOGIN_URL));
    let redirect = "https://www.zoom-platform.com/?li_token=TOKEN123456";
    assert_eq!(&re.captures(redirect).unwrap()[1], "TOKEN123456");
    assert_eq!(extract_token(redirect).as_deref(), Some("TOKEN123456"));
}

#[tokio::test]
async fn login_refresh_install_update_uninstall() {
    let f = fixture();
    let store = &f.store;
    store.init().await.unwrap();
    assert!(store.user().is_none());

    let err = store
        .login("https://www.zoom-platform.com/?li_token=bad-token-0000000")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("did not accept"));

    let user = store
        .login("https://www.zoom-platform.com/?li_token=good-token-123456&x=1")
        .await
        .unwrap();
    assert_eq!(user.display_name, "player");

    store.refresh(false).await.unwrap();
    let games = store.games();
    assert_eq!(games.len(), 2);
    let g1 = store.game("1").unwrap();
    assert_eq!(g1.title, "First Game");
    assert!(g1.is_linux_native);
    assert_eq!(store.game("2").unwrap().art_cover, None);
    assert_eq!(
        store.extra_info("1").await.unwrap().description.as_deref(),
        Some("One")
    );

    let info = store
        .install_info("1", InstallPlatform::Linux, &InstallInfoQuery::default())
        .await
        .unwrap();
    assert_eq!(info.download_size, f.archive_len as u64);
    assert_eq!(info.version.as_deref(), Some("1.0.2"));
    let err = store
        .install_info("1", InstallPlatform::Windows, &InstallInfoQuery::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no Windows build"));

    let outcome = store
        .install("1", &linux_request(&f), OpContext::detached())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    let g1 = store.game("1").unwrap();
    assert!(g1.is_installed);
    let install_dir = f.games_dir.join("first-game");
    let inst = g1.install.as_ref().unwrap();
    assert_eq!(inst.install_path, install_dir.to_string_lossy());
    assert_eq!(
        inst.executable,
        install_dir.join("Game/start.sh").to_string_lossy()
    );
    assert_eq!(inst.version, "1.0.2");
    assert!(install_dir.join("Game/data.bin").is_file());
    assert!(!f.games_dir.join(".zoom-download").exists());
    assert!(store.is_available("1").await);

    // Launch plan for the native build.
    let req = LaunchRequest {
        settings: Default::default(),
        environment: LaunchEnvironment::default(),
        option: None,
        extra_args: vec![],
        offline: false,
        skip_version_check: false,
        log_file: f.games_dir.join("log"),
    };
    let plan = store
        .launch("1", &req, OpContext::detached())
        .await
        .unwrap();
    assert_eq!(plan.command.program, install_dir.join("Game/start.sh"));

    // Same build → no update; a new file id after a full refresh → update.
    assert!(store.updatable_games().await.unwrap().is_empty());
    f.new_build.store(true, Ordering::SeqCst);
    store.refresh(true).await.unwrap();
    assert_eq!(
        store.updatable_games().await.unwrap(),
        vec!["1".to_string()]
    );
    store.set_version_pinned("1", true).await.unwrap();
    assert!(store.updatable_games().await.unwrap().is_empty());
    store.set_version_pinned("1", false).await.unwrap();
    let outcome = store
        .update("1", &UpdateRequest::default(), OpContext::detached())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert!(store.updatable_games().await.unwrap().is_empty());

    // Moving the install only changes the recorded path.
    let moved = f.games_dir.join("moved");
    std::fs::rename(&install_dir, &moved).unwrap();
    store.set_install_path("1", &moved).await.unwrap();
    assert_eq!(
        store
            .game("1")
            .unwrap()
            .install
            .as_ref()
            .unwrap()
            .executable,
        moved.join("Game/start.sh").to_string_lossy()
    );

    store
        .uninstall(
            "1",
            RemoveArgs {
                delete_files: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!moved.exists());
    assert!(!store.game("1").unwrap().is_installed);

    store.logout().await.unwrap();
    assert!(store.user().is_none());
    assert!(store.games().is_empty());
}

#[tokio::test]
async fn paused_install_resumes_with_range() {
    let f = fixture();
    let store = &f.store;
    store.init().await.unwrap();
    store.login("li_token=good-token-123456").await.unwrap();
    store.refresh(false).await.unwrap();
    f.slow.store(true, Ordering::SeqCst);

    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let progress: ember_core::ProgressSink = Arc::new(move |p: ember_core::InstallProgress| {
        if p.bytes_done.unwrap_or(0) > 40_000 {
            c2.cancel();
        }
    });
    let outcome = store
        .install("1", &linux_request(&f), ctx_with(cancel, progress))
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Abort);
    assert!(!store.game("1").unwrap().is_installed);
    let info = store
        .install_info("1", InstallPlatform::Linux, &InstallInfoQuery::default())
        .await
        .unwrap();
    assert!(info.resumable);

    f.slow.store(false, Ordering::SeqCst);
    let outcome = store
        .install("1", &linux_request(&f), OpContext::detached())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    let ranged = f
        .server
        .requests()
        .iter()
        .filter(|r| r.path == "/bin/game.tar.gz")
        .filter_map(|r| r.header("range").map(str::to_string))
        .collect::<Vec<_>>();
    assert_eq!(ranged.len(), 1, "resume should use one ranged request");
    assert!(f.games_dir.join("first-game/Game/start.sh").is_file());
}

#[tokio::test]
async fn cancelled_install_cleanup() {
    let f = fixture();
    let store = &f.store;
    store.init().await.unwrap();
    store.login("li_token=good-token-123456").await.unwrap();
    store.refresh(false).await.unwrap();
    f.slow.store(true, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let progress: ember_core::ProgressSink = Arc::new(move |p: ember_core::InstallProgress| {
        if p.bytes_done.unwrap_or(0) > 20_000 {
            c2.cancel();
        }
    });
    let outcome = store
        .install("1", &linux_request(&f), ctx_with(cancel, progress))
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Abort);
    assert!(f.games_dir.join(".zoom-download/1").is_dir());

    // The queue's "cancel and delete files" path.
    store
        .uninstall(
            "1",
            RemoveArgs {
                delete_files: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!f.games_dir.join(".zoom-download").exists());
}

#[tokio::test]
async fn import_detects_executable() {
    let f = fixture();
    let store = &f.store;
    store.init().await.unwrap();
    let dir = f.games_dir.join("imported");
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/Second.exe"), b"MZ").unwrap();
    std::fs::write(dir.join("unins000.exe"), b"MZ").unwrap();
    store
        .import("2", &dir, InstallPlatform::Windows)
        .await
        .unwrap();
    let g = store.game("2").unwrap();
    assert!(g.is_installed);
    assert_eq!(
        g.install.as_ref().unwrap().executable,
        dir.join("bin/Second.exe").to_string_lossy()
    );
    assert!(
        store
            .import("2", &dir.join("nope"), InstallPlatform::Windows)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn expired_token_logs_out() {
    let f = fixture();
    let store = &f.store;
    store.init().await.unwrap();
    records::save_auth(
        store.layout(),
        &Auth {
            token: "stale-token-0000000".into(),
            username: "old".into(),
        },
    )
    .unwrap();
    store.init().await.unwrap();
    assert!(store.is_logged_in());
    assert!(store.refresh(false).await.is_err());
    assert!(!store.is_logged_in());
    assert!(!store.layout().auth_file().exists());
}
