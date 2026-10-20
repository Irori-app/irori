//! Guard for recursive deletions of game, prefix and download folders. Every
//! crate that deletes a folder chosen by the user or recorded by a store
//! checks it here first, so one bad path (a library root, the home folder)
//! can never wipe unrelated data.

use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};

use crate::paths::{Paths, expand_home};
use crate::settings::AppSettings;

/// System directories whose contents are never deleted.
const SYSTEM_ROOTS: &[&str] = &[
    "/bin", "/boot", "/dev", "/etc", "/lib", "/lib32", "/lib64", "/proc", "/root", "/run", "/sbin",
    "/snap", "/sys", "/usr", "/var",
];

/// Refuse to delete `dir` unless it is clearly a single game's (or prefix's)
/// own folder: an absolute path at least two levels deep, not the home
/// directory, one of its ancestors or direct children (`~/Downloads`,
/// `~/Games`…), not inside a system directory, and not containing any
/// `protected` path.
pub fn check_deletable(dir: &Path, home: Option<&Path>, protected: &[PathBuf]) -> Result<()> {
    let refuse = |why: &str| -> Result<()> {
        bail!(
            "Refusing to delete {} because {why}. Delete the files manually if you really want to.",
            dir.display()
        )
    };
    if !dir.is_absolute() || dir.components().any(|c| c == Component::ParentDir) {
        return refuse("it is not a plain full path");
    }
    let depth = dir
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    if depth < 2 {
        return refuse("it is a top-level folder");
    }
    if SYSTEM_ROOTS.iter().any(|root| dir.starts_with(root)) {
        return refuse("it is a system folder");
    }
    if let Some(home) = home {
        if home.starts_with(dir) {
            return refuse("it contains your home folder");
        }
        if dir.parent() == Some(home) {
            return refuse("it is a folder directly inside your home folder");
        }
    }
    if protected.iter().any(|p| p.starts_with(dir)) {
        return refuse("it contains launcher or library folders");
    }
    Ok(())
}

/// Folders that must survive any deletion: Ember's own directories and the
/// configured library and prefix roots.
pub fn protected_dirs(app: &AppSettings) -> Vec<PathBuf> {
    let p = Paths::get();
    let mut dirs = vec![p.config.clone(), p.data.clone(), p.cache.clone()];
    for s in [&app.default_install_path, &app.default_wine_prefix_dir] {
        if !s.is_empty() {
            dirs.push(expand_home(s));
        }
    }
    dirs
}

/// [`check_deletable`] with the real home folder and [`protected_dirs`],
/// resolving symlinks first so a link cannot smuggle a protected folder in.
pub fn ensure_deletable(dir: &Path, app: &AppSettings) -> Result<()> {
    let resolved = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let home = dirs::home_dir().map(|h| std::fs::canonicalize(&h).unwrap_or(h));
    let protected: Vec<PathBuf> = protected_dirs(app)
        .into_iter()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .collect();
    check_deletable(&resolved, home.as_deref(), &protected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_broad_folders() {
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
            "/home/u/Games",
            "/home/u/Downloads",
            "/home/u/Games/Ember",
            "/usr/share/x",
            "relative/dir",
            "/home/u/a/../b",
        ] {
            assert!(check(bad).is_err(), "{bad} should be refused");
        }
        for ok in [
            "/home/u/Games/Ember/Hades",
            "/mnt/disk/games/Hades",
            "/home/u/Games/Other/x",
        ] {
            assert!(check(ok).is_ok(), "{ok} should be allowed");
        }
    }
}
