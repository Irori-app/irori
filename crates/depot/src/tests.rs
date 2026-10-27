//! End-to-end tests of the engine against an in-process HTTP server.

use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;

use super::*;
use crate::download::STALL;
use crate::test_server::{Resp, TestServer, serve_bytes};

fn sha256(data: &[u8]) -> Digest {
    use sha2::Digest as _;
    Digest::new(Algorithm::Sha256, sha2::Sha256::digest(data).to_vec())
}

fn data(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn whole(server: &TestServer, path: &str, body: &[u8]) -> FileEntry {
    FileEntry::whole(
        path.into(),
        body.len() as u64,
        Some(sha256(body)),
        vec![server.url(&format!("/f/{path}"))],
    )
}

/// Serves `/f/<path>` from a map, with ranges.
fn file_server(files: Vec<(&str, Vec<u8>)>) -> TestServer {
    let files: Vec<(String, Vec<u8>)> =
        files.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    TestServer::start(move |req| {
        let path = req.path.strip_prefix("/f/").unwrap_or_default();
        match files.iter().find(|(k, _)| k == path) {
            Some((_, body)) => serve_bytes(req, body, true),
            None => Resp::status(404),
        }
    })
}

fn opts() -> Options {
    Options {
        retries: 1,
        ..Default::default()
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn run(
    root: &std::path::Path,
    files: &[FileEntry],
    remove: &[String],
    opts: &Options,
) -> anyhow::Result<Finished> {
    download(
        &client(),
        Job {
            root,
            files,
            remove,
        },
        opts,
        ignore_progress(),
        &CancellationToken::new(),
    )
    .await
}

#[tokio::test]
async fn downloads_skips_and_removes() {
    let a = data(300_000, 1);
    let b = data(10, 2);
    let server = file_server(vec![("a.bin", a.clone()), ("dir/b.txt", b.clone())]);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(root.join("old")).unwrap();
    std::fs::write(root.join("old/gone.txt"), b"x").unwrap();
    let mut files = vec![whole(&server, "a.bin", &a), whole(&server, "dir/b.txt", &b)];
    files.push(FileEntry {
        path: "empty".into(),
        size: 0,
        hash: None,
        parts: vec![],
        executable: true,
    });
    let reports = Arc::new(Mutex::new(Vec::new()));
    let sink = reports.clone();
    let journal = dir.path().join("journal");
    let journaled = Options {
        journal: Some(journal.clone()),
        ..opts()
    };
    let res = download(
        &client(),
        Job {
            root: &root,
            files: &files,
            remove: &["old/gone.txt".to_string()],
        },
        &journaled,
        Arc::new(move |p| sink.lock().push(p)),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(res, Finished::Done);
    assert_eq!(std::fs::read(root.join("a.bin")).unwrap(), a);
    assert_eq!(std::fs::read(root.join("dir/b.txt")).unwrap(), b);
    assert_eq!(std::fs::metadata(root.join("empty")).unwrap().len(), 0);
    assert!(!root.join("old").exists(), "emptied folders are pruned");
    assert!(!journal.exists(), "journal removed after success");
    let last = *reports.lock().last().unwrap();
    assert_eq!((last.bytes_done, last.bytes_total), (300_010, 300_010));
    assert_eq!(last.percent, 100.0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(root.join("empty"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }

    // Everything is intact: nothing is fetched again.
    let before = server.requests().len();
    run(&root, &files, &[], &opts()).await.unwrap();
    assert_eq!(server.requests().len(), before);

    // A damaged file is fetched again.
    std::fs::write(root.join("dir/b.txt"), b"0123456789").unwrap();
    run(&root, &files, &[], &opts()).await.unwrap();
    assert_eq!(std::fs::read(root.join("dir/b.txt")).unwrap(), b);
    assert_eq!(server.requests().len(), before + 1);
}

#[tokio::test]
async fn resumes_partial_files_with_ranges() {
    let a = data(100_000, 3);
    let server = file_server(vec![("a.bin", a.clone())]);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.bin.irori-part"), &a[..40_000]).unwrap();
    run(root, &[whole(&server, "a.bin", &a)], &[], &opts())
        .await
        .unwrap();
    assert_eq!(std::fs::read(root.join("a.bin")).unwrap(), a);
    assert!(!root.join("a.bin.irori-part").exists());
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].header("range"), Some("bytes=40000-"));
}

#[tokio::test]
async fn retries_cut_connections_and_rejects_bad_data() {
    let a = data(200_000, 4);
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let body = a.clone();
    let server = TestServer::start(move |req| {
        let mut resp = serve_bytes(req, &body, true);
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            resp.cut_after = Some(50_000);
        }
        resp
    });
    let dir = tempfile::tempdir().unwrap();
    let file = FileEntry::whole(
        "a".into(),
        a.len() as u64,
        Some(sha256(&a)),
        vec![server.url("/a")],
    );
    run(dir.path(), &[file], &[], &opts()).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("a")).unwrap(), a);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(server.requests()[1].header("range"), Some("bytes=50000-"));

    // The server's data never matches the checksum.
    let bad = FileEntry::whole(
        "b".into(),
        a.len() as u64,
        Some(sha256(b"other")),
        vec![server.url("/a")],
    );
    let err = run(dir.path(), &[bad], &[], &opts()).await.unwrap_err();
    assert!(format!("{err:#}").contains("checksum"), "{err:#}");
    assert!(!dir.path().join("b").exists());

    // Client errors are not retried.
    let server404 = TestServer::start(|_| Resp::status(404));
    let missing = FileEntry::whole("c".into(), 1, None, vec![server404.url("/c")]);
    let err = run(dir.path(), &[missing], &[], &opts()).await.unwrap_err();
    assert!(format!("{err:#}").contains("404"), "{err:#}");
    assert_eq!(server404.requests().len(), 1);
}

struct Reverse;

impl Decoder for Reverse {
    fn decode(&self, mut data: Vec<u8>) -> anyhow::Result<Vec<u8>> {
        data.reverse();
        Ok(data)
    }
}

#[tokio::test]
async fn builds_files_from_shared_chunks() {
    let c1 = data(1000, 5);
    let c2 = data(500, 6);
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    zlib.write_all(&c1).unwrap();
    let c1_z = zlib.finish().unwrap();
    let c2_rev: Vec<u8> = c2.iter().rev().copied().collect();
    let server = file_server(vec![("c1", c1_z.clone()), ("c2", c2_rev.clone())]);
    let chunk1 = Arc::new(Chunk {
        key: "c1".into(),
        urls: vec![server.url("/f/c1")],
        download_size: c1_z.len() as u64,
        size: 1000,
        hash: Some(sha256(&c1)),
        codec: Codec::Zlib,
        range: None,
    });
    let chunk2 = Arc::new(Chunk {
        key: "c2".into(),
        urls: vec![server.url("/f/missing"), server.url("/f/c2")],
        download_size: 500,
        size: 500,
        hash: None,
        codec: Codec::Custom(Arc::new(Reverse)),
        range: None,
    });
    let part = |c: &Arc<Chunk>, offset, size| Part::new(c.clone(), offset, size);
    let x: Vec<u8> = [&c1[100..400], &c2[..], &c1[900..]].concat();
    let y: Vec<u8> = c1[..50].to_vec();
    let files = vec![
        FileEntry {
            path: "x".into(),
            size: x.len() as u64,
            hash: Some(sha256(&x)),
            parts: vec![
                part(&chunk1, 100, 300),
                part(&chunk2, 0, 500),
                part(&chunk1, 900, 100),
            ],
            executable: false,
        },
        FileEntry {
            path: "sub/y".into(),
            size: 50,
            hash: Some(Digest::new(Algorithm::Md5, {
                use md5::Digest as _;
                md5::Md5::digest(&y).to_vec()
            })),
            parts: vec![part(&chunk1, 0, 50)],
            executable: false,
        },
    ];
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        workers: 1,
        retries: 0,
        ..Default::default()
    };
    run(dir.path(), &files, &[], &opts).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("x")).unwrap(), x);
    assert_eq!(std::fs::read(dir.path().join("sub/y")).unwrap(), y);
    let fetches = |path: &str| server.requests().iter().filter(|r| r.path == path).count();
    assert_eq!(fetches("/f/c1"), 1, "shared chunk downloaded once");
    // The first mirror of c2 refuses; the second one serves it.
    assert_eq!((fetches("/f/missing"), fetches("/f/c2")), (1, 1));
}

