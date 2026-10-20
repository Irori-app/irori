//! Epic build manifests: the binary format (zlib-compressed, optionally
//! AES-GCM encrypted since feature level 24) and the older JSON format.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::io::{Read as _, Write as _};

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha1::{Digest as _, Sha1};

use crate::binary::{Reader, Writer};
use crate::crypto;

const MAGIC: u32 = 0x44BE_C00C;

/// A 128-bit GUID stored as four little-endian u32s.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Guid(pub [u32; 4]);

impl Guid {
    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self([r.u32()?, r.u32()?, r.u32()?, r.u32()?]))
    }

    pub(crate) fn write(&self, w: &mut Writer) {
        for v in self.0 {
            w.u32(v);
        }
    }

    pub(crate) fn le_bytes(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        for (i, v) in self.0.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// Upper-case hex of the four words (`%08X` each), the key of secrets.
    pub fn hex_upper(&self) -> String {
        self.0.iter().map(|v| format!("{v:08X}")).collect()
    }

    /// Parse the JSON manifest's 32-digit big-endian hex form.
    pub(crate) fn from_json_hex(s: &str) -> Result<Self> {
        let raw = hex::decode(s.trim()).context("invalid GUID")?;
        if raw.len() != 16 {
            bail!("invalid GUID length");
        }
        let word = |i: usize| u32::from_be_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]);
        Ok(Self([word(0), word(4), word(8), word(12)]))
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 4]
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.0;
        write!(f, "{a:08x}-{b:08x}-{c:08x}-{d:08x}")
    }
}

/// Build metadata.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub data_version: u8,
    pub feature_level: u32,
    pub is_file_data: bool,
    pub app_id: u32,
    pub app_name: String,
    pub build_version: String,
    pub launch_exe: String,
    pub launch_command: String,
    pub prereq_ids: Vec<String>,
    pub prereq_name: String,
    pub prereq_path: String,
    pub prereq_args: String,
    /// Stored since data version 1; derived otherwise (see [`Meta::build_id`]).
    pub(crate) stored_build_id: String,
    pub uninstall_action_path: String,
    pub uninstall_action_args: String,
}

impl Meta {
    /// The build id used for delta manifests.
    pub fn build_id(&self) -> String {
        if !self.stored_build_id.is_empty() {
            return self.stored_build_id.clone();
        }
        let mut h = Sha1::new();
        h.update(self.app_id.to_le_bytes());
        h.update(self.app_name.as_bytes());
        h.update(self.build_version.as_bytes());
        h.update(self.launch_exe.as_bytes());
        h.update(self.launch_command.as_bytes());
        URL_SAFE_NO_PAD.encode(h.finalize())
    }
}

/// One downloadable chunk.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChunkInfo {
    pub guid: Guid,
    /// Epic's rolling hash of the content (part of the CDN path).
    pub hash: u64,
    pub sha1: [u8; 20],
    pub group: u8,
    /// Decoded size.
    pub window_size: u32,
    /// Download size.
    pub file_size: u64,
    /// Feature level 22+: key id of an encrypted chunk.
    pub secret_guid: Guid,
    pub window_size_compressed: u32,
    pub encryption_tag: [u8; 16],
}

/// A slice of a chunk that makes up part of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkPart {
    pub guid: Guid,
    pub offset: u32,
    pub size: u32,
    /// Position of the part in its file.
    pub file_offset: u64,
}

/// One file of a build.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileManifest {
    /// Path with `/` separators, as stored.
    pub filename: String,
    pub symlink_target: String,
    pub sha1: [u8; 20],
    pub flags: u8,
    pub install_tags: Vec<String>,
    pub parts: Vec<ChunkPart>,
    pub size: u64,
    pub md5: Option<[u8; 16]>,
    pub mime_type: String,
    pub sha256: Option<[u8; 32]>,
}

impl FileManifest {
    pub fn is_executable(&self) -> bool {
        self.flags & 0x4 != 0
    }

