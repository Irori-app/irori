//! Download plans: the files of a build (v1 or v2 depots, redistributables,
//! Linux installers) as [`ember_depot`] entries, and the difference between
//! an installed build and a new one.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use ember_depot::{Algorithm, Chunk, Codec, Digest, FileEntry, Part, Reuse};
use futures::StreamExt as _;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::content::{
    BuildMeta, Client, DepotItem, Endpoint, Repository, V1File, galaxy_path, parse_v1_depot,
    parse_v2_depot,
};
use crate::linux::Installer;

/// What to install of a build.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Install language (canonical code).
    pub language: String,
    /// DLC product ids.
    pub dlcs: Vec<String>,
    /// `windows` or `osx` (v1 depot paths).
    pub os: String,
}

/// The resolved files of a build.
#[derive(Debug, Clone, Default)]
pub struct BuildFiles {
    pub generation: u32,
    /// v2 files, links and directories (redistributables included).
    pub items: Vec<DepotItem>,
    /// v1 files.
    pub v1_files: Vec<V1File>,
    pub v1_dirs: Vec<String>,
}

/// The depots of `meta` that `sel` needs.
fn depots(meta: &BuildMeta, sel: &Selection) -> Vec<crate::content::DepotRef> {
    let mut products = vec![meta.base_product.clone()];
    products.extend(sel.dlcs.iter().cloned());
    meta.depots_for(&products, &sel.language)
        .into_iter()
        .cloned()
        .collect()
}

/// How many depot manifests [`resolve`] fetches.
pub fn manifest_count(meta: &BuildMeta, sel: &Selection) -> usize {
    depots(meta, sel).len()
}

/// Fetch the depot manifests of `meta` for `sel`. `fetched` hears how many
/// have arrived after each one.
pub async fn resolve(
    client: &Client,
    meta: &BuildMeta,
    sel: &Selection,
    fetched: &(dyn Fn(usize) + Send + Sync),
) -> Result<BuildFiles> {
    let depots = depots(meta, sel);
    let mut out = BuildFiles {
        generation: meta.generation,
        ..Default::default()
    };
    // Owned items keep the future `Send` for every caller lifetime.
    let requests: Vec<(String, String)> = depots
        .iter()
        .map(|d| (d.product_id.clone(), d.manifest.clone()))
        .collect();
    let (generation, os, timestamp) = (meta.generation, sel.os.clone(), meta.timestamp.clone());
    let fetched: Vec<Result<serde_json::Value>> = futures::stream::iter(requests)
        .map(|(product, manifest)| {
            let (client, os, timestamp) = (client.clone(), os.clone(), timestamp.clone());
            async move {
                if generation == 1 {
                    client.v1_depot(&product, &os, &timestamp, &manifest).await
                } else {
                    client.v2_depot(&manifest).await
                }
            }
        })
        .buffered(8)
        .enumerate()
        .map(|(i, v)| {
            fetched(i + 1);
            v
        })
        .collect()
        .await;
    let mut seen_v1 = HashSet::new();
    for (d, v) in depots.iter().zip(fetched) {
        let v = v?;
        if meta.generation == 1 {
            // Shared v1 depots appear once per product.
            if !seen_v1.insert(d.manifest.clone()) {
                continue;
            }
            let (files, dirs) = parse_v1_depot(&v, &d.product_id)?;
            out.v1_files.extend(files);
            out.v1_dirs.extend(dirs);
        } else {
            out.items.extend(parse_v2_depot(&v, &d.product_id)?);
        }
    }
    Ok(out)
}

/// Files of the redistributables `ids`: either those installed into the
/// game folder (`global == false`) or into the shared folder.
pub async fn resolve_dependencies(
    client: &Client,
    repo: &Repository,
    ids: &[String],
    global: bool,
) -> Result<Vec<DepotItem>> {
    let depots: Vec<_> = repo
        .depots
        .iter()
        .filter(|d| ids.contains(&d.dependency_id) && d.is_global() == global)
        .collect();
    let mut out = Vec::new();
    for d in depots {
        let v = client.dependency_depot(&d.manifest).await?;
        out.extend(parse_v2_depot(&v, REDIST)?);
    }
    Ok(out)
}

