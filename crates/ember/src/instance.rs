//! Single instance: the first process listens on a Unix socket; later
//! invocations forward their arguments to it and exit.

use std::io::{Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    // One instance per data folder, so a portable install (EMBER_HOME) runs
    // beside the regular one.
    let config = ember_core::Paths::get()
        .config
        .to_string_lossy()
        .into_owned();
    let tag = config.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    dir.join(format!("ember-{}-{tag:016x}.sock", nix::unistd::getuid()))
}

/// Become the primary instance (returns its listener) or forward `args` to the
/// running one (returns `None`).
pub fn acquire(args: &[String]) -> Option<UnixListener> {
    let path = socket_path();
    if let Ok(mut stream) = UnixStream::connect(&path) {
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let payload = args.join("\0");
        if stream.write_all(payload.as_bytes()).is_ok() {
            return None;
        }
    }
    let _ = std::fs::remove_file(&path);
    match UnixListener::bind(&path) {
        Ok(l) => Some(l),
        Err(e) => {
            tracing::warn!("single-instance socket unavailable: {e}");
            // Run anyway; a second instance just won't be forwarded.
            UnixListener::bind(
                std::env::temp_dir().join(format!("ember-{}.sock", std::process::id())),
            )
            .ok()
        }
    }
}

/// Accept forwarded invocations on a background thread.
pub fn serve(listener: UnixListener, on_args: impl Fn(Vec<String>) + Send + 'static) {
    std::thread::Builder::new()
        .name("ember-instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buf = String::new();
                if stream.read_to_string(&mut buf).is_ok() {
                    let args = buf
                        .split('\0')
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                    on_args(args);
                }
            }
        })
        .ok();
}

/// Remove the socket on exit.
pub fn release() {
    let _ = std::fs::remove_file(socket_path());
}
