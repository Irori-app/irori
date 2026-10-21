//! The store end to end against an in-process Epic stand-in.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use ember_core::{AppSettings, GameSettings, LaunchEnvironment, WineKind};
use ember_depot::test_server::{Resp, TestServer};
use ember_epic_api::chunk;
use ember_epic_api::{ChunkInfo, ChunkPart, FileManifest, Guid};
use serde_json::json;
use sha1::Digest as _;

use super::*;

const CODE: &str = "0123456789abcdef0123456789abcdef";
const APP: &str = "Sugar";

fn exe() -> Vec<u8> {
    vec![1u8; 100_000]
}

fn common() -> Vec<u8> {
    vec![2u8; 1000]
}

/// The variable part of `Data/a.pak` in build `v`.
fn variable(v: u8) -> Vec<u8> {
    vec![10 + v; 500]
}

fn pak(v: u8) -> Vec<u8> {
    [common(), variable(v)].concat()
}

/// Manifest bytes and chunk files (by CDN path) of build `v`.
fn build(v: u8) -> (Vec<u8>, HashMap<String, Vec<u8>>) {
    let mut m = Manifest::new(APP.into(), format!("v{v}"));
    m.meta.launch_exe = "Bin/Game.exe".into();
    m.meta.launch_command = "-epicflag".into();
    let shared = [exe(), common()].concat();
    let mut files = HashMap::new();
    let mut add = |guid: Guid, data: &[u8], m: &mut Manifest| {
        let (bytes, hash, sha1) = chunk::encode(guid, data).unwrap();
        let info = ChunkInfo {
            guid,
            hash,
            sha1,
            group: (guid.0[0] % 100) as u8,
            window_size: chunk::CHUNK_SIZE as u32,
            file_size: bytes.len() as u64,
            ..Default::default()
        };
        files.insert(m.chunk_path(&info), bytes);
        m.chunks.push(info);
    };
    add(Guid([1, 1, 1, 1]), &shared, &mut m);
    add(Guid([2, 2, 2, u32::from(v)]), &variable(v), &mut m);
    let sha = |d: &[u8]| -> [u8; 20] { sha1::Sha1::digest(d).into() };
    m.files = vec![
        FileManifest {
            filename: "Bin/Game.exe".into(),
            sha1: sha(&exe()),
            flags: 0x4,
            parts: vec![ChunkPart {
                guid: Guid([1, 1, 1, 1]),
                offset: 0,
                size: 100_000,
                file_offset: 0,
            }],
            size: 100_000,
            ..Default::default()
        },
        FileManifest {
            filename: "Data/a.pak".into(),
            sha1: sha(&pak(v)),
            parts: vec![
                ChunkPart {
                    guid: Guid([1, 1, 1, 1]),
                    offset: 100_000,
                    size: 1000,
                    file_offset: 0,
                },
                ChunkPart {
                    guid: Guid([2, 2, 2, u32::from(v)]),
                    offset: 0,
                    size: 500,
                    file_offset: 1000,
                },
            ],
            size: 1500,
            ..Default::default()
        },
    ];
    (m.to_bytes().unwrap(), files)
}