    /// Whether the file belongs to an install with the selected `tags`
    /// (untagged files always do; an empty selection means everything).
    pub fn selected_by(&self, tags: &[String]) -> bool {
        if tags.is_empty() || self.install_tags.is_empty() {
            return true;
        }
        self.install_tags.iter().any(|t| tags.contains(t))
    }
}

/// The encrypted part of a feature-level-24 manifest.
#[derive(Debug, Clone, PartialEq, Default)]
struct Encrypted {
    secret_guid: Guid,
    tag: [u8; 16],
    iv: Vec<u8>,
    compressed: bool,
    ciphertext: Vec<u8>,
}

/// A parsed build manifest.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Manifest {
    /// Serialisation version from the header.
    pub version: u32,
    pub meta: Meta,
    pub chunks: Vec<ChunkInfo>,
    pub files: Vec<FileManifest>,
    pub custom_fields: Vec<(String, String)>,
    encrypted: Option<Encrypted>,
    /// Version used for chunk paths (the feature level).
    chunk_version: u32,
}

/// Secrets (key id → hex key) for encrypted builds.
pub type Secrets = HashMap<String, String>;

fn chunk_dir(version: u32) -> &'static str {
    match version {
        v if v >= 22 => "ChunksV5",
        v if v >= 15 => "ChunksV4",
        v if v >= 6 => "ChunksV3",
        v if v >= 3 => "ChunksV2",
        _ => "Chunks",
    }
}

impl Manifest {
    /// Parse a binary or JSON manifest, decrypting it with `secrets` if it is
    /// encrypted.
    pub fn parse(data: &[u8], secrets: &Secrets) -> Result<Self> {
        let mut m = if data.first() == Some(&b'{') {
            crate::json_manifest::parse(data)?
        } else {
            Self::parse_binary(data)?
        };
        m.decrypt(secrets)?;
        Ok(m)
    }

    fn parse_binary(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        if r.u32()? != MAGIC {
            bail!("not an Epic manifest");
        }
        let header_size = r.u32()? as usize;
        let size_uncompressed = r.u32()? as usize;
        let _size_compressed = r.u32()?;
        let sha: [u8; 20] = r.array()?;
        let stored_as = r.u8()?;
        let version = r.u32()?;
        let mut header_secret = None;
        if version >= 22 {
            let guid = Guid::read(&mut r)?;
            let tag: [u8; 16] = r.array()?;
            header_secret = Some((guid, tag));
        }
        r.seek(header_size)?;
        let body = r.rest();
        let body = if stored_as & 0x1 != 0 {
            let mut out = Vec::with_capacity(size_uncompressed.min(512 << 20));
            flate2::read::ZlibDecoder::new(body)
                .take(1 << 30)
                .read_to_end(&mut out)
                .context("the manifest is damaged")?;
            if Sha1::digest(&out).as_slice() != sha {
                bail!("the manifest checksum does not match");
            }
            out
        } else {
            body.to_vec()
        };

        let mut r = Reader::new(&body);
        let meta = read_meta(&mut r)?;
        let chunk_version = meta.feature_level;
        let chunks = read_cdl(&mut r, chunk_version)?;
        let files = read_fml(&mut r)?;
        let custom_fields = read_custom_fields(&mut r)?;
        let mut encrypted = None;
        if meta.feature_level >= 24 && r.remaining() > 0 {
            let mut e = read_encrypted(&mut r)?;
            if stored_as & 0x2 != 0 {
                let (guid, tag) = header_secret.context("encrypted manifest without key id")?;
                e.secret_guid = guid;
                e.tag = tag;
                encrypted = Some(e);
            }
        }
        Ok(Self {
            version,
            meta,
            chunks,
            files,
            custom_fields,
            encrypted,
            chunk_version,
        })
    }

    /// Whether the manifest still needs [`Manifest::decrypt`].
    pub fn is_encrypted(&self) -> bool {
        self.encrypted.is_some()
    }

