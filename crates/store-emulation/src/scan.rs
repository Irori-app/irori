//! Scanning ROM folders into library entries. Blocking; run on the blocking
//! pool.
//!
//! A file's system comes from the folder it sits in, its extension, or its
//! header ([`ember_engines::systems::detect`]). Files a `.cue` sheet or an
//! `.m3u` playlist lists belong to that sheet; discs of one game become one
//! entry (with a playlist written for engines that switch discs). DOS and
//! ScummVM games are folders. Detection results are cached by path, size and
//! modification time, so a rescan of a 10,000-file library only lists
//! folders. Nothing here touches the network.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ember_engines::systems::{self, System};
use serde::{Deserialize, Serialize};
use sha1::{Digest as _, Sha1};

use crate::names::{self, RomName};

/// Folder levels searched below a ROM folder.
const MAX_DEPTH: usize = 8;

/// One game in the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomEntry {
    pub app_name: String,
    pub system: String,
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// What the engine opens: a ROM, a `.cue`/`.m3u`, a DOS program, or a
    /// game folder.
    pub path: PathBuf,
    /// The discs of a multi-disc game, in order (empty otherwise).
    #[serde(default)]
    pub discs: Vec<PathBuf>,
    /// Bytes on disk (all discs).
    #[serde(default)]
    pub size: u64,
    /// The file name the title came from, for thumbnails.
    #[serde(default)]
    pub stem: String,
    /// The ROM folder the game was found in.
    pub root: PathBuf,
}

/// What a file was detected as last time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub size: u64,
    pub mtime: u64,
    pub system: Option<String>,
}

pub type ScanCache = HashMap<PathBuf, Seen>;

#[derive(Debug, Default)]
pub struct Scanned {
    pub entries: Vec<RomEntry>,
    pub cache: ScanCache,
    /// Game-like files no system claims.
    pub unrecognized: usize,
}

/// A stable id for the game at `source` (its first disc or folder).
pub fn app_name_of(system: &str, source: &Path) -> String {
    let mut h = Sha1::new();
    h.update(system.as_bytes());
    h.update([0]);
    h.update(source.to_string_lossy().as_bytes());
    hex::encode(&h.finalize()[..8])
}

/// Scan `roots`, reusing `previous` detections. Playlists of multi-disc
/// games are written into `playlists`. `progress` is called with the number
/// of games found so far.
pub fn scan(
    roots: &[PathBuf],
    previous: &ScanCache,
    playlists: &Path,
    progress: &dyn Fn(usize),
) -> Scanned {
    let mut out = Scanned::default();
    for root in roots {
        let hint = root
            .file_name()
            .and_then(|n| systems::by_folder(&n.to_string_lossy()));
        let mut stack = vec![(root.clone(), hint, 0usize)];
        while let Some((dir, hint, depth)) = stack.pop() {
            scan_dir(
                &dir, root, hint, depth, previous, playlists, &mut out, &mut stack,
            );
            progress(out.entries.len());
        }
    }
    out.entries.sort_by_key(|e| e.title.to_lowercase());
    out
}

struct Candidate {
    system: &'static System,
    path: PathBuf,
    size: u64,
    name: RomName,
    stem: String,
}

