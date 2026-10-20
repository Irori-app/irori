//! Cloud saves: resolving the save folder, deciding the sync direction,
//! downloading a save and packaging one for upload. Saves are stored as a
//! manifest plus chunks, like game builds.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use sha1::{Digest as _, Sha1};

use crate::api::{Client, Session};
use crate::chunk::{self, CHUNK_SIZE};
use crate::manifest::{ChunkInfo, ChunkPart, FileManifest, Guid, Manifest, Secrets};
use crate::registry;

const MANIFEST_TIME: &[time::format_description::FormatItem<'static>] =
    time::macros::format_description!("[year].[month].[day]-[hour].[minute].[second]");

/// Unix time of a save manifest name (`2024.01.31-12.00.00.manifest`).
pub fn manifest_time(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".manifest").unwrap_or(name);
    let t = time::PrimitiveDateTime::parse(stem, MANIFEST_TIME).ok()?;
    Some(t.assume_utc().unix_timestamp().max(0) as u64)
}

fn manifest_name(unix: u64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(unix as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    t.format(MANIFEST_TIME).unwrap_or_default()
}

/// Where the host keeps the save data of a game.
pub struct SaveLocation<'a> {
    /// `CloudSaveFolder(_MAC)` template from the game's metadata.
    pub template: &'a str,
    pub install_dir: &'a Path,
    pub account_id: &'a str,
    /// Wine prefix of a Windows game run through Wine (its real root, the
    /// one holding `user.reg`).
    pub wine_prefix: Option<&'a Path>,
    /// `Mac` for macOS builds.
    pub platform: &'a str,
}

/// Resolve a save folder template (`{AppData}/Game/Saved`) to a host path.
pub fn resolve_save_path(loc: &SaveLocation<'_>) -> Result<PathBuf> {
    let mut template = loc.template.replace('\\', "/");
    let mut vars: HashMap<String, PathBuf> = HashMap::new();
    vars.insert("{installdir}".into(), loc.install_dir.to_path_buf());
    vars.insert("{epicid}".into(), PathBuf::from(loc.account_id));
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    match (loc.wine_prefix, loc.platform) {
        (Some(prefix), _) => {
            let f = registry::shell_folders(prefix)?;
            let get = |k: &str| {
                f.get(k)
                    .cloned()
                    .with_context(|| format!("the Wine prefix has no {k} folder"))
            };
            vars.insert("{appdata}".into(), get("Local AppData")?);
            let personal = get("Personal")?;
            vars.insert(
                "{userdir}".into(),
                std::fs::canonicalize(&personal).unwrap_or(personal),
            );
            if let Ok(saved) = get("{4C5C32FF-BB9D-43B0-B5B4-2D72E54EAAA4}") {
                vars.insert("{usersavedgames}".into(), saved);
            }
            // Legacy prefixes do not resolve Epic's relative LocalLow and
            // Roaming paths from LOCALAPPDATA.
            let appdata = get("AppData")?;
            if !appdata.to_string_lossy().to_lowercase().contains("roaming") {
                if let Ok(low) = get("{A520A1A4-1780-4FF6-BD18-167343C5AF16}") {
                    vars.insert("{locallow}".into(), low);
                }
                vars.insert("{roaming}".into(), appdata);
                let lower = template.to_lowercase();
                if let Some(i) = lower.find("locallow/") {
                    template = format!("{{locallow}}/{}", &template[i + 9..]);
                } else if let Some(i) = lower.find("roaming/") {
                    template = format!("{{roaming}}/{}", &template[i + 8..]);
                }
            }
        }
        (None, "Mac") => {
            vars.insert("{appdata}".into(), home.join("Library/Application Support"));
            vars.insert("{userdir}".into(), home.join("Documents"));
            vars.insert("{userlibrary}".into(), home.join("Library"));
        }
        (None, _) => {
            let local = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData/Local"));
            vars.insert("{appdata}".into(), local);
            vars.insert("{userdir}".into(), home.join("Documents"));
            vars.insert("{userprofile}".into(), home.clone());
            vars.insert("{usersavedgames}".into(), home.join("Saved Games"));
        }
    }
    let mut out = PathBuf::new();
    for part in template.split('/').filter(|p| !p.is_empty()) {
        match vars.get(&part.to_lowercase()) {
            Some(v) => out.push(v),
            None if part.starts_with('{') || part.contains('%') => {
                bail!("The save folder uses an unknown variable {part}")
            }
            None => out.push(part),
        }
    }
    if !out.is_absolute() {
        bail!("The save folder {} could not be resolved", out.display());
    }
    Ok(case_insensitive(&out))
}

