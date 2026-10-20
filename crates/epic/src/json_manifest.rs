//! The older JSON manifest format. Numbers are stored as strings of
//! three-digit decimal bytes, little-endian ("blobs").

use anyhow::{Context as _, Result, bail};
use serde_json::{Map, Value};

use crate::manifest::{ChunkInfo, ChunkPart, FileManifest, Guid, Manifest, Meta};

/// Decode a blob (`"001002"` → `0x0201`).
fn blob(s: &str) -> Result<u64> {
    if !s.len().is_multiple_of(3) || s.len() > 24 {
        bail!("invalid number {s:?} in manifest");
    }
    let mut n = 0u64;
    for (i, digits) in s.as_bytes().chunks(3).enumerate() {
        let byte: u64 = std::str::from_utf8(digits)?.parse()?;
        if byte > 255 {
            bail!("invalid number {s:?} in manifest");
        }
        n |= byte << (8 * i);
    }
    Ok(n)
}

fn blob_of(v: Option<&Value>, default: u64) -> Result<u64> {
    match v.and_then(Value::as_str) {
        Some(s) => blob(s),
        None => Ok(default),
    }
}

fn text(obj: &Map<String, Value>, key: &str) -> String {
    obj.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn field<'a>(obj: &'a Map<String, Value>, key: &str) -> &'a str {
    obj.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn object<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a Map<String, Value>> {
    obj.get(key)
        .and_then(Value::as_object)
        .with_context(|| format!("the manifest has no {key}"))
}

/// The 20-byte SHA-1 stored as a blob (little-endian).
fn sha_from_blob(s: &str) -> Result<[u8; 20]> {
    if s.len() != 60 {
        bail!("invalid file hash in manifest");
    }
    let mut out = [0u8; 20];
    for (i, digits) in s.as_bytes().chunks(3).enumerate() {
        let byte: u16 = std::str::from_utf8(digits)?.parse()?;
        out[i] = u8::try_from(byte).context("invalid file hash in manifest")?;
    }
    Ok(out)
}

pub(crate) fn parse(data: &[u8]) -> Result<Manifest> {
    let root: Value = serde_json::from_slice(data).context("the manifest is not valid JSON")?;
    let obj = root
        .as_object()
        .context("the manifest is not a JSON object")?;
    let version = blob_of(obj.get("ManifestFileVersion"), 13)? as u32;
    let meta = Meta {
        feature_level: version,
        is_file_data: obj.get("bIsFileData").and_then(Value::as_bool) == Some(true),
        app_id: blob_of(obj.get("AppID"), 0)? as u32,
        app_name: text(obj, "AppNameString"),
        build_version: text(obj, "BuildVersionString"),
        launch_exe: text(obj, "LaunchExeString"),
        launch_command: text(obj, "LaunchCommand"),
        prereq_ids: obj
            .get("PrereqIds")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        prereq_name: text(obj, "PrereqName"),
        prereq_path: text(obj, "PrereqPath"),
        prereq_args: text(obj, "PrereqArgs"),
        ..Default::default()
    };

    let sizes = object(obj, "ChunkFilesizeList")?;
    let hashes = object(obj, "ChunkHashList")?;
    let shas = object(obj, "ChunkShaList")?;
    let groups = object(obj, "DataGroupList")?;
    let mut chunks = Vec::with_capacity(sizes.len());
    for (guid, size) in sizes {
        let mut sha1 = [0u8; 20];
        if let Ok(raw) = hex::decode(field(shas, guid))
            && raw.len() == 20
        {
            sha1.copy_from_slice(&raw);
        }
        chunks.push(ChunkInfo {
            guid: Guid::from_json_hex(guid)?,
            file_size: blob(size.as_str().unwrap_or_default())?,
            hash: blob(field(hashes, guid))?,
            sha1,
            group: blob(field(groups, guid))? as u8,
            window_size: 1 << 20,
            ..Default::default()
        });
    }

    let mut files = Vec::new();
    for f in obj
        .get("FileManifestList")
        .and_then(Value::as_array)
        .context("the manifest has no file list")?
    {
        let f = f.as_object().context("invalid file entry in manifest")?;
        let flag = |k: &str| f.get(k).and_then(Value::as_bool) == Some(true);
        let mut fm = FileManifest {
            filename: text(f, "Filename"),
            sha1: sha_from_blob(
                f.get("FileHash")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )?,
            flags: flag("bIsReadOnly") as u8
                | (flag("bIsCompressed") as u8) << 1
                | (flag("bIsUnixExecutable") as u8) << 2,
            install_tags: f
                .get("InstallTags")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        };
        let mut offset = 0u64;
        for p in f
            .get("FileChunkParts")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let p = p.as_object().context("invalid chunk part in manifest")?;
            let size = blob_of(p.get("Size"), 0)? as u32;
            fm.parts.push(ChunkPart {
                guid: Guid::from_json_hex(&text(p, "Guid"))?,
                offset: blob_of(p.get("Offset"), 0)? as u32,
                size,
                file_offset: offset,
            });
            offset += u64::from(size);
        }
        fm.size = offset;
        files.push(fm);
    }
    let custom_fields = obj
        .get("CustomFields")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default();
    Ok(Manifest::from_json_parts(
        version,
        meta,
        chunks,
        files,
        custom_fields,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Secrets;

    #[test]
    fn parses_json_manifests() {
        let sha = "001".repeat(20);
        let json = format!(
            r#"{{"ManifestFileVersion": "013000000000", "bIsFileData": false, "AppID": "000000000000",
            "AppNameString": "Old", "BuildVersionString": "1.0", "LaunchExeString": "Game.exe",
            "LaunchCommand": "", "PrereqIds": [], "PrereqName": "", "PrereqPath": "", "PrereqArgs": "",
            "FileManifestList": [{{"Filename": "Game.exe", "FileHash": "{sha}", "bIsUnixExecutable": true,
              "FileChunkParts": [{{"Guid": "0000000100000002000000030000000A", "Offset": "016000000000", "Size": "000001000000"}}]}}],
            "ChunkHashList": {{"0000000100000002000000030000000A": "255000000000000000000000"}},
            "ChunkShaList": {{"0000000100000002000000030000000A": "{}"}},
            "DataGroupList": {{"0000000100000002000000030000000A": "042"}},
            "ChunkFilesizeList": {{"0000000100000002000000030000000A": "232003000000"}},
            "CustomFields": {{"CloudSaveFolder": "x"}}}}"#,
            "ab".repeat(20)
        );
        let m = Manifest::parse(json.as_bytes(), &Secrets::new()).unwrap();
        assert_eq!(m.version, 13);
        assert_eq!(m.meta.app_name, "Old");
        let c = &m.chunks[0];
        assert_eq!(c.guid, Guid([1, 2, 3, 10]));
        assert_eq!((c.file_size, c.hash, c.group), (1000, 255, 42));
        assert_eq!(c.sha1, [0xab; 20]);
        let f = &m.files[0];
        assert_eq!(f.sha1, [1; 20]);
        assert!(f.is_executable());
        assert_eq!((f.parts[0].offset, f.parts[0].size, f.size), (16, 256, 256));
        assert_eq!(m.custom_fields, [("CloudSaveFolder".into(), "x".into())]);
        assert!(m.chunk_path(c).starts_with("ChunksV3/42/00000000000000FF_"));
        assert!(blob("1").is_err());
        assert!(blob("999").is_err());
    }
}