#[allow(clippy::too_many_arguments)] // one call site; a struct would only rename them
fn scan_dir(
    dir: &Path,
    root: &Path,
    hint: Option<&'static System>,
    depth: usize,
    previous: &ScanCache,
    playlists: &Path,
    out: &mut Scanned,
    stack: &mut Vec<(PathBuf, Option<&'static System>, usize)>,
) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            dirs.push(entry.path());
        } else if meta.is_file() {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            files.push((entry.path(), meta.len(), mtime));
        }
    }
    files.sort();
    dirs.sort();

    // Folder games: each subfolder of a DOS or ScummVM folder is one game.
    if let Some(system) = hint.filter(|s| s.folder_games) {
        for game in &dirs {
            if let Some(entry) = folder_game(system, game, root) {
                out.entries.push(entry);
            }
        }
        if system.id == "dos" {
            for (path, size, _) in &files {
                if is_dos_program(path) {
                    let stem = stem_of(path);
                    let name = names::parse(&stem);
                    out.entries.push(RomEntry {
                        app_name: app_name_of(&system.id, path),
                        system: system.id.clone(),
                        title: name.title,
                        tags: name.tags,
                        path: path.clone(),
                        discs: Vec::new(),
                        size: *size,
                        stem,
                        root: root.to_path_buf(),
                    });
                }
            }
        }
        return;
    }

    // Files a cue sheet or playlist lists are not games of their own.
    let mut owned: HashSet<PathBuf> = HashSet::new();
    for (path, _, _) in &files {
        match extension(path).as_deref() {
            Some("cue") => owned.extend(cue_tracks(path)),
            Some("m3u") => owned.extend(playlist_entries(path)),
            _ => {}
        }
    }

    let mut candidates = Vec::new();
    for (path, size, mtime) in files {
        if owned.contains(&path) {
            continue;
        }
        let system = match previous.get(&path) {
            Some(seen) if seen.size == size && seen.mtime == mtime => {
                seen.system.as_deref().and_then(systems::get)
            }
            _ => systems::detect(&path, hint),
        };
        out.cache.insert(
            path.clone(),
            Seen {
                size,
                mtime,
                system: system.map(|s| s.id.clone()),
            },
        );
        let Some(system) = system else {
            if is_game_like(&path) {
                out.unrecognized += 1;
            }
            continue;
        };
        // A track `.bin` beside nothing that lists it, or a DOS program
        // outside a DOS folder, is not a game.
        if system.folder_games {
            continue;
        }
        let stem = stem_of(&path);
        candidates.push(Candidate {
            system,
            name: names::parse(&stem),
            stem,
            path,
            size,
        });
    }

    // Group the discs of one game.
    let mut groups: BTreeMap<(String, String), Vec<Candidate>> = BTreeMap::new();
    for c in candidates {
        let key = if c.system.multi_disc && c.name.disc.is_some() {
            (c.system.id.clone(), c.name.group.clone())
        } else {
            (c.system.id.clone(), format!("\0{}", c.path.display()))
        };
        groups.entry(key).or_default().push(c);
    }
    for (_, mut discs) in groups {
        discs.sort_by_key(|c| c.name.disc.unwrap_or(0));
        let first = &discs[0];
        let system = first.system;
        let app_name = app_name_of(&system.id, &first.path);
        let (path, disc_paths) = if discs.len() > 1 {
            let disc_paths: Vec<PathBuf> = discs.iter().map(|c| c.path.clone()).collect();
            let path = if system.disc_images {
                write_playlist(playlists, &app_name, &disc_paths).unwrap_or(first.path.clone())
            } else {
                first.path.clone()
            };
            (path, disc_paths)
        } else {
            (first.path.clone(), Vec::new())
        };
        out.entries.push(RomEntry {
            app_name,
            system: system.id.clone(),
            title: first.name.title.clone(),
            tags: first.name.tags.clone(),
            path,
            discs: disc_paths,
            size: discs.iter().map(|c| c.size).sum(),
            stem: first.stem.clone(),
            root: root.to_path_buf(),
        });
    }

    if depth < MAX_DEPTH {
        for sub in dirs.into_iter().rev() {
            let named = sub
                .file_name()
                .and_then(|n| systems::by_folder(&n.to_string_lossy()));
            stack.push((sub, named.or(hint), depth + 1));
        }
    }
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// Files that look like a game a user would expect to see, for the
/// "not recognised" count.
fn is_game_like(path: &Path) -> bool {
    matches!(
        extension(path).as_deref(),
        Some("zip" | "7z" | "iso" | "bin" | "rom" | "img" | "chd" | "cue")
    )
}