/// Product id of redistributable files.
pub const REDIST: &str = "redist";

/// A download plan for the game folder and the support folder.
#[derive(Debug, Default)]
pub struct Plan {
    pub game: Vec<FileEntry>,
    pub support: Vec<FileEntry>,
    pub remove_game: Vec<String>,
    pub remove_support: Vec<String>,
    /// Symbolic links `(path, target)` to create afterwards.
    pub links: Vec<(String, String)>,
    pub dirs: Vec<String>,
    pub download_size: u64,
    pub disk_size: u64,
}

/// Paths compare case-insensitively, with either separator.
fn path_key(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

/// Key of a v2 chunk, resolved by [`Signer`].
fn v2_key(product: &str, compressed_md5: &str) -> String {
    format!("v2/{product}/{compressed_md5}")
}

fn v1_key(product: &str, offset: u64, size: u64) -> String {
    format!("v1/{product}/{offset}/{size}")
}

fn md5_digest(hex: &str) -> Option<Digest> {
    Digest::from_hex(Algorithm::Md5, hex).ok()
}

/// What identifies a v2 file's content.
fn signature(item: &DepotItem) -> Option<String> {
    let DepotItem::File {
        md5,
        sha256,
        chunks,
        ..
    } = item
    else {
        return None;
    };
    md5.clone().or_else(|| sha256.clone()).or_else(|| {
        (!chunks.is_empty()).then(|| {
            chunks
                .iter()
                .map(|c| c.md5.as_str())
                .collect::<Vec<_>>()
                .join(",")
        })
    })
}

/// Where a v2 file goes: the game folder, or `<product>/<path>` in the
/// support folder.
fn destination(item: &DepotItem) -> Result<(bool, String)> {
    match item {
        DepotItem::File {
            product,
            path,
            support: true,
            ..
        } => Ok((
            true,
            ember_depot::path::normalize(&format!("{product}/{path}"))?,
        )),
        other => Ok((false, ember_depot::path::normalize(other.path())?)),
    }
}

struct Builder {
    chunks: HashMap<String, Arc<Chunk>>,
}

impl Builder {
    fn chunk(&mut self, key: String, f: impl FnOnce() -> Chunk) -> Arc<Chunk> {
        self.chunks
            .entry(key)
            .or_insert_with(|| Arc::new(f()))
            .clone()
    }

    fn v2_entry(
        &mut self,
        item: &DepotItem,
        path: String,
        old: Option<(&str, &DepotItem)>,
    ) -> Option<FileEntry> {
        let DepotItem::File {
            product,
            chunks,
            md5,
            sha256,
            executable,
            ..
        } = item
        else {
            return None;
        };
        // Chunks of the installed version, by content.
        let mut old_offsets: HashMap<&str, u64> = HashMap::new();
        if let Some((_, DepotItem::File { chunks: oc, .. })) = old {
            let mut off = 0;
            for c in oc {
                old_offsets.entry(c.md5.as_str()).or_insert(off);
                off += c.size;
            }
        }
        let parts = chunks
            .iter()
            .map(|c| {
                let key = v2_key(product, &c.compressed_md5);
                let chunk = self.chunk(key.clone(), || Chunk {
                    key,
                    download_size: c.compressed_size,
                    size: c.size,
                    hash: md5_digest(&c.md5),
                    codec: Codec::Zlib,
                    ..Default::default()
                });
                let mut part = Part::new(chunk, 0, c.size);
                if let (Some((old_path, _)), Some(off)) = (old, old_offsets.get(c.md5.as_str())) {
                    part.reuse = Some(Reuse {
                        path: old_path.to_string(),
                        offset: *off,
                    });
                }
                part
            })
            .collect::<Vec<_>>();
        let hash = md5
            .as_deref()
            .and_then(md5_digest)
            .or_else(|| {
                sha256
                    .as_deref()
                    .and_then(|h| Digest::from_hex(Algorithm::Sha256, h).ok())
            })
            .or_else(|| {
                (chunks.len() == 1)
                    .then(|| md5_digest(&chunks[0].md5))
                    .flatten()
            });
        Some(FileEntry {
            path,
            size: chunks.iter().map(|c| c.size).sum(),
            hash,
            parts,
            executable: *executable,
        })
    }
}

/// Plan an install (no `old`) or an update from `old`. `exists` tells
/// whether a file is present (support files prefixed `support:`), so
/// missing unchanged files are fetched again.
pub fn plan(
    new: &BuildFiles,
    old: Option<&BuildFiles>,
    exists: &dyn Fn(bool, &str) -> bool,
) -> Result<Plan> {
    let mut out = Plan::default();
    let mut b = Builder {
        chunks: HashMap::new(),
    };
    // A v1 install upgrading to v2 (or back) has nothing to compare.
    let old = old.filter(|o| o.generation == new.generation);

    if new.generation == 1 {
        let old_files: HashMap<String, &V1File> = old
            .map(|o| {
                o.v1_files
                    .iter()
                    .map(|f| (f.path.to_lowercase(), f))
                    .collect()
            })
            .unwrap_or_default();
        let mut present = HashSet::new();
        for f in &new.v1_files {
            present.insert(f.path.to_lowercase());
            let (support, rel) = if f.support {
                (
                    true,
                    ember_depot::path::normalize(&format!("{}/{}", f.product, f.path))?,
                )
            } else {
                (false, ember_depot::path::normalize(&f.path)?)
            };
            let unchanged = old_files
                .get(&f.path.to_lowercase())
                .is_some_and(|o| o.md5 == f.md5);
            if unchanged && exists(support, &rel) {
                continue;
            }
            let parts = if f.size == 0 {
                Vec::new()
            } else {
                let key = v1_key(&f.product, f.offset, f.size);
                let chunk = b.chunk(key.clone(), || Chunk {
                    key,
                    download_size: f.size,
                    size: f.size,
                    codec: Codec::Raw,
                    range: Some((f.offset, f.size)),
                    ..Default::default()
                });
                vec![Part::new(chunk, 0, f.size)]
            };
            out.download_size += f.size;
            out.disk_size += f.size;
            let entry = FileEntry {
                path: rel,
                size: f.size,
                hash: md5_digest(&f.md5),
                parts,
                executable: f.executable,
            };
            if support {
                out.support.push(entry);
            } else {
                out.game.push(entry);
            }
        }
        let mut gone: Vec<_> = old_files
            .iter()
            .filter(|(p, _)| !present.contains(*p))
            .collect();
        gone.sort_by_key(|(p, _)| p.as_str());
        for (_, f) in gone {
            {
                let rel = ember_depot::path::normalize(&f.path)?;
                if f.support {
                    out.remove_support
                        .push(ember_depot::path::normalize(&format!(
                            "{}/{rel}",
                            f.product
                        ))?);
                } else {
                    out.remove_game.push(rel);
                }
            }
        }
        out.dirs = new
            .v1_dirs
            .iter()
            .filter_map(|d| ember_depot::path::normalize(d).ok())
            .collect();
        return Ok(out);
    }

    let old_items: HashMap<String, &DepotItem> = old
        .map(|o| {
            o.items
                .iter()
                .filter(|i| !matches!(i, DepotItem::Dir { .. }))
                .map(|i| (path_key(i.path()), i))
                .collect()
        })
        .unwrap_or_default();
    let mut present = HashSet::new();
    let mut counted = HashSet::new();
    for item in &new.items {
        present.insert(path_key(item.path()));
        match item {
            DepotItem::Dir { path } => {
                if let Ok(p) = ember_depot::path::normalize(path) {
                    out.dirs.push(p);
                }
            }
            DepotItem::Link { path, target } => out
                .links
                .push((ember_depot::path::normalize(path)?, target.clone())),
            DepotItem::File { chunks, .. } => {
                let (support, rel) = destination(item)?;
                let previous = old_items.get(&path_key(item.path()));
                let unchanged = previous
                    .is_some_and(|o| signature(o).is_some() && signature(o) == signature(item));
                if unchanged && exists(support, &rel) {
                    continue;
                }
                // The installed copy (its path may differ in case).
                let old_path = previous
                    .filter(|_| exists(support, &rel))
                    .and_then(|o| destination(o).ok().map(|(_, p)| (p, *o)));
                let reuse = old_path.as_ref().map(|(p, o)| (p.as_str(), *o));
                let Some(entry) = b.v2_entry(item, rel.clone(), reuse) else {
                    continue;
                };
                for (c, p) in chunks.iter().zip(&entry.parts) {
                    if p.reuse.is_none() && counted.insert(c.compressed_md5.clone()) {
                        out.download_size += c.compressed_size;
                    }
                }
                out.disk_size += entry.size;
                if support {
                    out.support.push(entry);
                } else {
                    out.game.push(entry);
                }
            }
        }
    }
    for (path, item) in &old_items {
        if !present.contains(path) {
            let (support, rel) = destination(item)?;
            if support {
                out.remove_support.push(rel);
            } else {
                out.remove_game.push(rel);
            }
        }
    }
    out.remove_game.sort();
    out.remove_support.sort();
    Ok(out)
}

/// What Ember records about an installed Linux build.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LinuxFiles {
    /// Install path → CRC-32.
    pub files: BTreeMap<String, u32>,
}

