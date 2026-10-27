//! Finding the program to start in an unpacked itch.io upload (blocking).
//!
//! A `.itch.toml` manifest names it when the developer wrote one (its first
//! action's `path`, `{{EXT}}` replaced by the system's extension).
//! Otherwise the folder is searched, a few levels deep: `.exe` files for
//! Windows builds, `.app` bundles for macOS builds and ELF programs, shell
//! scripts and AppImages for Linux builds. Uninstallers, crash reporters and
//! redistributable installers are skipped. The shallowest match wins, then
//! the largest.

use std::path::{Path, PathBuf};

use irori_core::InstallPlatform;
use once_cell::sync::Lazy;
use regex::Regex;

const MAX_DEPTH: usize = 4;

/// Parts of file names that are never the game.
const NOT_THE_GAME: [&str; 12] = [
    "unins",
    "uninstall",
    "crashhandler",
    "crashreport",
    "crashpad",
    "vc_redist",
    "vcredist",
    "dxsetup",
    "dxwebsetup",
    "dotnet",
    "ue4prereq",
    "redist",
];

static MANIFEST_PATH: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*path\s*=\s*["']([^"']+)["']"#).expect("valid regex"));

/// The program to start, relative to `root`.
pub(crate) fn find_executable(root: &Path, platform: InstallPlatform) -> Option<PathBuf> {
    if let Some(p) = from_manifest(root, platform) {
        return Some(p);
    }
    let mut found: Vec<(usize, u64, PathBuf)> = Vec::new();
    walk(root, root, 0, platform, &mut found);
    found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    found.into_iter().next().map(|(_, _, p)| p)
}

fn from_manifest(root: &Path, platform: InstallPlatform) -> Option<PathBuf> {
    let text = std::fs::read_to_string(root.join(".itch.toml")).ok()?;
    let ext = match platform {
        InstallPlatform::Windows => ".exe",
        InstallPlatform::Mac => ".app",
        _ => "",
    };
    let raw = MANIFEST_PATH.captures(&text)?[1].replace("{{EXT}}", ext);
    let rel = PathBuf::from(raw.trim_start_matches("./"));
    (rel.is_relative() && root.join(&rel).exists()).then_some(rel)
}

fn walk(
    root: &Path,
    dir: &Path,
    depth: usize,
    platform: InstallPlatform,
    out: &mut Vec<(usize, u64, PathBuf)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if NOT_THE_GAME.iter().any(|n| name.contains(n)) || name == "__macosx" {
            continue;
        }
        let rel = || path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        if meta.is_dir() {
            if name.ends_with(".app") {
                if platform == InstallPlatform::Mac {
                    out.push((depth, 0, rel()));
                }
                continue;
            }
            if depth < MAX_DEPTH {
                walk(root, &path, depth + 1, platform, out);
            }
            continue;
        }
        let candidate = match platform {
            InstallPlatform::Windows => name.ends_with(".exe"),
            InstallPlatform::Linux => is_linux_program(&path, &name),
            _ => false,
        };
        if candidate {
            out.push((depth, meta.len(), rel()));
        }
    }
}

fn is_linux_program(path: &Path, name: &str) -> bool {
    if name.ends_with(".so") || name.contains(".so.") {
        return false;
    }
    if [".x86_64", ".x86", ".sh", ".appimage"]
        .iter()
        .any(|e| name.ends_with(e))
    {
        return true;
    }
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic))
        .is_ok_and(|()| magic == *b"\x7fELF")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    #[test]
    fn finds_the_game_program() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(root, "Game/unins000.exe", &[0; 900]);
        touch(root, "Game/UnityCrashHandler64.exe", &[0; 900]);
        touch(root, "Game/Game.exe", &[0; 100]);
        touch(root, "Game/tools/editor.exe", &[0; 5000]);
        touch(root, "Game/game.x86_64", b"\x7fELF....");
        touch(root, "Game/libgame.so", b"\x7fELF....");
        touch(root, "Game/readme.txt", b"hi");
        std::fs::create_dir_all(root.join("Game.app/Contents/MacOS")).unwrap();
        assert_eq!(
            find_executable(root, InstallPlatform::Windows).unwrap(),
            Path::new("Game/Game.exe")
        );
        assert_eq!(
            find_executable(root, InstallPlatform::Linux).unwrap(),
            Path::new("Game/game.x86_64")
        );
        assert_eq!(
            find_executable(root, InstallPlatform::Mac).unwrap(),
            Path::new("Game.app")
        );
        let empty = tempfile::tempdir().unwrap();
        assert!(find_executable(empty.path(), InstallPlatform::Windows).is_none());
    }

    #[test]
    fn follows_the_itch_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(root, "bin/launcher.exe", b"MZ");
        touch(root, "bin/big.exe", &[0; 9000]);
        touch(
            root,
            ".itch.toml",
            b"[[actions]]\nname = \"play\"\npath = \"bin/launcher{{EXT}}\"\n",
        );
        assert_eq!(
            find_executable(root, InstallPlatform::Windows).unwrap(),
            Path::new("bin/launcher.exe")
        );
    }
}