/// Find an existing path matching `path` case-insensitively (Windows games
/// on case-sensitive file systems); unmatched parts are kept as given.
pub fn case_insensitive(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut components = path.components().peekable();
    while let Some(c) = components.next() {
        let candidate = out.join(c.as_os_str());
        if candidate.exists() || out.as_os_str().is_empty() {
            out = candidate;
            continue;
        }
        let wanted = c.as_os_str().to_string_lossy().to_lowercase();
        let found = std::fs::read_dir(&out).ok().and_then(|entries| {
            entries
                .flatten()
                .find(|e| e.file_name().to_string_lossy().to_lowercase() == wanted)
                .map(|e| e.file_name())
        });
        match found {
            Some(name) => out.push(name),
            None => {
                out.push(c.as_os_str());
                out.extend(components.by_ref().map(|c| c.as_os_str().to_owned()));
            }
        }
    }
    out
}

/// How local and cloud saves compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStatus {
    NoSave,
    LocalNewer,
    RemoteNewer,
    SameAge,
    /// Both changed since the last sync.
    Conflict,
}

/// Newest modification time of the files under `dir` (0 when empty).
pub fn newest_mtime(dir: &Path) -> u64 {
    walk(dir)
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok())
        .filter_map(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .max()
        .unwrap_or(0)
}

/// Compare the local save (newest mtime) with the newest cloud save.
pub fn status(local: u64, remote: Option<u64>, last_sync: Option<u64>) -> SaveStatus {
    match (local, remote) {
        (0, None) => SaveStatus::NoSave,
        (_, None) => SaveStatus::LocalNewer,
        (0, Some(_)) => SaveStatus::RemoteNewer,
        (l, Some(r)) => {
            if let Some(sync) = last_sync.filter(|s| *s > 0)
                && r.saturating_sub(sync) > 60
                && l.saturating_sub(sync) > 60
            {
                return SaveStatus::Conflict;
            }
            if l.abs_diff(r) < 60 {
                SaveStatus::SameAge
            } else if l > r {
                SaveStatus::LocalNewer
            } else {
                SaveStatus::RemoteNewer
            }
        }
    }
}

/// A cloud save manifest of the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSave {
    /// Full storage path of the manifest.
    pub path: String,
    pub app_name: String,
    pub name: String,
    pub time: u64,
    pub read_link: Option<String>,
}

/// The cloud saves of an app, oldest first.
pub async fn remote_saves(
    client: &Client,
    session: &Session,
    app: &str,
) -> Result<Vec<RemoteSave>> {
    let files = client.save_files(session, app, true).await?;
    let mut out: Vec<RemoteSave> = files
        .into_iter()
        .filter(|(k, _)| k.contains(".manifest"))
        .filter_map(|(path, f)| {
            let parts: Vec<&str> = path.split('/').collect();
            let (app_name, name) = (parts.get(2)?.to_string(), parts.get(4)?.to_string());
            Some(RemoteSave {
                time: manifest_time(&name)?,
                app_name,
                name,
                read_link: f.read_link,
                path,
            })
        })
        .collect();
    out.sort_by_key(|s| s.time);
    Ok(out)
}

/// Glob match with `*` and `?` (Python's `fnmatch` on POSIX).
fn glob(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

fn matches(rel: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        (p.ends_with('/') && rel.starts_with(p.as_str()))
            || rel.ends_with(p.as_str())
            || glob(p, rel)
    })
}

/// Save files to consider, by the game's include/exclude lists.
fn filtered(rel: &str, include: Option<&[String]>, exclude: Option<&[String]>) -> bool {
    if let Some(inc) = include.filter(|i| !i.is_empty())
        && !matches(rel, inc)
    {
        return false;
    }
    if let Some(exc) = exclude.filter(|e| !e.is_empty())
        && matches(rel, exc)
    {
        return false;
    }
    true
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => out.push(e.path()),
                _ => {}
            }
        }
    }
    out
}

fn relative(dir: &Path, file: &Path) -> String {
    file.strip_prefix(dir)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Filters from the game's metadata.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub include: Option<Vec<String>>,
    pub exclude: Option<Vec<String>>,
}

