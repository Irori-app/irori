//! What to download: files built from parts of (possibly shared) chunks, and
//! the difference between two builds.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use anyhow::Result;

use crate::hash::Digest;

/// Turns a downloaded chunk into its content (decompression, container
/// formats). Called on a blocking thread.
pub trait Decoder: Send + Sync {
    fn decode(&self, data: Vec<u8>) -> Result<Vec<u8>>;
}

/// How a chunk is stored on the server.
#[derive(Clone, Default)]
pub enum Codec {
    /// Served as is.
    #[default]
    Raw,
    /// zlib stream (GOG v2 chunks).
    Zlib,
    /// Raw deflate stream (zip entries).
    Deflate,
    /// A store-specific container.
    Custom(Arc<dyn Decoder>),
}

impl fmt::Debug for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Codec::Raw => "Raw",
            Codec::Zlib => "Zlib",
            Codec::Deflate => "Deflate",
            Codec::Custom(_) => "Custom",
        })
    }
}

/// One downloadable unit.
#[derive(Debug, Clone, Default)]
pub struct Chunk {
    /// Identifies the chunk across files (chunks shared by several files are
    /// downloaded once while cached).
    pub key: String,
    /// Mirrors, tried in order.
    pub urls: Vec<String>,
    /// Bytes transferred.
    pub download_size: u64,
    /// Bytes after decoding.
    pub size: u64,
    /// Digest of the decoded content.
    pub hash: Option<Digest>,
    pub codec: Codec,
    /// Byte range `(start, length)` of the object behind the URL (a slice of
    /// a bigger file), fetched with an HTTP range request.
    pub range: Option<(u64, u64)>,
}

/// A slice of a decoded chunk.
#[derive(Debug, Clone)]
pub struct Part {
    pub chunk: Arc<Chunk>,
    pub offset: u64,
    pub size: u64,
    /// The same bytes in a file already on disk, read instead of downloading
    /// the chunk (unchanged regions of a file an update modifies). Only
    /// trusted while the finished file matches its digest; a retry downloads.
    pub reuse: Option<Reuse>,
}

/// Where a part's bytes already exist locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reuse {
    /// Normalised path of the file to read, relative to the install folder.
    /// Must be the file being rebuilt or one the job does not modify.
    pub path: String,
    pub offset: u64,
}

impl Part {
    pub fn new(chunk: Arc<Chunk>, offset: u64, size: u64) -> Self {
        Self {
            chunk,
            offset,
            size,
            reuse: None,
        }
    }
}

/// A file of a build.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Normalised relative path (see [`crate::path::normalize`]).
    pub path: String,
    pub size: u64,
    /// Digest of the whole file, used to skip and verify it.
    pub hash: Option<Digest>,
    /// The file's content, in order.
    pub parts: Vec<Part>,
    /// Set the executable bit (Unix).
    pub executable: bool,
}

impl FileEntry {
    /// A file served as one object (Amazon), streamed straight to disk with
    /// resume support.
    pub fn whole(path: String, size: u64, hash: Option<Digest>, urls: Vec<String>) -> Self {
        let chunk = Arc::new(Chunk {
            key: path.clone(),
            urls,
            download_size: size,
            size,
            hash: None,
            codec: Codec::Raw,
            range: None,
        });
        Self {
            path,
            size,
            hash,
            parts: vec![Part::new(chunk, 0, size)],
            executable: false,
        }
    }

    /// Whether the file is one object (raw, or raw-deflate compressed) that
    /// is streamed to disk and resumed.
    pub(crate) fn is_stream(&self) -> bool {
        matches!(self.parts.as_slice(), [p] if matches!(p.chunk.codec, Codec::Raw | Codec::Deflate)
            && p.reuse.is_none() && p.offset == 0 && p.size == self.size && p.chunk.size == self.size)
    }
}

/// The result of comparing an installed build with a new one.
#[derive(Debug, Default)]
pub struct Diff {
    /// New or modified files of the new build.
    pub changed: Vec<FileEntry>,
    /// Paths of the old build missing from the new one.
    pub removed: Vec<String>,
    /// Number of files identical in both builds.
    pub unchanged: usize,
}

/// Compare the installed build's `(path, digest)` list with the new build.
/// A file counts as changed when its digest differs or either side has none.
pub fn diff<'a>(
    old: impl IntoIterator<Item = (&'a str, Option<&'a Digest>)>,
    new: &[FileEntry],
) -> Diff {
    let old: HashMap<&str, Option<&Digest>> = old.into_iter().collect();
    let mut out = Diff::default();
    let mut present = HashSet::new();
    for f in new {
        present.insert(f.path.as_str());
        match old.get(f.path.as_str()) {
            Some(Some(h)) if f.hash.as_ref() == Some(*h) => out.unchanged += 1,
            _ => out.changed.push(f.clone()),
        }
    }
    let mut removed: Vec<String> = old
        .keys()
        .filter(|p| !present.contains(*p))
        .map(|p| p.to_string())
        .collect();
    removed.sort();
    out.removed = removed;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Algorithm;

    fn d(n: u8) -> Digest {
        Digest::new(Algorithm::Sha256, vec![n; 32])
    }

    fn file(path: &str, h: u8) -> FileEntry {
        FileEntry::whole(path.into(), 1, Some(d(h)), vec![])
    }

    #[test]
    fn diffs_builds() {
        let (a, b, c) = (d(1), d(2), d(3));
        let old = [
            ("same", Some(&a)),
            ("changed", Some(&b)),
            ("gone", Some(&c)),
            ("nohash", None),
        ];
        let new = [
            file("same", 1),
            file("changed", 9),
            file("added", 4),
            file("nohash", 5),
        ];
        let diff = diff(old, &new);
        let changed: Vec<&str> = diff.changed.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(changed, ["changed", "added", "nohash"]);
        assert_eq!(diff.removed, ["gone"]);
        assert_eq!(diff.unchanged, 1);
    }

    #[test]
    fn whole_files_stream() {
        assert!(file("a", 1).is_stream());
        let mut f = file("a", 1);
        f.parts[0].offset = 1;
        assert!(!f.is_stream());
    }
}
