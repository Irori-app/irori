//! Turning manifests into [`irori_depot`] download plans.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use futures::future::BoxFuture;
use irori_depot::{Algorithm, Chunk, Codec, Digest, FileEntry, Part, Reuse};

use crate::api::{Asset, Client, Session, Ticket, now};
use crate::chunk::ChunkDecoder;
use crate::manifest::{Comparison, FileManifest, Guid, Manifest, Secrets};

/// What the plan is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Every selected file (files already intact on disk are skipped by the
    /// engine).
    Install,
    /// Only what changed since `old`, plus selected files missing on disk.
    Update,
}

/// Inputs of [`plan`].
pub struct PlanInput<'a> {
    pub manifest: &'a Manifest,
    /// The installed build (updates).
    pub old: Option<&'a Manifest>,
    /// Chunk folders, tried in order.
    pub base_urls: &'a [String],
    pub secrets: Arc<Secrets>,
    /// Selected install tags (empty: everything).
    pub install_tags: &'a [String],
    /// Whether a file exists in the install folder.
    pub exists: &'a dyn Fn(&str) -> bool,
}

/// A download plan.
#[derive(Debug)]
pub struct Plan {
    pub files: Vec<FileEntry>,
    /// Paths to delete afterwards: files the new build dropped or the
    /// selection excludes.
    pub remove: Vec<String>,
    /// Bytes to download (chunks not covered by reused local data).
    pub download_size: u64,
    /// Size of the selected files of the build.
    pub install_size: u64,
}

/// Download entries for files of `manifest`.
pub struct EntryBuilder<'a> {
    manifest: &'a Manifest,
    chunks: HashMap<Guid, Arc<Chunk>>,
}

impl<'a> EntryBuilder<'a> {
    pub fn new(manifest: &'a Manifest, base_urls: &[String], secrets: Arc<Secrets>) -> Self {
        let codec = Codec::Custom(Arc::new(ChunkDecoder { secrets }));
        let chunks = manifest
            .chunks
            .iter()
            .map(|c| {
                let path = manifest.chunk_path(c);
                let urls = base_urls
                    .iter()
                    .map(|b| format!("{}/{path}", b.trim_end_matches('/')))
                    .collect();
                let chunk = Chunk {
                    key: path,
                    urls,
                    download_size: c.file_size,
                    size: u64::from(c.window_size),
                    hash: None,
                    codec: codec.clone(),
                    range: None,
                };
                (c.guid, Arc::new(chunk))
            })
            .collect();
        Self { manifest, chunks }
    }

