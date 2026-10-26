//! Cloud saves: GOG's cloud storage keeps each save file gzip-compressed
//! under `<user>/<game client>/<location name>/<path>`. Sync decisions use
//! modification times against the time of the last sync.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

const USER_AGENT: &str = "GOGGalaxyCommunicationService/2.0.13.27 (Windows_32bit) dont_sync_marker/true installation_source/gog";
/// Hash GOG reports for deleted files (they are skipped).
const DELETED_MD5: &str = "aadd86936a80ee8a369579c3926f1b3c";

/// A save file in the cloud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudFile {
    /// Path below the location folder (`/`-separated).
    pub path: String,
    pub md5: String,
    /// Unix time.
    pub modified: u64,
}

/// A save file on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub path: String,
    pub abs: PathBuf,
    pub modified: u64,
}

/// What a sync does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Download,
    Upload,
    /// Both sides changed since the last sync.
    Conflict,
    Nothing,
}

/// The requested direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preference {
    Auto,
    Download,
    Upload,
    ForceDownload,
    ForceUpload,
}

/// Files changed on each side since `last_sync`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Classification {
    pub updated_local: Vec<String>,
    pub updated_cloud: Vec<String>,
    pub only_local: Vec<String>,
    pub only_cloud: Vec<String>,
}

impl Classification {
    pub fn new(local: &[LocalFile], cloud: &[CloudFile], last_sync: u64) -> Self {
        let mut c = Self::default();
        for f in local {
            if !cloud.iter().any(|x| x.path == f.path) {
                c.only_local.push(f.path.clone());
            }
            if f.modified > last_sync {
                c.updated_local.push(f.path.clone());
            }
        }
        for f in cloud.iter().filter(|f| f.md5 != DELETED_MD5) {
            if !local.iter().any(|x| x.path == f.path) {
                c.only_cloud.push(f.path.clone());
            }
            if f.modified > last_sync {
                c.updated_cloud.push(f.path.clone());
            }
        }
        c
    }

    pub fn action(&self) -> Action {
        match (self.updated_local.is_empty(), self.updated_cloud.is_empty()) {
            (true, false) => Action::Download,
            (false, true) => Action::Upload,
            (true, true) => Action::Nothing,
            (false, false) => Action::Conflict,
        }
    }
}

/// The action for a sync, given what changed and what the user asked for.
pub fn decide(c: &Classification, local: usize, cloud: usize, pref: Preference) -> Action {
    match pref {
        Preference::ForceDownload => return Action::Download,
        Preference::ForceUpload => return Action::Upload,
        _ => {}
    }
    let action = if local > 0 && cloud == 0 {
        Action::Upload
    } else if local == 0 && cloud > 0 {
        Action::Download
    } else {
        c.action()
    };
    match (pref, action) {
        (Preference::Upload, Action::Download) | (Preference::Download, Action::Upload) => {
            Action::Nothing
        }
        _ => action,
    }
}

fn parse_time(s: &str) -> Option<u64> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|t| t.unix_timestamp().max(0) as u64)
}

fn format_time(unix: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// The save files under `dir`.
pub fn local_files(dir: &Path) -> Vec<LocalFile> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() => {
                    let modified = e
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs());
                    out.push(LocalFile {
                        path: p
                            .strip_prefix(dir)
                            .unwrap_or(&p)
                            .to_string_lossy()
                            .replace('\\', "/"),
                        abs: p,
                        modified,
                    });
                }
                _ => {}
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Client of the cloud storage for one game and location.
pub struct Storage {
    http: reqwest::Client,
    base: String,
    token: String,
    /// `<user id>/<game client id>`.
    root: String,
    /// Location name (`saves`).
    location: String,
}

/// What a sync did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub action: Action,
    pub uploaded: usize,
    pub downloaded: usize,
    pub deleted: usize,
    /// Unix time the newest local file changed, if there is one.
    pub local_newest: Option<u64>,
    /// Unix time the newest cloud file changed, if there is one.
    pub cloud_newest: Option<u64>,
}

fn encode(path: &str) -> String {
    path.split('/')
        .map(|p| {
            url::form_urlencoded::byte_serialize(p.as_bytes())
                .collect::<String>()
                .replace('+', "%20")
        })
        .collect::<Vec<_>>()
        .join("/")
}