    /// Restore the encrypted fields (launch command, prerequisites and
    /// file names) with the key from `secrets`.
    pub fn decrypt(&mut self, secrets: &Secrets) -> Result<()> {
        let Some(enc) = self.encrypted.take() else {
            return Ok(());
        };
        let key_id = enc.secret_guid.hex_upper();
        let key =
            crypto::find_key(secrets, &key_id).context("Cannot decrypt the game's manifest")?;
        let mut plain = crypto::decrypt(&key, &enc.iv, &enc.ciphertext, &enc.tag)
            .context("Cannot decrypt the game's manifest")?;
        if enc.compressed {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(plain.as_slice())
                .take(1 << 30)
                .read_to_end(&mut out)?;
            plain = out;
        }
        let mut r = Reader::new(&plain);
        let m = &mut self.meta;
        m.launch_exe = r.fstring()?;
        m.launch_command = r.fstring()?;
        let n = r.count(4)?;
        m.prereq_ids = (0..n).map(|_| r.fstring()).collect::<Result<_>>()?;
        m.prereq_name = r.fstring()?;
        m.prereq_path = r.fstring()?;
        m.prereq_args = r.fstring()?;
        m.uninstall_action_path = r.fstring()?;
        m.uninstall_action_args = r.fstring()?;
        for f in &mut self.files {
            f.filename = r.fstring()?;
            f.symlink_target = r.fstring()?;
        }
        Ok(())
    }

    /// CDN path of a chunk (below the build's base URL).
    pub fn chunk_path(&self, c: &ChunkInfo) -> String {
        if self.chunk_version >= 22 {
            let b64 = |b: &[u8]| URL_SAFE_NO_PAD.encode(b);
            return format!(
                "{}/{}/{:02}/{}_{}.chunk",
                chunk_dir(self.chunk_version),
                b64(&c.secret_guid.le_bytes()),
                c.group,
                b64(&c.hash.to_le_bytes()),
                b64(&c.guid.le_bytes()),
            );
        }
        format!(
            "{}/{:02}/{:016X}_{}.chunk",
            chunk_dir(self.chunk_version),
            c.group,
            c.hash,
            c.guid.hex_upper()
        )
    }

    /// Chunks by GUID.
    pub fn chunk_map(&self) -> HashMap<Guid, &ChunkInfo> {
        self.chunks.iter().map(|c| (c.guid, c)).collect()
    }

    pub fn file(&self, name: &str) -> Option<&FileManifest> {
        self.files.iter().find(|f| f.filename == name)
    }

    /// Total size of the files selected by `tags`.
    pub fn install_size(&self, tags: &[String]) -> u64 {
        self.files
            .iter()
            .filter(|f| f.selected_by(tags))
            .map(|f| f.size)
            .sum()
    }

    /// Bytes to download for the files selected by `tags`.
    pub fn download_size(&self, tags: &[String]) -> u64 {
        let map = self.chunk_map();
        let needed: BTreeSet<Guid> = self
            .files
            .iter()
            .filter(|f| f.selected_by(tags))
            .flat_map(|f| f.parts.iter().map(|p| p.guid))
            .collect();
        needed
            .iter()
            .filter_map(|g| map.get(g))
            .map(|c| c.file_size)
            .sum()
    }

    /// All install tags used by the build.
    pub fn install_tags(&self) -> Vec<String> {
        let tags: BTreeSet<&String> = self.files.iter().flat_map(|f| &f.install_tags).collect();
        tags.into_iter().cloned().collect()
    }