#[tokio::test]
async fn cancels() {
    let a = data(2_000_000, 7);
    let body = a.clone();
    let server = TestServer::start(move |req| {
        let mut r = serve_bytes(req, &body, true);
        r.chunk_delay = Some(std::time::Duration::from_millis(20));
        r
    });
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let files = [whole(&server, "a", &a)];
    let task = {
        let root = dir.path().to_path_buf();
        tokio::spawn(async move {
            download(
                &client(),
                Job {
                    root: &root,
                    files: &files,
                    remove: &[],
                },
                &opts(),
                ignore_progress(),
                &token,
            )
            .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if std::fs::metadata(dir.path().join("a.irori-part")).is_ok_and(|m| m.len() > 0) {
                break;
            }
            assert!(!task.is_finished(), "download finished before cancellation");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("partial download begins");
    cancel.cancel();
    assert_eq!(task.await.unwrap().unwrap(), Finished::Cancelled);
    assert!(!dir.path().join("a").exists());
    let partial = std::fs::metadata(dir.path().join("a.irori-part"))
        .unwrap()
        .len();
    assert!(partial > 0 && partial < a.len() as u64);
}

#[tokio::test]
async fn verifies_installs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("good"), b"hello").unwrap();
    std::fs::write(root.join("bad"), b"hellO").unwrap();
    std::fs::write(root.join("short"), b"hell").unwrap();
    let h = sha256(b"hello");
    let entry = |p: &str| FileEntry::whole(p.into(), 5, Some(h.clone()), vec![]);
    let files = [
        entry("good"),
        entry("bad"),
        entry("short"),
        entry("missing"),
    ];
    let res = verify(
        root,
        &files,
        2,
        ignore_progress(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        res,
        Verified::Checked(vec![
            (1, Problem::WrongHash),
            (2, Problem::WrongSize),
            (3, Problem::Missing)
        ])
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        verify(root, &files, 2, ignore_progress(), &cancel)
            .await
            .unwrap(),
        Verified::Cancelled
    );
}

#[tokio::test]
async fn rejects_unsafe_paths_and_low_space() {
    let dir = tempfile::tempdir().unwrap();
    let evil = FileEntry::whole("../x".into(), 1, None, vec![]);
    assert!(run(dir.path(), &[evil], &[], &opts()).await.is_err());
    let huge = FileEntry::whole(
        "x".into(),
        u64::MAX / 2,
        None,
        vec!["http://127.0.0.1:1/".into()],
    );
    let err = run(dir.path(), &[huge], &[], &opts()).await.unwrap_err();
    assert!(
        err.to_string().contains("Not enough free disk space"),
        "{err:#}"
    );
}

fn raw_chunk(server: &TestServer, key: &str, data: &[u8]) -> Arc<Chunk> {
    Arc::new(Chunk {
        key: key.into(),
        urls: vec![server.url(&format!("/f/{key}"))],
        download_size: data.len() as u64,
        size: data.len() as u64,
        hash: Some(sha256(data)),
        codec: Codec::Raw,
        range: None,
    })
}

#[tokio::test]
async fn reuses_local_bytes_and_falls_back_when_they_are_wrong() {
    let c1 = data(1000, 8);
    let c2 = data(1000, 9);
    let server = file_server(vec![("c1", c1.clone()), ("c2", c2.clone())]);
    let (k1, k2) = (raw_chunk(&server, "c1", &c1), raw_chunk(&server, "c2", &c2));
    // The old file holds c1's bytes at offset 500; the new one is c1 + c2.
    let dir = tempfile::tempdir().unwrap();
    let old: Vec<u8> = [vec![0u8; 500], c1.clone()].concat();
    std::fs::write(dir.path().join("x"), &old).unwrap();
    let new: Vec<u8> = [c1.clone(), c2.clone()].concat();
    let mut reused = Part::new(k1.clone(), 0, 1000);
    reused.reuse = Some(Reuse {
        path: "x".into(),
        offset: 500,
    });
    let file = FileEntry {
        path: "x".into(),
        size: 2000,
        hash: Some(sha256(&new)),
        parts: vec![reused.clone(), Part::new(k2.clone(), 0, 1000)],
        executable: false,
    };
    run(dir.path(), std::slice::from_ref(&file), &[], &opts())
        .await
        .unwrap();
    assert_eq!(std::fs::read(dir.path().join("x")).unwrap(), new);
    let fetched = |p: &str| server.requests().iter().filter(|r| r.path == p).count();
    assert_eq!((fetched("/f/c1"), fetched("/f/c2")), (0, 1));

    // Wrong local bytes: the checksum fails and the retry downloads.
    std::fs::write(dir.path().join("x"), vec![7u8; 2000]).unwrap();
    run(dir.path(), &[file], &[], &opts()).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("x")).unwrap(), new);
    assert_eq!(fetched("/f/c1"), 1);
}

