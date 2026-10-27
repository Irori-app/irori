//! EA app: installed games from each game folder's `__Installer/installerdata.xml`.
//!
//! The EA app (and Origin before it) writes a manifest into every game it
//! installs: its content ids (offer ids), its title per locale and its
//! runtime launchers (the game's programs, often written as
//! `[HKEY_…\Install Dir]Game\Launcher.exe`). Games live in one folder per
//! game under the library folders in [`GAME_DIRS`].
//!
//! Games launch through the client with
//! `origin2://game/launch?offerIds=<ids>&autoDownload=1`.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

use crate::{
    InstalledGame, LaunchSpec, Launcher, ScanReport, drive_c, folder_name, retain_present,
};

/// Library folders holding one folder per game, relative to `drive_c` (or
/// `C:\`): the EA app's default, then Origin's.
pub const GAME_DIRS: &[&str] = &["Program Files/EA Games", "Program Files (x86)/Origin Games"];

/// The manifest inside a game's folder.
pub const INSTALLER_DATA: &str = "__Installer/installerdata.xml";

/// The locale whose title Irori prefers when the manifest has several.
const PREFERRED_LOCALE: &str = "en_US";

/// How the EA app launches a game with these content ids.
pub fn launch_spec(content_ids: &[String]) -> LaunchSpec {
    LaunchSpec::Uri(format!(
        "origin2://game/launch?offerIds={}&autoDownload=1",
        content_ids.join(",")
    ))
}

/// The game an `installerdata.xml` describes; `install_dir` is the game's
/// folder as a Windows path.
pub fn parse_installer_data(bytes: &[u8], install_dir: PathBuf) -> anyhow::Result<InstalledGame> {
    let text = decode_text(bytes);
    let mut reader = Reader::from_str(&text);
    let mut stack: Vec<String> = Vec::new();
    let mut current_locale: Option<String> = None;
    let mut text_buf = String::new();

    let mut content_ids: Vec<String> = Vec::new();
    let mut titles: Vec<(Option<String>, String)> = Vec::new();
    // (file path, is trial) per runtime launcher.
    let mut launchers: Vec<(String, bool)> = Vec::new();
    let mut launcher_path = String::new();
    let mut launcher_trial = false;

    loop {
        let event = reader
            .read_event()
            .context("The EA game's installerdata.xml isn't valid XML")?;
        match event {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_string();
                if name == "gameTitle" {
                    current_locale = e.try_get_attribute("locale").ok().flatten().and_then(|a| {
                        a.normalized_value(XmlVersion::Implicit1_0)
                            .ok()
                            .map(|v| v.into_owned())
                    });
                }
                if name == "launcher" {
                    launcher_path.clear();
                    launcher_trial = false;
                }
                stack.push(name);
                text_buf.clear();
            }
            Event::Text(t) => text_buf.push_str(&t.xml10_content()),
            Event::CData(t) => text_buf.push_str(t.as_ref()),
            Event::GeneralRef(r) => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    text_buf.push(c);
                } else {
                    let entity = format!("&{};", r.xml10_content());
                    if let Ok(resolved) = quick_xml::escape::unescape(&entity) {
                        text_buf.push_str(&resolved);
                    }
                }
            }
            Event::End(_) => {
                let name = stack.pop().unwrap_or_default();
                let parent = stack.last().map(String::as_str).unwrap_or("");
                let value = text_buf.trim().to_string();
                match (parent, name.as_str()) {
                    ("contentIDs", "contentID") if !value.is_empty() => {
                        if !content_ids.contains(&value) {
                            content_ids.push(value);
                        }
                    }
                    ("gameTitles", "gameTitle") if !value.is_empty() => {
                        titles.push((current_locale.take(), value));
                    }
                    ("launcher", "filePath") => launcher_path = value,
                    ("launcher", "trial") => launcher_trial = value == "1",
                    (_, "launcher") if !launcher_path.is_empty() => {
                        launchers.push((std::mem::take(&mut launcher_path), launcher_trial));
                    }
                    _ => {}
                }
                text_buf.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }

    if content_ids.is_empty() {
        bail!("The EA game's installerdata.xml lists no content ids");
    }
    let title = titles
        .iter()
        .find(|(locale, _)| {
            locale
                .as_deref()
                .is_some_and(|l| l.eq_ignore_ascii_case(PREFERRED_LOCALE))
        })
        .or_else(|| titles.first())
        .map(|(_, t)| t.clone())
        .or_else(|| folder_name(&install_dir.to_string_lossy()).map(str::to_string))
        .unwrap_or_else(|| content_ids[0].clone());
    let executable = launchers
        .iter()
        .find(|(_, trial)| !trial)
        .or_else(|| launchers.first())
        .map(|(path, _)| strip_registry_placeholder(path));

    Ok(InstalledGame {
        launcher: Launcher::EaApp,
        id: content_ids[0].clone(),
        title,
        install_dir,
        executable,
        launch: launch_spec(&content_ids),
    })
}

