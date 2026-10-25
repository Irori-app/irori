//! Finding what to run inside an installed game (blocking filesystem scans).

use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;

/// Executables that are never the game.
static NOT_GAME: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)setup|unins|redist|vcredist|vc_redist|dxwebsetup|directx|dotnet|crashreport|crashhandler|crashpad|uninstall|install",
    )
    .expect("exe filter regex")
});

const MAX_DEPTH: usize = 8;

/// Files below `root` (recursive, symlinked directories not followed).
fn walk(root: &Path, max_depth: usize, out: &mut Vec<PathBuf>) {
    fn inner(dir: &Path, depth: usize, max: usize, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Ok(ty) = e.file_type() else { continue };
            if ty.is_dir() {
                if depth < max {
                    inner(&e.path(), depth + 1, max, out);
                }
            } else if ty.is_file() {
                out.push(e.path());
            }
        }
    }
    inner(root, 0, max_depth, out);
}

fn files_with_ext(root: &Path, ext: &str) -> Vec<PathBuf> {
    let mut all = Vec::new();
    walk(root, MAX_DEPTH, &mut all);
    all.retain(|p| {
        p.extension()
            .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(ext))
    });
    all
}

/// Every `.conf` file below `root` (DOSBox configurations).
pub fn conf_files(root: &Path) -> Vec<PathBuf> {
    files_with_ext(root, "conf")
}

/// `dosbox.exe` below `dir`, case-insensitive.
pub fn find_dosbox_exe(dir: &Path) -> Option<PathBuf> {
    files_with_ext(dir, "exe").into_iter().find(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("dosbox.exe"))
    })
}

fn alnum_lower(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}

/// Pick among candidates: the only one, else one whose path contains the
/// title, else the largest file.
fn choose(root: &Path, candidates: Vec<PathBuf>, title: &str) -> Option<String> {
    if candidates.len() <= 1 {
        return candidates.first().map(|p| rel(root, p));
    }
    let wanted = alnum_lower(title);
    if !wanted.is_empty()
        && let Some(p) = candidates
            .iter()
            .find(|p| alnum_lower(&rel(root, p)).contains(&wanted))
    {
        return Some(rel(root, p));
    }
    candidates
        .iter()
        .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .map(|p| rel(root, p))
}

/// The game's Windows executable, relative to `root`.
pub fn windows_executable(root: &Path, title: &str) -> Option<String> {
    let exes: Vec<PathBuf> = files_with_ext(root, "exe")
        .into_iter()
        .filter(|p| !NOT_GAME.is_match(&rel(root, p)))
        .collect();
    choose(root, exes, title)
}

fn is_elf_or_script(p: &Path) -> bool {
    let mut magic = [0u8; 4];
    match File::open(p).and_then(|mut f| f.read_exact(&mut magic)) {
        Ok(()) => &magic == b"\x7fELF" || magic.starts_with(b"#!"),
        Err(_) => false,
    }
}

/// The game's Linux launcher, relative to `root`: `start.sh` (shallowest),
/// else an ELF binary or script that is not an installer/uninstaller.
pub fn linux_executable(root: &Path, title: &str) -> Option<String> {
    let mut all = Vec::new();
    walk(root, 3, &mut all);
    let depth = |p: &Path| {
        p.strip_prefix(root)
            .map(|r| r.components().count())
            .unwrap_or(99)
    };
    if let Some(start) = all
        .iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "start.sh"))
        .min_by_key(|p| depth(p))
    {
        return Some(rel(root, start));
    }
    let candidates: Vec<PathBuf> = all
        .into_iter()
        .filter(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let lower = name.to_ascii_lowercase();
            let skip_ext = [".so", ".txt", ".pdf", ".png", ".dll", ".py", ".pak", ".dat"]
                .iter()
                .any(|e| lower.ends_with(e) || lower.contains(".so."));
            !skip_ext && !NOT_GAME.is_match(&name) && is_elf_or_script(p)
        })
        .collect();
    // Prefer the shallowest candidates.
    let min = candidates.iter().map(|p| depth(p)).min()?;
    let shallow = candidates.into_iter().filter(|p| depth(p) == min).collect();
    choose(root, shallow, title)
}

/// Installer technology of a Windows setup executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallerKind {
    InnoSetup,
    Nsis,
    Unknown,
}

/// Sniff the installer type from the first megabytes of the executable.
pub fn installer_kind(exe: &Path) -> InstallerKind {
    let Ok(f) = File::open(exe) else {
        return InstallerKind::Unknown;
    };
    let mut buf = Vec::with_capacity(8 << 20);
    if f.take(8 << 20).read_to_end(&mut buf).is_err() {
        return InstallerKind::Unknown;
    }
    let has = |needle: &[u8]| buf.windows(needle.len()).any(|w| w == needle);
    if has(b"Inno Setup") {
        InstallerKind::InnoSetup
    } else if has(b"NullsoftInst") || has(b"Nullsoft Install System") {
        InstallerKind::Nsis
    } else {
        InstallerKind::Unknown
    }
}

