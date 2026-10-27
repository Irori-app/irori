//! Amazon's signed build manifests (`manifest.proto`, schema
//! `tv.twitch.fuel.sds`): a length-prefixed header, then the LZMA-compressed
//! file list, signed with Amazon's RSA key.

use anyhow::{Context as _, Result, bail};
use irori_depot::{Algorithm, Digest, FileEntry};
use prost::Message;
use rsa::RsaPublicKey;
use rsa::pkcs8::DecodePublicKey as _;
use rsa::signature::Verifier as _;

const AMAZON_KEY: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA6fSRMUi3VpTtv9P4+KvM
AcAIP4SYbTQfB1ns7vyUjsj8nrF2lGNtQTtGLnrNmM2ElZ2R7VmQtNiRtPMxToIW
Rajin0H0OyzGrHA8P6w96Mj4q1JeORCzJeVFgLOBClCCMmB+5bJBWnJcq/sEMwu9
gGynCeiYNLt7ZMVpL1GOsNjl+yLk7OMMGpMj1JWCVFfgYE9Lud1QZJllFAWhRBoT
wTctAUZTikObFUoBm+KEiCsKIcay4WOybvJwxTNBUl2GL8c+ihrT2ntLPpb9aIJE
/gXU3Ihl5oXe/0P/QN0CRu/ybXWLiGzIYqKIok4nepkdo8V3gWR55K801pOuck0B
awIDAQAB
-----END PUBLIC KEY-----";