struct TestSigner {
    base: String,
    calls: Mutex<Vec<Vec<String>>>,
}

/// A signing service that never answers.
struct HungSigner;

impl UrlSigner for HungSigner {
    fn sign<'a>(
        &'a self,
        _: &'a [String],
    ) -> futures::future::BoxFuture<'a, anyhow::Result<std::collections::HashMap<String, String>>>
    {
        Box::pin(futures::future::pending())
    }
}

#[tokio::test]
async fn stops_at_once_when_cancelled_while_signing_hangs() {
    let chunk = Arc::new(Chunk {
        key: "k0".into(),
        download_size: 100,
        size: 100,
        ..Default::default()
    });
    let files = [FileEntry {
        path: "f".into(),
        size: 100,
        hash: None,
        parts: vec![Part::new(chunk, 0, 100)],
        executable: false,
    }];
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let task = {
        let root = dir.path().to_path_buf();
        tokio::spawn(async move {
            let opts = Options {
                signer: Some(Signer(Arc::new(HungSigner))),
                ..Default::default()
            };
            download(
                &client(),
                Job {
                    root: &root,
                    files: &files,
                    remove: &[],
                },
                &opts,
                ignore_progress(),
                &token,
            )
            .await
        })
    };
    // Past one signing timeout: the hung batch is being retried.
    tokio::time::sleep(STALL + STALL / 2).await;
    assert!(!task.is_finished(), "a hung signer is retried, not fatal");
    cancel.cancel();
    // Well inside one signing timeout: the cancel itself ends the download.
    let finished = tokio::time::timeout(STALL / 2, task)
        .await
        .expect("cancelling stops a download stuck on signing")
        .unwrap()
        .unwrap();
    assert_eq!(finished, Finished::Cancelled);
}