/// Download a cloud save into `dir`, first deleting the local files the
/// filters cover (games keeping saves in their install folder).
pub async fn download(
    client: &Client,
    session: &Session,
    save: &RemoteSave,
    dir: &Path,
    filters: &Filters,
) -> Result<()> {
    let link = save
        .read_link
        .as_deref()
        .context("The cloud save has no download address")?;
    let raw = client.fetch_link(link).await?;
    if raw.is_empty() {
        bail!("The cloud save {} is empty", save.name);
    }
    let manifest = Manifest::parse(&raw, &Secrets::new())?;
    let paths: Vec<String> = manifest
        .chunks
        .iter()
        .map(|c| manifest.chunk_path(c))
        .collect();
    let links = client.save_links(session, &save.app_name, &paths).await?;
    write_save(client, &manifest, links, save.time, dir, filters).await
}

/// Fetch the chunks of a save manifest and write its files into `dir`.
async fn write_save(
    client: &Client,
    manifest: &Manifest,
    links: HashMap<String, crate::SaveFile>,
    time: u64,
    dir: &Path,
    filters: &Filters,
) -> Result<()> {
    if links.len() != manifest.chunks.len() {
        bail!(
            "The cloud save is incomplete: {} of {} chunks are available",
            links.len(),
            manifest.chunks.len()
        );
    }
    let fetched = futures::future::try_join_all(links.values().map(|f| async move {
        let link = f
            .read_link
            .as_deref()
            .context("a chunk has no download address")?;
        let raw = client.fetch_link(link).await?;
        tokio::task::spawn_blocking(move || chunk::decode(&raw, &HashMap::new())).await?
    }))
    .await?;
    let chunks: HashMap<Guid, Vec<u8>> = fetched.into_iter().map(|c| (c.guid, c.data)).collect();

    let dir = dir.to_path_buf();
    let (include, exclude) = (filters.include.clone(), filters.exclude.clone());
    let mtime = UNIX_EPOCH + Duration::from_secs(time);
    let manifest = manifest.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        for file in walk(&dir) {
            let rel = relative(&dir, &file);
            if filtered(&rel, include.as_deref(), exclude.as_deref()) {
                std::fs::remove_file(&file)
                    .with_context(|| format!("Could not delete {}", file.display()))?;
            }
        }
        for f in &manifest.files {
            let rel = ember_depot::path::normalize(&f.filename)?;
            let path = ember_depot::path::join(&dir, &rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut data = Vec::with_capacity(f.size as usize);
            for p in &f.parts {
                let c = chunks
                    .get(&p.guid)
                    .with_context(|| format!("A chunk of {rel} is missing"))?;
                let range = p.offset as usize..(p.offset + p.size) as usize;
                data.extend_from_slice(
                    c.get(range)
                        .with_context(|| format!("A chunk of {rel} is damaged"))?,
                );
            }
            std::fs::write(&path, &data).with_context(|| format!("Could not write {rel}"))?;
            let file = std::fs::File::options().write(true).open(&path)?;
            let _ = file.set_modified(mtime);
        }
        Ok(())
    })
    .await?
}

/// Files to upload for a save: chunks and the manifest, by storage path
/// relative to the app.
pub struct Package {
    pub files: Vec<(String, Vec<u8>)>,
    pub manifest_name: String,
}

/// Pack the save files in `dir` (respecting the filters) into chunks and a
/// manifest named after `time`. `None` when there is nothing to upload.
pub fn package(
    dir: &Path,
    app_name: &str,
    account_id: &str,
    folders: (&str, Option<&str>),
    filters: &Filters,
    time: u64,
) -> Result<Option<Package>> {
    let mut files: Vec<PathBuf> = walk(dir)
        .into_iter()
        .filter(|f| {
            filtered(
                &relative(dir, f),
                filters.include.as_deref(),
                filters.exclude.as_deref(),
            )
        })
        .collect();
    if files.is_empty() {
        return Ok(None);
    }
    files.sort_by_cached_key(|f| relative(dir, f).to_lowercase());

    let build = manifest_name(time);
    let mut m = Manifest::new(format!("{app_name}{account_id}"), build.clone());
    m.custom_fields
        .push(("CloudSaveFolder".into(), folders.0.into()));
    if let Some(mac) = folders.1 {
        m.custom_fields
            .push(("CloudSaveFolder_MAC".into(), mac.into()));
    }
    let mut out = Vec::new();
    let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK_SIZE);
    let mut guid = new_guid();
    let flush = |buffer: &mut Vec<u8>,
                 guid: Guid,
                 m: &mut Manifest,
                 out: &mut Vec<(String, Vec<u8>)>|
     -> Result<()> {
        let (bytes, hash, sha) = chunk::encode(guid, buffer)?;
        let info = ChunkInfo {
            guid,
            hash,
            sha1: sha,
            group: group_of(guid),
            window_size: CHUNK_SIZE as u32,
            file_size: bytes.len() as u64,
            ..Default::default()
        };
        out.push((m.chunk_path(&info), bytes));
        m.chunks.push(info);
        buffer.clear();
        Ok(())
    };
    for file in &files {
        let data = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
        let mut fm = FileManifest {
            filename: relative(dir, file),
            sha1: Sha1::digest(&data).into(),
            size: data.len() as u64,
            ..Default::default()
        };
        let mut pos = 0;
        while pos < data.len() {
            let take = (CHUNK_SIZE - buffer.len()).min(data.len() - pos);
            fm.parts.push(ChunkPart {
                guid,
                offset: buffer.len() as u32,
                size: take as u32,
                file_offset: pos as u64,
            });
            buffer.extend_from_slice(&data[pos..pos + take]);
            pos += take;
            if buffer.len() >= CHUNK_SIZE {
                flush(&mut buffer, guid, &mut m, &mut out)?;
                guid = new_guid();
            }
        }
        m.files.push(fm);
    }
    if !buffer.is_empty() {
        flush(&mut buffer, guid, &mut m, &mut out)?;
    }
    let manifest_path = format!("manifests/{build}.manifest");
    out.push((manifest_path, m.to_bytes()?));
    Ok(Some(Package {
        files: out,
        manifest_name: format!("{build}.manifest"),
    }))
}