/// Manifest bytes and chunk files of each build.
type Builds = HashMap<u8, (Vec<u8>, HashMap<String, Vec<u8>>)>;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    server: TestServer,
    version: Arc<AtomicU8>,
    store: Arc<EpicStore>,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let version = Arc::new(AtomicU8::new(1));
    let builds: Builds = [1, 2].into_iter().map(|v| (v, build(v))).collect();
    let current = version.clone();
    let base = Arc::new(Mutex::new(String::new()));
    let server_base = base.clone();
    let server = TestServer::start(move |req| {
        let v = current.load(Ordering::SeqCst);
        let base = server_base.lock().clone();
        let p = req.path.as_str();
        if p == "/account/api/oauth/token" {
            return Resp::json(json!({
                "access_token": "AT", "expires_in": 7200, "refresh_token": "RT",
                "account_id": "acc", "displayName": "Player"}));
        }
        if p == "/account/api/oauth/exchange" {
            return Resp::json(json!({"code": "XCHG"}));
        }
        if p.starts_with("/account/api/oauth/sessions/kill/") {
            return Resp::status(204);
        }
        if p.starts_with("/launcher/api/public/assets/Windows") {
            return Resp::json(json!([
                {"appName": APP, "buildVersion": format!("v{v}"), "catalogItemId": "cat",
                 "labelName": "Live-Windows", "namespace": "ns"},
                {"appName": "SugarDlc", "buildVersion": "1", "catalogItemId": "dlc",
                 "labelName": "Live-Windows", "namespace": "ns"}]));
        }
        if p.starts_with("/catalog/api/shared/namespace/ns/bulk/items?id=cat") {
            return Resp::json(
                json!({"cat": {"id": "cat", "namespace": "ns", "title": "Sugar Game",
                "releaseInfo": [{"appId": APP, "platform": ["Windows"]}],
                "customAttributes": {"FolderName": {"value": "SugarDir"}},
                "dlcItemList": [{"title": "Sugar DLC", "releaseInfo": [{"appId": "SugarDlc"}]}]}}),
            );
        }
        if p.starts_with("/catalog/api/shared/namespace/ns/bulk/items?id=dlc") {
            return Resp::json(
                json!({"dlc": {"id": "dlc", "namespace": "ns", "title": "Sugar DLC",
                "releaseInfo": [{"appId": "SugarDlc"}], "mainGameItem": {"id": "cat"}}}),
            );
        }
        if p.starts_with("/library/api/public/items") {
            return Resp::json(json!({"records": [], "responseMetadata": {}}));
        }
        if p.starts_with(&format!(
            "/launcher/api/public/assets/v2/platform/Windows/namespace/ns/catalogItem/cat/app/{APP}"
        )) {
            let bytes = &builds[&v].0;
            return Resp::json(json!({"elements": [{
                "appName": APP, "labelName": "Live-Windows", "buildVersion": format!("v{v}"),
                "hash": hex::encode(sha1::Sha1::digest(bytes)), "useSignedUrl": false,
                "manifests": [{"uri": format!("{base}/cdn/v{v}/build.manifest")}]}]}));
        }
        if let Some(rest) = p.strip_prefix("/cdn/v") {
            let (ver, file) = rest.split_once('/').unwrap_or_default();
            let Ok(ver) = ver.parse::<u8>() else {
                return Resp::status(404);
            };
            let (manifest, chunks) = &builds[&ver];
            let body = if file == "build.manifest" {
                Some(manifest.clone())
            } else {
                chunks.get(file).cloned()
            };
            return match body {
                Some(body) => Resp {
                    status: 200,
                    body,
                    ..Default::default()
                },
                None => Resp::status(404),
            };
        }
        Resp::status(404)
    });
    *base.lock() = server.base();
    let store = new_store(&root, &server).await;
    Fixture {
        _dir: dir,
        root,
        server,
        version,
        store,
    }
}

async fn new_store(root: &Path, server: &TestServer) -> Arc<EpicStore> {
    let install_base = root.join("games");
    let settings: SettingsFn = Arc::new(move || {
        Arc::new(AppSettings {
            default_install_path: install_base.to_string_lossy().into_owned(),
            ..Default::default()
        })
    });
    let b = server.base();
    let store = EpicStore::with_layout(
        EventBus::new(),
        settings,
        Layout {
            config: root.join("config"),
            cache: root.join("cache"),
            tools: root.join("tools"),
        },
        Endpoints {
            oauth: b.clone(),
            launcher: b.clone(),
            launcher2: b.clone(),
            entitlements: b.clone(),
            catalog: b.clone(),
            ecommerce: b.clone(),
            datastorage: b.clone(),
            library: b.clone(),
            artifact_delivery: b,
        },
    );
    // Keep the tests off the Internet: Epic's status page and legendary's
    // overrides service.
    *store.service_status.lock() = Some((Instant::now(), false));
    store.overrides.set("versionInfo", &json!({})).await;
    store
}

fn fetches(f: &Fixture, needle: &str) -> usize {
    f.server
        .requests()
        .iter()
        .filter(|r| r.path.contains(needle))
        .count()
}

