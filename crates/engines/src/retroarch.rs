//! RetroArch specifics: libretro cores from the libretro buildbot (the same
//! files RetroArch's own online updater fetches), and the settings file
//! Irori hands RetroArch so BIOS files, saves and cores are found without the
//! user opening RetroArch's menus.
//!
//! Cores live in Irori's folder, whichever RetroArch install runs them, so a
//! Flatpak or system RetroArch plays with the same cores as a managed one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use irori_core::{OpContext, json_store};
use irori_depot::{FileEntry, Finished, Job, Options};
use serde_json::Value;

use crate::host::{Host, Os};
use crate::state::{self, CoreInstall};

const BUILDBOT: &str = "https://buildbot.libretro.com/nightly";
/// Cores are rebuilt nightly, so a core counts as outdated only when its build
/// is this many days behind; otherwise there would be an update every day.
const CORE_UPDATE_DAYS: i64 = 14;

/// The buildbot folder of this host's cores and their file extension.
fn platform_dir(host: Host) -> Option<(&'static str, &'static str)> {
    Some(match (host.os, host.arch) {
        (Os::Linux, "x86_64") => ("linux/x86_64/latest", "so"),
        (Os::Linux, "aarch64") => ("linux/aarch64/latest", "so"),
        (Os::Macos, "aarch64") => ("apple/osx/arm64/latest", "dylib"),
        (Os::Macos, "x86_64") => ("apple/osx/x86_64/latest", "dylib"),
        (Os::Windows, "x86_64") => ("windows/x86_64/latest", "dll"),
        _ => return None,
    })
}

/// Where cores are kept.
pub fn cores_dir() -> PathBuf {
    crate::dirs::engine("retroarch").join("cores")
}

/// Where Irori keeps RetroArch's settings, saves and states.
pub fn home() -> PathBuf {
    crate::dirs::home("retroarch")
}

/// The file name of `core` on this host (`mgba_libretro.so`).
pub fn core_file(core: &str) -> Option<String> {
    let (_, ext) = platform_dir(Host::current())?;
    Some(format!("{core}_libretro.{ext}"))
}

/// The installed library of `core`, if any.
pub fn installed_core(core: &str) -> Option<PathBuf> {
    let s = state::load();
    s.cores
        .get(core)
        .map(|c| c.path.clone())
        .filter(|p| p.exists())
        .or_else(|| {
            let p = cores_dir().join(core_file(core)?);
            p.exists().then_some(p)
        })
}

/// One line of the buildbot's `.index-extended`: date, the CRC-32 of the
/// library inside the zip, and the zip's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub date: String,
    pub crc32: String,
    pub file: String,
}

pub fn parse_index(text: &str) -> Vec<IndexEntry> {
    text.lines()
        .filter_map(|l| {
            let mut p = l.split_whitespace();
            Some(IndexEntry {
                date: p.next()?.to_string(),
                crc32: p.next()?.to_ascii_lowercase(),
                file: p.next()?.to_string(),
            })
        })
        .collect()
}

/// Installed cores whose buildbot build is well ahead of theirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreUpdates {
    pub cores: Vec<String>,
    /// The oldest installed build among them.
    pub oldest: String,
    /// The newest build on offer.
    pub newest: String,
}

/// Days of a `YYYY-MM-DD` date, close enough to compare builds weeks apart.
fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.split('-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (parts.next()??, parts.next()??, parts.next()??);
    Some(year * 365 + month * 30 + day)
}

/// What [`CoreUpdates`] holds for `installed` (core id and build date) against
/// the buildbot's `index`. A core with no recorded date counts as outdated.
fn outdated_cores(
    index: &[IndexEntry],
    installed: &[(String, String)],
    ext: &str,
) -> Option<CoreUpdates> {
    let mut cores = Vec::new();
    let (mut oldest, mut newest) = (String::new(), String::new());
    for (core, date) in installed {
        let file = format!("{core}_libretro.{ext}.zip");
        let Some(entry) = index.iter().find(|e| e.file == file) else {
            continue;
        };
        let behind = match (day_number(date), day_number(&entry.date)) {
            (Some(have), Some(offered)) => offered - have >= CORE_UPDATE_DAYS,
            (None, Some(_)) => true,
            _ => false,
        };
        if behind {
            cores.push(core.clone());
            if oldest.is_empty() || date < &oldest {
                oldest.clone_from(date);
            }
            if entry.date > newest {
                newest.clone_from(&entry.date);
            }
        }
    }
    (!cores.is_empty()).then_some(CoreUpdates {
        cores,
        oldest,
        newest,
    })
}

/// Look for newer builds of the `installed` cores.
pub async fn check_core_updates(
    installed: &[(String, CoreInstall)],
) -> Result<Option<CoreUpdates>> {
    let Some((dir, ext)) = platform_dir(Host::current()) else {
        return Ok(None);
    };
    if installed.is_empty() {
        return Ok(None);
    }
    let index = fetch_index(&format!("{BUILDBOT}/{dir}")).await?;
    let dates: Vec<(String, String)> = installed
        .iter()
        .map(|(core, i)| (core.clone(), i.date.clone()))
        .collect();
    Ok(outdated_cores(&index, &dates, ext))
}

