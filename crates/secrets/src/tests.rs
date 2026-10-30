use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::file;
use crate::vault::{Keyring, Vault};

const WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Default)]
struct Memory(Arc<Mutex<Option<String>>>);

impl Keyring for Memory {
    fn get(&self) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn set(&self, key: &str) -> Result<()> {
        *self.0.lock().unwrap() = Some(key.to_string());
        Ok(())
    }
}

struct Broken;

impl Keyring for Broken {
    fn get(&self) -> Result<Option<String>> {
        bail!("no Secret Service")
    }

    fn set(&self, _: &str) -> Result<()> {
        bail!("no Secret Service")
    }
}

fn vault(keyring: impl Keyring) -> Vault {
    Vault::new(Box::new(keyring), WAIT)
}

fn temp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stores/session.json");
    (dir, path)
}

fn read(vault: Option<&Vault>, path: &Path) -> Result<Option<Value>> {
    file::read(vault, path, &AtomicBool::new(false))
}

fn on_disk(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_missing_file_is_none() {
    let (_dir, path) = temp();
    assert!(
        read(Some(&vault(Memory::default())), &path)
            .unwrap()
            .is_none()
    );
}

#[test]
fn encrypts_with_a_key_from_the_keyring() {
    let (_dir, path) = temp();
    let keyring = Memory::default();
    let vault = vault(keyring.clone());
    file::write(Some(&vault), &path, &json!({"token": "hunter2"})).unwrap();

    assert!(!on_disk(&path).contains("hunter2"));
    assert!(keyring.0.lock().unwrap().is_some());
    assert_eq!(
        read(Some(&vault), &path).unwrap(),
        Some(json!({"token": "hunter2"}))
    );
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn a_new_run_opens_what_the_last_one_wrote() {
    let (_dir, path) = temp();
    let keyring = Memory::default();
    file::write(Some(&vault(keyring.clone())), &path, &json!({"a": 1})).unwrap();
    assert_eq!(
        read(Some(&vault(keyring)), &path).unwrap(),
        Some(json!({"a": 1}))
    );
}

#[test]
fn encrypting_twice_never_repeats_the_output() {
    let (_dir, path) = temp();
    let vault = vault(Memory::default());
    file::write(Some(&vault), &path, &json!({"a": 1})).unwrap();
    let first = on_disk(&path);
    file::write(Some(&vault), &path, &json!({"a": 1})).unwrap();
    assert_ne!(first, on_disk(&path));
}

#[test]
fn leaves_no_temp_files() {
    let (dir, path) = temp();
    file::write(Some(&vault(Memory::default())), &path, &json!(1)).unwrap();
    let names = std::fs::read_dir(dir.path().join("stores"))
        .unwrap()
        .count();
    assert_eq!(names, 1);
}

#[test]
fn logins_from_earlier_versions_are_encrypted_when_read() {
    let (_dir, path) = temp();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"token": "hunter2"}"#).unwrap();
    let vault = vault(Memory::default());

    assert_eq!(
        read(Some(&vault), &path).unwrap(),
        Some(json!({"token": "hunter2"}))
    );
    assert!(!on_disk(&path).contains("hunter2"));
    assert_eq!(
        read(Some(&vault), &path).unwrap(),
        Some(json!({"token": "hunter2"}))
    );
}

#[test]
fn without_a_keyring_files_stay_plain_and_private() {
    let (_dir, path) = temp();
    let vault = vault(Broken);
    file::write(Some(&vault), &path, &json!({"token": "x"})).unwrap();
    assert!(on_disk(&path).contains("token"));
    assert_eq!(
        read(Some(&vault), &path).unwrap(),
        Some(json!({"token": "x"}))
    );
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600);
}

#[cfg(unix)]
#[test]
fn reading_tightens_a_file_everyone_could_read() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_dir, path) = temp();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"token": "x"}"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    read(None, &path).unwrap();
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn a_locked_keyring_keeps_the_file_and_says_so() {
    let (_dir, path) = temp();
    file::write(Some(&vault(Memory::default())), &path, &json!({"a": 1})).unwrap();
    let before = on_disk(&path);
    let unreadable = AtomicBool::new(false);

    let read: Result<Option<Value>> = file::read(Some(&vault(Broken)), &path, &unreadable);

    assert!(format!("{:#}", read.unwrap_err()).contains("keyring"));
    assert!(unreadable.load(Ordering::Relaxed));
    assert_eq!(before, on_disk(&path));
}

