//! The systems catalog (`assets/engines/systems.json`): every console or
//! platform Ember plays, the files that hold its games, how to recognise them
//! and the BIOS files it needs.
//!
//! Detecting a game file's system uses, in order: the folder it sits in
//! (`…/SNES/…`), its extension when only one system claims it, and the
//! file's header when several do (`.iso`, `.bin`).

use std::io::Read as _;
use std::path::Path;

use anyhow::{Context as _, Result};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

/// Bytes a header check may look at: enough for every rule in the catalog.
const HEADER_LEN: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderRule {
    pub offset: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ascii: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hex: Option<String>,
}

impl HeaderRule {
    fn bytes(&self) -> Vec<u8> {
        match (&self.ascii, &self.hex) {
            (Some(a), _) => a.as_bytes().to_vec(),
            (None, Some(h)) => hex::decode(h).unwrap_or_default(),
            (None, None) => Vec::new(),
        }
    }

    pub fn matches(&self, header: &[u8]) -> bool {
        let want = self.bytes();
        !want.is_empty()
            && header
                .get(self.offset..self.offset + want.len())
                .is_some_and(|got| got == want.as_slice())
    }
}

/// How many of a system's BIOS files it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BiosRule {
    /// Games play without them; some features need them.
    #[default]
    Optional,
    /// One of the files (usually one per region) is enough.
    AnyOf,
    /// Every file is needed.
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BiosFile {
    /// The file name engines expect.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Known-good MD5 digests, lower case. Empty when any dump works.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub md5: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BiosSpec {
    #[serde(default)]
    pub rule: BiosRule,
    #[serde(default)]
    pub files: Vec<BiosFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct System {
    pub id: String,
    pub name: String,
    pub short: String,
    pub maker: String,
    pub year: u16,
    /// Background and accent colours of the generated cover.
    pub colors: [String; 2],
    /// Game file extensions, lower case, without the dot.
    pub extensions: Vec<String>,
    /// Folder names (lower case) that hold this system's games.
    pub folders: Vec<String>,
    #[serde(default)]
    pub headers: Vec<HeaderRule>,
    #[serde(default)]
    pub bios: BiosSpec,
    /// Engines that play it, best first (`engine` or `retroarch:<core>`).
    pub engines: Vec<String>,
    /// Games come on discs: `.cue` sheets own their `.bin` tracks.
    #[serde(default)]
    pub disc_images: bool,
    /// Games may span several discs, grouped into one entry.
    #[serde(default)]
    pub multi_disc: bool,
    /// Each game is a folder (DOS, ScummVM) rather than one file.
    #[serde(default)]
    pub folder_games: bool,
}

impl System {
    pub fn has_extension(&self, ext: &str) -> bool {
        self.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
    }

    pub fn needs_bios(&self) -> bool {
        self.bios.rule != BiosRule::Optional && !self.bios.files.is_empty()
    }
}

#[derive(Debug, Deserialize)]
struct Catalog {
    systems: Vec<System>,
}

static SYSTEMS: Lazy<Vec<System>> = Lazy::new(|| {
    serde_json::from_str::<Catalog>(include_str!("../../../assets/engines/systems.json"))
        .expect("the bundled systems catalog parses")
        .systems
});

/// Every system, in catalog order.
pub fn all() -> &'static [System] {
    &SYSTEMS
}

pub fn get(id: &str) -> Option<&'static System> {
    SYSTEMS.iter().find(|s| s.id == id)
}

/// The system whose folder name `name` is, if any.
pub fn by_folder(name: &str) -> Option<&'static System> {
    let name = normalize_folder(name);
    SYSTEMS
        .iter()
        .find(|s| s.id == name || s.folders.contains(&name))
}

