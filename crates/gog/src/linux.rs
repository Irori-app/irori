//! GOG's Linux installers: a shell script with a ZIP archive appended.
//! Only the archive index is downloaded; files are then fetched one by one
//! with range requests.

use anyhow::{Context as _, Result, bail};

const LOCAL_HEADER: &[u8] = b"PK\x03\x04";
const CENTRAL_HEADER: &[u8] = b"PK\x01\x02";
const END_OF_CD: &[u8] = b"PK\x05\x06";
const ZIP64_LOCATOR: &[u8] = b"PK\x06\x07";
const ZIP64_END_OF_CD: &[u8] = b"PK\x06\x06";
/// Where the archive's own files live inside it.
pub const DATA_PREFIX: &str = "data/noarch/";

/// A file of the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// 0 stored, 8 deflate.
    pub method: u16,
    pub crc32: u32,
    pub compressed_size: u64,
    pub size: u64,
    /// Offset of the file's data from the start of the installer.
    pub data_offset: u64,
    pub symlink: bool,
    pub executable: bool,
}

impl Entry {
    /// Path inside the install folder, for files of the game itself.
    pub fn install_path(&self) -> Option<&str> {
        self.name
            .strip_prefix(DATA_PREFIX)
            .filter(|p| !p.is_empty() && !p.ends_with('/'))
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn find_last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

fn u16_at(b: &[u8], at: usize) -> Result<u16> {
    let s = b.get(at..at + 2).context("truncated archive index")?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32> {
    let s = b.get(at..at + 4).context("truncated archive index")?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64_at(b: &[u8], at: usize) -> Result<u64> {
    let s = b.get(at..at + 8).context("truncated archive index")?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Ok(u64::from_le_bytes(a))
}

/// Where the central directory is, from the tail of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Directory {
    Located {
        offset: u64,
        size: u64,
        records: u64,
    },
    /// ZIP64: the end record must be read at this archive offset first.
    Zip64At(u64),
}

/// Parse the end of the installer (its last bytes).
pub(crate) fn parse_tail(tail: &[u8]) -> Result<Directory> {
    let at = find_last(tail, END_OF_CD).context("the installer is not a valid archive")?;
    let records = u64::from(u16_at(tail, at + 10)?);
    let size = u64::from(u32_at(tail, at + 12)?);
    let offset = u32_at(tail, at + 16)?;
    if offset == u32::MAX {
        let loc = find_last(tail, ZIP64_LOCATOR).context("missing ZIP64 locator")?;
        return Ok(Directory::Zip64At(u64_at(tail, loc + 8)?));
    }
    Ok(Directory::Located {
        offset: u64::from(offset),
        size,
        records,
    })
}

/// Parse a ZIP64 end-of-directory record.
pub(crate) fn parse_zip64_end(data: &[u8]) -> Result<Directory> {
    if !data.starts_with(ZIP64_END_OF_CD) {
        bail!("invalid ZIP64 directory record");
    }
    Ok(Directory::Located {
        records: u64_at(data, 32)?,
        size: u64_at(data, 40)?,
        offset: u64_at(data, 48)?,
    })
}

/// Parse the central directory. `archive_start` is the offset of the
/// archive in the installer, `directory_offset` the directory's offset in
/// the archive.
pub(crate) fn parse_directory(
    data: &[u8],
    records: u64,
    archive_start: u64,
    directory_offset: u64,
) -> Result<Vec<Entry>> {
    let mut entries: Vec<(u64, Entry)> = Vec::new();
    let mut pos = 0usize;
    for _ in 0..records {
        if data.get(pos..pos + 4) != Some(CENTRAL_HEADER) {
            bail!("invalid archive directory");
        }
        let method = u16_at(data, pos + 10)?;
        let crc32 = u32_at(data, pos + 16)?;
        let mut compressed_size = u64::from(u32_at(data, pos + 20)?);
        let mut size = u64::from(u32_at(data, pos + 24)?);
        let name_len = usize::from(u16_at(data, pos + 28)?);
        let extra_len = usize::from(u16_at(data, pos + 30)?);
        let comment_len = usize::from(u16_at(data, pos + 32)?);
        let attrs = u32_at(data, pos + 38)?;
        let mut local_offset = u64::from(u32_at(data, pos + 42)?);
        let name_start = pos + 46;
        let name = data
            .get(name_start..name_start + name_len)
            .context("truncated archive directory")?;
        let name = String::from_utf8_lossy(name).into_owned();
        let extra = data
            .get(name_start + name_len..name_start + name_len + extra_len)
            .context("truncated archive directory")?;
        // ZIP64 extended sizes and offset.
        let mut e = 0;
        while e + 4 <= extra.len() {
            let id = u16_at(extra, e)?;
            let len = usize::from(u16_at(extra, e + 2)?);
            if id == 1 {
                let mut f = e + 4;
                if size == u64::from(u32::MAX) {
                    size = u64_at(extra, f)?;
                    f += 8;
                }
                if compressed_size == u64::from(u32::MAX) {
                    compressed_size = u64_at(extra, f)?;
                    f += 8;
                }
                if local_offset == u64::from(u32::MAX) {
                    local_offset = u64_at(extra, f)?;
                }
                break;
            }
            e += 4 + len;
        }
        let mode = attrs >> 16;
        entries.push((
            local_offset,
            Entry {
                name,
                method,
                crc32,
                compressed_size,
                size,
                data_offset: 0,
                symlink: mode & 0o170000 == 0o120000,
                executable: mode & 0o111 != 0,
            },
        ));
        pos = name_start + name_len + extra_len + comment_len;
    }
    // A file's data ends where the next local header starts (the archives
    // have no data descriptors), so the data offset needs no extra requests.
    entries.sort_by_key(|(off, _)| *off);
    let ends: Vec<u64> = entries
        .iter()
        .skip(1)
        .map(|(off, _)| *off)
        .chain(std::iter::once(directory_offset))
        .collect();
    Ok(entries
        .into_iter()
        .zip(ends)
        .map(|((_, mut entry), end)| {
            entry.data_offset = archive_start + end.saturating_sub(entry.compressed_size);
            entry
        })
        .collect())
}

/// An installer's archive index.
#[derive(Debug, Clone, PartialEq)]
pub struct Installer {
    /// Address the data was read from (after redirects).
    pub url: String,
    pub entries: Vec<Entry>,
}

async fn fetch_range(
    http: &reqwest::Client,
    url: &str,
    range: String,
) -> Result<(Vec<u8>, String, Option<u64>)> {
    let resp = http
        .get(url)
        .header(reqwest::header::RANGE, range)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Could not reach GOG ({})", e.without_url()))?;
    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        bail!("the installer server answered {}", resp.status());
    }
    let total = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit('/').next())
        .and_then(|v| v.parse().ok());
    let final_url = resp.url().to_string();
    Ok((resp.bytes().await?.to_vec(), final_url, total))
}

/// Read the archive index of an installer.
pub async fn read_installer(http: &reqwest::Client, url: &str) -> Result<Installer> {
    let (head, url, total) = fetch_range(http, url, "bytes=0-2097151".into())
        .await
        .context("Could not read the Linux installer")?;
    let total = total.context("the installer server did not report the file size")?;
    let start = find(&head, LOCAL_HEADER).context("the installer contains no archive")? as u64;
    let (tail, _, _) =
        fetch_range(http, &url, format!("bytes={}-", total.saturating_sub(100))).await?;
    let (offset, size, records) = match parse_tail(&tail)? {
        Directory::Located {
            offset,
            size,
            records,
        } => (offset, size, records),
        Directory::Zip64At(at) => {
            let (rec, _, _) = fetch_range(
                http,
                &url,
                format!("bytes={}-{}", start + at, start + at + 199),
            )
            .await?;
            match parse_zip64_end(&rec)? {
                Directory::Located {
                    offset,
                    size,
                    records,
                } => (offset, size, records),
                Directory::Zip64At(_) => bail!("invalid ZIP64 directory"),
            }
        }
    };
    let (dir, _, _) = fetch_range(
        http,
        &url,
        format!("bytes={}-{}", start + offset, start + offset + size - 1),
    )
    .await?;
    let entries = parse_directory(&dir, records, start, offset)?;
    Ok(Installer { url, entries })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write as _;

    use super::*;

    /// A shell installer: a script followed by a zip with `files`.
    pub(crate) fn installer(files: &[(&str, &[u8], bool, Option<&str>)]) -> Vec<u8> {
        let mut zip = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut zip));
            for (name, data, deflate, link) in files {
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(if *deflate {
                        zip::CompressionMethod::Deflated
                    } else {
                        zip::CompressionMethod::Stored
                    })
                    .unix_permissions(if name.ends_with(".sh") { 0o755 } else { 0o644 });
                match link {
                    Some(target) => w.add_symlink(*name, *target, opts).unwrap(),
                    None => {
                        w.start_file(*name, opts).unwrap();
                        w.write_all(data).unwrap();
                    }
                }
            }
            w.finish().unwrap();
        }
        [b"#!/bin/sh\necho installer\nexit 0\n".to_vec(), zip].concat()
    }

    #[tokio::test]
    async fn reads_installer_indexes() {
        let big = vec![7u8; 200_000];
        let data = installer(&[
            ("data/noarch/game/start.sh", b"#!/bin/sh", false, None),
            ("data/noarch/game/big.pak", &big, true, None),
            ("data/noarch/game/lib.so", b"", false, Some("lib.so.1")),
            ("scripts/postinst", b"x", false, None),
        ]);
        let served = data.clone();
        let server = ember_depot::test_server::TestServer::start(move |req| {
            ember_depot::test_server::serve_bytes(req, &served, true)
        });
        let http = reqwest::Client::new();
        let inst = read_installer(&http, &server.url("/i.sh")).await.unwrap();
        let start = inst
            .entries
            .iter()
            .find(|e| e.name.ends_with("start.sh"))
            .unwrap();
        assert!(start.executable && !start.symlink);
        assert_eq!(start.install_path(), Some("game/start.sh"));
        let off = start.data_offset as usize;
        assert_eq!(&data[off..off + 9], b"#!/bin/sh");
        let pak = inst
            .entries
            .iter()
            .find(|e| e.name.ends_with("big.pak"))
            .unwrap();
        assert_eq!((pak.method, pak.size), (8, 200_000));
        let raw = &data[pak.data_offset as usize..(pak.data_offset + pak.compressed_size) as usize];
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::DeflateDecoder::new(raw), &mut out).unwrap();
        assert_eq!(out, big);
        assert!(inst.entries.iter().any(|e| e.symlink));
        assert!(
            inst.entries
                .iter()
                .find(|e| e.name == "scripts/postinst")
                .unwrap()
                .install_path()
                .is_none()
        );
    }
}