#[test]
fn another_key_cannot_open_the_file() {
    let (_dir, path) = temp();
    file::write(Some(&vault(Memory::default())), &path, &json!({"a": 1})).unwrap();
    let before = on_disk(&path);
    let unreadable = AtomicBool::new(false);

    let read: Result<Option<Value>> =
        file::read(Some(&vault(Memory::default())), &path, &unreadable);

    assert!(read.is_err());
    assert!(unreadable.load(Ordering::Relaxed));
    assert_eq!(before, on_disk(&path));
}

#[test]
fn a_changed_file_is_refused() {
    let (_dir, path) = temp();
    let vault = vault(Memory::default());
    file::write(Some(&vault), &path, &json!({"a": 1})).unwrap();
    let mut sealed: Value = serde_json::from_str(&on_disk(&path)).unwrap();
    sealed["data"] = json!("AAAAAAAAAAAAAAAAAAAAAAAA");
    std::fs::write(&path, sealed.to_string()).unwrap();
    assert!(read(Some(&vault), &path).is_err());
}

#[test]
fn a_file_from_a_newer_version_is_refused() {
    let (_dir, path) = temp();
    let vault = vault(Memory::default());
    file::write(Some(&vault), &path, &json!({"a": 1})).unwrap();
    let mut sealed: Value = serde_json::from_str(&on_disk(&path)).unwrap();
    sealed["irori_sealed"] = json!(2);
    std::fs::write(&path, sealed.to_string()).unwrap();
    assert!(format!("{:#}", read(Some(&vault), &path).unwrap_err()).contains("newer"));
}

#[test]
fn a_stored_value_that_is_no_key_is_replaced() {
    let (_dir, path) = temp();
    let keyring = Memory(Arc::new(Mutex::new(Some("not a key".into()))));
    let vault = vault(keyring.clone());
    file::write(Some(&vault), &path, &json!({"a": 1})).unwrap();
    assert_ne!(keyring.0.lock().unwrap().as_deref(), Some("not a key"));
    assert_eq!(read(Some(&vault), &path).unwrap(), Some(json!({"a": 1})));
}

/// A keyring waiting for the user to answer an unlock prompt.
struct Prompting {
    answer: Mutex<mpsc::Receiver<()>>,
    inner: Memory,
}

impl Keyring for Prompting {
    fn get(&self) -> Result<Option<String>> {
        let _ = self.answer.lock().unwrap().recv();
        self.inner.get()
    }

    fn set(&self, key: &str) -> Result<()> {
        self.inner.set(key)
    }
}

#[test]
fn a_prompt_does_not_hold_callers_and_the_key_arrives_after_it() {
    let (tx, rx) = mpsc::channel();
    let vault = Vault::new(
        Box::new(Prompting {
            answer: Mutex::new(rx),
            inner: Memory::default(),
        }),
        Duration::from_millis(20),
    );
    assert!(vault.cipher().is_none());

    tx.send(()).unwrap();
    tx.send(()).unwrap();
    let deadline = Instant::now() + WAIT;
    while vault.cipher().is_none() {
        assert!(Instant::now() < deadline);
    }
}

#[test]
fn asks_the_keyring_once_per_run() {
    #[derive(Default)]
    struct Counting(Arc<Mutex<u32>>);
    impl Keyring for Counting {
        fn get(&self) -> Result<Option<String>> {
            *self.0.lock().unwrap() += 1;
            bail!("locked")
        }
        fn set(&self, _: &str) -> Result<()> {
            bail!("locked")
        }
    }
    let calls = Arc::new(Mutex::new(0));
    let vault = vault(Counting(calls.clone()));
    assert!(vault.cipher().is_none());
    assert!(vault.cipher().is_none());
    assert_eq!(*calls.lock().unwrap(), 1);
}

/// Reaches the real keyring, so it needs an opt-in and a keyring to spare:
/// `dbus-run-session` with a throwaway `gnome-keyring-daemon` on Linux.
#[test]
#[ignore]
fn os_keyring_round_trip() {
    if std::env::var_os("IRORI_KEYRING_SMOKE").is_none() {
        return;
    }
    let (_dir, path) = temp();
    let first = Vault::new(Box::new(crate::os_keyring::OsKeyring), WAIT);
    file::write(Some(&first), &path, &json!({"token": "hunter2"})).unwrap();
    assert!(!on_disk(&path).contains("hunter2"));

    let next_run = Vault::new(Box::new(crate::os_keyring::OsKeyring), WAIT);
    assert_eq!(
        read(Some(&next_run), &path).unwrap(),
        Some(json!({"token": "hunter2"}))
    );
}