impl UrlSigner for TestSigner {
    fn sign<'a>(
        &'a self,
        keys: &'a [String],
    ) -> futures::future::BoxFuture<'a, anyhow::Result<std::collections::HashMap<String, String>>>
    {
        Box::pin(async move {
            let mut calls = self.calls.lock();
            calls.push(keys.to_vec());
            // The first signature of "k0" is already expired.
            let round = calls.len();
            Ok(keys
                .iter()
                .map(|k| {
                    let sig = if k == "k0" && round == 1 { "old" } else { "ok" };
                    (k.clone(), format!("{}/s/{k}?sig={sig}", self.base))
                })
                .collect())
        })
    }
}

#[tokio::test]
async fn signs_chunk_addresses_in_batches() {
    let blobs: Vec<Vec<u8>> = (0..60).map(|i| data(100, i as u8)).collect();
    let served = blobs.clone();
    let server = TestServer::start(move |req| {
        let Some((key, sig)) = req
            .path
            .strip_prefix("/s/")
            .and_then(|r| r.split_once("?sig="))
        else {
            return Resp::status(404);
        };
        if sig != "ok" {
            return Resp::status(403);
        }
        let i: usize = key.trim_start_matches('k').parse().unwrap();
        serve_bytes(req, &served[i], false)
    });
    let signer = Arc::new(TestSigner {
        base: server.base(),
        calls: Mutex::new(Vec::new()),
    });
    let files: Vec<FileEntry> = blobs
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let chunk = Arc::new(Chunk {
                key: format!("k{i}"),
                urls: vec![],
                download_size: 100,
                size: 100,
                hash: None,
                codec: Codec::Raw,
                range: None,
            });
            FileEntry {
                path: format!("f{i:02}"),
                size: 100,
                hash: Some(sha256(b)),
                parts: vec![Part::new(chunk, 0, 100)],
                executable: false,
            }
        })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        workers: 1,
        retries: 2,
        signer: Some(Signer(signer.clone())),
        ..Default::default()
    };
    run(dir.path(), &files, &[], &opts).await.unwrap();
    for (i, b) in blobs.iter().enumerate() {
        assert_eq!(
            &std::fs::read(dir.path().join(format!("f{i:02}"))).unwrap(),
            b
        );
    }
    let calls = signer.calls.lock();
    // First batch of 50, a re-sign of the expired k0, then the remaining 10.
    assert_eq!(calls[0].len(), SIGN_BATCH);
    assert_eq!(calls[1][0], "k0");
    assert!(calls.iter().all(|c| c.len() <= SIGN_BATCH));
    let signed: std::collections::HashSet<&String> = calls.iter().flatten().collect();
    assert_eq!(signed.len(), 60);
}

