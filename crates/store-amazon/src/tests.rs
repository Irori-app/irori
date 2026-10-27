//! The store end to end against an in-process Amazon stand-in.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use irori_amazon_api::testing;
use irori_core::{AppSettings, GameSettings, LaunchEnvironment, WineKind};
use irori_depot::test_server::{Resp, TestServer};
use parking_lot::Mutex;
use serde_json::json;

use super::*;

const FUEL: &[u8] =
    br#"{"SchemaVersion": "1", "Main": {"Command": "Bin\\Game.exe", "Args": ["-fuel"]}}"#;

/// Files of build `v`: `a` never changes, `b` changes, `c` is dropped and
/// `d` added in v2.
fn build(v: u8) -> Vec<(&'static str, Vec<u8>)> {
    let mut files = vec![
        ("fuel.json", FUEL.to_vec()),
        ("Bin\\Game.exe", vec![1u8; 70_000]),
        ("a.pak", vec![2u8; 1000]),
        ("b.pak", vec![v; 500]),
    ];
    if v == 1 {
        files.push(("old\\c.txt", b"c".to_vec()));
    } else {
        files.push(("new\\d.txt", b"d".to_vec()));
    }
    files
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    server: TestServer,
    version: Arc<AtomicU8>,
    store: Arc<AmazonStore>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let version = Arc::new(AtomicU8::new(1));
    // Every build's files by hash.
    let blobs: HashMap<String, Vec<u8>> = [1, 2]
        .into_iter()
        .flat_map(build)
        .map(|(_, data)| (hex::encode(testing::sha256(&data)), data))
        .collect();
    let manifests: HashMap<u8, Vec<u8>> = [1, 2]
        .into_iter()
        .map(|v| {
            let files = build(v);
            let refs: Vec<(&str, &[u8])> = files.iter().map(|(p, d)| (*p, d.as_slice())).collect();
            (v, testing::build(&refs, true))
        })
        .collect();
    let current = version.clone();
    let base = Arc::new(Mutex::new(String::new()));
    let server_base = base.clone();
    let server = TestServer::start(move |req| {
        let v = current.load(Ordering::SeqCst);
        let base = server_base.lock().clone();
        let target = req.header("x-amz-target").unwrap_or_default().to_string();
        match req.path.as_str() {
            "/auth/register" => Resp::json(json!({"response": {"success": {
                "tokens": {"bearer": {"access_token": "AT", "refresh_token": "RT", "expires_in": "3600"}},
                "extensions": {
                    "device_info": {"device_serial_number": "SERIAL"},
                    "customer_info": {"given_name": "Radu", "user_id": "amzn1.account.X"}}}}})),
            "/auth/token" => Resp::json(json!({"access_token": "AT2", "expires_in": 3600})),
            "/auth/deregister" => Resp::json(json!({})),
            "/ent" => Resp::json(json!({"entitlements": [
                {"id": "ent-1", "product": {"id": "amzn1.adg.product.foo", "title": "Foo: Bar", "sku": "SKU-1"}}
            ]})),
            "/dist" if target.ends_with(".GetGameDownload") => Resp::json(json!({
                "downloadUrl": format!("{base}/cdn/v{v}"), "versionId": format!("v{v}")})),
            "/dist" if target.ends_with(".GetLiveVersionIds") => Resp::json(json!({
                "adgProductIdToVersionIdMap": {"amzn1.adg.product.foo": format!("v{v}")}})),
            p => {
                let Some(rest) = p.strip_prefix("/cdn/v") else {
                    return Resp::status(404);
                };
                let (ver, file) = rest.split_once('/').unwrap_or_default();
                match file {
                    "manifest.proto" => Resp {
                        status: 200,
                        body: manifests[&ver.parse::<u8>().unwrap()].clone(),
                        ..Default::default()
                    },
                    f => match f.strip_prefix("files/").and_then(|h| blobs.get(h)) {
                        Some(data) => irori_depot::test_server::serve_bytes(req, data, true),
                        None => Resp::status(404),
                    },
                }
            }
        }
    });
    *base.lock() = server.base();
    let install_base = root.join("games");
    let settings: SettingsFn = Arc::new(move || {
        Arc::new(AppSettings {
            default_install_path: install_base.to_string_lossy().into_owned(),
            ..Default::default()
        })
    });
    let store = AmazonStore::with_dirs(
        EventBus::new(),
        settings,
        root.join("config"),
        root.join("cache"),
        root.join("sdk"),
        irori_amazon_api::Endpoints {
            auth: server.base(),
            distribution: server.url("/dist"),
            entitlements: server.url("/ent"),
        },
        Some(testing::public_key_pem()),
    );
    Fixture {
        _dir: dir,
        root,
        server,
        version,
        store,
    }
}

