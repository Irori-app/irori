//! Managed installs: download a release (resumable, verified when the
//! project publishes checksums), unpack it into `<data>/engines/<id>/<version>`
//! beside other versions, and record it. Removing a version deletes only its
//! own folder.
//!
//! Installing never changes which version games use: the first managed
//! version becomes the engine's default, later ones only when the caller
//! asks (the Update button does).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use irori_core::{AppSettings, OpContext, util};
use irori_depot::{Algorithm, Digest, FileEntry, Finished, Job, Options};

use crate::discovery::locate_program;
use crate::host::Os;
use crate::manifest::EngineManifest;
use crate::releases::{Release, ReleaseFile};
use crate::state::{self, ManagedInstall};

/// Download `release` of `manifest` into the cache, resuming a partial
/// download. Returns the downloaded file.
pub async fn download(
    manifest: &EngineManifest,
    release: &Release,
    ctx: &OpContext,
) -> Result<Option<PathBuf>> {
    let main = ReleaseFile {
        url: release.url.clone(),
        file_name: release.file_name.clone(),
        size: release.size,
    };
    download_file(manifest, release, &main, ctx).await
}

/// Download one file of `release` (the main one or an extra).
async fn download_file(
    manifest: &EngineManifest,
    release: &Release,
    file: &ReleaseFile,
    ctx: &OpContext,
) -> Result<Option<PathBuf>> {
    let folder = crate::dirs::cache().join("downloads").join(&manifest.id);
    let name = format!("{}-{}", sanitize(&release.version), file.file_name);
    let expected = match &release.checksum_url {
        Some(url) => Some(expected_sha256(url, &file.file_name).await?),
        None => None,
    };
    let size = match file.size {
        Some(size) => size,
        None => content_length(&file.url).await?,
    };
    let entry = FileEntry::whole(
        name.clone(),
        size,
        expected.map(|v| Digest::new(Algorithm::Sha256, v)),
        vec![file.url.clone()],
    );
    let progress_sink = ctx.progress.clone();
    let progress = Arc::new(move |p: irori_depot::Progress| {
        progress_sink(irori_core::InstallProgress {
            percent: (p.bytes_total > 0).then_some(p.percent),
            bytes_done: Some(p.bytes_done),
            bytes_total: (p.bytes_total > 0).then_some(p.bytes_total),
            eta_secs: p.eta_secs,
            download_speed: Some(p.download_speed),
            ..Default::default()
        })
    });
    let finished = irori_depot::download(
        irori_core::http::download_client(),
        Job {
            root: &folder,
            files: &[entry],
            remove: &[],
        },
        &Options::default(),
        progress,
        &ctx.cancel,
    )
    .await
    .with_context(|| format!("Could not download {} {}", manifest.name, release.version))?;
    Ok(match finished {
        Finished::Done => Some(folder.join(name)),
        Finished::Cancelled => None,
    })
}