async fn fetch_index(base: &str) -> Result<Vec<IndexEntry>> {
    let text = irori_core::http::client()
        .get(format!("{base}/.index-extended"))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .context("Could not reach the libretro buildbot")?
        .text()
        .await?;
    Ok(parse_index(&text))
}

/// Download `core` (verified against the buildbot's CRC-32) into
/// [`cores_dir`]. Returns `None` when cancelled.
pub async fn install_core(core: &str, ctx: &OpContext) -> Result<Option<CoreInstall>> {
    let host = Host::current();
    let Some((dir, _)) = platform_dir(host) else {
        bail!("libretro has no cores for {host}");
    };
    let file = core_file(core).context("no cores for this computer")?;
    let zip_name = format!("{file}.zip");
    let base = format!("{BUILDBOT}/{dir}");
    let entry = fetch_index(&base)
        .await?
        .into_iter()
        .find(|e| e.file == zip_name)
        .with_context(|| format!("The libretro buildbot has no {core} core for this computer"))?;
    install_core_from(core, &format!("{base}/{zip_name}"), &entry, ctx).await
}

pub(crate) async fn install_core_from(
    core: &str,
    url: &str,
    entry: &IndexEntry,
    ctx: &OpContext,
) -> Result<Option<CoreInstall>> {
    let size = crate::install::content_length(url).await?;
    let downloads = crate::dirs::cache().join("downloads").join("cores");
    let progress = ctx.progress.clone();
    let finished = irori_depot::download(
        irori_core::http::download_client(),
        Job {
            root: &downloads,
            files: &[FileEntry::whole(
                entry.file.clone(),
                size,
                // The index's CRC-32 is of the library inside the zip;
                // it is checked once unpacked.
                None,
                vec![url.to_string()],
            )],
            remove: &[],
        },
        &Options::default(),
        std::sync::Arc::new(move |p: irori_depot::Progress| {
            progress(irori_core::InstallProgress {
                percent: Some(p.percent),
                bytes_done: Some(p.bytes_done),
                bytes_total: Some(p.bytes_total),
                download_speed: Some(p.download_speed),
                ..Default::default()
            })
        }),
        &ctx.cancel,
    )
    .await
    .with_context(|| format!("Could not download the {core} core"))?;
    if finished == Finished::Cancelled {
        return Ok(None);
    }
    let zip = downloads.join(&entry.file);
    let dest = cores_dir();
    let (zip2, dest2, cancel) = (zip.clone(), dest.clone(), ctx.cancel.clone());
    let staging = dest.join(format!(".{core}.partial"));
    let staging2 = staging.clone();
    tokio::task::spawn_blocking(move || {
        let _ = std::fs::remove_dir_all(&staging2);
        crate::archive::unpack(&zip2, "core.zip", &staging2, &cancel)?;
        std::fs::create_dir_all(&dest2)?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("unpacking stopped")??;
    let name = core_file(core).context("no cores for this computer")?;
    let unpacked = crate::discovery::locate_program(&staging, std::slice::from_ref(&name))
        .with_context(|| format!("The {core} download holds no {name}"))?;
    let actual = {
        let file = unpacked.clone();
        tokio::task::spawn_blocking(move || std::fs::read(&file).map(|b| crc32fast::hash(&b)))
            .await
            .context("checking stopped")??
    };
    if format!("{actual:08x}") != entry.crc32 {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        let _ = tokio::fs::remove_file(&zip).await;
        anyhow::bail!("The {core} core is damaged (its checksum does not match); try again");
    }
    let path = dest.join(&name);
    tokio::fs::rename(&unpacked, &path)
        .await
        .with_context(|| format!("saving {}", path.display()))?;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    let _ = tokio::fs::remove_file(&zip).await;
    let installed = CoreInstall {
        size: tokio::fs::metadata(&path)
            .await
            .map(|m| m.len())
            .unwrap_or(0),
        path,
        date: entry.date.clone(),
    };
    let (core, record) = (core.to_string(), installed.clone());
    tokio::task::spawn_blocking(move || state::update(|s| s.cores.insert(core, record)))
        .await
        .context("saving the core")??;
    Ok(Some(installed))
}

/// Delete an installed core.
pub async fn remove_core(core: &str) -> Result<()> {
    if let Some(path) = installed_core(core)
        && path.starts_with(cores_dir())
    {
        tokio::fs::remove_file(&path)
            .await
            .with_context(|| format!("Could not delete {}", path.display()))?;
    }
    let core = core.to_string();
    tokio::task::spawn_blocking(move || state::update(|s| s.cores.remove(&core)))
        .await
        .context("saving the removal")??;
    Ok(())
}

/// RetroArch settings Irori manages, from the engine's option values.
pub fn managed_settings(values: &BTreeMap<String, Value>) -> Vec<(&'static str, String)> {
    let flag = |id: &str, default: bool| {
        values
            .get(id)
            .and_then(Value::as_bool)
            .unwrap_or(default)
            .to_string()
    };
    let home = home();
    let mut out = vec![
        (
            "system_directory",
            crate::dirs::system().display().to_string(),
        ),
        ("libretro_directory", cores_dir().display().to_string()),
        (
            "savefile_directory",
            home.join("saves").display().to_string(),
        ),
        (
            "savestate_directory",
            home.join("states").display().to_string(),
        ),
        (
            "screenshot_directory",
            home.join("screenshots").display().to_string(),
        ),
        ("video_fullscreen", flag("fullscreen", true)),
        ("video_vsync", flag("vsync", true)),
        ("fps_show", flag("show_fps", false)),
        ("pause_nonactive", flag("pause_in_background", true)),
        // Start + Select opens the menu on any controller; Escape twice quits.
        ("input_menu_toggle_gamepad_combo", "4".to_string()),
        ("quit_press_twice", "true".to_string()),
        ("config_save_on_exit", "true".to_string()),
    ];
    if let Some(driver) = values.get("video_driver").and_then(Value::as_str)
        && !driver.is_empty()
    {
        out.push(("video_driver", driver.to_string()));
    }
    out
}

/// Set `settings` in the RetroArch config at `path`, keeping every other
/// line (RetroArch saves its own changes there on exit).
pub fn write_config(path: &Path, settings: &[(&str, String)]) -> Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    for (key, value) in settings {
        let line = format!("{key} = \"{}\"", value.replace('"', ""));
        let found = lines
            .iter_mut()
            .find(|l| l.split_once('=').is_some_and(|(k, _)| k.trim() == *key));
        match found {
            Some(l) => *l = line,
            None => lines.push(line),
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    json_store::write_bytes(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cores_well_behind_are_outdated() {
        let index = parse_index(
            "2026-10-30 aaaa mgba_libretro.so.zip\n\
             2026-10-30 bbbb snes9x_libretro.so.zip\n\
             2026-10-30 cccc gambatte_libretro.so.zip\n",
        );
        let installed = [
            ("mgba".to_string(), "2026-10-25".to_string()),
            ("snes9x".to_string(), "2026-06-01".to_string()),
            ("gambatte".to_string(), String::new()),
            ("gone".to_string(), "2020-01-01".to_string()),
        ];
        let updates = outdated_cores(&index, &installed, "so").unwrap();
        assert_eq!(updates.cores, ["snes9x", "gambatte"]);
        assert_eq!(updates.newest, "2026-10-30");
        assert!(outdated_cores(&index, &installed[..1], "so").is_none());
    }

    #[test]
    fn parses_the_buildbot_index() {
        let entries = parse_index("2025-10-01 0a1b2c3d mgba_libretro.so.zip\n\nbad\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].crc32, "0a1b2c3d");
        assert_eq!(entries[0].file, "mgba_libretro.so.zip");
    }

    #[test]
    fn config_keeps_the_users_lines() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("retroarch.cfg");
        std::fs::write(
            &cfg,
            "menu_driver = \"ozone\"\nvideo_fullscreen = \"false\"\n",
        )
        .unwrap();
        write_config(
            &cfg,
            &[
                ("video_fullscreen", "true".into()),
                ("fps_show", "false".into()),
            ],
        )
        .unwrap();
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert_eq!(
            text,
            "menu_driver = \"ozone\"\nvideo_fullscreen = \"true\"\nfps_show = \"false\"\n"
        );
    }

    #[tokio::test]
    async fn installs_a_core_checked_against_its_crc() {
        let _root = crate::test_root("cores");
        let Some(name) = core_file("testcore") else {
            return;
        };
        let zip = {
            use std::io::Write as _;
            let mut out = std::io::Cursor::new(Vec::new());
            let mut z = zip::ZipWriter::new(&mut out);
            z.start_file(name.as_str(), zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"core").unwrap();
            z.finish().unwrap();
            out.into_inner()
        };
        let crc = format!("{:08x}", crc32fast::hash(b"core"));
        let served = zip.clone();
        let server = irori_depot::test_server::TestServer::start(move |req| {
            irori_depot::test_server::serve_bytes(req, &served, true)
        });
        let entry = IndexEntry {
            date: "2025-10-01".into(),
            crc32: crc,
            file: format!("{name}.zip"),
        };
        let core = install_core_from(
            "testcore",
            &server.url("/core.zip"),
            &entry,
            &OpContext::detached(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(std::fs::read(&core.path).unwrap(), b"core");
        assert_eq!(installed_core("testcore").unwrap(), core.path);
        remove_core("testcore").await.unwrap();
        assert!(installed_core("testcore").is_none());

        // A damaged download fails instead of installing.
        let bad = IndexEntry {
            crc32: "00000000".into(),
            ..entry
        };
        assert!(
            install_core_from(
                "testcore",
                &server.url("/core.zip"),
                &bad,
                &OpContext::detached()
            )
            .await
            .is_err()
        );
    }
}