const APP: &str = "amzn1.adg.product.foo";

fn fetches(f: &Fixture, data: &[u8]) -> usize {
    let path = format!("/files/{}", hex::encode(testing::sha256(data)));
    f.server
        .requests()
        .iter()
        .filter(|r| r.path.ends_with(&path))
        .count()
}

async fn sign_in(f: &Fixture) {
    f.store.init().await.unwrap();
    assert!(!f.store.is_logged_in());
    let LoginFlow::Browser { url, capture, .. } = f.store.login_flow() else {
        panic!("browser flow expected");
    };
    assert!(url.starts_with("https://amazon.com/ap/signin?"));
    assert_eq!(capture, auth::CODE_CAPTURE);
    let user = f
        .store
        .login("https://www.amazon.com/?openid.oa2.authorization_code=ANcode123456&x=1")
        .await
        .unwrap();
    assert_eq!(user.display_name, "Radu");
    f.store.refresh(false).await.unwrap();
}

#[tokio::test]
async fn installs_updates_repairs_launches_and_uninstalls() {
    let f = fixture();
    sign_in(&f).await;
    let game = f.store.game(APP).unwrap();
    assert_eq!(game.title, "Foo: Bar");
    assert!(!game.is_installed);

    let info = f
        .store
        .install_info(APP, InstallPlatform::Windows, &InstallInfoQuery::default())
        .await
        .unwrap();
    let v1_size: u64 = build(1).iter().map(|(_, d)| d.len() as u64).sum();
    assert_eq!(info.download_size, v1_size);
    assert!(!info.resumable);

    let reports = Arc::new(Mutex::new(Vec::new()));
    let sink = reports.clone();
    let ctx = OpContext {
        progress: Arc::new(move |p| sink.lock().push(p)),
        ..OpContext::detached()
    };
    let outcome = f
        .store
        .install(APP, &InstallRequest::default(), ctx)
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    let dir = f.root.join("games/Foo Bar");
    assert_eq!(
        std::fs::read(dir.join("Bin/Game.exe")).unwrap(),
        vec![1u8; 70_000]
    );
    assert_eq!(std::fs::read(dir.join("old/c.txt")).unwrap(), b"c");
    let last = *reports.lock().last().unwrap();
    assert_eq!(last.percent, Some(100.0));
    assert_eq!(last.bytes_total, Some(v1_size));
    let game = f.store.game(APP).unwrap();
    assert!(game.is_installed);
    assert_eq!(game.install.as_ref().unwrap().version, "v1");
    assert_eq!(Path::new(&game.install.as_ref().unwrap().install_path), dir);
    assert!(f.store.updatable_games().await.unwrap().is_empty());

    // Version 2: only the changed and new files are fetched.
    f.version.store(2, Ordering::SeqCst);
    assert_eq!(f.store.updatable_games().await.unwrap(), [APP]);
    let outcome = f
        .store
        .update(APP, &UpdateRequest::default(), OpContext::detached())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(std::fs::read(dir.join("b.pak")).unwrap(), vec![2u8; 500]);
    assert_eq!(std::fs::read(dir.join("new/d.txt")).unwrap(), b"d");
    assert!(
        !dir.join("old").exists(),
        "dropped files and folders removed"
    );
    assert_eq!(
        fetches(&f, &[1u8; 70_000]),
        1,
        "unchanged files not refetched"
    );
    assert_eq!(
        f.store.game(APP).unwrap().install.as_ref().unwrap().version,
        "v2"
    );
    assert!(f.store.updatable_games().await.unwrap().is_empty());

    // Repair restores a damaged file.
    std::fs::write(dir.join("a.pak"), b"broken").unwrap();
    let outcome = f.store.repair(APP, OpContext::detached()).await.unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(std::fs::read(dir.join("a.pak")).unwrap(), vec![2u8; 1000]);
    assert_eq!(fetches(&f, &[2u8; 1000]), 2);

    // Launch through Wine (offline: no SDK download).
    let prefix = f.root.join("prefix");
    let request = LaunchRequest {
        settings: GameSettings::default(),
        environment: LaunchEnvironment {
            env: BTreeMap::new(),
            wrappers: vec![],
            wine: Some(WineLaunch {
                bin: "/usr/bin/wine".into(),
                build_bin: "/usr/bin/wine".into(),
                kind: WineKind::Wine,
                prefix: prefix.clone(),
                uses_umu: false,
                proton_verb: "waitforexitandrun",
            }),
        },
        option: None,
        extra_args: vec![],
        offline: true,
        skip_version_check: false,
        log_file: f.root.join("launch.log"),
    };
    let plan = f
        .store
        .launch(APP, &request, OpContext::detached())
        .await
        .unwrap();
    assert_eq!(plan.command.program, PathBuf::from("/usr/bin/wine"));
    assert_eq!(Path::new(&plan.command.args[0]), dir.join("Bin/Game.exe"));
    assert_eq!(&plan.command.args[1..], ["-fuel"]);
    assert_eq!(plan.command.cwd.as_deref(), Some(dir.as_path()));
    assert_eq!(
        plan.command.env["AMAZON_GAMES_FUEL_ENTITLEMENT_ID"],
        "ent-1"
    );
    assert_eq!(plan.command.env["AMAZON_GAMES_FUEL_PRODUCT_SKU"], "SKU-1");
    assert_eq!(plan.command.env["AMAZON_GAMES_FUEL_DISPLAY_NAME"], "Radu");
    let done: Vec<String> =
        serde_json::from_slice(&std::fs::read(prefix.join("installed_games")).unwrap()).unwrap();
    assert_eq!(done, [APP]);

    // Uninstall keeps files the game did not ship (saves).
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
    assert!(f.store.layout.read_installed().is_empty());
    assert!(f.store.layout.read_manifest(APP).is_none());

    f.store.logout().await.unwrap();
    assert!(!f.store.is_logged_in());
    assert!(f.store.layout.read_credentials().is_none());
    assert!(
        f.server
            .requests()
            .iter()
            .any(|r| r.path == "/auth/deregister")
    );
}

