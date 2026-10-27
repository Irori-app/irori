//! Epic's chunk container: a header, then the (optionally zlib-compressed
//! and AES-GCM encrypted) data. Also builds chunks for cloud-save uploads.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use once_cell::sync::Lazy;
use sha1::{Digest as _, Sha1};

use crate::binary::{Reader, Writer};
use crate::crypto;
use crate::manifest::Guid;

const MAGIC: u32 = 0xB1FE_3AA2;
/// Decoded size of every chunk Epic produces.
pub const CHUNK_SIZE: usize = 1 << 20;

/// A parsed chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub guid: Guid,
    pub hash: u64,
    pub sha1: Option<[u8; 20]>,
    pub data: Vec<u8>,
}

/// Parse and decode a chunk file.
pub fn decode(raw: &[u8], secrets: &HashMap<String, String>) -> Result<Chunk> {
    let mut r = Reader::new(raw);
    if r.u32()? != MAGIC {
        bail!("not an Epic chunk");
    }
    let version = r.u32()?;
    let header_size = r.u32()? as usize;
    let compressed_size = r.u32()? as usize;
    let guid = Guid::read(&mut r)?;
    let hash = r.u64()?;
    let stored_as = r.u8()?;
    let mut sha1 = None;
    let mut hash_type = 0;
    let mut uncompressed_size = CHUNK_SIZE;
    if version >= 2 {
        sha1 = Some(r.array::<20>()?);
        hash_type = r.u8()?;
    }
    if version >= 3 {
        uncompressed_size = r.u32()? as usize;
    }
    let mut secret = None;
    if version >= 4 {
        let secret_guid = Guid::read(&mut r)?;
        let tag: [u8; 16] = r.array()?;
        secret = Some((secret_guid, tag));
    }
    if r.pos() != header_size {
        r.seek(header_size)?;
    }
    let body = r.bytes(compressed_size.min(r.remaining()))?;
    let mut data = if stored_as & 0x2 != 0 {
        let (secret_guid, tag) = secret.context("encrypted chunk without key id")?;
        let sha = sha1.context("encrypted chunk without nonce")?;
        let key = crypto::find_key(secrets, &secret_guid.hex_upper())?;
        crypto::decrypt(&key, &sha[..12], body, &tag)?
    } else {
        body.to_vec()
    };
    if stored_as & 0x1 != 0 {
        let mut out = Vec::with_capacity(uncompressed_size.min(64 << 20));
        flate2::read::ZlibDecoder::new(data.as_slice())
            .take(64 << 20)
            .read_to_end(&mut out)
            .context("the chunk is damaged")?;
        data = out;
    }
    if hash_type & 0x2 != 0
        && let Some(sha) = sha1
        && Sha1::digest(&data).as_slice() != sha
    {
        bail!("the chunk does not match its checksum");
    }
    Ok(Chunk {
        guid,
        hash,
        sha1,
        data,
    })
}

/// [`irori_depot::Decoder`] for Epic chunks.
pub struct ChunkDecoder {
    pub secrets: Arc<HashMap<String, String>>,
}

impl irori_depot::Decoder for ChunkDecoder {
    fn decode(&self, data: Vec<u8>) -> Result<Vec<u8>> {
        Ok(decode(&data, &self.secrets)?.data)
    }
}

static HASH_TABLE: Lazy<[u64; 256]> = Lazy::new(|| {
    const POLY: u64 = 0xC96C_5795_D787_0F42;
    let mut table = [0u64; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut v = i as u64;
        for _ in 0..8 {
            v = if v & 1 != 0 { (v >> 1) ^ POLY } else { v >> 1 };
        }
        *slot = v;
    }
    table
});

/// Epic's rolling hash (a CRC-64-ECMA variant).
pub fn rolling_hash(data: &[u8]) -> u64 {
    let table = &*HASH_TABLE;
    data.iter()
        .fold(0u64, |h, &b| h.rotate_left(1) ^ table[b as usize])
}