fn is_dos_program(path: &Path) -> bool {
    matches!(extension(path).as_deref(), Some("exe" | "com" | "bat"))
}

/// Programs that set a DOS game up rather than start it.
fn is_dos_helper(path: &Path) -> bool {
    let stem = stem_of(path).to_lowercase();
    [
        "setup", "install", "config", "uninst", "readme", "setsound", "sound",
    ]
    .iter()
    .any(|w| stem.starts_with(w))
}

/// A DOS or ScummVM game folder.
fn folder_game(system: &System, folder: &Path, root: &Path) -> Option<RomEntry> {
    let folder_name = folder.file_name()?.to_string_lossy().into_owned();
    let name = names::parse(&folder_name);
    let path = if system.id == "dos" {
        dos_program(folder)?
    } else if !system.folder_main.is_empty() {
        // Not a game folder unless the program engines open is inside.
        system
            .folder_main
            .iter()
            .map(|rel| folder.join(rel))
            .find(|p| p.is_file())?
    } else {
        // ScummVM finds the game itself; an empty folder is not a game.
        std::fs::read_dir(folder).ok()?.next()?.ok()?;
        folder.to_path_buf()
    };
    Some(RomEntry {
        app_name: app_name_of(&system.id, folder),
        system: system.id.clone(),
        title: name.title,
        tags: name.tags,
        path,
        discs: Vec::new(),
        size: 0,
        stem: folder_name,
        root: root.to_path_buf(),
    })
}

/// The program that starts the DOS game in `folder`: its DOSBox config if it
/// has one, else the one program that isn't an installer, preferring one
/// named like the folder.
pub fn dos_program(folder: &Path) -> Option<PathBuf> {
    fn programs(dir: &Path) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|r| r.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        out.sort();
        out
    }
    let top = programs(folder);
    if let Some(conf) = top.iter().find(|p| extension(p).as_deref() == Some("conf")) {
        return Some(conf.clone());
    }
    let wanted = folder
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let pick = |list: &[PathBuf]| -> Option<PathBuf> {
        let runnable: Vec<&PathBuf> = list
            .iter()
            .filter(|p| is_dos_program(p) && !is_dos_helper(p))
            .collect();
        runnable
            .iter()
            .find(|p| {
                let s = stem_of(p).to_lowercase();
                !s.is_empty() && (wanted.starts_with(&s) || s.starts_with(&wanted))
            })
            .or_else(|| {
                runnable
                    .iter()
                    .find(|p| extension(p).as_deref() != Some("bat"))
            })
            .or(runnable.first())
            .map(|p| (*p).clone())
    };
    pick(&top).or_else(|| {
        top.iter()
            .filter(|p| p.is_dir())
            .find_map(|sub| pick(&programs(sub)))
    })
}

/// Track files a cue sheet lists.
fn cue_tracks(cue: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(cue) else {
        return Vec::new();
    };
    let dir = cue.parent().unwrap_or(Path::new("."));
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("FILE ")?;
            let name = match rest.strip_prefix('"') {
                Some(r) => r.split('"').next()?,
                None => rest.split_whitespace().next()?,
            };
            Some(dir.join(name))
        })
        .collect()
}

/// Files an `.m3u` playlist lists.
fn playlist_entries(m3u: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(m3u) else {
        return Vec::new();
    };
    let dir = m3u.parent().unwrap_or(Path::new("."));
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| dir.join(l))
        .collect()
}