/// Manifests are UTF-8, but old Origin ones may be UTF-16 with a byte-order mark.
fn decode_text(bytes: &[u8]) -> String {
    let utf16 = |bytes: &[u8], from: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| from(*c)).collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xff, 0xfe, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xfe, 0xff, rest @ ..] => utf16(rest, u16::from_be_bytes),
        [0xef, 0xbb, 0xbf, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// `[HKEY_…\Install Dir]Game\x.exe` → `Game\x.exe`: the registry value is the
/// game's own folder, which Irori already knows.
fn strip_registry_placeholder(path: &str) -> String {
    match path.strip_prefix('[').and_then(|rest| rest.split_once(']')) {
        Some((_, relative)) => relative.trim_start_matches(['\\', '/']).to_string(),
        None => path.to_string(),
    }
}

/// The EA app games installed in the Wine prefix at `prefix`.
pub fn scan_prefix(prefix: &Path) -> ScanReport {
    scan_drive(&drive_c(prefix))
}

/// The EA app games installed on `drive` (`drive_c`, or `C:\`).
pub fn scan_drive(drive: &Path) -> ScanReport {
    let mut report = ScanReport::default();
    for dir in GAME_DIRS {
        let Ok(entries) = std::fs::read_dir(drive.join(dir)) else {
            continue;
        };
        let mut folders: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
        folders.sort();
        for folder in folders {
            let manifest = drive.join(dir).join(&folder).join(INSTALLER_DATA);
            if !manifest.is_file() {
                continue;
            }
            let windows_dir = format!(
                "C:\\{}\\{}",
                dir.replace('/', "\\"),
                folder.to_string_lossy()
            );
            let result = std::fs::read(&manifest)
                .with_context(|| format!("Couldn't read {}", manifest.display()))
                .and_then(|bytes| parse_installer_data(&bytes, PathBuf::from(&windows_dir)))
                .with_context(|| format!("Skipped the EA game in {windows_dir}"));
            match result {
                Ok(game) if report.games.iter().any(|g| g.id == game.id) => {}
                Ok(game) => report.games.push(game),
                Err(e) => report.warnings.push(format!("{e:#}")),
            }
        }
    }
    retain_present(drive, &mut report);
    report
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<DiPManifest version="4.0">
  <buildMetaData>
    <featureFlags autoUpdateEnabled="1"/>
  </buildMetaData>
  <contentIDs>
    <contentID>71171</contentID>
    <contentID>1026023</contentID>
    <contentID>71171</contentID>
  </contentIDs>
  <gameTitles>
    <gameTitle locale="de_DE">Mass Effect™ Legendary Edition (DE)</gameTitle>
    <gameTitle locale="en_US">Mass Effect™ Legendary Edition &amp; More</gameTitle>
  </gameTitles>
  <runtime>
    <launcher uid="trial">
      <filePath>[HKEY_LOCAL_MACHINE\SOFTWARE\BioWare\Mass Effect Legendary Edition\Install Dir]Game\Trial.exe</filePath>
      <trial>1</trial>
    </launcher>
    <launcher uid="full">
      <filePath>[HKEY_LOCAL_MACHINE\SOFTWARE\BioWare\Mass Effect Legendary Edition\Install Dir]Game\Launcher\MassEffectLauncher.exe</filePath>
      <parameters></parameters>
      <name locale="en_US">Mass Effect Legendary Edition</name>
      <trial>0</trial>
    </launcher>
  </runtime>
</DiPManifest>
"#;

    #[test]
    fn parses_ids_title_and_launcher() {
        let dir = PathBuf::from(r"C:\Program Files\EA Games\Mass Effect Legendary Edition");
        let game = parse_installer_data(SAMPLE.as_bytes(), dir.clone()).expect("parse");
        assert_eq!(game.app_name(), "ea:71171");
        assert_eq!(game.title, "Mass Effect™ Legendary Edition & More");
        assert_eq!(game.install_dir, dir);
        assert_eq!(
            game.executable.as_deref(),
            Some(r"Game\Launcher\MassEffectLauncher.exe")
        );
        assert_eq!(
            game.launch,
            LaunchSpec::Uri("origin2://game/launch?offerIds=71171,1026023&autoDownload=1".into())
        );
    }

    #[test]
    fn reads_utf16_and_falls_back_to_the_folder_name() {
        let xml =
            "<DiPManifest><contentIDs><contentID>OFB-EAST:1</contentID></contentIDs></DiPManifest>";
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
        let game = parse_installer_data(
            &bytes,
            PathBuf::from(r"C:\Program Files\EA Games\Old Game\"),
        )
        .expect("parse");
        assert_eq!(game.id, "OFB-EAST:1");
        assert_eq!(game.title, "Old Game");
        assert_eq!(game.executable, None);
    }

    #[test]
    fn rejects_manifests_without_ids_or_broken_xml() {
        assert!(parse_installer_data(b"<DiPManifest/>", PathBuf::new()).is_err());
        assert!(parse_installer_data(b"<a><contentIDs></b>", PathBuf::new()).is_err());
    }
}
