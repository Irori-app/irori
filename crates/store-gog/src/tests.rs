//! The store end to end against an in-process GOG stand-in.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use irori_core::{AppSettings, InstallRequest, OpContext, Outcome, RemoveArgs, UpdateRequest};
use irori_depot::test_server::{Resp, TestServer};
use irori_gog_api::content::{Endpoints, galaxy_path};
use serde_json::{Value, json};

use super::*;

const APP: &str = "1";

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(data).unwrap();
    z.finish().unwrap()
}

fn md5_hex(data: &[u8]) -> String {
    format!("{:x}", md5::compute(data))
}

/// A v2 chunk of `data`: its manifest entry and its CDN bytes.
fn chunk(data: &[u8]) -> (Value, String, Vec<u8>) {
    let z = zlib(data);
    let compressed_md5 = md5_hex(&z);
    (
        json!({"md5": md5_hex(data), "size": data.len(), "compressedMd5": compressed_md5,
               "compressedSize": z.len()}),
        compressed_md5,
        z,
    )
}

fn exe() -> Vec<u8> {
    vec![1u8; 70_000]
}

fn pak(v: u8) -> Vec<u8> {
    vec![10 + v; 5_000]
}

/// Everything the stand-in serves for build `v`.
struct Build {
    meta: Vec<u8>,
    depot: Vec<u8>,
    depot_hash: String,
}