#[tokio::test]
async fn downloads_ranges_and_deflate_entries() {
    // A "zip" with a stored file at 10..1010 and a deflated one after it.
    let stored = data(1000, 11);
    let plain = data(300_000, 12);
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&plain).unwrap();
    let deflated = z.finish().unwrap();
    let archive: Vec<u8> = [vec![0u8; 10], stored.clone(), deflated.clone()].concat();
    let server = file_server(vec![("archive", archive)]);
    let crc = |d: &[u8]| {
        let mut c = flate2::Crc::new();
        c.update(d);
        Digest::new(Algorithm::Crc32, c.sum().to_be_bytes().to_vec())
    };
    let chunk = |range: (u64, u64), size: u64, codec: Codec| {
        Arc::new(Chunk {
            key: format!("{range:?}"),
            urls: vec![server.url("/f/archive")],
            download_size: range.1,
            size,
            codec,
            range: Some(range),
            ..Default::default()
        })
    };
    let files = vec![
        FileEntry {
            path: "stored.bin".into(),
            size: 1000,
            hash: Some(crc(&stored)),
            parts: vec![Part::new(chunk((10, 1000), 1000, Codec::Raw), 0, 1000)],
            executable: false,
        },
        FileEntry {
            path: "big.bin".into(),
            size: plain.len() as u64,
            hash: Some(crc(&plain)),
            parts: vec![Part::new(
                chunk(
                    (1010, deflated.len() as u64),
                    plain.len() as u64,
                    Codec::Deflate,
                ),
                0,
                plain.len() as u64,
            )],
            executable: false,
        },
        // A small slice fetched as a chunk.
        FileEntry {
            path: "slice.bin".into(),
            size: 100,
            hash: None,
            parts: vec![Part::new(chunk((10, 1000), 1000, Codec::Raw), 100, 100)],
            executable: false,
        },
    ];
    let dir = tempfile::tempdir().unwrap();
    run(dir.path(), &files, &[], &opts()).await.unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("stored.bin")).unwrap(),
        stored
    );
    assert_eq!(std::fs::read(dir.path().join("big.bin")).unwrap(), plain);
    assert_eq!(
        std::fs::read(dir.path().join("slice.bin")).unwrap(),
        &stored[100..200]
    );
    assert!(
        server
            .requests()
            .iter()
            .all(|r| r.header("range").is_some()),
        "every request is ranged"
    );
}

/// A file made of two pieces of the object at each of `urls`.
fn chunked(path: &str, body: &[u8], urls: Vec<String>) -> FileEntry {
    let half = body.len() as u64 / 2;
    let piece = |i: usize, start: u64, len: u64| {
        Part::new(
            Arc::new(Chunk {
                key: format!("{path}{i}"),
                urls: urls.clone(),
                download_size: len,
                size: len,
                hash: None,
                codec: Codec::Raw,
                range: Some((start, len)),
            }),
            0,
            len,
        )
    };
    FileEntry {
        path: path.into(),
        size: body.len() as u64,
        hash: Some(sha256(body)),
        parts: vec![piece(0, 0, half), piece(1, half, body.len() as u64 - half)],
        executable: false,
    }
}

/// A server that answers every request after headers and `sent` bytes
/// with silence.
fn stalled_server(body: Vec<u8>, sent: usize) -> TestServer {
    TestServer::start(move |req| {
        let mut r = serve_bytes(req, &body, true);
        r.hang_after = Some(sent);
        r
    })
}