/// System DOSBox (any common build).
pub fn system_dosbox() -> Option<PathBuf> {
    ["dosbox", "dosbox-staging", "dosbox-x"]
        .iter()
        .find_map(|b| which::which(b).ok())
}

/// Total size of the files below `root`.
pub fn dir_size(root: &Path) -> u64 {
    let mut all = Vec::new();
    walk(root, 64, &mut all);
    all.iter()
        .filter_map(|p| std::fs::symlink_metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// DOSBox setup of a Windows install: new `.conf` files next to a bundled
/// `dosbox.exe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DosboxGame {
    /// `dosbox.exe`, relative to the install root.
    pub exe: String,
    /// Working directory (directory of the first conf), relative.
    pub dir: String,
    /// Configurations, relative.
    pub confs: Vec<String>,
}

/// Detect a DOSBox game from configuration files (absolute paths).
pub fn dosbox_game(root: &Path, confs: &[PathBuf]) -> Option<DosboxGame> {
    let first = confs.first()?;
    let dir = first.parent()?;
    let exe = find_dosbox_exe(dir).or_else(|| find_dosbox_exe(root))?;
    Some(DosboxGame {
        exe: rel(root, &exe),
        dir: rel(root, dir),
        confs: confs.iter().map(|c| rel(root, c)).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str, data: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }

    #[test]
    fn picks_windows_exe() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        touch(r, "unins000.exe", b"x");
        touch(r, "redist/vcredist_x86.exe", b"x");
        touch(r, "Game.exe", b"x");
        assert_eq!(
            windows_executable(r, "Whatever").as_deref(),
            Some("Game.exe")
        );
        touch(r, "bin/Tool.exe", &[0; 100]);
        touch(r, "bin/SuperGame2.exe", b"x");
        assert_eq!(
            windows_executable(r, "Super Game 2")
                .as_deref()
                .map(Path::new),
            Some(Path::new("bin/SuperGame2.exe"))
        );
        assert_eq!(
            windows_executable(r, "Nope").as_deref().map(Path::new),
            Some(Path::new("bin/Tool.exe"))
        );
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(windows_executable(empty.path(), "x"), None);
    }

    #[test]
    fn picks_linux_launcher() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        touch(r, "Game/game.x86_64", b"\x7fELF....");
        touch(r, "Game/lib/libfoo.so", b"\x7fELF");
        touch(r, "Game/readme.txt", b"hello");
        assert_eq!(
            linux_executable(r, "Game").as_deref().map(Path::new),
            Some(Path::new("Game/game.x86_64"))
        );
        touch(r, "Game/start.sh", b"#!/bin/sh\n");
        assert_eq!(
            linux_executable(r, "Game").as_deref().map(Path::new),
            Some(Path::new("Game/start.sh"))
        );
        touch(r, "start.sh", b"#!/bin/sh\n");
        assert_eq!(linux_executable(r, "Game").as_deref(), Some("start.sh"));
    }

    #[test]
    fn detects_dosbox_and_installers() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        touch(r, "DOSBOX/DOSBox.exe", b"MZ");
        touch(r, "game/dosbox_game.conf", b"[autoexec]");
        touch(r, "game/dosbox_game_single.conf", b"[autoexec]");
        let confs = conf_files(r);
        assert_eq!(confs.len(), 2);
        let g = dosbox_game(r, &confs).unwrap();
        assert_eq!(Path::new(&g.exe), Path::new("DOSBOX/DOSBox.exe"));
        assert_eq!(g.dir, "game");
        assert_eq!(
            g.confs.iter().map(Path::new).collect::<Vec<_>>(),
            vec![
                Path::new("game/dosbox_game.conf"),
                Path::new("game/dosbox_game_single.conf")
            ]
        );

        touch(
            r,
            "inno.exe",
            b"MZ....This installation was built with Inno Setup....",
        );
        touch(r, "nsis.exe", b"MZ....\xef\xbe\xad\xdeNullsoftInst....");
        touch(r, "plain.exe", b"MZ");
        assert_eq!(
            installer_kind(&r.join("inno.exe")),
            InstallerKind::InnoSetup
        );
        assert_eq!(installer_kind(&r.join("nsis.exe")), InstallerKind::Nsis);
        assert_eq!(installer_kind(&r.join("plain.exe")), InstallerKind::Unknown);
        assert!(dir_size(r) > 0);
    }
}