fn build(v: u8, chunks: &mut HashMap<String, Vec<u8>>) -> Build {
    let mut file = |path: &str, data: &[u8], flags: &[&str]| {
        let (entry, key, bytes) = chunk(data);
        chunks.insert(key, bytes);
        json!({"type": "DepotFile", "path": path, "chunks": [entry], "flags": flags,
               "md5": md5_hex(data)})
    };
    let mut items = vec![
        file("bin\\game.exe", &exe(), &["executable"]),
        file("data.pak", &pak(v), &[]),
        file("setup.exe", b"MZsetup", &["support"]),
        json!({"type": "DepotDirectory", "path": "saves"}),
    ];
    if v == 1 {
        items.push(file("old.txt", b"old", &[]));
    }
    let depot = zlib(&serde_json::to_vec(&json!({"depot": {"items": items}})).unwrap());
    let depot_hash = format!("dddd{v}");
    let meta = json!({"version": 2, "baseProductId": APP, "buildId": format!("b{v}"),
        "installDirectory": "Game Dir", "clientId": "55", "clientSecret": "s",
        "products": [{"productId": APP, "name": "Game"}],
        "depots": [{"productId": APP, "languages": ["*"], "manifest": depot_hash,
                    "size": 80_000, "compressedSize": 1_000}],
        "dependencies": []});
    Build {
        meta: zlib(&serde_json::to_vec(&meta).unwrap()),
        depot,
        depot_hash,
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    server: TestServer,
    version: Arc<AtomicU8>,
    store: Arc<GogStore>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let version = Arc::new(AtomicU8::new(1));
    let mut chunks = HashMap::new();
    let builds: HashMap<u8, Build> = [1, 2].map(|v| (v, build(v, &mut chunks))).into();
    let isi = chunk(b"MZscriptinterpreter");
    chunks.insert(isi.1.clone(), isi.2.clone());
    let repo = zlib(
        &serde_json::to_vec(&json!({"depots": [{"dependencyId": "ISI",
            "executable": {"path": "__redist/ISI/si.exe", "arguments": ""},
            "manifest": "isim", "size": 19, "compressedSize": 19, "readableName": "ISI"}]}))
        .unwrap(),
    );
    let isi_depot = zlib(
        &serde_json::to_vec(&json!({"depot": {"items": [{"type": "DepotFile",
            "path": "__redist/ISI/si.exe", "chunks": [isi.0]}]}}))
        .unwrap(),
    );
    let current = version.clone();
    let base = Arc::new(parking_lot::Mutex::new(String::new()));
    let server_base = base.clone();
    let server = TestServer::start(move |req| {
        let v = current.load(Ordering::SeqCst);
        let p = req.path.as_str();
        let base = server_base.lock().clone();
        if p.starts_with(&format!("/products/{APP}/os/windows/builds")) {
            return Resp::json(
                json!({"items": [{"build_id": format!("b{v}"), "os": "windows",
                "branch": null, "version_name": format!("v{v}"), "generation": 2,
                "date_published": "2026-01-01T00:00:00+0000",
                "link": format!("{base}/meta/{v}")}], "has_private_branches": false}),
            );
        }
        if let Some(n) = p.strip_prefix("/meta/") {
            let n: u8 = n.parse().unwrap_or(0);
            let etag = format!("\"e{n}\"");
            if req.header("if-none-match") == Some(etag.as_str()) {
                return Resp::status(304);
            }
            let Some(b) = builds.get(&n) else {
                return Resp::status(404);
            };
            return Resp {
                status: 200,
                body: b.meta.clone(),
                headers: vec![("ETag".into(), etag)],
                ..Default::default()
            };
        }
        if let Some(b) = builds
            .values()
            .find(|b| p == format!("/content-system/v2/meta/{}", galaxy_path(&b.depot_hash)))
        {
            return Resp {
                status: 200,
                body: b.depot.clone(),
                ..Default::default()
            };
        }
        if p.starts_with(&format!("/products/{APP}/secure_link")) {
            return Resp::json(json!({"urls": [{"url_format": "{base}/store{path}",
                "parameters": {"base": base, "path": "/v2/1"}}]}));
        }
        if p.starts_with("/open_link") {
            return Resp::json(json!({"urls": [{"url": format!("{base}/deps")}]}));
        }
        if p == "/user/data/games" {
            return Resp::json(json!({"owned": [1]}));
        }
        if p.starts_with("/dependencies/repository") {
            return Resp::json(
                json!({"build_id": "r1", "repository_manifest": format!("{base}/repo")}),
            );
        }
        if p == "/repo" {
            return Resp {
                status: 200,
                body: repo.clone(),
                ..Default::default()
            };
        }
        if p == format!(
            "/content-system/v2/dependencies/meta/{}",
            galaxy_path("isim")
        ) {
            return Resp {
                status: 200,
                body: isi_depot.clone(),
                ..Default::default()
            };
        }
        let key = p.rsplit('/').next().unwrap_or_default();
        if (p.starts_with("/store/v2/1/") || p.starts_with("/deps/"))
            && let Some(bytes) = chunks.get(key)
        {
            return Resp {
                status: 200,
                body: bytes.clone(),
                ..Default::default()
            };
        }
        Resp::status(404)
    });
    let b = server.base();
    *base.lock() = b.clone();
    let install_base = root.join("games");
    let settings: SettingsFn = Arc::new(move || {
        Arc::new(AppSettings {
            default_install_path: install_base.to_string_lossy().into_owned(),
            ..Default::default()
        })
    });
    let paths = GogPaths::under(&root);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    json_store::write(
        &paths.auth_file(),
        &json!({auth::CLIENT_ID: {"access_token": "AT", "refresh_token": "RT", "user_id": "7",
                                  "expires_in": 3600, "loginTime": now}}),
    )
    .unwrap();
    let store = GogStore::with_endpoints(
        EventBus::new(),
        settings,
        paths,
        Endpoints {
            content_system: b.clone(),
            cdn: b.clone(),
            api: b.clone(),
            embed: b.clone(),
            cloud_storage: b,
        },
    );
    Fixture {
        _dir: dir,
        root,
        server,
        version,
        store,
    }
}

fn fetches(f: &Fixture, needle: &str) -> usize {
    f.server
        .requests()
        .iter()
        .filter(|r| r.path.contains(needle))
        .count()
}

#[tokio::test]
async fn installs_updates_repairs_and_uninstalls() {
    let f = fixture();
    f.store.load_state().await;
    f.store.state.write().user = Some(UserData {
        id: "7".into(),
        username: "player".into(),
        avatar: None,
    });

    let info = f
        .store
        .get_install_info(APP, InstallPlatform::Windows, &Default::default())
        .await
        .unwrap();
    assert_eq!(info.version.as_deref(), Some("v1"));
    assert_eq!(info.download_size, 1_000);

    let request = InstallRequest {
        path: f.root.join("games"),
        platform: InstallPlatform::Windows,
        ..Default::default()
    };
    let outcome = f
        .store
        .install(APP, &request, OpContext::detached())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    let dir = f.root.join("games/Game Dir");
    assert_eq!(std::fs::read(dir.join("bin/game.exe")).unwrap(), exe());
    assert_eq!(std::fs::read(dir.join("data.pak")).unwrap(), pak(1));
    assert!(dir.join("old.txt").is_file() && dir.join("saves").is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(dir.join("bin/game.exe"))
            .unwrap()
            .permissions()
            .mode();
        assert!(mode & 0o100 != 0);
    }
    let paths = f.store.paths().clone();
    assert!(paths.support_dir(APP).join("1/setup.exe").is_file());
    assert!(paths.redist.join("__redist/ISI/si.exe").is_file());
    let rec = f.store.record(APP).unwrap();
    assert_eq!(rec.version, "v1");
    assert_eq!(rec.build_id.as_deref(), Some("b1"));
    assert_eq!(rec.version_etag.as_deref(), Some("\"e1\""));
    let build = f.store.build_record(APP).await.unwrap();
    assert_eq!(
        (build.build_id.as_str(), build.language.as_str()),
        ("b1", "en-US")
    );
    assert!(f.store.list_updatable().await.unwrap().is_empty());

    // Update: only the changed chunk is fetched, the removed file goes.
    f.version.store(2, Ordering::SeqCst);
    assert_eq!(f.store.list_updatable().await.unwrap(), [APP]);
    let exe_key = chunk(&exe()).1;
    let before = fetches(&f, &exe_key);
    let outcome = f
        .store
        .update(APP, &UpdateRequest::default(), OpContext::detached())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(std::fs::read(dir.join("data.pak")).unwrap(), pak(2));
    assert!(!dir.join("old.txt").exists());
    assert_eq!(
        fetches(&f, &exe_key),
        before,
        "unchanged files are not downloaded"
    );
    let rec = f.store.record(APP).unwrap();
    assert_eq!(
        (rec.version.as_str(), rec.build_id.as_deref()),
        ("v2", Some("b2"))
    );
    assert!(f.store.list_updatable().await.unwrap().is_empty());

    // Repair restores a damaged file and a missing one.
    std::fs::write(dir.join("data.pak"), b"broken").unwrap();
    std::fs::remove_file(paths.support_dir(APP).join("1/setup.exe")).unwrap();
    let outcome = f.store.repair(APP, OpContext::detached()).await.unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(std::fs::read(dir.join("data.pak")).unwrap(), pak(2));
    assert!(paths.support_dir(APP).join("1/setup.exe").is_file());

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
    assert!(!dir.exists());
    assert!(f.store.record(APP).is_none());
    assert!(!paths.build_file(APP).exists());
    assert!(!paths.support_dir(APP).exists());
}

#[tokio::test]
async fn imports_installed_games() {
    let f = fixture();
    f.store.load_state().await;
    let dir = f.root.join("elsewhere/Game");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("goggame-1.info"),
        r#"{"gameId": "1", "rootGameId": "1", "name": "Game", "language": "English"}"#,
    )
    .unwrap();
    std::fs::write(dir.join("goggame-1.id"), r#"{"buildId": "b1"}"#).unwrap();
    f.store
        .import_game(APP, &dir, InstallPlatform::Windows)
        .await
        .unwrap();
    let rec = f.store.record(APP).unwrap();
    assert_eq!(rec.language.as_deref(), Some("en-US"));
    assert_eq!(rec.version, "v1");
    assert_eq!(rec.version_etag.as_deref(), Some("\"e1\""));
    let build = f.store.build_record(APP).await.unwrap();
    assert_eq!(build.build_id, "b1");
    assert!(
        f.store
            .import_game("2", &dir, InstallPlatform::Windows)
            .await
            .is_err()
    );
}