fn write_playlist(dir: &Path, app_name: &str, discs: &[PathBuf]) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("{app_name}.m3u"));
    let text: String = discs.iter().map(|d| format!("{}\n", d.display())).collect();
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        ember_core::json_store::write_bytes(&path, text.as_bytes()).ok()?;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn scans_systems_discs_and_folder_games() {
        let dir = tempfile::tempdir().unwrap();
        let roms = dir.path().join("roms");
        touch(&roms.join("SNES/Chrono Trigger (USA).sfc"), b"x");
        touch(&roms.join("SNES/notes.txt"), b"x");
        touch(&roms.join("loose/Metroid Fusion (USA).gba"), b"x");
        // A disc game with its tracks, and a two-disc game.
        touch(
            &roms.join("PS1/Vagrant Story (USA).cue"),
            b"FILE \"Vagrant Story (USA).bin\" BINARY\n",
        );
        touch(&roms.join("PS1/Vagrant Story (USA).bin"), b"x");
        touch(&roms.join("PS1/Final Fantasy VII (USA) (Disc 1).chd"), b"x");
        touch(&roms.join("PS1/Final Fantasy VII (USA) (Disc 2).chd"), b"x");
        // DOS folders and a ScummVM folder.
        touch(&roms.join("DOS/Commander Keen/KEEN1.EXE"), b"MZ");
        touch(&roms.join("DOS/Commander Keen/SETUP.EXE"), b"MZ");
        touch(&roms.join("ScummVM/Monkey Island/MONKEY.000"), b"x");
        touch(
            &roms.join("PS3/Demon's Souls/PS3_GAME/USRDIR/EBOOT.BIN"),
            b"x",
        );
        touch(&roms.join("PS3/Empty Folder/readme.txt"), b"x");
        touch(&roms.join("MAME/pacman.zip"), b"PK");
        touch(&roms.join("PS1/unknown.zip"), b"PK");
        touch(&roms.join("other/mystery.iso"), &[0u8; 64]);

        let playlists = dir.path().join("playlists");
        let found = scan(
            std::slice::from_ref(&roms),
            &ScanCache::new(),
            &playlists,
            &|_| {},
        );
        let titles: Vec<(&str, &str)> = found
            .entries
            .iter()
            .map(|e| (e.system.as_str(), e.title.as_str()))
            .collect();
        assert_eq!(
            titles,
            [
                ("snes", "Chrono Trigger"),
                ("dos", "Commander Keen"),
                ("ps3", "Demon's Souls"),
                ("psx", "Final Fantasy VII"),
                ("gba", "Metroid Fusion"),
                ("scummvm", "Monkey Island"),
                ("arcade", "pacman"),
                ("psx", "unknown"),
                ("psx", "Vagrant Story"),
            ]
        );
        let ff7 = &found.entries[3];
        assert_eq!(ff7.discs.len(), 2);
        assert_eq!(ff7.path.extension().unwrap(), "m3u");
        let playlist = std::fs::read_to_string(&ff7.path).unwrap();
        assert!(playlist.contains("(Disc 1).chd\n") && playlist.contains("(Disc 2).chd\n"));
        assert!(found.entries[1].path.ends_with("KEEN1.EXE"));
        assert_eq!(found.entries[0].tags, ["USA"]);
        assert_eq!(found.unrecognized, 1);

        // A rescan reuses detections and keeps ids stable.
        let again = scan(&[roms], &found.cache, &playlists, &|_| {});
        let ids = |s: &Scanned| {
            s.entries
                .iter()
                .map(|e| e.app_name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&again), ids(&found));
    }

    #[test]
    fn picks_dos_programs() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("Doom");
        touch(&game.join("INSTALL.EXE"), b"MZ");
        touch(&game.join("DOOM.EXE"), b"MZ");
        touch(&game.join("GO.BAT"), b"x");
        assert!(dos_program(&game).unwrap().ends_with("DOOM.EXE"));
        let conf = dir.path().join("Quake");
        touch(&conf.join("QUAKE.EXE"), b"MZ");
        touch(&conf.join("dosbox.conf"), b"[autoexec]");
        assert!(dos_program(&conf).unwrap().ends_with("dosbox.conf"));
        let nested = dir.path().join("Duke");
        touch(&nested.join("DUKE3D/DUKE3D.EXE"), b"MZ");
        assert!(dos_program(&nested).unwrap().ends_with("DUKE3D/DUKE3D.EXE"));
    }
}
