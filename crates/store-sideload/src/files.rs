//! Filesystem helpers: guarded deletion of app folders and executable discovery.

use std::path::{Path, PathBuf};

use crate::app::SideloadPlatform;

pub(crate) use ember_core::fs_guard::check_deletable;

/// The single likely game executable directly inside `dir`, ignoring
/// installers, uninstallers and crash handlers. `None` when there is none or
/// more than one candidate.
pub(crate) fn find_single_executable(dir: &Path, platform: SideloadPlatform) -> Option<PathBuf> {
    let mut candidates = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_type().is_ok_and(|t| {
                t.is_file()
                    || (platform == SideloadPlatform::Mac
                        && t.is_dir()
                        && e.path().extension().is_some_and(|x| x == "app"))
            })
        })
        .map(|e| e.path())
        .filter(|p| is_candidate(p, platform));
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

fn is_candidate(path: &Path, platform: SideloadPlatform) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    const IGNORED: &[&str] = &[
        "unins",
        "setup",
        "install",
        "vcredist",
        "dxsetup",
        "crash",
        "redist",
        "launcherhelper",
    ];
    if IGNORED.iter().any(|w| name.contains(w)) {
        return false;
    }
    match platform {
        SideloadPlatform::Windows => name.ends_with(".exe"),
        SideloadPlatform::Mac if name.ends_with(".app") => true,
        SideloadPlatform::Linux | SideloadPlatform::Mac => {
            name.ends_with(".sh")
                || name.ends_with(".x86_64")
                || (ember_core::util::is_executable(path)
                    && !name.contains(".so")
                    && !name.ends_with(".exe")
                    && !name.ends_with(".dll"))
        }
        SideloadPlatform::Browser => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletion_guard() {
        let home = Path::new("/home/u");
        let protected = vec![
            PathBuf::from("/home/u/.config/ember"),
            PathBuf::from("/home/u/Games/Ember"),
        ];
        let check = |p: &str| check_deletable(Path::new(p), Some(home), &protected);
        for bad in [
            "/",
            "/games",
            "/home",
            "/home/u",
            "/home/u/Downloads",
            "/home/u/.config",
            "/home/u/Games/Ember",
            "/usr/share/x",
            "/home/u/Games/../x",
            "relative/dir",
        ] {
            assert!(check(bad).is_err(), "{bad} should be refused");
        }
        for ok in [
            "/home/u/Games/Celeste",
            "/home/u/Games/Ember/Celeste",
            "/mnt/data/game",
            "/opt/game",
        ] {
            assert!(check(ok).is_ok(), "{ok} should be allowed");
        }
    }

    #[test]
    fn finds_single_executable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("game.exe"), b"").unwrap();
        std::fs::write(dir.path().join("unins000.exe"), b"").unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"").unwrap();
        assert_eq!(
            find_single_executable(dir.path(), SideloadPlatform::Windows),
            Some(dir.path().join("game.exe"))
        );
        std::fs::write(dir.path().join("editor.exe"), b"").unwrap();
        assert_eq!(
            find_single_executable(dir.path(), SideloadPlatform::Windows),
            None
        );
        std::fs::write(dir.path().join("start.sh"), b"").unwrap();
        assert_eq!(
            find_single_executable(dir.path(), SideloadPlatform::Linux),
            Some(dir.path().join("start.sh"))
        );
    }
}