/// Plan a Linux install or update from installers (game and DLCs).
pub fn linux_plan(
    installers: &[(String, Installer)],
    old: Option<&LinuxFiles>,
    exists: &dyn Fn(&str) -> bool,
) -> Result<(Plan, LinuxFiles)> {
    let mut out = Plan::default();
    let mut record = LinuxFiles::default();
    for (product, inst) in installers {
        for e in &inst.entries {
            let Some(path) = e.install_path() else {
                continue;
            };
            let rel = ember_depot::path::normalize(path)?;
            record.files.insert(rel.clone(), e.crc32);
            if e.symlink {
                // The link target is the entry's (stored) content.
                let key = format!("link/{product}/{}", e.data_offset);
                out.links.push((rel, key));
                continue;
            }
            if old.and_then(|o| o.files.get(&rel)) == Some(&e.crc32) && exists(&rel) {
                continue;
            }
            let parts = if e.size == 0 {
                Vec::new()
            } else {
                vec![Part::new(
                    Arc::new(Chunk {
                        key: format!("linux/{product}/{}", e.data_offset),
                        urls: vec![inst.url.clone()],
                        download_size: e.compressed_size,
                        size: e.size,
                        codec: if e.method == 8 {
                            Codec::Deflate
                        } else {
                            Codec::Raw
                        },
                        range: Some((e.data_offset, e.compressed_size)),
                        ..Default::default()
                    }),
                    0,
                    e.size,
                )]
            };
            out.download_size += e.compressed_size;
            out.disk_size += e.size;
            out.game.push(FileEntry {
                path: rel,
                size: e.size,
                hash: Some(Digest::new(
                    Algorithm::Crc32,
                    e.crc32.to_be_bytes().to_vec(),
                )),
                parts,
                executable: e.executable,
            });
        }
    }
    if let Some(old) = old {
        out.remove_game = old
            .files
            .keys()
            .filter(|k| !record.files.contains_key(*k))
            .cloned()
            .collect();
    }
    Ok((out, record))
}