/// Upload a packaged save.
pub async fn upload(
    client: &Client,
    session: &Session,
    app_name: &str,
    pkg: Package,
) -> Result<()> {
    let names: Vec<String> = pkg.files.iter().map(|(n, _)| n.clone()).collect();
    let links = client.save_links(session, app_name, &names).await?;
    let mut files: HashMap<String, Vec<u8>> = pkg.files.into_iter().collect();
    for (name, f) in links {
        let Some(body) = files.remove(&name) else {
            continue;
        };
        let link = f
            .write_link
            .with_context(|| format!("Epic gave no upload address for {name}"))?;
        client.put_link(&link, body).await?;
    }
    if !files.is_empty() {
        bail!("Epic did not accept {} of the save files", files.len());
    }
    Ok(())
}

fn new_guid() -> Guid {
    let b = *uuid::Uuid::new_v4().as_bytes();
    let w = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    Guid([w(0), w(4), w(8), w(12)])
}

/// Epic's chunk group: CRC-32 of the GUID bytes modulo 100.
fn group_of(guid: Guid) -> u8 {
    let mut crc = flate2::Crc::new();
    crc.update(&guid.le_bytes());
    (crc.sum() % 100) as u8
}

/// Current Unix time, for naming uploads.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Paths of `dir` the filters keep (for tests and callers listing saves).
pub fn save_files(dir: &Path, filters: &Filters) -> HashSet<String> {
    walk(dir)
        .iter()
        .map(|f| relative(dir, f))
        .filter(|r| filtered(r, filters.include.as_deref(), filters.exclude.as_deref()))
        .collect()
}

#[cfg(test)]
mod tests {
    use ember_depot::test_server::{Resp, TestServer};
    use parking_lot::Mutex;
    use serde_json::json;

    use super::*;
    use crate::api::tests::{endpoints, session};
    use crate::registry::tests::USER_REG;

    #[test]
    fn names_and_status() {
        let t = manifest_time("2024.01.31-12.00.05.manifest").unwrap();
        assert_eq!(t, 1_706_702_405);
        assert_eq!(manifest_name(t), "2024.01.31-12.00.05");
        assert!(manifest_time("garbage").is_none());
        assert_eq!(status(0, None, None), SaveStatus::NoSave);
        assert_eq!(status(5, None, None), SaveStatus::LocalNewer);
        assert_eq!(status(0, Some(5), None), SaveStatus::RemoteNewer);
        assert_eq!(status(1000, Some(1030), None), SaveStatus::SameAge);
        assert_eq!(status(2000, Some(1000), None), SaveStatus::LocalNewer);
        assert_eq!(status(1000, Some(2000), Some(500)), SaveStatus::Conflict);
        assert_eq!(
            status(1000, Some(2000), Some(1500)),
            SaveStatus::RemoteNewer
        );
    }

    #[test]
    fn filters_follow_legendary() {
        let inc = vec!["*.sav".to_string(), "Profiles/".to_string()];
        assert!(filtered("a/b.sav", Some(&inc), None));
        assert!(filtered("Profiles/x.ini", Some(&inc), None));
        assert!(!filtered("log.txt", Some(&inc), None));
        let exc = vec!["*.log".to_string()];
        assert!(!filtered("x.log", None, Some(&exc)));
        assert!(filtered("x.sav", None, Some(&exc)));
        assert!(glob("a?c*", "abcdef") && !glob("a?c", "ac"));
    }