impl Storage {
    /// `token` must belong to the game's own client (`client_id`).
    pub fn new(
        base: &str,
        token: &str,
        user_id: &str,
        client_id: &str,
        location: &str,
    ) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(120))
                .build()?,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            root: format!("{user_id}/{client_id}"),
            location: location.to_string(),
        })
    }

    fn file_url(&self, path: &str) -> String {
        format!(
            "{}/v1/{}/{}/{}",
            self.base,
            self.root,
            encode(&self.location),
            encode(path)
        )
    }

    /// Files of this location.
    pub async fn list(&self) -> Result<Vec<CloudFile>> {
        #[derive(Deserialize)]
        struct Item {
            name: String,
            #[serde(default)]
            hash: String,
            #[serde(default)]
            last_modified: String,
        }
        let resp = self
            .http
            .get(format!("{}/v1/{}", self.base, self.root))
            .bearer_auth(&self.token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!("Could not reach GOG cloud saves ({})", e.without_url())
            })?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if !resp.status().is_success() {
            bail!("GOG cloud saves answered {}", resp.status());
        }
        let items: Vec<Item> = resp.json().await.context("unexpected cloud save list")?;
        let prefix = format!("{}/", self.location);
        Ok(items
            .into_iter()
            .filter_map(|i| {
                Some(CloudFile {
                    path: i.name.strip_prefix(&prefix)?.to_string(),
                    md5: i.hash,
                    modified: parse_time(&i.last_modified).unwrap_or(0),
                })
            })
            .collect())
    }

    pub async fn upload(&self, f: &LocalFile) -> Result<()> {
        let abs = f.abs.clone();
        let body = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            use std::io::Write as _;
            let data = std::fs::read(&abs)?;
            let mut gz = flate2::GzBuilder::new()
                .mtime(0)
                .operating_system(255)
                .write(Vec::new(), flate2::Compression::new(6));
            gz.write_all(&data)?;
            Ok(gz.finish()?)
        })
        .await??;
        let etag = format!("{:x}", md5::compute(&body));
        self.http
            .put(self.file_url(&f.path))
            .bearer_auth(&self.token)
            .header("X-Object-Meta-User-Agent", USER_AGENT)
            .header("X-Object-Meta-LocalLastModified", format_time(f.modified))
            .header("Etag", etag)
            .header("Content-Encoding", "gzip")
            .body(body)
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("Could not upload {}", f.path))?;
        Ok(())
    }

    pub async fn download(&self, path: &str, dir: &Path) -> Result<()> {
        let resp = self
            .http
            .get(self.file_url(path))
            .bearer_auth(&self.token)
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("Could not download {path}"))?;
        let modified = resp
            .headers()
            .get("X-Object-Meta-LocalLastModified")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_time);
        let gzipped = resp
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"gzip"));
        let bytes = resp.bytes().await?.to_vec();
        let rel = ember_depot::path::normalize(path)?;
        let dest = ember_depot::path::join(dir, &rel);
        tokio::task::spawn_blocking(move || -> Result<()> {
            use std::io::Read as _;
            // The body is gzip unless the transport already inflated it.
            let data = if gzipped || bytes.starts_with(&[0x1f, 0x8b]) {
                let mut out = Vec::new();
                match flate2::read::GzDecoder::new(bytes.as_slice()).read_to_end(&mut out) {
                    Ok(_) => out,
                    Err(_) => bytes,
                }
            } else {
                bytes
            };
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, data)?;
            if let Some(t) = modified {
                let f = std::fs::File::options().write(true).open(&dest)?;
                let _ = f.set_modified(UNIX_EPOCH + Duration::from_secs(t));
            }
            Ok(())
        })
        .await?
    }

    pub async fn delete(&self, path: &str) -> Result<()> {
        self.http
            .delete(self.file_url(path))
            .bearer_auth(&self.token)
            .send()
            .await?;
        Ok(())
    }

    /// Sync `dir` with this location.
    pub async fn sync(&self, dir: &Path, last_sync: u64, pref: Preference) -> Result<Report> {
        let local = {
            let dir = dir.to_path_buf();
            tokio::task::spawn_blocking(move || local_files(&dir)).await?
        };
        let cloud = self.list().await?;
        let live: Vec<&CloudFile> = cloud.iter().filter(|f| f.md5 != DELETED_MD5).collect();
        let c = Classification::new(&local, &cloud, last_sync);
        let action = decide(&c, local.len(), live.len(), pref);
        let mut report = Report {
            action,
            uploaded: 0,
            downloaded: 0,
            deleted: 0,
            local_newest: local.iter().map(|f| f.modified).max(),
            cloud_newest: live.iter().map(|f| f.modified).max(),
        };
        let force = matches!(pref, Preference::ForceDownload | Preference::ForceUpload);
        let first = local.is_empty() || live.is_empty();
        match action {
            Action::Upload => {
                for f in &local {
                    if force || first || c.updated_local.contains(&f.path) {
                        self.upload(f).await?;
                        report.uploaded += 1;
                    }
                }
                for p in &c.only_cloud {
                    self.delete(p).await?;
                    report.deleted += 1;
                }
            }
            Action::Download => {
                for f in &live {
                    if force || first || c.updated_cloud.contains(&f.path) {
                        self.download(&f.path, dir).await?;
                        report.downloaded += 1;
                    }
                }
                for p in &c.only_local {
                    if let Some(f) = local.iter().find(|l| &l.path == p) {
                        let _ = tokio::fs::remove_file(&f.abs).await;
                        report.deleted += 1;
                    }
                }
            }
            Action::Conflict | Action::Nothing => {}
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use ember_depot::test_server::{Resp, TestServer};
    use parking_lot::Mutex;
    use serde_json::json;

    use super::*;

    fn local(path: &str, modified: u64) -> LocalFile {
        LocalFile {
            path: path.into(),
            abs: PathBuf::from(path),
            modified,
        }
    }

    fn cloud(path: &str, modified: u64) -> CloudFile {
        CloudFile {
            path: path.into(),
            md5: "x".into(),
            modified,
        }
    }

    #[test]
    fn classifies_changes() {
        let l = [local("a", 200), local("b", 50)];
        let c = [cloud("a", 100), cloud("c", 300)];
        let k = Classification::new(&l, &c, 150);
        assert_eq!(k.updated_local, ["a"]);
        assert_eq!(k.updated_cloud, ["c"]);
        assert_eq!(k.only_local, ["b"]);
        assert_eq!(k.only_cloud, ["c"]);
        assert_eq!(k.action(), Action::Conflict);
        assert_eq!(decide(&k, 2, 2, Preference::Auto), Action::Conflict);
        // Syncing before or after play never skips a conflict quietly.
        assert_eq!(decide(&k, 2, 2, Preference::Download), Action::Conflict);
        assert_eq!(decide(&k, 2, 2, Preference::Upload), Action::Conflict);
        assert_eq!(decide(&k, 2, 2, Preference::ForceUpload), Action::Upload);
        let only_up = Classification::new(&l, &[cloud("a", 100)], 150);
        assert_eq!(decide(&only_up, 2, 1, Preference::Auto), Action::Upload);
        assert_eq!(
            decide(&only_up, 2, 1, Preference::Download),
            Action::Nothing
        );
        assert_eq!(
            decide(&Classification::default(), 0, 3, Preference::Auto),
            Action::Download
        );
        assert_eq!(encode("a b/c+d"), "a%20b/c%2Bd");
    }

    #[tokio::test]
    async fn uploads_and_downloads() {
        let stored: std::sync::Arc<Mutex<Vec<String>>> = Default::default();
        let log = stored.clone();
        let server = TestServer::start(move |req| {
            log.lock().push(req.path.clone());
            match req.path.as_str() {
                "/v1/u/c" => Resp::json(json!([
                    {"name": "saves/slot 1.sav", "hash": "h", "last_modified": "2030-01-01T00:00:00+00:00"},
                    {"name": "other/x", "hash": "h", "last_modified": "2030-01-01T00:00:00+00:00"}])),
                "/v1/u/c/saves/slot%201.sav" => {
                    use std::io::Write as _;
                    let mut gz =
                        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                    gz.write_all(b"cloud data").unwrap();
                    Resp {
                        status: 200,
                        headers: vec![(
                            "X-Object-Meta-LocalLastModified".into(),
                            "2030-01-01T00:00:00+00:00".into(),
                        )],
                        body: gz.finish().unwrap(),
                        ..Default::default()
                    }
                }
                _ => Resp::status(200),
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(&server.base(), "tok", "u", "c", "saves").unwrap();
        // Nothing local: everything is downloaded.
        let r = s.sync(dir.path(), 0, Preference::Auto).await.unwrap();
        assert_eq!((r.action, r.downloaded), (Action::Download, 1));
        assert_eq!(
            (r.local_newest, r.cloud_newest),
            (None, Some(1_893_456_000))
        );
        assert_eq!(
            std::fs::read(dir.path().join("slot 1.sav")).unwrap(),
            b"cloud data"
        );
        let files = local_files(dir.path());
        assert_eq!(files[0].modified, 1_893_456_000);
        // A newer local file is uploaded.
        std::fs::write(dir.path().join("new.sav"), b"x").unwrap();
        std::fs::File::options()
            .write(true)
            .open(dir.path().join("new.sav"))
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_900_000_000))
            .unwrap();
        let r = s
            .sync(dir.path(), 1_893_456_000, Preference::Auto)
            .await
            .unwrap();
        assert_eq!((r.action, r.uploaded), (Action::Upload, 1));
        assert!(stored.lock().iter().any(|p| p == "/v1/u/c/saves/new.sav"));
    }
}