/// The target of a symbolic link stored in an installer.
pub async fn read_link_target(
    http: &reqwest::Client,
    inst: &Installer,
    entry: &crate::linux::Entry,
) -> Result<String> {
    let resp = http
        .get(&inst.url)
        .header(
            reqwest::header::RANGE,
            format!(
                "bytes={}-{}",
                entry.data_offset,
                entry.data_offset + entry.compressed_size.max(1) - 1
            ),
        )
        .send()
        .await?
        .error_for_status()?;
    Ok(String::from_utf8_lossy(&resp.bytes().await?).into_owned())
}

/// Replace the link placeholders of a [`linux_plan`] with the targets
/// stored in the installers.
pub async fn resolve_linux_links(
    http: &reqwest::Client,
    installers: &[(String, Installer)],
    plan: &mut Plan,
) -> Result<()> {
    for (_, target) in &mut plan.links {
        let Some(rest) = target.strip_prefix("link/") else {
            continue;
        };
        let (product, offset) = rest.rsplit_once('/').context("invalid link placeholder")?;
        let offset: u64 = offset.parse().context("invalid link placeholder")?;
        let (inst, entry) = installers
            .iter()
            .filter(|(p, _)| p == product)
            .find_map(|(_, i)| {
                i.entries
                    .iter()
                    .find(|e| e.data_offset == offset)
                    .map(|e| (i, e))
            })
            .context("a link of the installer was not found")?;
        *target = read_link_target(http, inst, entry).await?;
    }
    Ok(())
}

