//! Small JSON files written atomically (write to a temp file, then rename), so
//! a crash never leaves a half-written settings or state file.

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Read and decode; `Ok(None)` when the file does not exist.
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("invalid JSON in {}", path.display())
            })?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Read, falling back to `Default` on a missing or corrupt file (logged).
pub fn read_or_default<T: DeserializeOwned + Default>(path: &Path) -> T {
    match read(path) {
        Ok(Some(v)) => v,
        Ok(None) => T::default(),
        Err(e) => {
            tracing::warn!("{e:#}; using defaults");
            T::default()
        }
    }
}

/// Encode and atomically replace `path`.
pub fn write<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_bytes(path, &bytes)
}

pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "{}.{}.{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("json"),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    // A new file, so a link placed at this name is never followed.
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(bytes))
        .with_context(|| format!("writing {}", tmp.display()))
        .and_then(|()| {
            std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b.json");
        assert!(read::<Vec<u32>>(&p).unwrap().is_none());
        write(&p, &vec![1u32, 2]).unwrap();
        assert_eq!(read::<Vec<u32>>(&p).unwrap(), Some(vec![1, 2]));
        std::fs::write(&p, b"{").unwrap();
        assert_eq!(read_or_default::<Vec<u32>>(&p), Vec::<u32>::new());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_at_the_temp_name_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, "kept").unwrap();
        let p = dir.path().join("a.txt");
        std::os::unix::fs::symlink(&outside, dir.path().join("a.txt.tmp")).unwrap();
        write_bytes(&p, b"new").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        assert_eq!(std::fs::read(&outside).unwrap(), b"kept");
    }
}
