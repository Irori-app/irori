//! Path checks for unpacking downloaded archives. An archive is untrusted
//! input: whatever its entries say, extraction must never create, change or
//! link to anything outside the destination folder. Extractors run every
//! entry through these checks before touching the disk.

use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};

/// The entry path `path` as a plain relative path (`./` dropped), or `None`
/// when it is empty, absolute or climbs with `..`.
pub fn relative_entry_path(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(name) => out.push(name),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Fail when any existing component of `rel` under `dest` is a symbolic
/// link, so nothing is created or written through one. Components that do
/// not exist yet are created by the caller as real folders.
pub fn ensure_no_symlinks(dest: &Path, rel: &Path) -> Result<()> {
    let mut path = dest.to_path_buf();
    for part in rel.components() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                bail!("unsafe path in archive: {} is a link", rel.display())
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

/// Check the symbolic link entry `rel -> link` before creating it under
/// `dest`. Returns `Ok(true)` to create it and `Ok(false)` when the same
/// link is already there (a resumed or repeated extraction), which is left
/// as it is.
///
/// Fails when the target resolves outside `dest`, or when the link would
/// replace another entry: that could move the target of a link checked
/// earlier (`b -> a/..`, then `a -> .`) outside.
pub fn check_symlink(dest: &Path, rel: &Path, link: &Path) -> Result<bool> {
    let path = dest.join(rel);
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() && std::fs::read_link(&path).is_ok_and(|t| t == link) {
            return Ok(false);
        }
        bail!(
            "unsafe link in archive: {} -> {} replaces an entry",
            rel.display(),
            link.display()
        );
    }
    let base = rel.parent().unwrap_or(Path::new(""));
    if resolve_inside(dest, base, link, &mut 0).is_none() {
        bail!(
            "unsafe link in archive: {} -> {}",
            rel.display(),
            link.display()
        );
    }
    Ok(true)
}

/// Most links followed while resolving one target, as in Linux's ELOOP.
const MAX_LINK_HOPS: u32 = 40;

/// Resolve `link`, the target of a symbolic link in the folder `base`
/// (relative to `dest`), and return where it points relative to `dest`, or
/// `None` when it leaves `dest`. Links already extracted are followed, so
/// `self -> .` cannot make `self/..` look like `dest`. `..` after a
/// component that does not exist yet is refused: the archive could later
/// make that component a link and move the target outside.
fn resolve_inside(dest: &Path, base: &Path, link: &Path, hops: &mut u32) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    let mut exists = true;
    for part in link.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !exists || !out.pop() {
                    return None;
                }
            }
            Component::Normal(name) => {
                out.push(name);
                if !exists {
                    continue;
                }
                match std::fs::symlink_metadata(dest.join(&out)) {
                    Ok(m) if m.file_type().is_symlink() => {
                        *hops += 1;
                        if *hops > MAX_LINK_HOPS {
                            return None;
                        }
                        let next = std::fs::read_link(dest.join(&out)).ok()?;
                        out.pop();
                        out = resolve_inside(dest, &out, &next, hops)?;
                        exists = std::fs::symlink_metadata(dest.join(&out)).is_ok();
                    }
                    Ok(_) => {}
                    Err(_) => exists = false,
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_paths_stay_relative() {
        assert_eq!(
            relative_entry_path(Path::new("./a/./b")),
            Some(PathBuf::from("a/b"))
        );
        assert_eq!(relative_entry_path(Path::new("a/../b")), None);
        assert_eq!(relative_entry_path(Path::new("/etc/passwd")), None);
        assert_eq!(relative_entry_path(Path::new("./")), None);
    }

    #[test]
    fn link_targets_must_stay_inside() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path();
        std::fs::create_dir_all(dest.join("lib64")).unwrap();
        std::fs::create_dir_all(dest.join("bin")).unwrap();
        assert!(check_symlink(dest, Path::new("lib"), Path::new("lib64")).unwrap());
        assert!(check_symlink(dest, Path::new("bin/l"), Path::new("../lib64/x")).unwrap());
        assert!(check_symlink(dest, Path::new("up"), Path::new("..")).is_err());
        assert!(check_symlink(dest, Path::new("abs"), Path::new("/etc")).is_err());
        // `missing` could become a link to anywhere before the target is used.
        assert!(check_symlink(dest, Path::new("m"), Path::new("missing/../..")).is_err());
        assert!(check_symlink(dest, Path::new("m"), Path::new("missing/..")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn links_already_on_disk_are_followed() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path();
        std::os::unix::fs::symlink(".", dest.join("self")).unwrap();
        assert!(check_symlink(dest, Path::new("up"), Path::new("self/..")).is_err());
        // A loop is refused instead of recursing forever.
        std::os::unix::fs::symlink("loop", dest.join("loop")).unwrap();
        assert!(check_symlink(dest, Path::new("l"), Path::new("loop/x")).is_err());
        // The same link again is kept; a different one may not replace it.
        assert!(!check_symlink(dest, Path::new("self"), Path::new(".")).unwrap());
        assert!(check_symlink(dest, Path::new("self"), Path::new("lib")).is_err());
        assert!(ensure_no_symlinks(dest, Path::new("self/x")).is_err());
        assert!(ensure_no_symlinks(dest, Path::new("new/x")).is_ok());
    }
}