#[tokio::test]
async fn installs_updates_repairs_launches_and_uninstalls() {
    let f = fixture().await;
    f.store.init().await.unwrap();
    assert!(!f.store.is_logged_in());
    let LoginFlow::Browser { url, capture, .. } = f.store.login_flow() else {
        panic!("browser flow expected");
    };
    assert!(url.starts_with("https://www.epicgames.com/id/login?redirectUrl="));
    let text = format!(r#"{{"authorizationCode":"{CODE}"}}"#);
    assert!(regex::Regex::new(&capture).unwrap().is_match(&text));
    let user = f.store.login(&text).await.unwrap();
    assert_eq!(user.display_name, "Player");
    f.store.refresh(false).await.unwrap();
    let game = f.store.game(APP).unwrap();
    assert_eq!(game.title, "Sugar Game");
    assert!(!game.is_installed);

    let info = f
        .store
        .install_info(APP, InstallPlatform::Windows, &InstallInfoQuery::default())
        .await
        .unwrap();
    assert_eq!(info.disk_size, 101_500);
    assert_eq!(info.version.as_deref(), Some("v1"));
    assert_eq!(info.dlcs.len(), 1);
    assert_eq!(info.dlcs[0].app_name, "SugarDlc");

    let outcome = f
        .store
        .install(APP, &InstallRequest::default(), OpContext::detached())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    let dir = f.root.join("games/SugarDir");
    assert_eq!(std::fs::read(dir.join("Bin/Game.exe")).unwrap(), exe());
    assert_eq!(std::fs::read(dir.join("Data/a.pak")).unwrap(), pak(1));
    let g = f.store.game(APP).unwrap();
    assert!(g.is_installed);
    assert_eq!(g.install.as_ref().unwrap().version, "v1");
    assert!(f.store.updatable_games().await.unwrap().is_empty());

    // Launch through Wine.
    let request = LaunchRequest {
        settings: GameSettings::default(),
        environment: LaunchEnvironment {
            env: Default::default(),
            wrappers: vec![],
            wine: Some(WineLaunch {
                bin: "/nonexistent/wine".into(),
                build_bin: "/nonexistent/wine".into(),
                kind: WineKind::Wine,
                prefix: f.root.join("prefix"),
                uses_umu: false,
                proton_verb: "waitforexitandrun",
            }),
        },
        option: None,
        extra_args: vec!["-extra".into()],
        offline: false,
        skip_version_check: false,
        log_file: f.root.join("launch.log"),
    };
    let plan = f
        .store
        .launch(APP, &request, OpContext::detached())
        .await
        .unwrap();
    assert_eq!(plan.command.program, PathBuf::from("/nonexistent/wine"));
    let args: Vec<String> = plan
        .command
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args[0], dir.join("Bin/Game.exe").to_string_lossy());
    assert_eq!(args[1], "-epicflag");
    assert_eq!(args[2], "-extra");
    assert!(args.contains(&"-AUTH_PASSWORD=XCHG".to_string()));
    assert!(args.contains(&"-epicapp=Sugar".to_string()));
    assert!(args.contains(&"-epicsandboxid=ns".to_string()));
    assert_eq!(plan.command.cwd.as_deref(), Some(dir.join("Bin").as_path()));
    assert_eq!(plan.command.env["STORE"], "egs");

    // Build 2 changes the pak's tail: only the new chunk is downloaded and
    // the pak's unchanged head is reused from disk.
    f.version.store(2, Ordering::SeqCst);
    assert_eq!(f.store.updatable_games().await.unwrap(), [APP]);
    let err = f
        .store
        .launch(APP, &request, OpContext::detached())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("has an update"), "{err:#}");
    let before = fetches(&f, "ChunksV4/01/");
    let outcome = f
        .store
        .update(APP, &UpdateRequest::default(), OpContext::detached())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(std::fs::read(dir.join("Data/a.pak")).unwrap(), pak(2));
    assert_eq!(
        fetches(&f, "ChunksV4/01/"),
        before,
        "shared chunk not refetched"
    );
    assert_eq!(
        f.store.game(APP).unwrap().install.as_ref().unwrap().version,
        "v2"
    );

    // Repair restores a damaged file.
    std::fs::write(dir.join("Bin/Game.exe"), b"broken").unwrap();
    let outcome = f.store.repair(APP, OpContext::detached()).await.unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(std::fs::read(dir.join("Bin/Game.exe")).unwrap(), exe());

    // A new store instance reads the same state.
    let again = new_store(&f.root, &f.server).await;
    again.init().await.unwrap();
    assert!(again.is_logged_in());
    assert!(again.game(APP).unwrap().is_installed);

    // Uninstall keeps files the game did not ship.
    std::fs::write(dir.join("save.dat"), b"progress").unwrap();
    f.store
        .uninstall(
            APP,
            RemoveArgs {
                delete_files: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!dir.join("Bin").exists());
    assert!(dir.join("save.dat").exists());
    assert!(!f.store.game(APP).unwrap().is_installed);

    f.store.logout().await.unwrap();
    assert!(!f.store.is_logged_in());
    assert!(f.store.games().is_empty());
    assert!(fetches(&f, "/sessions/kill/") > 0);
}