    /// Replace and add files and chunks from an optimised delta manifest.
    pub fn apply_delta(&mut self, delta: Manifest) {
        let mut by_name: HashMap<String, usize> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.filename.clone(), i))
            .collect();
        for f in delta.files {
            match by_name.get(&f.filename) {
                Some(&i) => self.files[i] = f,
                None => {
                    by_name.insert(f.filename.clone(), self.files.len());
                    self.files.push(f);
                }
            }
        }
        let known: std::collections::HashSet<Guid> = self.chunks.iter().map(|c| c.guid).collect();
        self.chunks.extend(
            delta
                .chunks
                .into_iter()
                .filter(|c| !known.contains(&c.guid)),
        );
    }

    /// Serialise as a binary manifest (feature level below 22: unencrypted
    /// builds and cloud saves).
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        if self.meta.feature_level >= 22 || self.encrypted.is_some() {
            bail!("writing manifests of feature level 22+ is not supported");
        }
        let version = self.meta.feature_level.max(17);
        let mut w = Writer::default();
        write_meta(&mut w, &self.meta, version);
        write_cdl(&mut w, &self.chunks);
        write_fml(&mut w, &self.files);
        write_custom_fields(&mut w, &self.custom_fields);
        let body = w.buf;
        let sha = Sha1::digest(&body);
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&body)?;
        let compressed = z.finish()?;

        let mut out = Writer::default();
        out.u32(MAGIC);
        out.u32(41);
        out.u32(body.len() as u32);
        out.u32(compressed.len() as u32);
        out.bytes(&sha);
        out.u8(1);
        out.u32(version);
        out.bytes(&compressed);
        Ok(out.buf)
    }

    /// A new, empty manifest (cloud saves).
    pub fn new(app_name: String, build_version: String) -> Self {
        Self {
            version: 18,
            meta: Meta {
                feature_level: 18,
                app_name,
                build_version,
                ..Default::default()
            },
            chunk_version: 18,
            ..Default::default()
        }
    }

    pub(crate) fn from_json_parts(
        version: u32,
        meta: Meta,
        chunks: Vec<ChunkInfo>,
        files: Vec<FileManifest>,
        custom_fields: Vec<(String, String)>,
    ) -> Self {
        Self {
            version,
            chunk_version: version,
            meta,
            chunks,
            files,
            custom_fields,
            encrypted: None,
        }
    }
}

fn read_meta(r: &mut Reader<'_>) -> Result<Meta> {
    let start = r.pos();
    let size = r.u32()? as usize;
    let mut m = Meta {
        data_version: r.u8()?,
        feature_level: r.u32()?,
        is_file_data: r.u8()? == 1,
        app_id: r.u32()?,
        ..Default::default()
    };
    m.app_name = r.fstring()?;
    m.build_version = r.fstring()?;
    m.launch_exe = r.fstring()?;
    m.launch_command = r.fstring()?;
    let n = r.count(4)?;
    m.prereq_ids = (0..n).map(|_| r.fstring()).collect::<Result<_>>()?;
    m.prereq_name = r.fstring()?;
    m.prereq_path = r.fstring()?;
    m.prereq_args = r.fstring()?;
    if m.data_version >= 1 {
        m.stored_build_id = r.fstring()?;
    }
    if m.data_version >= 2 {
        m.uninstall_action_path = r.fstring()?;
        m.uninstall_action_args = r.fstring()?;
    }
    r.seek(start + size)?;
    Ok(m)
}

fn write_meta(w: &mut Writer, m: &Meta, version: u32) {
    let at = w.start_size();
    w.u8(0);
    w.u32(version);
    w.u8(m.is_file_data as u8);
    w.u32(m.app_id);
    w.fstring(&m.app_name);
    w.fstring(&m.build_version);
    w.fstring(&m.launch_exe);
    w.fstring(&m.launch_command);
    w.u32(m.prereq_ids.len() as u32);
    for id in &m.prereq_ids {
        w.fstring(id);
    }
    w.fstring(&m.prereq_name);
    w.fstring(&m.prereq_path);
    w.fstring(&m.prereq_args);
    w.finish_size(at);
}

fn read_cdl(r: &mut Reader<'_>, version: u32) -> Result<Vec<ChunkInfo>> {
    let start = r.pos();
    let size = r.u32()? as usize;
    let _list_version = r.u8()?;
    let n = r.count(16 + 8 + 20 + 1 + 4 + 8)?;
    let mut chunks = vec![ChunkInfo::default(); n];
    for c in &mut chunks {
        c.guid = Guid::read(r)?;
    }
    for c in &mut chunks {
        c.hash = r.u64()?;
    }
    for c in &mut chunks {
        c.sha1 = r.array()?;
    }
    for c in &mut chunks {
        c.group = r.u8()?;
    }
    for c in &mut chunks {
        c.window_size = r.u32()?;
    }
    for c in &mut chunks {
        c.file_size = u64::try_from(r.i64()?).context("negative chunk size")?;
    }
    if version >= 22 {
        for c in &mut chunks {
            c.secret_guid = Guid::read(r)?;
        }
        for c in &mut chunks {
            c.window_size_compressed = r.u32()?;
        }
        for c in &mut chunks {
            c.encryption_tag = r.array()?;
        }
    }
    r.seek(start + size)?;
    Ok(chunks)
}

