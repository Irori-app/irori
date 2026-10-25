//! One instance per configuration directory, using a lifetime lock and an
//! authenticated loopback channel on every desktop platform.
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

pub struct Instance {
    listener: TcpListener,
    token: String,
    _lock: File,
}
#[derive(Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
}
#[derive(Serialize, Deserialize)]
struct Invocation {
    token: String,
    args: Vec<String>,
}

pub fn acquire(args: &[String]) -> Result<Option<Instance>> {
    acquire_in(&ember_core::Paths::get().config, args)
}

fn acquire_in(dir: &Path, args: &[String]) -> Result<Option<Instance>> {
    std::fs::create_dir_all(dir)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let lock = options.open(dir.join("instance.lock"))?;
    let endpoint_file = dir.join("instance.json");
    // The lock holder may still be publishing its endpoint. Never delete an
    // active instance's lock or start a second writer to the same settings.
    for _ in 0..40 {
        match lock.try_lock() {
            Ok(()) => {
                let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
                let token = uuid::Uuid::new_v4().to_string();
                let endpoint = Endpoint {
                    port: listener.local_addr()?.port(),
                    token: token.clone(),
                };
                let mut private = OpenOptions::new();
                private.write(true).create(true).truncate(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt as _;
                    private.mode(0o600);
                }
                private
                    .open(&endpoint_file)?
                    .write_all(&serde_json::to_vec(&endpoint)?)?;
                return Ok(Some(Instance {
                    listener,
                    token,
                    _lock: lock,
                }));
            }
            Err(TryLockError::WouldBlock) => {
                if let Ok(Some(endpoint)) = ember_core::json_store::read::<Endpoint>(&endpoint_file)
                {
                    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
                    if let Ok(mut stream) =
                        TcpStream::connect_timeout(&address, Duration::from_millis(200))
                    {
                        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                        let invocation = Invocation {
                            token: endpoint.token,
                            args: args.to_vec(),
                        };
                        stream.write_all(&serde_json::to_vec(&invocation)?)?;
                        stream.shutdown(std::net::Shutdown::Write)?;
                        let mut ack = [0];
                        if stream.read_exact(&mut ack).is_ok() && ack[0] == 1 {
                            return Ok(None);
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(TryLockError::Error(e)) => {
                return Err(e).context("Could not lock Ember's configuration");
            }
        }
    }
    bail!("Another Ember instance is starting or is not responding")
}

fn receive(mut stream: TcpStream, token: &str) -> Option<Vec<String>> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .ok()?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut stream)
        .take(64 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    let invocation: Invocation = serde_json::from_slice(&bytes).ok()?;
    if invocation.token != token {
        return None;
    }
    stream.write_all(&[1]).ok()?;
    Some(invocation.args)
}

pub fn serve(instance: Instance, on_args: impl Fn(Vec<String>) + Send + 'static) -> Result<()> {
    std::thread::Builder::new()
        .name("ember-instance".into())
        .spawn(move || {
            let _lock = instance._lock;
            for stream in instance.listener.incoming().flatten() {
                if let Some(args) = receive(stream, &instance.token) {
                    on_args(args);
                }
            }
        })
        .context("Could not start Ember's instance listener")?;
    Ok(())
}

// Locks are released by the OS on process exit, including crashes. Leaving
// the stable lock inode avoids races with another instance acquiring it.
pub fn release() {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forwards_arguments_without_losing_spaces_or_empty_values() {
        let dir = tempfile::tempdir().unwrap();
        let primary = acquire_in(dir.path(), &[]).unwrap().unwrap();
        let worker = std::thread::spawn(move || {
            let _lock = primary._lock;
            let (stream, _) = primary.listener.accept().unwrap();
            receive(stream, &primary.token).unwrap()
        });
        let args = vec!["--launch".into(), "gog/game with spaces".into(), "".into()];
        assert!(acquire_in(dir.path(), &args).unwrap().is_none());
        assert_eq!(worker.join().unwrap(), args);
        assert!(acquire_in(dir.path(), &[]).unwrap().is_some());
    }
    #[test]
    fn rejects_unauthenticated_invocations() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .write_all(br#"{"token":"wrong","args":["--launch","gog/1"]}"#)
            .unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(receive(listener.accept().unwrap().0, "secret").is_none());
    }
}