#[tokio::test]
async fn imports_existing_folders_and_renews_tokens() {
    let f = fixture();
    sign_in(&f).await;
    let dir = f.root.join("elsewhere");
    for (path, data) in build(1) {
        let p = irori_depot::path::join(&dir, &irori_depot::path::normalize(path).unwrap());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }
    std::fs::write(dir.join("a.pak"), b"bad").unwrap();
    let err = f
        .store
        .import(APP, &dir, InstallPlatform::Windows)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("missing or damaged"), "{err:#}");
    std::fs::write(dir.join("a.pak"), vec![2u8; 1000]).unwrap();
    f.store
        .import(APP, &dir, InstallPlatform::Windows)
        .await
        .unwrap();
    assert_eq!(f.store.installed(APP).unwrap().path, dir);

    // An expired token is renewed and saved before the next call.
    let mut creds = f.store.layout.read_credentials().unwrap();
    creds.expires_at = 0;
    *f.store.creds.lock().await = Some(creds);
    f.store.refresh(false).await.unwrap();
    assert_eq!(
        f.store.layout.read_credentials().unwrap().access_token,
        "AT2"
    );

    // A new store instance sees the same state.
    let again = AmazonStore::with_dirs(
        EventBus::new(),
        f.store.settings.clone(),
        f.root.join("config"),
        f.root.join("cache"),
        f.root.join("sdk"),
        f.store.endpoints.clone(),
        None,
    );
    again.init().await.unwrap();
    assert!(again.is_logged_in());
    assert!(again.game(APP).unwrap().is_installed);
}

#[tokio::test]
async fn converts_state_left_by_nile() {
    let f = fixture();
    let old = f.root.join("config/nile");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(
        old.join("user.json"),
        json!({
            "tokens": {"bearer": {"access_token": "a", "refresh_token": "r", "expires_in": 1}},
            "extensions": {
                "device_info": {"device_serial_number": "S"},
                "customer_info": {"given_name": "Radu", "user_id": "U"}}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        old.join("installed.json"),
        br#"[{"id":"amzn1.adg.product.foo","version":"v1","path":"/g/Foo"}]"#,
    )
    .unwrap();
    f.store.init().await.unwrap();
    assert!(!old.exists());
    assert!(f.root.join("config/nile-backup/user.json").is_file());
    assert_eq!(f.store.user().unwrap().display_name, "Radu");
    // Installed but not yet in the synced library: kept as an orphan entry.
    let game = f.store.game(APP).unwrap();
    assert!(game.is_installed);
    assert_eq!(game.title, "Foo");
}