/// Resolves GOG chunk keys to signed addresses using the products' secure
/// links, fetched once per product and again when they expire.
pub struct Signer {
    pub client: Client,
    /// Product id → generation of its depots.
    links: tokio::sync::Mutex<HashMap<(String, u32), Vec<Endpoint>>>,
    issued: std::sync::Mutex<HashSet<String>>,
}

impl Signer {
    /// `client` must carry a valid Galaxy token.
    pub fn new(client: Client) -> Self {
        Self {
            client,
            links: Default::default(),
            issued: Default::default(),
        }
    }

    async fn endpoints(
        &self,
        product: &str,
        generation: u32,
        fresh: bool,
    ) -> Result<Vec<Endpoint>> {
        let key = (product.to_string(), generation);
        let mut links = self.links.lock().await;
        if !fresh && let Some(e) = links.get(&key) {
            return Ok(e.clone());
        }
        let e = if product == REDIST {
            self.client.dependency_link().await?
        } else {
            self.client.secure_link(product, generation).await?
        };
        links.insert(key, e.clone());
        Ok(e)
    }
}

impl ember_depot::UrlSigner for Signer {
    fn sign<'a>(&'a self, keys: &'a [String]) -> BoxFuture<'a, Result<HashMap<String, String>>> {
        Box::pin(async move {
            let mut out = HashMap::new();
            for key in keys {
                let mut parts = key.splitn(3, '/');
                let (Some(kind), Some(product), Some(rest)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                // A key signed before has expired: refresh its product's links.
                let fresh = !self
                    .issued
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key.clone());
                let (generation, suffix) = match kind {
                    "v1" => (1, "main.bin".to_string()),
                    _ => (2, galaxy_path(rest)),
                };
                let endpoints = self.endpoints(product, generation, fresh).await?;
                let url = endpoints
                    .iter()
                    .find_map(|e| e.url_for(&suffix))
                    .context("GOG gave no usable download address")?;
                out.insert(key.clone(), url);
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::content::V2Chunk;

    fn file(path: &str, chunks: &[(&str, u64)], md5: Option<&str>) -> DepotItem {
        DepotItem::File {
            product: "1".into(),
            path: path.into(),
            chunks: chunks
                .iter()
                .map(|(m, size)| V2Chunk {
                    md5: m.to_string(),
                    size: *size,
                    compressed_md5: format!("z{m}"),
                    compressed_size: size / 2,
                })
                .collect(),
            md5: md5.map(str::to_string),
            sha256: None,
            executable: false,
            support: false,
        }
    }

    fn v2(items: Vec<DepotItem>) -> BuildFiles {
        BuildFiles {
            generation: 2,
            items,
            ..Default::default()
        }
    }

    const A: &str = "0cc175b9c0f1b6a831c399e269772661";
    const B: &str = "92eb5ffee6ae2fec3ad71c777531578f";
    const C: &str = "4a8a08f09d37b73795649038408b5f33";

    #[test]
    fn plans_v2_installs_and_updates() {
        let yes = |_: bool, _: &str| true;
        let mut support = file("setup.exe", &[(C, 10)], None);
        if let DepotItem::File { support: s, .. } = &mut support {
            *s = true;
        }
        let new = v2(vec![
            file("bin\\game.exe", &[(A, 100), (B, 50)], Some(A)),
            file("data.pak", &[(A, 100)], None),
            file("empty.txt", &[], None),
            support,
            DepotItem::Link {
                path: "lib/a.so".into(),
                target: "a.so.1".into(),
            },
            DepotItem::Dir {
                path: "saves".into(),
            },
        ]);
        let p = plan(&new, None, &yes).unwrap();
        let paths: Vec<&str> = p.game.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["bin/game.exe", "data.pak", "empty.txt"]);
        assert_eq!(p.support[0].path, "1/setup.exe");
        assert_eq!(p.links, [("lib/a.so".to_string(), "a.so.1".to_string())]);
        assert_eq!(p.dirs, ["saves"]);
        // Chunk A is shared by two files and downloaded once.
        assert!(Arc::ptr_eq(
            &p.game[0].parts[0].chunk,
            &p.game[1].parts[0].chunk
        ));
        assert_eq!(p.download_size, 50 + 25 + 5);
        assert_eq!(p.game[0].hash.as_ref().unwrap().hex(), A);
        assert_eq!(
            p.game[1].hash.as_ref().unwrap().hex(),
            A,
            "single chunk: chunk md5"
        );
        assert!(p.game[2].parts.is_empty());
        assert_eq!(p.game[0].parts[0].chunk.key, format!("v2/1/z{A}"));

        // Update: game.exe gains a chunk (A reused from disk), data.pak is
        // unchanged, empty.txt is gone.
        let newer = v2(vec![
            file("bin/game.exe", &[(A, 100), (C, 30)], Some(C)),
            file("data.pak", &[(A, 100)], None),
        ]);
        let u = plan(&newer, Some(&new), &yes).unwrap();
        assert_eq!(u.game.len(), 1);
        let exe = &u.game[0];
        assert_eq!(
            exe.parts[0].reuse,
            Some(Reuse {
                path: "bin/game.exe".into(),
                offset: 0
            })
        );
        assert!(exe.parts[1].reuse.is_none());
        assert_eq!(u.download_size, 15);
        assert_eq!(u.remove_game, ["empty.txt", "lib/a.so"]);
        assert_eq!(u.remove_support, ["1/setup.exe"]);

        // A missing unchanged file is fetched again, without reuse.
        let gone = |_: bool, p: &str| p != "data.pak";
        let u = plan(&newer, Some(&new), &gone).unwrap();
        assert!(
            u.game
                .iter()
                .any(|f| f.path == "data.pak" && f.parts[0].reuse.is_none())
        );
    }

    #[test]
    fn plans_v1_files() {
        let files = BuildFiles {
            generation: 1,
            v1_files: vec![
                V1File {
                    product: "10".into(),
                    path: "game.exe".into(),
                    offset: 100,
                    size: 50,
                    md5: A.into(),
                    executable: true,
                    support: false,
                },
                V1File {
                    product: "10".into(),
                    path: "setup.ini".into(),
                    offset: 0,
                    size: 0,
                    md5: String::new(),
                    executable: false,
                    support: true,
                },
            ],
            v1_dirs: vec!["data".into()],
            ..Default::default()
        };
        let p = plan(&files, None, &|_, _| true).unwrap();
        let exe = &p.game[0];
        assert_eq!(exe.parts[0].chunk.range, Some((100, 50)));
        assert_eq!(exe.parts[0].chunk.key, "v1/10/100/50");
        assert!(exe.executable);
        assert_eq!(p.support[0].path, "10/setup.ini");
        assert_eq!(p.dirs, ["data"]);
        let again = plan(&files, Some(&files), &|_, _| true).unwrap();
        assert!(again.game.is_empty());
    }

    #[tokio::test]
    async fn plans_and_downloads_linux_installers() {
        let big = vec![3u8; 50_000];
        let data = crate::linux::tests::installer(&[
            ("data/noarch/start.sh", b"#!/bin/sh\necho hi\n", false, None),
            ("data/noarch/game/big.pak", &big, true, None),
            ("data/noarch/game/lib.so", b"", false, Some("lib.so.1")),
            ("scripts/postinst", b"x", false, None),
        ]);
        let served = data.clone();
        let server = ember_depot::test_server::TestServer::start(move |req| {
            ember_depot::test_server::serve_bytes(req, &served, true)
        });
        let http = reqwest::Client::new();
        let inst = crate::linux::read_installer(&http, &server.url("/i.sh"))
            .await
            .unwrap();
        let installers = vec![("1".to_string(), inst)];
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Game");
        let exists = |rel: &str| ember_depot::path::join(&root, rel).is_file();
        let (mut p, files) = linux_plan(&installers, None, &exists).unwrap();
        resolve_linux_links(&http, &installers, &mut p)
            .await
            .unwrap();
        assert_eq!(
            p.links,
            [("game/lib.so".to_string(), "lib.so.1".to_string())]
        );
        assert_eq!(files.files.len(), 3);
        let finished = ember_depot::download(
            &http,
            ember_depot::Job {
                root: &root,
                files: &p.game,
                remove: &p.remove_game,
            },
            &Default::default(),
            ember_depot::ignore_progress(),
            &Default::default(),
        )
        .await
        .unwrap();
        assert_eq!(finished, ember_depot::Finished::Done);
        assert_eq!(std::fs::read(root.join("game/big.pak")).unwrap(), big);
        assert_eq!(
            std::fs::read(root.join("start.sh")).unwrap(),
            b"#!/bin/sh\necho hi\n"
        );
        let verified = ember_depot::verify(
            &root,
            &p.game,
            0,
            ember_depot::ignore_progress(),
            &Default::default(),
        )
        .await
        .unwrap();
        assert_eq!(verified, ember_depot::Verified::Checked(Vec::new()));

        // Nothing changed: an update plans nothing; a file gone from the
        // new installer is removed.
        let exists = |rel: &str| ember_depot::path::join(&root, rel).is_file();
        let (again, _) = linux_plan(&installers, Some(&files), &exists).unwrap();
        assert!(again.game.is_empty());
        let mut older = files.clone();
        older.files.insert("game/gone.txt".into(), 1);
        let (update, _) = linux_plan(&installers, Some(&older), &exists).unwrap();
        assert_eq!(update.remove_game, ["game/gone.txt"]);
    }

    #[tokio::test]
    async fn signs_keys_with_secure_links() {
        let server = ember_depot::test_server::TestServer::start(|req| {
            let p = &req.path;
            if p.starts_with("/products/1/secure_link") {
                let gen1 = p.contains("type=depot");
                ember_depot::test_server::Resp::json(json!({"urls": [{
                    "url_format": "{base}{path}?sig={sig}",
                    "parameters": {"base": "https://cdn", "path": if gen1 { "/v1/1" } else { "/v2/1" }, "sig": "s"}}]}))
            } else if p.starts_with("/open_link") {
                ember_depot::test_server::Resp::json(
                    json!({"urls": [{"url": "https://redist/store"}]}),
                )
            } else {
                ember_depot::test_server::Resp::status(404)
            }
        });
        let client = Client::new(crate::content::Endpoints {
            content_system: server.base(),
            ..Default::default()
        })
        .unwrap();
        let signer = Signer::new(client);
        let keys = vec![
            format!("v2/1/z{A}"),
            "v1/1/100/50".to_string(),
            format!("v2/{REDIST}/abcdef"),
        ];
        let urls = ember_depot::UrlSigner::sign(&signer, &keys).await.unwrap();
        assert_eq!(urls[&keys[0]], format!("https://cdn/v2/1/z0/cc/z{A}?sig=s"));
        assert_eq!(urls[&keys[1]], "https://cdn/v1/1/main.bin?sig=s");
        assert_eq!(urls[&keys[2]], "https://redist/store/ab/cd/abcdef");
        let before = server.requests().len();
        ember_depot::UrlSigner::sign(&signer, &keys[..1])
            .await
            .unwrap();
        assert_eq!(
            server.requests().len(),
            before + 1,
            "re-signing refreshes the link"
        );
    }
}
