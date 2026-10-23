//! Minimal in-process HTTP/1.1 server for tests (no network access).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Req {
    /// Path including the query string.
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl Req {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, Default)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Close the connection after this many body bytes (Content-Length still
    /// announces the full body).
    pub cut_after: Option<usize>,
    /// Pause between 16 KiB body chunks.
    pub chunk_delay: Option<Duration>,
    /// Send this many body bytes, then nothing more (a stalled server).
    pub hang_after: Option<usize>,
}

impl Resp {
    pub fn status(status: u16) -> Self {
        Self {
            status,
            ..Default::default()
        }
    }

    pub fn json(value: serde_json::Value) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: value.to_string().into_bytes(),
            ..Default::default()
        }
    }
}

type Handler = dyn Fn(&Req) -> Resp + Send + Sync;

pub struct TestServer {
    pub addr: SocketAddr,
    requests: Arc<Mutex<Vec<Req>>>,
}

impl TestServer {
    pub fn start(handler: impl Fn(&Req) -> Resp + Send + Sync + 'static) -> Self {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        std_listener.set_nonblocking(true).expect("nonblocking");
        let addr = std_listener.local_addr().expect("addr");
        let listener = TcpListener::from_std(std_listener).expect("listener");
        let handler: Arc<Handler> = Arc::new(handler);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reqs = requests.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let handler = handler.clone();
                let reqs = reqs.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, handler, reqs).await;
                });
            }
        });
        Self { addr, requests }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn requests(&self) -> Vec<Req> {
        self.requests.lock().clone()
    }
}

async fn serve(
    stream: tokio::net::TcpStream,
    handler: Arc<Handler>,
    reqs: Arc<Mutex<Vec<Req>>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let mut parts = line.split_whitespace().skip(1);
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let req = Req { path, headers };
    reqs.lock().push(req.clone());
    let resp = handler(&req);

    let mut stream = reader.into_inner();
    let mut head = format!("HTTP/1.1 {} X\r\n", resp.status);
    let has_len = resp
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-length"));
    for (k, v) in &resp.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has_len {
        head.push_str(&format!("Content-Length: {}\r\n", resp.body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await?;
    let body = match resp.cut_after.or(resp.hang_after) {
        Some(n) => &resp.body[..n.min(resp.body.len())],
        None => &resp.body[..],
    };
    for chunk in body.chunks(16 * 1024) {
        stream.write_all(chunk).await?;
        if let Some(d) = resp.chunk_delay {
            stream.flush().await?;
            tokio::time::sleep(d).await;
        }
    }
    stream.flush().await?;
    if resp.hang_after.is_some() {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
    stream.shutdown().await?;
    Ok(())
}

/// Serve `data`, honouring `Range: bytes=N-` and `bytes=N-M` when `ranges`
/// is true.
pub fn serve_bytes(req: &Req, data: &[u8], ranges: bool) -> Resp {
    let len = data.len();
    let range = req
        .header("range")
        .filter(|_| ranges)
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| {
            let (a, b) = r.split_once('-')?;
            let start: usize = a.parse().ok()?;
            let end: usize = if b.is_empty() {
                len.saturating_sub(1)
            } else {
                b.parse::<usize>().ok()?.min(len.saturating_sub(1))
            };
            Some((start, end))
        });
    match range {
        Some((s, _)) if s >= len => Resp {
            status: 416,
            headers: vec![("Content-Range".into(), format!("bytes */{len}"))],
            ..Default::default()
        },
        Some((s, e)) => Resp {
            status: 206,
            headers: vec![("Content-Range".into(), format!("bytes {s}-{e}/{len}"))],
            body: data[s..=e].to_vec(),
            ..Default::default()
        },
        None => Resp {
            status: 200,
            body: data.to_vec(),
            ..Default::default()
        },
    }
}