    /// The entry of `file`, reusing unchanged byte ranges of `old` (the same
    /// file in the installed build) when given.
    pub fn entry(&self, file: &FileManifest, old: Option<&FileManifest>) -> Result<FileEntry> {
        let path = irori_depot::path::normalize(&file.filename)?;
        // Old parts by chunk: (chunk range start, end, offset in old file).
        let mut old_parts: HashMap<Guid, Vec<(u32, u32, u64)>> = HashMap::new();
        for p in old.map(|o| o.parts.as_slice()).unwrap_or_default() {
            old_parts
                .entry(p.guid)
                .or_default()
                .push((p.offset, p.offset + p.size, p.file_offset));
        }
        let parts = file
            .parts
            .iter()
            .map(|p| {
                let chunk = self
                    .chunks
                    .get(&p.guid)
                    .with_context(|| format!("{} references an unknown chunk", file.filename))?
                    .clone();
                let mut part = Part::new(chunk, u64::from(p.offset), u64::from(p.size));
                part.reuse = old_parts.get(&p.guid).and_then(|ranges| {
                    ranges
                        .iter()
                        .find(|(start, end, _)| *start <= p.offset && p.offset + p.size <= *end)
                        .map(|(start, _, file_offset)| Reuse {
                            path: path.clone(),
                            offset: file_offset + u64::from(p.offset - start),
                        })
                });
                Ok(part)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(FileEntry {
            path,
            size: file.size,
            hash: Some(Digest::new(Algorithm::Sha1, file.sha1.to_vec())),
            parts,
            executable: file.is_executable(),
        })
    }

    /// Entries of every file selected by `tags`, sorted by path.
    pub fn all(&self, tags: &[String]) -> Result<Vec<FileEntry>> {
        let mut files: Vec<&FileManifest> = self
            .manifest
            .files
            .iter()
            .filter(|f| f.selected_by(tags))
            .collect();
        files.sort_by_cached_key(|f| f.filename.to_lowercase());
        files.into_iter().map(|f| self.entry(f, None)).collect()
    }
}

/// Plan an install or update.
pub fn plan(input: &PlanInput<'_>, mode: Mode) -> Result<Plan> {
    let m = input.manifest;
    let tags = input.install_tags;
    let builder = EntryBuilder::new(m, input.base_urls, input.secrets.clone());
    let comparison = Comparison::new(
        m,
        if mode == Mode::Update {
            input.old
        } else {
            None
        },
    );
    let old_files: HashMap<&str, &FileManifest> = input
        .old
        .map(|o| o.files.iter().map(|f| (f.filename.as_str(), f)).collect())
        .unwrap_or_default();

    let mut selected: Vec<&FileManifest> = m.files.iter().filter(|f| f.selected_by(tags)).collect();
    selected.sort_by_cached_key(|f| f.filename.to_lowercase());
    let mut files = Vec::new();
    for f in selected {
        let name = f.filename.as_str();
        let needed = match mode {
            Mode::Install => true,
            Mode::Update => {
                comparison.added.contains(name)
                    || comparison.changed.contains(name)
                    || !(input.exists)(name)
            }
        };
        if !needed {
            continue;
        }
        let old = (mode == Mode::Update && comparison.changed.contains(name))
            .then(|| old_files.get(name).copied())
            .flatten();
        files.push(builder.entry(f, old)?);
    }

    let mut remove: Vec<String> = comparison.removed.iter().cloned().collect();
    remove.extend(
        m.files
            .iter()
            .filter(|f| !f.selected_by(tags))
            .map(|f| f.filename.clone()),
    );
    let remove = remove
        .into_iter()
        .filter_map(|p| irori_depot::path::normalize(&p).ok())
        .collect();

    let mut counted = HashSet::new();
    let download_size = files
        .iter()
        .flat_map(|f| &f.parts)
        .filter(|p| p.reuse.is_none() && counted.insert(p.chunk.key.clone()))
        .map(|p| p.chunk.download_size)
        .sum();
    Ok(Plan {
        files,
        remove,
        download_size,
        install_size: m.install_size(tags),
    })
}

/// Fetches a valid session for long operations (the store refreshes tokens).
pub type SessionSource = Arc<dyn Fn() -> BoxFuture<'static, Result<Session>> + Send + Sync>;

/// Signs chunk addresses for builds served through signed URLs.
pub struct ChunkSigner {
    pub client: Client,
    pub session: SessionSource,
    pub asset: Asset,
    ticket: tokio::sync::Mutex<Option<Ticket>>,
}

impl ChunkSigner {
    pub fn new(client: Client, session: SessionSource, asset: Asset) -> Self {
        Self {
            client,
            session,
            asset,
            ticket: tokio::sync::Mutex::new(None),
        }
    }
}

impl irori_depot::UrlSigner for ChunkSigner {
    fn sign<'a>(&'a self, keys: &'a [String]) -> BoxFuture<'a, Result<HashMap<String, String>>> {
        Box::pin(async move {
            let session = (self.session)().await?;
            let ticket = {
                let mut guard = self.ticket.lock().await;
                match guard.as_ref() {
                    Some(t) if t.expires_at > now() + 300 => t.clone(),
                    _ => {
                        let t = self.client.download_ticket(&session, &self.asset).await?;
                        *guard = Some(t.clone());
                        t
                    }
                }
            };
            self.client.signed_chunk_urls(&session, &ticket, keys).await
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::manifest::tests::{guid, sample};
    use crate::manifest::{ChunkPart, FileManifest};

    static BASES: once_cell::sync::Lazy<Vec<String>> = once_cell::sync::Lazy::new(|| {
        vec!["https://a/base".to_string(), "https://b/base/".to_string()]
    });

    fn input<'a>(
        m: &'a Manifest,
        old: Option<&'a Manifest>,
        tags: &'a [String],
        exists: &'a dyn Fn(&str) -> bool,
    ) -> PlanInput<'a> {
        PlanInput {
            manifest: m,
            old,
            base_urls: &BASES,
            secrets: Arc::default(),
            install_tags: tags,
            exists,
        }
    }

    #[test]
    fn plans_installs_with_tags() {
        let m = sample(18);
        let yes = |_: &str| true;
        let all = plan(&input(&m, None, &[], &yes), Mode::Install).unwrap();
        let paths: Vec<&str> = all.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["Bin/Game.exe", "Data/de.pak"]);
        assert_eq!((all.download_size, all.install_size), (1500, 175));
        let exe = &all.files[0];
        assert!(exe.executable);
        assert_eq!(exe.hash.as_ref().unwrap().value, vec![9; 20]);
        let chunk = &exe.parts[0].chunk;
        assert_eq!(
            chunk.urls,
            [
                format!("https://a/base/{}", m.chunk_path(&m.chunks[0])),
                format!("https://b/base/{}", m.chunk_path(&m.chunks[0]))
            ]
        );
        assert_eq!(chunk.size, 1 << 20);
        // Both files share chunk 1.
        assert!(Arc::ptr_eq(chunk, &all.files[1].parts[0].chunk));

        let tags = vec!["fr".to_string()];
        let fr = plan(&input(&m, None, &tags, &yes), Mode::Install).unwrap();
        assert_eq!(fr.files.len(), 1);
        assert_eq!(fr.remove, ["Data/de.pak"]);
        assert_eq!((fr.download_size, fr.install_size), (1000, 100));
    }

    #[test]
    fn plans_updates_reusing_unchanged_ranges() {
        let old = sample(18);
        let mut new = sample(18);
        // de.pak changes: its first 50 bytes stay in chunk 1, a new chunk
        // replaces the rest; the exe is unchanged; a file is added.
        new.chunks.push(crate::manifest::ChunkInfo {
            guid: guid(30),
            file_size: 300,
            window_size: 1 << 20,
            ..Default::default()
        });
        new.files[1].sha1 = [4; 20];
        new.files[1].parts = vec![
            ChunkPart {
                guid: guid(1),
                offset: 110,
                size: 40,
                file_offset: 0,
            },
            ChunkPart {
                guid: guid(30),
                offset: 0,
                size: 10,
                file_offset: 40,
            },
        ];
        new.files.push(FileManifest {
            filename: "new.txt".into(),
            parts: vec![ChunkPart {
                guid: guid(30),
                offset: 10,
                size: 5,
                file_offset: 0,
            }],
            size: 5,
            ..Default::default()
        });
        new.files.remove(0);
        let missing: BTreeSet<&str> = BTreeSet::new();
        let exists = |p: &str| !missing.contains(p);
        let p = plan(&input(&new, Some(&old), &[], &exists), Mode::Update).unwrap();
        let paths: Vec<&str> = p.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["Data/de.pak", "new.txt"]);
        assert_eq!(p.remove, ["Bin/Game.exe"]);
        let de = &p.files[0];
        // Old de.pak had chunk 1 bytes 100..150 at file offset 0, so the new
        // part (chunk 1 bytes 110..150) is at old file offset 10.
        assert_eq!(
            de.parts[0].reuse,
            Some(Reuse {
                path: "Data/de.pak".into(),
                offset: 10
            })
        );
        assert_eq!(de.parts[1].reuse, None);
        // Only the new chunk is downloaded.
        assert_eq!(p.download_size, 300);

        // An unchanged file missing on disk is downloaded again.
        let gone = |p: &str| p != "Bin/Game.exe";
        let same = sample(18);
        let p = plan(&input(&same, Some(&old), &[], &gone), Mode::Update).unwrap();
        assert_eq!(p.files.len(), 1);
        assert_eq!(p.files[0].path, "Bin/Game.exe");
    }
}