fn write_cdl(w: &mut Writer, chunks: &[ChunkInfo]) {
    let at = w.start_size();
    w.u8(0);
    w.u32(chunks.len() as u32);
    for c in chunks {
        c.guid.write(w);
    }
    for c in chunks {
        w.u64(c.hash);
    }
    for c in chunks {
        w.bytes(&c.sha1);
    }
    for c in chunks {
        w.u8(c.group);
    }
    for c in chunks {
        w.u32(c.window_size);
    }
    for c in chunks {
        w.i64(c.file_size as i64);
    }
    w.finish_size(at);
}

fn read_fml(r: &mut Reader<'_>) -> Result<Vec<FileManifest>> {
    let start = r.pos();
    let size = r.u32()? as usize;
    let list_version = r.u8()?;
    let n = r.count(4 + 4 + 20 + 1 + 4 + 4)?;
    let mut files = vec![FileManifest::default(); n];
    for f in &mut files {
        f.filename = r.fstring()?;
    }
    for f in &mut files {
        f.symlink_target = r.fstring()?;
    }
    for f in &mut files {
        f.sha1 = r.array()?;
    }
    for f in &mut files {
        f.flags = r.u8()?;
    }
    for f in &mut files {
        let n = r.count(4)?;
        f.install_tags = (0..n).map(|_| r.fstring()).collect::<Result<_>>()?;
    }
    for f in &mut files {
        let n = r.count(28)?;
        f.parts.reserve_exact(n);
        let mut offset = 0u64;
        for _ in 0..n {
            let part_start = r.pos();
            let part_size = r.u32()? as usize;
            let guid = Guid::read(r)?;
            let chunk_offset = r.u32()?;
            let len = r.u32()?;
            f.parts.push(ChunkPart {
                guid,
                offset: chunk_offset,
                size: len,
                file_offset: offset,
            });
            offset += u64::from(len);
            if part_size > 28 {
                r.seek(part_start + part_size)?;
            }
        }
        f.size = offset;
    }
    if list_version >= 1 {
        for f in &mut files {
            if r.u32()? != 0 {
                f.md5 = Some(r.array()?);
            }
        }
        for f in &mut files {
            f.mime_type = r.fstring()?;
        }
    }
    if list_version >= 2 {
        for f in &mut files {
            f.sha256 = Some(r.array()?);
        }
    }
    r.seek(start + size)?;
    Ok(files)
}

fn write_fml(w: &mut Writer, files: &[FileManifest]) {
    let at = w.start_size();
    w.u8(0);
    w.u32(files.len() as u32);
    for f in files {
        w.fstring(&f.filename);
    }
    for f in files {
        w.fstring(&f.symlink_target);
    }
    for f in files {
        w.bytes(&f.sha1);
    }
    for f in files {
        w.u8(f.flags);
    }
    for f in files {
        w.u32(f.install_tags.len() as u32);
        for t in &f.install_tags {
            w.fstring(t);
        }
    }
    for f in files {
        w.u32(f.parts.len() as u32);
        for p in &f.parts {
            w.u32(28);
            p.guid.write(w);
            w.u32(p.offset);
            w.u32(p.size);
        }
    }
    w.finish_size(at);
}

fn read_custom_fields(r: &mut Reader<'_>) -> Result<Vec<(String, String)>> {
    if r.remaining() == 0 {
        return Ok(Vec::new());
    }
    let start = r.pos();
    let size = r.u32()? as usize;
    let _version = r.u8()?;
    let n = r.count(8)?;
    let keys: Vec<String> = (0..n).map(|_| r.fstring()).collect::<Result<_>>()?;
    let values: Vec<String> = (0..n).map(|_| r.fstring()).collect::<Result<_>>()?;
    r.seek(start + size)?;
    Ok(keys.into_iter().zip(values).collect())
}

