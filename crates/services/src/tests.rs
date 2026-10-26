//! Backend behaviour across stores, over an in-memory store.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};

use anyhow::{Result, bail};
use ember_core::{
    Event, ExtraInfo, GameInfo, InstallInfo, InstallInfoQuery, InstallPlatform, InstallRequest,
    LaunchPlan, LaunchRequest, LoginFlow, OpContext, Outcome, Paths, RemoveArgs, Runner,
    SessionExpired, Store, UpdateRequest, UserInfo, async_trait,
};

use crate::Backend;

/// Keep every file the backend writes in a private temporary directory.
fn isolate_paths() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let root = std::env::temp_dir().join(format!("ember-services-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Paths::init(Paths::under(&root));
    });
}

/// A GOG-like store whose saved login can be refused.
#[derive(Default)]
struct LoginStore {
    refused: AtomicBool,
}

#[async_trait]
impl Store for LoginStore {
    fn runner(&self) -> Runner {
        Runner::Gog
    }
    async fn init(&self) -> Result<()> {
        Ok(())
    }
    fn user(&self) -> Option<UserInfo> {
        Some(UserInfo::default())
    }
    fn login_flow(&self) -> LoginFlow {
        LoginFlow::None
    }
    async fn login(&self, _: &str) -> Result<UserInfo> {
        bail!("not supported")
    }
    async fn logout(&self) -> Result<()> {
        Ok(())
    }
    async fn refresh(&self, _: bool) -> Result<()> {
        if self.refused.load(Ordering::SeqCst) {
            return Err(anyhow::Error::new(SessionExpired::new(
                Runner::Gog,
                "GOG refused the login (401)",
            ))
            .context("Could not refresh the GOG library"));
        }
        Ok(())
    }
    fn games(&self) -> Vec<Arc<GameInfo>> {
        Vec::new()
    }
    fn game(&self, _: &str) -> Option<Arc<GameInfo>> {
        None
    }
    async fn extra_info(&self, _: &str) -> Result<ExtraInfo> {
        Ok(ExtraInfo::default())
    }
    async fn install_info(
        &self,
        _: &str,
        _: InstallPlatform,
        _: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        bail!("not supported")
    }
    async fn updatable_games(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    async fn install(&self, _: &str, _: &InstallRequest, _: OpContext) -> Result<Outcome> {
        bail!("not supported")
    }
    async fn update(&self, _: &str, _: &UpdateRequest, _: OpContext) -> Result<Outcome> {
        bail!("not supported")
    }
    async fn repair(&self, _: &str, _: OpContext) -> Result<Outcome> {
        bail!("not supported")
    }
    async fn uninstall(&self, _: &str, _: RemoveArgs) -> Result<()> {
        bail!("not supported")
    }
    async fn import(&self, _: &str, _: &Path, _: InstallPlatform) -> Result<()> {
        bail!("not supported")
    }
    async fn set_install_path(&self, _: &str, _: &Path) -> Result<()> {
        bail!("not supported")
    }
    async fn launch(&self, _: &str, _: &LaunchRequest, _: OpContext) -> Result<LaunchPlan> {
        bail!("not supported")
    }
}

/// Notices about logging in again that have been sent on `rx` so far.
fn login_notices(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> usize {
    let mut n = 0;
    while let Ok(event) = rx.try_recv() {
        if let Event::Notice(notice) = event
            && notice.title_key == Some("shell.notice.login_expired")
        {
            assert_eq!(notice.account, Some(Runner::Gog));
            n += 1;
        }
    }
    n
}

#[test]
fn asks_once_to_log_in_again_and_forgets_after_a_good_refresh() {
    isolate_paths();
    let store = Arc::new(LoginStore::default());
    let dyn_store: ember_core::DynStore = store.clone();
    let b = Backend::start_headless(move |_, _| vec![dyn_store]).unwrap();
    let mut rx = b.subscribe();

    store.refused.store(true, Ordering::SeqCst);
    for _ in 0..2 {
        let e = b
            .runtime()
            .block_on(b.refresh_store(Runner::Gog, false))
            .unwrap_err();
        assert!(SessionExpired::of(&e).is_some());
    }
    assert_eq!(
        b.expired_logins().get(&Runner::Gog).map(String::as_str),
        Some("GOG refused the login (401)")
    );
    assert_eq!(login_notices(&mut rx), 1, "told once, not on every refresh");

    store.refused.store(false, Ordering::SeqCst);
    b.runtime()
        .block_on(b.refresh_store(Runner::Gog, false))
        .unwrap();
    assert!(b.expired_logins().is_empty());
}
