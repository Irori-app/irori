//! The key that encrypts secret files, fetched once per run from a keyring.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use aes_gcm::aead::{KeyInit as _, OsRng};
use aes_gcm::{Aes256Gcm, Key};
use anyhow::{Context as _, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// Where the key is kept. Calls can block for as long as the user takes to
/// answer an unlock prompt.
pub(crate) trait Keyring: Send + Sync + 'static {
    /// The stored key; `None` when there is none, or none that can be read.
    fn get(&self) -> Result<Option<String>>;
    fn set(&self, key: &str) -> Result<()>;
}

enum KeyState {
    Idle,
    Loading,
    /// The outcome for this run; `None` when there is no usable keyring.
    Ready(Option<Box<Aes256Gcm>>),
}

struct Shared {
    state: Mutex<KeyState>,
    ready: Condvar,
}

pub(crate) struct Vault {
    keyring: Arc<dyn Keyring>,
    shared: Arc<Shared>,
    wait: Duration,
}

impl Vault {
    pub(crate) fn new(keyring: Box<dyn Keyring>, wait: Duration) -> Self {
        Self {
            keyring: Arc::from(keyring),
            shared: Arc::new(Shared {
                state: Mutex::new(KeyState::Idle),
                ready: Condvar::new(),
            }),
            wait,
        }
    }

    /// The encryption key, or `None` when it isn't available (yet). The
    /// keyring is asked once, on a thread of its own: a caller gives up after
    /// `wait`, and a later one finds the key once the user has unlocked it.
    pub(crate) fn cipher(&self) -> Option<Aes256Gcm> {
        let mut state = self.shared.state.lock().ok()?;
        if matches!(*state, KeyState::Idle) {
            *state = KeyState::Loading;
            self.start_loading();
        }
        let (state, _) = self
            .shared
            .ready
            .wait_timeout_while(state, self.wait, |s| matches!(s, KeyState::Loading))
            .ok()?;
        match &*state {
            KeyState::Ready(cipher) => cipher.as_deref().cloned(),
            _ => None,
        }
    }

    fn start_loading(&self) {
        let keyring = self.keyring.clone();
        let shared = self.shared.clone();
        let done = move |cipher| {
            if let Ok(mut state) = shared.state.lock() {
                *state = KeyState::Ready(cipher);
            }
            shared.ready.notify_all();
        };
        let finish = done.clone();
        let spawned = std::thread::Builder::new()
            .name("irori-keyring".into())
            .spawn(move || {
                let cipher = load_or_create(keyring.as_ref())
                    .inspect_err(|e| tracing::warn!("system keyring: {e:#}"))
                    .ok()
                    .map(Box::new);
                finish(cipher);
            });
        if let Err(e) = spawned {
            tracing::warn!("system keyring: {e}");
            done(None);
        }
    }
}

fn load_or_create(keyring: &dyn Keyring) -> Result<Aes256Gcm> {
    if let Some(cipher) = keyring.get()?.as_deref().and_then(decode) {
        return Ok(cipher);
    }
    let key = Aes256Gcm::generate_key(OsRng);
    keyring.set(&STANDARD.encode(key))?;
    // Another Irori process may have stored its own key at the same moment;
    // everyone has to end up using the one that stayed.
    keyring
        .get()?
        .as_deref()
        .and_then(decode)
        .context("the keyring did not keep the key")
}

/// `None` for anything that isn't a key, which is then replaced: it couldn't
/// open anything anyway.
fn decode(stored: &str) -> Option<Aes256Gcm> {
    let bytes = STANDARD.decode(stored).ok()?;
    (bytes.len() == 32).then(|| Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&bytes)))
}
