//! Manifest paths: normalised to `/`-separated relative paths that cannot
//! leave the install directory.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// Normalise a path from a manifest (`\` or `/` separated) into a relative
/// `/`-separated path. Rejects anything that could escape the install
/// directory: absolute paths, drive letters, `..` and NUL bytes.
pub fn normalize(raw: &str) -> Result<String> {
    if raw.contains('\0') {
        bail!("invalid file name in manifest: {raw:?}");
    }
    let unified = raw.replace('\\', "/");
    if unified.starts_with('/') {
        bail!("absolute path in manifest: {raw:?}");
    }
    let mut parts = Vec::new();
    for part in unified.split('/') {
        match part {
            "" | "." => {}
            ".." => bail!("path escapes the install folder: {raw:?}"),
            // Drive letters and NTFS streams.
            p if p.contains(':') => bail!("invalid file name in manifest: {raw:?}"),
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        bail!("empty path in manifest");
    }
    Ok(parts.join("/"))
}

/// Join a path returned by [`normalize`] onto `root`.
pub fn join(root: &Path, rel: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    out.extend(rel.split('/'));
    out
}

/// Where a file is assembled before it replaces the real one.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".ember-part");
    dest.with_file_name(name)
}

/// Remove now-empty parent directories of `rel` up to (excluding) `root`.
pub fn prune_empty_parents(root: &Path, rel: &str) {
    let mut dir = join(root, rel);
    while dir.pop() && dir.starts_with(root) && dir != root {
        if std::fs::remove_dir(&dir).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_manifest_paths() {
        assert_eq!(normalize("a\\b/c.exe").unwrap(), "a/b/c.exe");
        assert_eq!(normalize("./a//b").unwrap(), "a/b");
        for bad in [
            "",
            "/etc/passwd",
            "\\x",
            "a/../../b",
            "C:\\x",
            "a/b:stream",
            "a\0b",
            "./",
        ] {
            assert!(normalize(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn joins_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let file = join(root, "a/b/c.txt");
        assert_eq!(file, root.join("a").join("b").join("c.txt"));
        assert_eq!(part_path(&file), root.join("a/b/c.txt.ember-part"));
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/keep"), b"").unwrap();
        prune_empty_parents(root, "a/b/c.txt");
        assert!(!root.join("a/b").exists());
        assert!(root.join("a").exists());
    }
}