/// Protobuf messages of `sds-proto2.proto`.
pub(crate) mod proto {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ManifestHeader {
        #[prost(message, optional, tag = "1")]
        pub compression: Option<CompressionSettings>,
        #[prost(message, optional, tag = "2")]
        pub hash: Option<Hash>,
        #[prost(message, optional, tag = "3")]
        pub signature: Option<Signature>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct CompressionSettings {
        /// 0: none, 1: lzma.
        #[prost(int32, optional, tag = "1")]
        pub algorithm: Option<i32>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Hash {
        /// 0: sha256, 1: shake128.
        #[prost(int32, optional, tag = "1")]
        pub algorithm: Option<i32>,
        #[prost(bytes = "vec", optional, tag = "2")]
        pub value: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Signature {
        /// 0: sha256_with_rsa.
        #[prost(int32, optional, tag = "1")]
        pub algorithm: Option<i32>,
        #[prost(bytes = "vec", optional, tag = "2")]
        pub value: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Manifest {
        #[prost(message, repeated, tag = "1")]
        pub packages: Vec<Package>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Package {
        #[prost(string, optional, tag = "1")]
        pub name: Option<String>,
        #[prost(message, repeated, tag = "2")]
        pub files: Vec<File>,
        #[prost(message, repeated, tag = "3")]
        pub dirs: Vec<Dir>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct File {
        #[prost(string, optional, tag = "1")]
        pub path: Option<String>,
        #[prost(uint32, optional, tag = "2")]
        pub mode: Option<u32>,
        #[prost(int64, optional, tag = "3")]
        pub size: Option<i64>,
        #[prost(string, optional, tag = "4")]
        pub created: Option<String>,
        #[prost(message, optional, tag = "5")]
        pub hash: Option<Hash>,
        #[prost(bool, optional, tag = "6")]
        pub hidden: Option<bool>,
        #[prost(bool, optional, tag = "7")]
        pub system: Option<bool>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Dir {
        #[prost(string, optional, tag = "1")]
        pub path: Option<String>,
        #[prost(uint32, optional, tag = "2")]
        pub mode: Option<u32>,
    }
}

/// The key Amazon signs manifests with.
pub(crate) fn amazon_key() -> RsaPublicKey {
    RsaPublicKey::from_public_key_pem(AMAZON_KEY).expect("valid Amazon key")
}

/// One file of a build.
#[derive(Debug, Clone, PartialEq)]
pub struct ManifestFile {
    /// Normalised relative path (`/`-separated).
    pub path: String,
    pub size: u64,
    pub hash: Digest,
}

/// A parsed, signature-checked build manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub files: Vec<ManifestFile>,
}

impl Manifest {
    /// Parse and verify a `manifest.proto` signed by Amazon.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Self::parse_with_key(bytes, &amazon_key())
    }

    pub(crate) fn parse_with_key(bytes: &[u8], key: &RsaPublicKey) -> Result<Self> {
        let header_len = bytes
            .get(..4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .context("the manifest is truncated")?;
        let header_end = 4usize
            .checked_add(header_len)
            .filter(|end| *end <= bytes.len())
            .context("the manifest is truncated")?;
        let header = proto::ManifestHeader::decode(&bytes[4..header_end])
            .context("the manifest header is invalid")?;
        let body = &bytes[header_end..];
        let raw = match header.compression.and_then(|c| c.algorithm).unwrap_or(0) {
            0 => body.to_vec(),
            1 => decompress_lzma(body)?,
            other => bail!("unknown manifest compression {other}"),
        };
        let signature = header.signature.context("the manifest is not signed")?;
        if signature.algorithm.unwrap_or(0) != 0 {
            bail!("unknown manifest signature algorithm");
        }
        let sig =
            rsa::pkcs1v15::Signature::try_from(signature.value.unwrap_or_default().as_slice())
                .context("the manifest signature is invalid")?;
        rsa::pkcs1v15::VerifyingKey::<sha2::Sha256>::new(key.clone())
            .verify(&raw, &sig)
            .context("the manifest signature does not match; refusing to install from it")?;
        let manifest =
            proto::Manifest::decode(raw.as_slice()).context("the manifest is invalid")?;
        // Builds have one package (the Amazon app reads only the first).
        let package = manifest
            .packages
            .into_iter()
            .next()
            .context("the manifest lists no files")?;
        let files = package
            .files
            .into_iter()
            .map(|f| {
                let raw_path = f.path.unwrap_or_default();
                let path = irori_depot::path::normalize(&raw_path)?;
                let hash = f
                    .hash
                    .with_context(|| format!("{raw_path} has no checksum"))?;
                let algorithm = match hash.algorithm.unwrap_or(0) {
                    0 => Algorithm::Sha256,
                    1 => Algorithm::Shake128,
                    other => bail!("{raw_path} uses unknown hash algorithm {other}"),
                };
                Ok(ManifestFile {
                    path,
                    size: u64::try_from(f.size.unwrap_or(0))
                        .with_context(|| format!("{raw_path} has a negative size"))?,
                    hash: Digest::new(algorithm, hash.value.unwrap_or_default()),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { files })
    }

    /// Total size of the build.
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Download entries for `files`, served below the build's download URL
    /// as `<download_url>/files/<hex digest>`.
    pub fn entries<'a>(
        files: impl IntoIterator<Item = &'a ManifestFile>,
        download_url: &str,
    ) -> Result<Vec<FileEntry>> {
        files
            .into_iter()
            .map(|f| {
                Ok(FileEntry::whole(
                    f.path.clone(),
                    f.size,
                    Some(f.hash.clone()),
                    vec![build_url(download_url, &format!("files/{}", f.hash.hex()))?],
                ))
            })
            .collect()
    }
}

/// `<base>/<rel>`, keeping the base's query (signed CDN addresses).
pub(crate) fn build_url(base: &str, rel: &str) -> Result<String> {
    let mut url =
        url::Url::parse(base).with_context(|| format!("invalid download address {base:?}"))?;
    let path = format!("{}/{rel}", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url.to_string())
}

/// LZMA data in either container (`.xz` or legacy `.lzma`), as Python's
/// `lzma.decompress` accepts.
fn decompress_lzma(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut input = std::io::BufReader::new(data);
    if data.starts_with(b"\xfd7zXZ\x00") {
        lzma_rs::xz_decompress(&mut input, &mut out).context("could not unpack the manifest")?;
    } else {
        lzma_rs::lzma_decompress(&mut input, &mut out).context("could not unpack the manifest")?;
    }
    Ok(out)
}

/// Signed manifests for tests of this and dependent crates.
#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use prost::Message as _;
    use rsa::pkcs1::DecodeRsaPrivateKey as _;
    use rsa::pkcs8::EncodePublicKey as _;
    use rsa::signature::{SignatureEncoding as _, Signer as _};

    use super::proto;

    const TEST_KEY: &str = include_str!("test_key.pem");

    pub fn test_key() -> rsa::RsaPrivateKey {
        rsa::RsaPrivateKey::from_pkcs1_pem(TEST_KEY).unwrap()
    }

    pub fn sha256(data: &[u8]) -> Vec<u8> {
        use sha2::Digest as _;
        sha2::Sha256::digest(data).to_vec()
    }

    /// Build a signed manifest for `files` (path, content).
    pub fn build(files: &[(&str, &[u8])], xz: bool) -> Vec<u8> {
        let manifest = proto::Manifest {
            packages: vec![proto::Package {
                name: Some("main".into()),
                files: files
                    .iter()
                    .map(|(p, data)| proto::File {
                        path: Some(p.to_string()),
                        size: Some(data.len() as i64),
                        hash: Some(proto::Hash {
                            algorithm: Some(0),
                            value: Some(sha256(data)),
                        }),
                        ..Default::default()
                    })
                    .collect(),
                dirs: vec![],
            }],
        };
        let raw = manifest.encode_to_vec();
        let signer = rsa::pkcs1v15::SigningKey::<sha2::Sha256>::new(test_key());
        let signature = signer.sign(&raw).to_vec();
        let mut body = Vec::new();
        if xz {
            lzma_rs::xz_compress(&mut raw.as_slice(), &mut body).unwrap();
        } else {
            lzma_rs::lzma_compress(&mut raw.as_slice(), &mut body).unwrap();
        }
        let header = proto::ManifestHeader {
            compression: Some(proto::CompressionSettings { algorithm: Some(1) }),
            hash: None,
            signature: Some(proto::Signature {
                algorithm: Some(0),
                value: Some(signature),
            }),
        }
        .encode_to_vec();
        let mut out = (header.len() as u32).to_be_bytes().to_vec();
        out.extend(header);
        out.extend(body);
        out
    }

    /// PEM of the key [`build`] signs with, for [`crate::Client::trust_manifest_key`].
    pub fn public_key_pem() -> String {
        test_key()
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{build, sha256, test_key};

    #[test]
    fn parses_signed_manifests() {
        let key = test_key().to_public_key();
        for xz in [true, false] {
            let bytes = build(&[("Game.exe", b"exe"), ("data\\a.pak", b"pak data")], xz);
            let m = Manifest::parse_with_key(&bytes, &key).unwrap();
            assert_eq!(m.files.len(), 2);
            assert_eq!(m.files[1].path, "data/a.pak");
            assert_eq!(m.files[1].size, 8);
            assert_eq!(
                m.files[1].hash,
                Digest::new(Algorithm::Sha256, sha256(b"pak data"))
            );
            assert_eq!(m.size(), 11);
        }
    }

    #[test]
    fn rejects_tampered_or_foreign_manifests() {
        let key = test_key().to_public_key();
        let mut bytes = build(&[("a", b"x")], true);
        // Amazon's key does not accept a test signature.
        let err = Manifest::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("signature"), "{err:#}");
        // Flip a byte of the header's signature.
        bytes[10] ^= 1;
        assert!(Manifest::parse_with_key(&bytes, &key).is_err());
        assert!(Manifest::parse_with_key(&[0, 0, 0, 9, 1], &key).is_err());
        assert!(Manifest::parse_with_key(&[], &key).is_err());
        let evil = build(&[("..\\..\\x", b"x")], true);
        assert!(Manifest::parse_with_key(&evil, &key).is_err());
    }

    #[test]
    fn builds_file_urls() {
        let m = Manifest::parse_with_key(
            &build(&[("a/b.exe", b"x")], true),
            &test_key().to_public_key(),
        )
        .unwrap();
        let entries =
            Manifest::entries(&m.files, "https://cdn.example/builds/v1/?sig=abc").unwrap();
        assert_eq!(
            entries[0].parts[0].chunk.urls,
            [format!(
                "https://cdn.example/builds/v1/files/{}?sig=abc",
                hex::encode(sha256(b"x"))
            )]
        );
        assert_eq!(entries[0].path, "a/b.exe");
        assert_eq!(
            build_url("https://h/x", "manifest.proto").unwrap(),
            "https://h/x/manifest.proto"
        );
    }

    #[test]
    fn diffs_builds() {
        let key = test_key().to_public_key();
        let old =
            Manifest::parse_with_key(&build(&[("a", b"1"), ("b", b"2"), ("c", b"3")], true), &key)
                .unwrap();
        let new = Manifest::parse_with_key(
            &build(&[("a", b"1"), ("b", b"two"), ("d", b"4")], true),
            &key,
        )
        .unwrap();
        let entries = Manifest::entries(&new.files, "https://h/v2").unwrap();
        let diff = irori_depot::diff(
            old.files.iter().map(|f| (f.path.as_str(), Some(&f.hash))),
            &entries,
        );
        let changed: Vec<&str> = diff.changed.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(changed, ["b", "d"]);
        assert_eq!(diff.removed, ["c"]);
        assert_eq!(diff.unchanged, 1);
    }
}