#[tokio::test]
async fn imports_existing_installs() {
    let f = fixture().await;
    f.store.init().await.unwrap();
    f.store
        .login(&format!(r#"{{"authorizationCode":"{CODE}"}}"#))
        .await
        .unwrap();
    f.store.refresh(false).await.unwrap();
    let dir = f.root.join("elsewhere");
    let err = f
        .store
        .import(APP, &dir, InstallPlatform::Windows)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("No files"), "{err:#}");
    std::fs::create_dir_all(dir.join("Bin")).unwrap();
    std::fs::write(dir.join("Bin/Game.exe"), exe()).unwrap();
    f.store
        .import(APP, &dir, InstallPlatform::Windows)
        .await
        .unwrap();
    let g = f.store.installed_entry(APP).unwrap();
    assert_eq!(g.install_path, dir.to_string_lossy());
    assert_eq!(g.version, "v1");
    // Repairing fetches the missing pak.
    let outcome = f.store.repair(APP, OpContext::detached()).await.unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(std::fs::read(dir.join("Data/a.pak")).unwrap(), pak(1));
}

#[tokio::test]
async fn converts_files_left_by_legendary() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config");
    std::fs::create_dir_all(cfg.join("metadata")).unwrap();
    std::fs::write(
        cfg.join("user.json"),
        r#"{"account_id":"acc","displayName":"Player","access_token":"a","refresh_token":"r",
            "expires_at":"2030-01-01T00:00:00.000Z"}"#,
    )
    .unwrap();
    std::fs::write(
        cfg.join("metadata/Sugar.json"),
        r#"{"app_name":"Sugar","app_title":"Sugar","asset_infos":{"Windows":{"build_version":"2"}},
            "metadata":{"namespace":"ns","title":"Sugar","releaseInfo":[{"appId":"Sugar","platform":["Windows"]}]}}"#,
    )
    .unwrap();
    std::fs::write(
        cfg.join("installed.json"),
        r#"{"Sugar":{"app_name":"Sugar","install_path":"/nonexistent/Sugar","platform":"Windows","version":"1","install_size":10}}"#,
    )
    .unwrap();
    std::fs::write(
        cfg.join("third-party-installed.json"),
        r#"[["EaGame","Windows"]]"#,
    )
    .unwrap();
    let server = TestServer::start(|_| Resp::status(404));
    let s = new_store(dir.path(), &server).await;
    s.init().await.unwrap();
    assert!(!cfg.join("user.json").exists());
    assert!(!cfg.join("metadata").exists());
    assert!(cfg.join("legendary-backup/user.json").is_file());
    assert_eq!(s.user().unwrap().display_name, "Player");
    let g = s.game("Sugar").unwrap();
    assert!(g.is_installed);
    assert_eq!(g.version.as_deref(), Some("2"));
    assert!(!s.is_available("Sugar").await);
    assert!(s.is_available("EaGame").await);
    assert_eq!(s.updatable_games().await.unwrap_or_default().len(), 0);
}
