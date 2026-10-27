//! Backend behaviour across stores, over an in-memory store.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};

use anyhow::{Result, bail};
use irori_core::{
    Event, ExtraInfo, GameInfo, InstallInfo, InstallInfoQuery, InstallPlatform, InstallRequest,
    LaunchPlan, LaunchRequest, LoginFlow, OpContext, Outcome, Paths, RemoveArgs, Runner,
    SessionExpired, Store, UpdateRequest, UserInfo, async_trait,
};

use crate::Backend;

/// Keep every file the backend writes in a private temporary directory.
fn isolate_paths() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let root = std::env::temp_dir().join(format!("irori-services-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Paths::init(Paths::under(&root));
    });
}

/// A GOG-like store whose saved login can be refused and whose games can
/// have updates.
#[derive(Default)]
struct LoginStore {
    refused: AtomicBool,
    updates: parking_lot::Mutex<Vec<String>>,
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
        Ok(self.updates.lock().clone())
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

/// Title keys of the notices sent on `rx` so far.
fn notices(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<&'static str> {
    let mut keys = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let Event::Notice(notice) = event
            && let Some(key) = notice.title_key
        {
            keys.push(key);
        }
    }
    keys
}

#[test]
fn asks_once_to_log_in_again_and_forgets_after_a_good_refresh() {
    isolate_paths();
    let store = Arc::new(LoginStore::default());
    let dyn_store: irori_core::DynStore = store.clone();
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
    assert_eq!(
        notices(&mut rx),
        ["shell.notice.login_expired"],
        "told once, not on every refresh"
    );

    store.refused.store(false, Ordering::SeqCst);
    b.runtime()
        .block_on(b.refresh_store(Runner::Gog, false))
        .unwrap();
    assert!(b.expired_logins().is_empty());
}

#[test]
fn tells_about_updates_released_while_running() {
    isolate_paths();
    let store = Arc::new(LoginStore::default());
    let dyn_store: irori_core::DynStore = store.clone();
    let b = Backend::start_headless(move |_, _| vec![dyn_store]).unwrap();
    let rt = b.runtime();
    // The first check of a session only fills the badges.
    *store.updates.lock() = vec!["a".into()];
    rt.block_on(b.refresh_updates());
    let mut rx = b.subscribe();
    rt.block_on(b.refresh_updates());
    assert!(notices(&mut rx).is_empty(), "nothing new");
    *store.updates.lock() = vec!["a".into(), "b".into()];
    rt.block_on(b.refresh_updates());
    assert_eq!(notices(&mut rx), ["shell.notice.update_available"]);
    *store.updates.lock() = vec!["a".into(), "b".into(), "c".into(), "d".into()];
    rt.block_on(b.refresh_updates());
    assert_eq!(notices(&mut rx), ["shell.notice.updates_available"]);
}

#[test]
fn never_touches_files_of_games_steam_manages() {
    isolate_paths();
    let b = Backend::start_headless(|_, _| Vec::new()).unwrap();
    let id = irori_core::GameId::new(Runner::Steam, "620");
    let dir = tempfile::tempdir().unwrap();
    let rt = b.runtime();
    let refusals = [
        rt.block_on(b.move_install(&id, dir.path().to_path_buf())),
        rt.block_on(b.uninstall(&id, RemoveArgs::default())),
        rt.block_on(b.repair(&id)),
        rt.block_on(b.set_install_path(&id, dir.path())),
        rt.block_on(b.import(&id, dir.path(), InstallPlatform::Windows)),
    ];
    for result in refusals {
        let e = result.unwrap_err().to_string();
        assert!(e.contains("Steam installs, updates and removes"), "{e}");
    }
}

const MENU_SCRIPT: &str = r#"
name: Test Game
game_slug: test-game
version: Local
slug: test-game-local
runner: linux
script:
  game:
    exe: $GAMEDIR/lang.cfg
  installer:
  - input_menu:
      id: lang
      description: Choose the language
      options:
      - en: English
      - de: Deutsch
      preselect: en
  - write_file:
      file: $GAMEDIR/lang.cfg
      content: lang=$INPUT_lang
"#;

fn script_install(game_dir: &Path) -> crate::ScriptInstall {
    let script = irori_scripts::Script::parse(MENU_SCRIPT).unwrap();
    let ctx = crate::scripts::script_context(&Default::default(), &script, game_dir);
    let approvals = irori_scripts::Approvals::all(&irori_scripts::consent(&script, &ctx));
    crate::ScriptInstall {
        script: Arc::new(script),
        game_dir: game_dir.to_path_buf(),
        approvals,
        picked: Default::default(),
        source_url: Some("file:///scripts/test-game.yml".into()),
        resolution: None,
    }
}

#[test]
fn script_installs_ask_the_ui_and_produce_a_library_entry() {
    use futures::StreamExt as _;
    isolate_paths();
    let b = Backend::start_headless(|_, _| Vec::new()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let game_dir = tmp.path().join("test-game");
    let mut run = b.start_script_install(script_install(&game_dir));
    let rt = b.runtime();
    match rt.block_on(run.prompts.next()) {
        Some(crate::ScriptPrompt::Menu { request, reply }) => {
            assert_eq!(request.preselect.as_deref(), Some("en"));
            reply.send(Some("de".into())).unwrap();
        }
        other => panic!("expected the language menu, got {other:?}"),
    }
    let outcome = rt.block_on(run.result).unwrap().unwrap();
    assert_eq!(
        std::fs::read_to_string(game_dir.join("lang.cfg")).unwrap(),
        "lang=de"
    );
    let progress = run.progress.lock().clone();
    assert_eq!(progress.step.map(|s| (s.number, s.total)), Some((2, 2)));
    assert!(progress.wine.is_none(), "a Linux script uses no Wine");
    assert!(run.log_file.is_file());

    let game = crate::scripts::script_game(&outcome, &game_dir, None, None).unwrap();
    assert!(matches!(
        game.entry,
        crate::scripts::ScriptLibraryEntry::App { windows: false, .. }
    ));
    let record = game
        .settings
        .iter()
        .find(|(k, _)| *k == "iroriLutrisScript")
        .map(|(_, v)| v.clone())
        .unwrap();
    assert_eq!(record["slug"], "test-game");
    assert_eq!(record["sourceUrl"], "file:///scripts/test-game.yml");
}

#[test]
fn unanswered_script_questions_cancel_and_roll_back() {
    use futures::StreamExt as _;
    isolate_paths();
    let b = Backend::start_headless(|_, _| Vec::new()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let game_dir = tmp.path().join("test-game");
    let mut run = b.start_script_install(script_install(&game_dir));
    let rt = b.runtime();
    // The window closed the question: its reply sender is dropped.
    drop(rt.block_on(run.prompts.next()));
    let e = rt.block_on(run.result).unwrap().unwrap_err();
    assert!(irori_runtime::is_cancelled(&e), "{e:#}");
    assert!(!game_dir.exists());
    assert!(run.progress.lock().rolling_back);
}
