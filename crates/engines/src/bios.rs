//! The BIOS folder: an index of the user's files, built in the background
//! and cached by path, size and modification time so nothing is hashed
//! twice, and per-system status from it. Before a launch, the files a system
//! needs are linked under the names engines expect.
//!
//! Ember never downloads BIOS files; it only recognises the user's own.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context as _, Result};
use ember_core::json_store;
use md5::{Digest as _, Md5};
use serde::{Deserialize, Serialize};

use crate::systems::{self, BiosRule, System};

/// Files larger than this are not BIOS images (the largest are a few MiB).
const MAX_BIOS_SIZE: u64 = 32 << 20;
/// Folder levels searched below the BIOS folder.
const MAX_DEPTH: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedFile {
    pub path: PathBuf,
    pub size: u64,
    /// Modification time, Unix seconds.
    pub mtime: u64,
    /// Lower-case MD5.
    pub md5: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BiosIndex {
    /// The folder this index is of.
    pub root: PathBuf,
    pub files: Vec<IndexedFile>,
}

fn index_file() -> PathBuf {
    crate::dirs::cache().join("bios-index.json")
}

/// The saved index (empty when there is none).
pub fn cached_index() -> BiosIndex {
    json_store::read_or_default(&index_file())
}

/// File names Ember knows as BIOS files of some system, lower case.
fn known_names() -> Vec<String> {
    systems::all()
        .iter()
        .flat_map(|s| s.bios.files.iter().map(|f| f.name.to_lowercase()))
        .collect()
}

/// Index `root`, reusing digests from `previous` for unchanged files, and
/// save the result. Only plausible BIOS files are hashed: known names, or
/// `.bin`/`.rom`/`.img` files of BIOS size. Blocking.
pub fn scan(root: &Path, previous: &BiosIndex) -> Result<BiosIndex> {
    let names = known_names();
    let mut reuse: BTreeMap<&Path, &IndexedFile> = BTreeMap::new();
    for f in &previous.files {
        reuse.insert(f.path.as_path(), f);
    }
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let size = meta.len();
            if size == 0 || size > MAX_BIOS_SIZE {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let plausible =
                names.contains(&name) || [".bin", ".rom", ".img"].iter().any(|e| name.ends_with(e));
            if !plausible {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let md5 = match reuse.get(path.as_path()) {
                Some(f) if f.size == size && f.mtime == mtime => f.md5.clone(),
                _ => match md5_file(&path) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::debug!("hashing {}: {e:#}", path.display());
                        continue;
                    }
                },
            };
            files.push(IndexedFile {
                path,
                size,
                mtime,
                md5,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let index = BiosIndex {
        root: root.to_path_buf(),
        files,
    };
    json_store::write(&index_file(), &index).context("saving the BIOS index")?;
    Ok(index)
}

fn md5_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// How one expected BIOS file stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileState {
    /// Found, and a known-good dump (or any dump works).
    Ready,
    /// A file of that name was found but it is not a dump Ember knows; it
    /// may not work.
    Unrecognized,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStatus {
    pub name: String,
    pub region: Option<String>,
    pub state: FileState,
    /// The user's file that provides it.
    pub found: Option<PathBuf>,
}

/// How a system's BIOS stands as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SystemState {
    /// The system needs no BIOS, or has every file it needs.
    Ready,
    /// Files are there but not all are known-good dumps.
    Unrecognized,
    /// A required file is missing; games won't start.
    Missing,
    /// Optional files are missing; games start, some features need them.
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemBios {
    pub system: String,
    pub state: SystemState,
    pub files: Vec<FileStatus>,
}

/// BIOS status of `system` from `index`. A file is found by its digest
/// whatever its name, else by its name.
pub fn status(system: &System, index: &BiosIndex) -> SystemBios {
    let files: Vec<FileStatus> = system
        .bios
        .files
        .iter()
        .map(|want| {
            let by_digest = index.files.iter().find(|f| want.md5.contains(&f.md5));
            let by_name = || {
                index.files.iter().find(|f| {
                    f.path
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&want.name))
                })
            };
            let (state, found) = match (by_digest, by_name()) {
                (Some(f), _) => (FileState::Ready, Some(f.path.clone())),
                (None, Some(f)) if want.md5.is_empty() => (FileState::Ready, Some(f.path.clone())),
                (None, Some(f)) => (FileState::Unrecognized, Some(f.path.clone())),
                (None, None) => (FileState::Missing, None),
            };
            FileStatus {
                name: want.name.clone(),
                region: want.region.clone(),
                state,
                found,
            }
        })
        .collect();
    let ready = files.iter().filter(|f| f.state == FileState::Ready).count();
    let present = files.iter().filter(|f| f.found.is_some()).count();
    let state = if files.is_empty() {
        SystemState::Ready
    } else {
        match system.bios.rule {
            BiosRule::AnyOf if ready > 0 => SystemState::Ready,
            BiosRule::AnyOf if present > 0 => SystemState::Unrecognized,
            BiosRule::AnyOf => SystemState::Missing,
            BiosRule::All if ready == files.len() => SystemState::Ready,
            BiosRule::All if present == files.len() => SystemState::Unrecognized,
            BiosRule::All => SystemState::Missing,
            BiosRule::Optional if present == files.len() => SystemState::Ready,
            BiosRule::Optional => SystemState::Optional,
        }
    };
    SystemBios {
        system: system.id.clone(),
        state,
        files,
    }
}

/// Link every found file of `system` into `target` under its expected
/// name, replacing older links. Returns how many were linked. Blocking.
pub fn link_into(system: &System, index: &BiosIndex, target: &Path) -> Result<usize> {
    let mut linked = 0;
    for file in status(system, index).files {
        let Some(found) = file.found else { continue };
        std::fs::create_dir_all(target)
            .with_context(|| format!("creating {}", target.display()))?;
        let dest = target.join(&file.name);
        if dest == found {
            continue;
        }
        if let Ok(meta) = std::fs::symlink_metadata(&dest) {
            if !meta.file_type().is_symlink() {
                // Without symbolic links (Windows) Ember places copies; one of
                // the same size is already in place. Any other real file is
                // the user's and is never replaced.
                if cfg!(not(unix)) && std::fs::metadata(&found).is_ok_and(|f| f.len() == meta.len())
                {
                    linked += 1;
                }
                continue;
            }
            let _ = std::fs::remove_file(&dest);
        }
        link(&found, &dest).with_context(|| format!("linking {}", dest.display()))?;
        linked += 1;
    }
    Ok(linked)
}

#[cfg(unix)]
fn link(from: &Path, to: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(from, to)
}

#[cfg(not(unix))]
fn link(from: &Path, to: &Path) -> std::io::Result<()> {
    // Symbolic links need developer mode on Windows; BIOS files are small.
    std::fs::hard_link(from, to).or_else(|_| std::fs::copy(from, to).map(|_| ()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn psx() -> &'static System {
        systems::get("psx").unwrap()
    }

    #[test]
    fn scans_hashes_once_and_reports_status() {
        let root = crate::test_root("bios");
        let bios = root.join("bios");
        std::fs::create_dir_all(bios.join("sony")).unwrap();
        // A file with a known name but unknown content.
        std::fs::write(bios.join("sony/SCPH5501.BIN"), b"not a real bios").unwrap();
        std::fs::write(bios.join("notes.txt"), b"ignored").unwrap();
        let index = scan(&bios, &BiosIndex::default()).unwrap();
        assert_eq!(index.files.len(), 1);
        let s = status(psx(), &index);
        assert_eq!(s.state, SystemState::Unrecognized);
        assert_eq!(s.files[0].state, FileState::Unrecognized);

        // A known digest is recognised whatever the file is called.
        let mut index = index;
        index.files.push(IndexedFile {
            path: bios.join("renamed.bin"),
            size: 524288,
            mtime: 0,
            md5: "8dd7d5296a650fac7319bce665a6a53c".into(),
        });
        let s = status(psx(), &index);
        assert_eq!(s.state, SystemState::Ready);
        let jp = s.files.iter().find(|f| f.name == "scph5500.bin").unwrap();
        assert_eq!(jp.state, FileState::Ready);

        // Unchanged files keep their digest without hashing.
        let mut previous = cached_index();
        previous.files[0].md5 = "cafe".into();
        let again = scan(&bios, &previous).unwrap();
        assert_eq!(again.files[0].md5, "cafe");
    }

    #[test]
    fn systems_without_files_are_missing_or_optional() {
        let empty = BiosIndex::default();
        assert_eq!(status(psx(), &empty).state, SystemState::Missing);
        let gba = systems::get("gba").unwrap();
        assert_eq!(status(gba, &empty).state, SystemState::Optional);
        let snes = systems::get("snes").unwrap();
        assert_eq!(status(snes, &empty).state, SystemState::Ready);
    }

    #[test]
    fn links_found_files_under_expected_names() {
        let dir = tempfile::tempdir().unwrap();
        let mine = dir.path().join("my-gba.bin");
        std::fs::write(&mine, b"x").unwrap();
        let index = BiosIndex {
            root: dir.path().into(),
            files: vec![IndexedFile {
                path: mine.clone(),
                size: 1,
                mtime: 0,
                md5: "a860e8c0b6d573d191e4ec7db1b1e4f6".into(),
            }],
        };
        let target = dir.path().join("system");
        let gba = systems::get("gba").unwrap();
        assert_eq!(link_into(gba, &index, &target).unwrap(), 1);
        assert_eq!(std::fs::read(target.join("gba_bios.bin")).unwrap(), b"x");
        // Linking again replaces the link.
        assert_eq!(link_into(gba, &index, &target).unwrap(), 1);
    }
}