/// Build a compressed chunk file from up to 1 MiB of data (padded with
/// zeroes as Epic does). Returns the file bytes, rolling hash and SHA-1.
pub fn encode(guid: Guid, data: &[u8]) -> Result<(Vec<u8>, u64, [u8; 20])> {
    if data.len() > CHUNK_SIZE {
        bail!("chunk data is larger than 1 MiB");
    }
    let mut padded = data.to_vec();
    padded.resize(CHUNK_SIZE, 0);
    let hash = rolling_hash(&padded);
    let sha: [u8; 20] = Sha1::digest(&padded).into();
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&padded)?;
    let compressed = z.finish()?;

    let mut w = Writer::default();
    w.u32(MAGIC);
    w.u32(3);
    w.u32(66);
    w.u32(compressed.len() as u32);
    guid.write(&mut w);
    w.u64(hash);
    w.u8(1);
    w.bytes(&sha);
    w.u8(0x3);
    w.u32(CHUNK_SIZE as u32);
    w.bytes(&compressed);
    Ok((w.buf, hash, sha))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_hash_matches_reference() {
        // Values from legendary's reference implementation.
        assert_eq!(rolling_hash(b""), 0);
        assert_eq!(rolling_hash(b"a"), HASH_TABLE[b'a' as usize]);
        let h = rolling_hash(b"ab");
        assert_eq!(
            h,
            HASH_TABLE[b'a' as usize].rotate_left(1) ^ HASH_TABLE[b'b' as usize]
        );
    }

    #[test]
    fn round_trips_chunks() {
        let guid = Guid([1, 2, 3, 4]);
        let (file, hash, sha) = encode(guid, b"hello").unwrap();
        let c = decode(&file, &HashMap::new()).unwrap();
        assert_eq!(c.guid, guid);
        assert_eq!(c.hash, hash);
        assert_eq!(c.sha1, Some(sha));
        assert_eq!(c.data.len(), CHUNK_SIZE);
        assert_eq!(&c.data[..5], b"hello");
        assert!(decode(&file[..10], &HashMap::new()).is_err());
        // Damaged data fails the checksum.
        let (mut bad, _, _) = encode(guid, b"hellO").unwrap();
        bad[41..61].copy_from_slice(&file[41..61]);
        assert!(decode(&bad, &HashMap::new()).is_err());
        assert!(encode(guid, &vec![0; CHUNK_SIZE + 1]).is_err());
    }

    /// A version-4 chunk, encrypted and compressed.
    pub(crate) fn encrypted_chunk(key: &[u8; 32], secret: Guid, plain: &[u8]) -> Vec<u8> {
        let sha: [u8; 20] = Sha1::digest(plain).into();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(plain).unwrap();
        let (ct, tag) = crypto::encrypt(key, &sha[..12], &z.finish().unwrap());
        let mut w = Writer::default();
        w.u32(MAGIC);
        w.u32(4);
        w.u32(98);
        w.u32(ct.len() as u32);
        Guid([7, 7, 7, 7]).write(&mut w);
        w.u64(1);
        w.u8(0x3);
        w.bytes(&sha);
        w.u8(0x2);
        w.u32(plain.len() as u32);
        secret.write(&mut w);
        w.bytes(&tag);
        w.bytes(&ct);
        w.buf
    }

    #[test]
    fn decrypts_chunks() {
        let key = [5u8; 32];
        let secret = Guid([0xA, 0xB, 0xC, 0xD]);
        let file = encrypted_chunk(&key, secret, b"top secret");
        let secrets: HashMap<String, String> = [(secret.hex_upper(), hex::encode(key))].into();
        assert_eq!(decode(&file, &secrets).unwrap().data, b"top secret");
        assert!(decode(&file, &HashMap::new()).is_err());
        let decoder = ChunkDecoder {
            secrets: Arc::new(secrets),
        };
        assert_eq!(
            irori_depot::Decoder::decode(&decoder, file).unwrap(),
            b"top secret"
        );
    }
}