/// The size of the file at `url`, which resumable downloads need up front.
pub(crate) async fn content_length(url: &str) -> Result<u64> {
    let response = irori_core::http::client()
        .head(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .context("Could not reach the download server")?;
    // `Response::content_length` describes the (empty) body of a HEAD
    // answer, not the header.
    response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .context("The download server did not say how big the file is")
}

/// The SHA-256 of `file_name` from a published checksum file.
async fn expected_sha256(url: &str, file_name: &str) -> Result<Vec<u8>> {
    let text = irori_core::http::client()
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .context("Could not fetch the checksums of the download")?
        .text()
        .await?;
    let digest = parse_sha256sums(&text, file_name)
        .context("The project's checksum file does not list this download")?;
    hex::decode(digest).context("bad checksum")
}

/// The digest for `file_name` in a `sha256sum` listing.
pub fn parse_sha256sums(text: &str, file_name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let digest = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (digest.len() == 64 && name.rsplit('/').next() == Some(file_name))
            .then(|| digest.to_ascii_lowercase())
    })
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Download, unpack and record `release`. Returns `None` when cancelled.
/// The download stays in the cache until the install succeeds, so a retry
/// resumes it.
pub async fn install(
    manifest: &EngineManifest,
    release: &Release,
    ctx: &OpContext,
) -> Result<Option<ManagedInstall>> {
    (ctx.step)("Downloading");
    let Some(file) = download(manifest, release, ctx).await? else {
        return Ok(None);
    };
    let mut extras = Vec::with_capacity(release.extras.len());
    for extra in &release.extras {
        let Some(path) = download_file(manifest, release, extra, ctx).await? else {
            return Ok(None);
        };
        extras.push((path, extra.file_name.clone()));
    }
    (ctx.step)("Unpacking");
    let root = crate::dirs::engine(&manifest.id);
    let dir = root.join(sanitize(&release.version));
    let staging = root.join(format!(".{}.partial", sanitize(&release.version)));
    let _ = tokio::fs::remove_dir_all(&staging).await;
    let kind = crate::archive::classify(&release.file_name);
    let unpacked = if kind == crate::archive::Kind::Dmg {
        let scratch = crate::dirs::cache().join("mount").join(&manifest.id);
        crate::archive::unpack_dmg(&file, &staging, &scratch).await
    } else {
        let (file, name, staging, cancel) = (
            file.clone(),
            release.file_name.clone(),
            staging.clone(),
            ctx.cancel.clone(),
        );
        tokio::task::spawn_blocking(move || crate::archive::unpack(&file, &name, &staging, &cancel))
            .await
            .context("unpacking stopped")?
    };
    let unpacked = match unpacked {
        Ok(()) if !extras.is_empty() => {
            // Runtime files unpack beside the program.
            let (extras, staging, cancel) = (extras.clone(), staging.clone(), ctx.cancel.clone());
            tokio::task::spawn_blocking(move || {
                extras.iter().try_for_each(|(extra, name)| {
                    crate::archive::unpack(extra, name, &staging, &cancel)
                })
            })
            .await
            .context("unpacking stopped")?
        }
        other => other,
    };
    if let Err(e) = unpacked {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        if ctx.cancel.is_cancelled() {
            return Ok(None);
        }
        return Err(e.context(format!(
            "Could not unpack {} {}",
            manifest.name, release.version
        )));
    }
    let names = manifest.binaries.get(Os::current()).clone();
    let Some(program) = locate_program(&staging, &names) else {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        bail!(
            "The {} download does not contain the program Irori expected ({}). The project may have changed how it packages releases.",
            manifest.name,
            names.join(", ")
        );
    };
    let relative = program
        .strip_prefix(&staging)
        .unwrap_or(&program)
        .to_path_buf();
    if tokio::fs::metadata(&dir).await.is_ok() {
        tokio::fs::remove_dir_all(&dir)
            .await
            .with_context(|| format!("replacing {}", dir.display()))?;
    }
    tokio::fs::rename(&staging, &dir)
        .await
        .with_context(|| format!("saving {}", dir.display()))?;
    let program = dir.join(relative);
    if program
        .extension()
        .is_none_or(|e| !e.eq_ignore_ascii_case("app"))
    {
        let _ = crate::archive::make_executable(&program);
    }
    let size = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || irori_runtime::download::dir_size(&dir))
            .await
            .unwrap_or(0)
    };
    let installed = ManagedInstall {
        version: release.version.clone(),
        dir,
        program,
        size,
        installed_at: util::now_rfc3339(),
        url: release.url.clone(),
    };
    let (id, record) = (manifest.id.clone(), installed.clone());
    tokio::task::spawn_blocking(move || {
        state::update(|s| {
            let list = s.managed.entry(id.clone()).or_default();
            list.retain(|m| m.version != record.version);
            list.push(record.clone());
            s.preferred
                .entry(id)
                .or_insert_with(|| format!("managed:{}", record.version));
        })
    })
    .await
    .context("saving the install")??;
    let _ = tokio::fs::remove_file(&file).await;
    for (extra, _) in &extras {
        let _ = tokio::fs::remove_file(extra).await;
    }
    Ok(Some(installed))
}

/// Delete a managed version and forget it. A default pointing at it falls
/// back to the newest remaining version.
pub async fn remove(engine: &str, version: &str, app: &AppSettings) -> Result<()> {
    let state = state::load();
    let Some(m) = state
        .managed_versions(engine)
        .iter()
        .find(|m| m.version == version)
        .cloned()
    else {
        return Ok(());
    };
    let root = crate::dirs::engine(engine);
    if !m.dir.starts_with(&root) {
        bail!("{} is not an engine folder Irori manages", m.dir.display());
    }
    if m.dir.exists() {
        irori_core::fs_guard::ensure_deletable(&m.dir, app)?;
        tokio::fs::remove_dir_all(&m.dir)
            .await
            .with_context(|| format!("Could not delete {}", m.dir.display()))?;
    }
    let (engine, version) = (engine.to_string(), version.to_string());
    tokio::task::spawn_blocking(move || {
        state::update(|s| {
            let list = s.managed.entry(engine.clone()).or_default();
            list.retain(|m| m.version != version);
            let newest = list
                .iter()
                .max_by(|a, b| crate::releases::compare_versions(&a.version, &b.version))
                .map(|m| format!("managed:{}", m.version));
            if s.preferred.get(&engine) == Some(&format!("managed:{version}")) {
                match newest {
                    Some(n) => {
                        s.preferred.insert(engine, n);
                    }
                    None => {
                        s.preferred.remove(&engine);
                    }
                }
            }
        })
    })
    .await
    .context("saving the removal")??;
    Ok(())
}