/// Lower case, with `_` and `.` read as spaces and runs of spaces collapsed.
fn normalize_folder(name: &str) -> String {
    name.to_lowercase()
        .replace(['_', '.'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Systems claiming the extension `ext`.
pub fn by_extension(ext: &str) -> Vec<&'static System> {
    SYSTEMS.iter().filter(|s| s.has_extension(ext)).collect()
}

/// The system of the game file at `path`. `folder_hint` is a system already
/// known from a parent folder; it wins when it accepts the extension.
pub fn detect(path: &Path, folder_hint: Option<&'static System>) -> Option<&'static System> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if let Some(hint) = folder_hint
        && (hint.has_extension(&ext) || matches!(ext.as_str(), "zip" | "7z"))
    {
        return Some(hint);
    }
    let candidates = by_extension(&ext);
    match candidates.as_slice() {
        [] => None,
        [one] if one.headers.is_empty() || !needs_sniff(&ext) => Some(one),
        _ => {
            let header = read_header(path).ok()?;
            candidates
                .iter()
                .copied()
                .find(|s| s.headers.iter().any(|r| r.matches(&header)))
                .or_else(|| {
                    // A single claimant without a matching header is still
                    // the best guess.
                    (candidates.len() == 1).then(|| candidates[0])
                })
        }
    }
}

/// Generic extensions that many systems share; their header decides.
fn needs_sniff(ext: &str) -> bool {
    matches!(ext, "iso" | "bin" | "img" | "rvz" | "gcz")
}

fn read_header(path: &Path) -> Result<Vec<u8>> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut buf = vec![0u8; HEADER_LEN];
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_consistent() {
        let ids: Vec<&str> = all().iter().map(|s| s.id.as_str()).collect();
        for (i, id) in ids.iter().enumerate() {
            assert!(!ids[..i].contains(id), "duplicate system {id}");
        }
        for s in all() {
            assert!(!s.engines.is_empty(), "{} has no engine", s.id);
            assert!(!s.extensions.is_empty(), "{} has no extensions", s.id);
            for ext in &s.extensions {
                assert_eq!(ext, &ext.to_ascii_lowercase());
            }
            for file in &s.bios.files {
                for md5 in &file.md5 {
                    assert_eq!(md5.len(), 32, "{}: {}", s.id, file.name);
                    assert_eq!(md5, &md5.to_ascii_lowercase());
                }
            }
            for rule in &s.headers {
                assert!(
                    !rule.bytes().is_empty(),
                    "{} has an empty header rule",
                    s.id
                );
            }
        }
    }

    #[test]
    fn folders_name_systems() {
        assert_eq!(by_folder("SNES").unwrap().id, "snes");
        assert_eq!(by_folder("Mega_Drive").unwrap().id, "genesis");
        assert_eq!(by_folder("PlayStation 1").unwrap().id, "psx");
        assert!(by_folder("Music").is_none());
    }

    #[test]
    fn detects_by_extension_folder_and_header() {
        let dir = tempfile::tempdir().unwrap();
        let sfc = dir.path().join("Chrono.sfc");
        std::fs::write(&sfc, b"x").unwrap();
        assert_eq!(detect(&sfc, None).unwrap().id, "snes");

        // `.iso` is claimed by several systems; the GameCube magic decides.
        let mut gc = vec![0u8; 64];
        gc[28..32].copy_from_slice(&[0xc2, 0x33, 0x9f, 0x3d]);
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &gc).unwrap();
        assert_eq!(detect(&iso, None).unwrap().id, "gamecube");

        // A folder hint wins for zips and its own extensions.
        let zip = dir.path().join("game.zip");
        std::fs::write(&zip, b"PK").unwrap();
        assert!(detect(&zip, None).is_none());
        assert_eq!(detect(&zip, by_folder("gba")).unwrap().id, "gba");
        let unknown_iso = dir.path().join("other.iso");
        std::fs::write(&unknown_iso, [0u8; 64]).unwrap();
        assert_eq!(detect(&unknown_iso, by_folder("psp")).unwrap().id, "psp");
        assert!(detect(&unknown_iso, None).is_none());
    }
}