#[tokio::test]
async fn leaves_stalled_servers_for_working_ones() {
    let a = data(100_000, 9);
    let stuck = stalled_server(a.clone(), 20_000);
    let good = file_server(vec![("a", a.clone()), ("b", a.clone())]);
    let files = [
        // Pieces of a build...
        chunked("a", &a, vec![stuck.url("/f/a"), good.url("/f/a")]),
        // ...and whole files, which resume where the stalled server stopped.
        FileEntry::whole(
            "b".into(),
            a.len() as u64,
            Some(sha256(&a)),
            vec![stuck.url("/f/b"), good.url("/f/b")],
        ),
    ];
    let dir = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    run(dir.path(), &files, &[], &opts()).await.unwrap();
    assert!(started.elapsed() < STALL * 4, "{:?}", started.elapsed());
    assert_eq!(std::fs::read(dir.path().join("a")).unwrap(), a);
    assert_eq!(std::fs::read(dir.path().join("b")).unwrap(), a);
    assert_eq!(stuck.requests().len(), 3);
    let resumed = good
        .requests()
        .into_iter()
        .find(|r| r.path == "/f/b")
        .unwrap();
    assert_eq!(resumed.header("range"), Some("bytes=20000-"));
}

#[tokio::test]
async fn keeps_retrying_busy_servers_and_says_so() {
    let a = data(50_000, 3);
    let body = a.clone();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    // Busy for the first six requests, then fine: more than `retries`
    // allows. One piece at a time keeps the count exact.
    let server = TestServer::start(move |req| match counter.fetch_add(1, Ordering::SeqCst) {
        0..3 => Resp::status(503),
        3..6 => Resp::status(429),
        _ => serve_bytes(req, &body, true),
    });
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        retries: 0,
        workers: 1,
        ..Default::default()
    };
    let files = [chunked("a", &a, vec![server.url("/a")])];
    run(dir.path(), &files, &[], &opts).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("a")).unwrap(), a);
    assert_eq!(hits.load(Ordering::SeqCst), 8);

    // A server that never recovers: the download waits (saying why) until
    // it is cancelled.
    let down = TestServer::start(|_| Resp::status(503));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let seen = seen.clone();
        Arc::new(move |p: Progress| seen.lock().push(p.bottleneck)) as ProgressFn
    };
    let cancel = CancellationToken::new();
    let files = [chunked("c", &a, vec![down.url("/c")])];
    let root = dir.path().to_path_buf();
    let task = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            download(
                &client(),
                Job {
                    root: &root,
                    files: &files,
                    remove: &[],
                },
                &opts,
                sink,
                &cancel,
            )
            .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    cancel.cancel();
    assert_eq!(task.await.unwrap().unwrap(), Finished::Cancelled);
    assert!(down.requests().len() > 3);
    assert!(
        seen.lock()
            .contains(&Some(irori_core::Bottleneck::Reconnecting)),
        "{:?}",
        seen.lock()
    );
}

#[tokio::test]
async fn races_a_lagging_piece_at_the_end() {
    let a = data(800_000, 5);
    let body = a.clone();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    // The first request crawls along (not slowly enough to count as
    // stalled); a second one is quick.
    let server = TestServer::start(move |req| {
        let mut r = serve_bytes(req, &body, true);
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            r.chunk_delay = Some(std::time::Duration::from_millis(150));
        }
        r
    });
    let dir = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let files = [chunked("a", &a, vec![server.url("/a")])];
    run(dir.path(), &files, &[], &opts()).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("a")).unwrap(), a);
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    // Alone, the slow piece needs 25 × 150 ms.
    assert!(started.elapsed() < STALL * 3, "{:?}", started.elapsed());
}

#[tokio::test]
async fn reports_the_speed_limit_as_the_bottleneck() {
    let a = data(1_500_000, 2);
    let server = file_server(vec![("a", a.clone())]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let seen = seen.clone();
        Arc::new(move |p: Progress| seen.lock().push(p.bottleneck)) as ProgressFn
    };
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        max_bytes_per_sec: 500_000,
        ..opts()
    };
    let files = [whole(&server, "a", &a)];
    download(
        &client(),
        Job {
            root: dir.path(),
            files: &files,
            remove: &[],
        },
        &opts,
        sink,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(
        seen.lock()
            .contains(&Some(irori_core::Bottleneck::SpeedLimit)),
        "{:?}",
        seen.lock()
    );
}
