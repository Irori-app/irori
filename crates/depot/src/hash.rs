//! Content digests used by store manifests.

use std::fmt;
use std::io::Read as _;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha1::Digest as _;
use sha3::digest::{ExtendableOutput as _, Update as _};

/// Hash algorithms found in Amazon, GOG and Epic manifests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Algorithm {
    Sha256,
    Sha1,
    Md5,
    /// SHAKE128; the output length is the length of the expected digest.
    Shake128,
    /// CRC-32 (zip), stored big-endian.
    Crc32,
}

impl Algorithm {
    fn name(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "sha256",
            Algorithm::Sha1 => "sha1",
            Algorithm::Md5 => "md5",
            Algorithm::Shake128 => "shake128",
            Algorithm::Crc32 => "crc32",
        }
    }
}

/// An expected digest: algorithm plus value.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Digest {
    pub algorithm: Algorithm,
    pub value: Vec<u8>,
}

impl Digest {
    pub fn new(algorithm: Algorithm, value: impl Into<Vec<u8>>) -> Self {
        Self {
            algorithm,
            value: value.into(),
        }
    }

    pub fn from_hex(algorithm: Algorithm, hex_value: &str) -> Result<Self, hex::FromHexError> {
        Ok(Self::new(algorithm, hex::decode(hex_value.trim())?))
    }

    pub fn hex(&self) -> String {
        hex::encode(&self.value)
    }

    /// A hasher producing digests comparable with this one.
    pub fn hasher(&self) -> Hasher {
        Hasher::new(self.algorithm, self.value.len())
    }

    /// Whether `data` hashes to this digest.
    pub fn matches(&self, data: &[u8]) -> bool {
        let mut h = self.hasher();
        h.update(data);
        h.finish() == self.value
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

/// `sha256:<hex>`.
impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm.name(), self.hex())
    }
}

impl FromStr for Digest {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (alg, value) = s.split_once(':').ok_or("missing algorithm")?;
        let algorithm = match alg {
            "sha256" => Algorithm::Sha256,
            "sha1" => Algorithm::Sha1,
            "md5" => Algorithm::Md5,
            "shake128" => Algorithm::Shake128,
            "crc32" => Algorithm::Crc32,
            other => return Err(format!("unknown hash algorithm {other}")),
        };
        Digest::from_hex(algorithm, value).map_err(|e| e.to_string())
    }
}

impl Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Incremental hasher for any [`Algorithm`].
pub struct Hasher {
    inner: Inner,
    out_len: usize,
}

enum Inner {
    Sha256(sha2::Sha256),
    Sha1(sha1::Sha1),
    Md5(md5::Md5),
    Shake128(Box<sha3::Shake128>),
    Crc32(flate2::Crc),
}

impl Hasher {
    /// `out_len` only matters for SHAKE128.
    pub fn new(algorithm: Algorithm, out_len: usize) -> Self {
        let inner = match algorithm {
            Algorithm::Sha256 => Inner::Sha256(sha2::Sha256::new()),
            Algorithm::Sha1 => Inner::Sha1(sha1::Sha1::new()),
            Algorithm::Md5 => Inner::Md5(md5::Md5::new()),
            Algorithm::Shake128 => Inner::Shake128(Box::default()),
            Algorithm::Crc32 => Inner::Crc32(flate2::Crc::new()),
        };
        Self { inner, out_len }
    }

    pub fn update(&mut self, data: &[u8]) {
        match &mut self.inner {
            Inner::Sha256(h) => sha1::Digest::update(h, data),
            Inner::Sha1(h) => sha1::Digest::update(h, data),
            Inner::Md5(h) => sha1::Digest::update(h, data),
            Inner::Shake128(h) => h.update(data),
            Inner::Crc32(h) => h.update(data),
        }
    }

    pub fn finish(self) -> Vec<u8> {
        match self.inner {
            Inner::Sha256(h) => h.finalize().to_vec(),
            Inner::Sha1(h) => h.finalize().to_vec(),
            Inner::Md5(h) => h.finalize().to_vec(),
            Inner::Shake128(h) => {
                let mut out = vec![0u8; self.out_len];
                sha3::digest::XofReader::read(&mut (*h).finalize_xof(), &mut out);
                out
            }
            Inner::Crc32(h) => h.sum().to_be_bytes().to_vec(),
        }
    }
}

/// Hash a file and compare it with `expected`, calling `on_bytes` after each
/// block read (for progress); returning `false` from it stops with
/// [`std::io::ErrorKind::Interrupted`]. Blocking; run it on a blocking thread.
pub fn hash_file(
    path: &Path,
    expected: &Digest,
    mut on_bytes: impl FnMut(u64) -> bool,
) -> std::io::Result<bool> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = expected.hasher();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        if !on_bytes(n as u64) {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
    }
    Ok(hasher.finish() == expected.value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_match_known_values() {
        let cases = [
            (
                Algorithm::Sha256,
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            ),
            (Algorithm::Sha1, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d"),
            (Algorithm::Md5, "5d41402abc4b2a76b9719d911017c592"),
            (Algorithm::Shake128, "8eb4b6a932f280335ee1a279f8c208a3"),
            (Algorithm::Crc32, "3610a686"),
        ];
        for (alg, hex) in cases {
            let d = Digest::from_hex(alg, hex).unwrap();
            assert!(d.matches(b"hello"), "{alg:?}");
            assert!(!d.matches(b"hellO"), "{alg:?}");
        }
    }

    #[test]
    fn digest_round_trips_as_text() {
        let d = Digest::from_hex(Algorithm::Md5, "5d41402abc4b2a76b9719d911017c592").unwrap();
        let s = d.to_string();
        assert_eq!(s, "md5:5d41402abc4b2a76b9719d911017c592");
        assert_eq!(s.parse::<Digest>().unwrap(), d);
        assert_eq!(serde_json::to_string(&d).unwrap(), format!("\"{s}\""));
        assert!("crc:00".parse::<Digest>().is_err());
        assert!("00".parse::<Digest>().is_err());
    }

    #[test]
    fn hashes_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"hello").unwrap();
        let d = Digest::from_hex(Algorithm::Md5, "5d41402abc4b2a76b9719d911017c592").unwrap();
        let mut seen = 0;
        assert!(
            hash_file(&p, &d, |n| {
                seen += n;
                true
            })
            .unwrap()
        );
        assert_eq!(seen, 5);
        let err = hash_file(&p, &d, |_| false).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    }
}