/// Make `installation` (an [`Installation::id`](crate::Installation::id))
/// the default of `engine`.
pub fn set_preferred(engine: &str, installation: &str) -> Result<()> {
    state::update(|s| {
        s.preferred
            .insert(engine.to_string(), installation.to_string());
    })
}

/// Use the program at `path` for `engine` (or stop, with `None`).
pub fn set_custom(engine: &str, path: Option<&Path>) -> Result<()> {
    state::update(|s| match path {
        Some(p) => {
            s.custom.insert(engine.to_string(), p.to_path_buf());
        }
        None => {
            s.custom.remove(engine);
            if s.preferred.get(engine).map(String::as_str) == Some("custom") {
                s.preferred.remove(engine);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sha256sums() {
        let text = "aa  other.zip\n".to_string() + &"b".repeat(64) + " *DuckStation-x64.AppImage\n";
        assert_eq!(
            parse_sha256sums(&text, "DuckStation-x64.AppImage").unwrap(),
            "b".repeat(64)
        );
        assert!(parse_sha256sums(&text, "missing").is_none());
    }

    #[test]
    fn sanitizes_versions_for_folders() {
        assert_eq!(sanitize("1.21.0"), "1.21.0");
        assert_eq!(sanitize("../x y"), ".._x_y");
    }

    #[tokio::test]
    async fn installs_a_release_from_a_server_and_removes_it() {
        let root = crate::test_root("install");
        let zip = {
            use std::io::Write as _;
            let mut out = std::io::Cursor::new(Vec::new());
            let mut z = zip::ZipWriter::new(&mut out);
            let o = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
            for name in ["emu/emu", "emu/emu.exe"] {
                z.start_file(name, o).unwrap();
                z.write_all(b"bin").unwrap();
            }
            z.finish().unwrap();
            out.into_inner()
        };
        let server = irori_depot::test_server::TestServer::start(move |req| {
            irori_depot::test_server::serve_bytes(req, &zip, true)
        });
        let manifest = EngineManifest::parse(
            &serde_json::json!({
                "id": "emu", "name": "Emu", "license": "MIT", "systems": ["nes"], "sources": [],
                "binaries": {"linux": ["emu"], "macos": ["emu"], "windows": ["emu.exe"]}
            })
            .to_string(),
        )
        .unwrap();
        let release = Release {
            version: "1.0".into(),
            date: None,
            url: server.url("/emu.zip"),
            file_name: "emu.zip".into(),
            size: None,
            checksum_url: None,
            extras: Vec::new(),
        };
        let installed = install(&manifest, &release, &OpContext::detached())
            .await
            .unwrap()
            .unwrap();
        assert!(
            installed
                .program
                .ends_with(if cfg!(windows) { "emu.exe" } else { "emu" })
        );
        assert!(installed.program.exists());
        let s = state::load();
        assert_eq!(s.preferred["emu"], "managed:1.0");

        let app = AppSettings {
            default_install_path: root.join("games").display().to_string(),
            ..Default::default()
        };
        remove("emu", "1.0", &app).await.unwrap();
        assert!(!installed.dir.exists());
        let s = state::load();
        assert!(s.managed_versions("emu").is_empty());
        assert!(!s.preferred.contains_key("emu"));
    }

    #[tokio::test]
    async fn installs_extra_files_beside_the_program() {
        let _root = crate::test_root("install-extras");
        let libs = {
            use std::io::Write as _;
            let mut out = std::io::Cursor::new(Vec::new());
            let mut z = zip::ZipWriter::new(&mut out);
            z.start_file(
                "libs/libkernel.prx",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            z.write_all(b"prx").unwrap();
            z.finish().unwrap();
            out.into_inner()
        };
        let server = irori_depot::test_server::TestServer::start(move |req| {
            if req.path.starts_with("/libs.zip") {
                irori_depot::test_server::serve_bytes(req, &libs, true)
            } else {
                irori_depot::test_server::serve_bytes(req, b"tool", true)
            }
        });
        let manifest = EngineManifest::parse(
            &serde_json::json!({
                "id": "tool", "name": "Tool", "license": "MIT", "systems": ["ps5"], "sources": [],
                "binaries": {"linux": ["tool-v*"], "macos": ["tool-v*"], "windows": ["tool-v*"]}
            })
            .to_string(),
        )
        .unwrap();
        let release = Release {
            version: "0.1".into(),
            date: None,
            url: server.url("/tool-v0.1"),
            file_name: "tool-v0.1".into(),
            size: None,
            checksum_url: None,
            extras: vec![ReleaseFile {
                url: server.url("/libs.zip"),
                file_name: "libs.zip".into(),
                size: None,
            }],
        };
        let installed = install(&manifest, &release, &OpContext::detached())
            .await
            .unwrap()
            .unwrap();
        assert!(installed.program.ends_with("tool-v0.1"));
        assert_eq!(
            std::fs::read(installed.dir.join("libs/libkernel.prx")).unwrap(),
            b"prx"
        );
    }
}