fn write_custom_fields(w: &mut Writer, fields: &[(String, String)]) {
    let at = w.start_size();
    w.u8(0);
    w.u32(fields.len() as u32);
    for (k, _) in fields {
        w.fstring(k);
    }
    for (_, v) in fields {
        w.fstring(v);
    }
    w.finish_size(at);
}

fn read_encrypted(r: &mut Reader<'_>) -> Result<Encrypted> {
    let start = r.pos();
    let size = r.u32()? as usize;
    let _version = r.u8()?;
    let cipher_size = r.u32()? as usize;
    let data = r.bytes(cipher_size)?;
    let mut d = Reader::new(data);
    let header_start = d.pos();
    let header_size = d.u32()? as usize;
    let _header_version = d.u32()?;
    let stored_as = d.u8()?;
    let _uncompressed = d.u32()?;
    let _compressed = d.u32()?;
    let iv_len = d.u32()? as usize;
    let iv = d.bytes(iv_len)?.to_vec();
    d.seek(header_start + header_size)?;
    let ct_len = d.u32()? as usize;
    let ciphertext = d.bytes(ct_len)?.to_vec();
    r.seek(start + size)?;
    Ok(Encrypted {
        compressed: stored_as & 0x1 != 0,
        iv,
        ciphertext,
        ..Default::default()
    })
}

/// How a new build differs from the installed one.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    pub added: BTreeSet<String>,
    pub changed: BTreeSet<String>,
    pub removed: BTreeSet<String>,
    pub unchanged: BTreeSet<String>,
}