    #[test]
    fn resolves_save_paths_in_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let pfx = dir.path().join("pfx");
        std::fs::create_dir_all(pfx.join("drive_c/users/steamuser/AppData/Local/MyGame")).unwrap();
        std::fs::write(pfx.join("user.reg"), USER_REG).unwrap();
        let loc = SaveLocation {
            template: r"{AppData}\mygame\Saved",
            install_dir: Path::new("/games/x"),
            account_id: "acc",
            wine_prefix: Some(&pfx),
            platform: "Windows",
        };
        assert_eq!(
            resolve_save_path(&loc).unwrap(),
            pfx.join("drive_c/users/steamuser/AppData/Local/MyGame/Saved")
        );
        let install = SaveLocation {
            template: "{InstallDir}/Saves/{EpicId}",
            wine_prefix: None,
            ..loc
        };
        assert_eq!(
            resolve_save_path(&install).unwrap(),
            Path::new("/games/x/Saves/acc")
        );
        let bad = SaveLocation {
            template: "{Unknown}/x",
            ..install
        };
        assert!(resolve_save_path(&bad).is_err());
    }

    #[tokio::test]
    async fn packages_uploads_and_downloads_saves() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("slot")).unwrap();
        std::fs::write(src.join("slot/1.sav"), vec![7u8; CHUNK_SIZE + 10]).unwrap();
        std::fs::write(src.join("small.sav"), b"tiny").unwrap();
        std::fs::write(src.join("debug.log"), b"skip").unwrap();
        let filters = Filters {
            include: None,
            exclude: Some(vec!["*.log".into()]),
        };
        let time = 1_706_702_405;
        let pkg = package(&src, "Sugar", "acc", ("{AppData}/S", None), &filters, time)
            .unwrap()
            .unwrap();
        assert_eq!(pkg.manifest_name, "2024.01.31-12.00.05.manifest");
        assert_eq!(pkg.files.len(), 3, "two chunks and the manifest");
        assert!(pkg.files[0].0.starts_with("ChunksV4/"));
        assert!(
            package(
                &dir.path().join("empty"),
                "S",
                "a",
                ("x", None),
                &filters,
                0
            )
            .unwrap()
            .is_none()
        );

        // A storage stand-in holding the package; it offers links to every
        // stored file (it cannot read request bodies).
        let stored: HashMap<String, Vec<u8>> = pkg.files.into_iter().collect();
        let names: Vec<String> = stored.keys().cloned().collect();
        let base = std::sync::Arc::new(Mutex::new(String::new()));
        let server_base = base.clone();
        let server = TestServer::start(move |req| {
            let base = server_base.lock().clone();
            if req.path == "/api/v1/access/egstore/savesync/acc/Sugar/" {
                let files: serde_json::Map<String, serde_json::Value> = names
                    .iter()
                    .filter(|k| !k.starts_with("manifests/"))
                    .map(|k| (k.clone(), json!({"readLink": format!("{base}/blob/{k}")})))
                    .collect();
                return Resp::json(json!({ "files": files }));
            }
            match req.path.strip_prefix("/blob/").and_then(|k| stored.get(k)) {
                Some(b) => Resp {
                    status: 200,
                    body: b.clone(),
                    ..Default::default()
                },
                None => Resp::status(404),
            }
        });
        *base.lock() = server.base();
        let client =
            crate::Client::with_endpoints(crate::Identity::default(), endpoints(&server)).unwrap();
        let save = RemoteSave {
            path: format!("x/acc/Sugar/manifests/{}", pkg.manifest_name),
            app_name: "Sugar".into(),
            name: pkg.manifest_name.clone(),
            time,
            read_link: Some(server.url(&format!("/blob/manifests/{}", pkg.manifest_name))),
        };
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("old.sav"), b"stale").unwrap();
        std::fs::write(dest.join("keep.log"), b"kept").unwrap();
        download(&client, &session(), &save, &dest, &filters)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(dest.join("slot/1.sav")).unwrap(),
            vec![7u8; CHUNK_SIZE + 10]
        );
        assert_eq!(std::fs::read(dest.join("small.sav")).unwrap(), b"tiny");
        assert!(
            !dest.join("old.sav").exists(),
            "files the save covers are replaced"
        );
        assert!(dest.join("keep.log").exists(), "excluded files are kept");
        assert_eq!(newest_mtime(&dest.join("slot")), time);
        assert_eq!(
            save_files(&dest, &filters),
            ["slot/1.sav".to_string(), "small.sav".to_string()].into()
        );
    }
}