impl Comparison {
    pub fn new(new: &Manifest, old: Option<&Manifest>) -> Self {
        let mut c = Self::default();
        let Some(old) = old else {
            c.added = new.files.iter().map(|f| f.filename.clone()).collect();
            return c;
        };
        let mut old_files: HashMap<&str, &[u8; 20]> = old
            .files
            .iter()
            .map(|f| (f.filename.as_str(), &f.sha1))
            .collect();
        for f in &new.files {
            match old_files.remove(f.filename.as_str()) {
                Some(h) if *h == f.sha1 => c.unchanged.insert(f.filename.clone()),
                Some(_) => c.changed.insert(f.filename.clone()),
                None => c.added.insert(f.filename.clone()),
            };
        }
        c.removed = old_files.into_keys().map(str::to_string).collect();
        c
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn guid(n: u32) -> Guid {
        Guid([n, n + 1, n + 2, n + 3])
    }

    pub(crate) fn sample(version: u32) -> Manifest {
        let mut m = Manifest::new("Sugar".into(), "1.0".into());
        m.meta.feature_level = version;
        m.chunk_version = version;
        m.meta.launch_exe = "Bin/Game.exe".into();
        m.meta.launch_command = "-epic".into();
        m.meta.prereq_ids = vec!["p".into()];
        m.meta.prereq_name = "Prereqs".into();
        m.meta.prereq_path = "Redist\\setup.exe".into();
        m.chunks = vec![
            ChunkInfo {
                guid: guid(1),
                hash: 0x1122_3344_5566_7788,
                sha1: [1; 20],
                group: 7,
                window_size: 1 << 20,
                file_size: 1000,
                ..Default::default()
            },
            ChunkInfo {
                guid: guid(5),
                hash: 2,
                sha1: [2; 20],
                group: 99,
                window_size: 1 << 20,
                file_size: 500,
                ..Default::default()
            },
        ];
        m.files = vec![
            FileManifest {
                filename: "Bin/Game.exe".into(),
                sha1: [9; 20],
                flags: 0x4,
                parts: vec![ChunkPart {
                    guid: guid(1),
                    offset: 0,
                    size: 100,
                    file_offset: 0,
                }],
                size: 100,
                ..Default::default()
            },
            FileManifest {
                filename: "Data/de.pak".into(),
                sha1: [8; 20],
                install_tags: vec!["de".into()],
                parts: vec![
                    ChunkPart {
                        guid: guid(1),
                        offset: 100,
                        size: 50,
                        file_offset: 0,
                    },
                    ChunkPart {
                        guid: guid(5),
                        offset: 0,
                        size: 25,
                        file_offset: 50,
                    },
                ],
                size: 75,
                ..Default::default()
            },
        ];
        m.custom_fields = vec![("CloudSaveFolder".into(), "{AppData}/Sugar".into())];
        m
    }

    #[test]
    fn binary_round_trip() {
        let m = sample(18);
        let bytes = m.to_bytes().unwrap();
        let back = Manifest::parse(&bytes, &Secrets::new()).unwrap();
        assert_eq!(back.meta.app_name, "Sugar");
        assert_eq!(back.meta.launch_exe, "Bin/Game.exe");
        assert_eq!(back.meta.prereq_ids, ["p"]);
        assert_eq!(back.chunks, m.chunks);
        assert_eq!(back.files, m.files);
        assert_eq!(back.custom_fields, m.custom_fields);
        assert!(back.files[0].is_executable());
        assert_eq!(back.install_size(&[]), 175);
        assert_eq!(back.install_size(&["fr".into()]), 100);
        assert_eq!(back.download_size(&[]), 1500);
        assert_eq!(back.download_size(&["fr".into()]), 1000);
        assert_eq!(back.install_tags(), ["de"]);
        assert_eq!(back.meta.build_id().len(), 27);

        let mut damaged = bytes.clone();
        let last = damaged.len() - 3;
        damaged[last] ^= 0xff;
        assert!(Manifest::parse(&damaged, &Secrets::new()).is_err());
        assert!(Manifest::parse(b"nope", &Secrets::new()).is_err());
    }

    #[test]
    fn chunk_paths_follow_epic() {
        let m = sample(18);
        assert_eq!(
            m.chunk_path(&m.chunks[0]),
            "ChunksV4/07/1122334455667788_00000001000000020000000300000004.chunk"
        );
        let mut v3 = sample(12);
        v3.chunk_version = 12;
        assert!(v3.chunk_path(&v3.chunks[1]).starts_with("ChunksV3/99/"));
        let mut v5 = sample(22);
        v5.chunk_version = 22;
        v5.chunks[0].secret_guid = guid(9);
        let p = v5.chunk_path(&v5.chunks[0]);
        assert!(p.starts_with("ChunksV5/CQAAAAoAAAALAAAADAAAAA/07/"), "{p}");
        assert!(p.ends_with(".chunk"));
    }

    #[test]
    fn compares_and_applies_deltas() {
        let old = sample(18);
        let mut new = sample(18);
        new.files[1].sha1 = [7; 20];
        new.files.push(FileManifest {
            filename: "New.txt".into(),
            ..Default::default()
        });
        new.files.remove(0);
        let c = Comparison::new(&new, Some(&old));
        assert_eq!(c.changed.iter().collect::<Vec<_>>(), ["Data/de.pak"]);
        assert_eq!(c.added.iter().collect::<Vec<_>>(), ["New.txt"]);
        assert_eq!(c.removed.iter().collect::<Vec<_>>(), ["Bin/Game.exe"]);
        assert_eq!(Comparison::new(&new, None).added.len(), 2);

        let mut base = sample(18);
        let mut delta = Manifest::new("Sugar".into(), "1.1".into());
        delta.files = vec![FileManifest {
            filename: "Data/de.pak".into(),
            sha1: [3; 20],
            ..Default::default()
        }];
        delta.chunks = vec![ChunkInfo {
            guid: guid(20),
            ..Default::default()
        }];
        base.apply_delta(delta);
        assert_eq!(base.files.len(), 2);
        assert_eq!(base.file("Data/de.pak").unwrap().sha1, [3; 20]);
        assert_eq!(base.chunks.len(), 3);
    }

    /// A feature-level-24 manifest whose names are encrypted with `key`.
    pub(crate) fn encrypted_manifest(key: &[u8; 32], secret: Guid) -> Vec<u8> {
        let mut plain = Writer::default();
        plain.fstring("Bin/Secret.exe");
        plain.fstring("-launch");
        plain.u32(0);
        plain.fstring("");
        plain.fstring("");
        plain.fstring("");
        plain.fstring("Uninstall.exe");
        plain.fstring("/S");
        plain.fstring("Bin/Secret.exe");
        plain.fstring("");
        let iv = [4u8; 12];
        let (ct, tag) = crypto::encrypt(key, &iv, &plain.buf);

        let mut w = Writer::default();
        let at = w.start_size();
        w.u8(2);
        w.u32(24);
        w.u8(0);
        w.u32(0);
        w.fstring("Enc");
        w.fstring("2.0");
        w.fstring("");
        w.fstring("");
        w.u32(0);
        w.fstring("");
        w.fstring("");
        w.fstring("");
        w.fstring("BUILDID");
        w.fstring("");
        w.fstring("");
        w.finish_size(at);
        // CDL with one chunk and the v22 fields.
        let at = w.start_size();
        w.u8(0);
        w.u32(1);
        guid(1).write(&mut w);
        w.u64(3);
        w.bytes(&[0; 20]);
        w.u8(1);
        w.u32(1 << 20);
        w.i64(10);
        guid(40).write(&mut w);
        w.u32(9);
        w.bytes(&[0; 16]);
        w.finish_size(at);
        // FML with one (nameless) file.
        let at = w.start_size();
        w.u8(0);
        w.u32(1);
        w.fstring("");
        w.fstring("");
        w.bytes(&[6; 20]);
        w.u8(0);
        w.u32(0);
        w.u32(1);
        w.u32(28);
        guid(1).write(&mut w);
        w.u32(0);
        w.u32(10);
        w.finish_size(at);
        write_custom_fields(&mut w, &[]);
        // Encrypted data.
        let at = w.start_size();
        w.u8(0);
        let cipher_at = w.start_size();
        let header_at = w.start_size();
        w.u32(0);
        w.u8(0);
        w.u32(plain.buf.len() as u32);
        w.u32(plain.buf.len() as u32);
        w.u32(iv.len() as u32);
        w.bytes(&iv);
        w.finish_size(header_at);
        w.u32(ct.len() as u32);
        w.bytes(&ct);
        // The cipher size field counts the bytes after it.
        let len = (w.buf.len() - cipher_at - 4) as u32;
        w.buf[cipher_at..cipher_at + 4].copy_from_slice(&len.to_le_bytes());
        w.finish_size(at);

        let body = w.buf;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&body).unwrap();
        let compressed = z.finish().unwrap();
        let mut out = Writer::default();
        out.u32(MAGIC);
        out.u32(73);
        out.u32(body.len() as u32);
        out.u32(compressed.len() as u32);
        out.bytes(&Sha1::digest(&body));
        out.u8(0x3);
        out.u32(24);
        secret.write(&mut out);
        out.bytes(&tag);
        out.bytes(&compressed);
        out.buf
    }

    #[test]
    fn decrypts_feature_level_24_manifests() {
        let key = [9u8; 32];
        let secret = Guid([0x10, 0x20, 0x30, 0x40]);
        let bytes = encrypted_manifest(&key, secret);
        let err = Manifest::parse(&bytes, &Secrets::new()).unwrap_err();
        assert!(format!("{err:#}").contains("key"), "{err:#}");
        let secrets: Secrets = [(secret.hex_upper(), hex::encode(key))].into();
        let m = Manifest::parse(&bytes, &secrets).unwrap();
        assert!(!m.is_encrypted());
        assert_eq!(m.meta.launch_exe, "Bin/Secret.exe");
        assert_eq!(m.meta.launch_command, "-launch");
        assert_eq!(m.meta.uninstall_action_path, "Uninstall.exe");
        assert_eq!(m.meta.build_id(), "BUILDID");
        assert_eq!(m.files[0].filename, "Bin/Secret.exe");
        assert_eq!(m.chunks[0].secret_guid, guid(40));
        assert_eq!(m.chunks[0].window_size_compressed, 9);
        assert!(m.chunk_path(&m.chunks[0]).starts_with("ChunksV5/"));
    }
}
