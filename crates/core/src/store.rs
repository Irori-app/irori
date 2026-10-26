//! The contract every store backend implements.
//!
//! A store owns its catalogue, credentials and downloads. The services layer
//! owns everything cross-cutting: the download queue, game statuses, launching
//! and process tracking, playtime, shortcuts and settings. Stores never touch
//! the UI; they report through [`OpContext`] and the [`EventBus`](crate::EventBus).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;

use crate::game::{Achievement, ExtraInfo, GameInfo, LaunchOption, UserInfo};
use crate::install::{
    ExtraFile, InstallInfo, InstallRequest, Outcome, ProgressSink, RemoveArgs, SyncDirection,
    UpdateRequest,
};
use crate::launch::{LaunchPlan, LaunchRequest};
use crate::process::CancellationToken;
use crate::runner::{InstallPlatform, Runner};

/// A store refused the saved login: the token was revoked or ran out, or the
/// account needs attention on the store's website. Logging in again fixes
/// it; retrying does not. Stores return it (anywhere in an `anyhow` chain)
/// so the launcher can ask the user to log in again instead of failing
/// quietly.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct SessionExpired {
    pub runner: Runner,
    /// The store's own explanation, in English.
    pub reason: String,
}

impl SessionExpired {
    pub fn new(runner: Runner, reason: impl Into<String>) -> Self {
        Self {
            runner,
            reason: reason.into(),
        }
    }

    /// The expired login behind `error`, if that is what it is.
    pub fn of(error: &anyhow::Error) -> Option<&SessionExpired> {
        error.downcast_ref()
    }
}

/// The login slot every store has: its own config and cache directories.
pub const MAIN_ACCOUNT: &str = "main";

/// One account of a store, as the accounts page lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreAccount {
    /// Which login this is: [`MAIN_ACCOUNT`] or a store-chosen id. Pass it
    /// back to [`Store::account_login`] and the other per-account calls.
    pub slot: String,
    pub account_id: String,
    pub display_name: String,
    /// False for an account that logged out while games installed with it
    /// stayed; logging in to it again brings them back.
    pub signed_in: bool,
    /// Games (not DLC) the account owns.
    pub games: usize,
    /// Games installed with this account.
    pub installed: usize,
    /// The store refused the saved login, with its reason.
    pub expired: Option<String>,
}

/// How a user logs in to a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginFlow {
    /// Open `url` in the browser. After signing in, the user copies what the
    /// page shows (an authorization code, a JSON blob or the final redirect URL)
    /// and Ember passes it to [`Store::login`]. The UI also watches the
    /// clipboard and submits a match of `capture` automatically.
    Browser {
        url: String,
        /// Regex with one capture group extracting the secret from pasted text.
        capture: String,
        /// What the user should copy, in plain words.
        instructions: String,
    },
    /// The store needs no account.
    None,
}

/// Reports from a running operation. Cheap to clone.
#[derive(Clone)]
pub struct OpContext {
    pub progress: ProgressSink,
    pub cancel: CancellationToken,
    /// Update the status detail line ("Installing redistributables").
    pub step: Arc<dyn Fn(&str) + Send + Sync>,
    /// Tool output for this operation is appended here.
    pub log_file: Option<PathBuf>,
    /// Effective settings of the game the operation is for (global defaults
    /// plus its overrides), when the caller knows the game.
    pub settings: Option<Arc<crate::settings::GameSettings>>,
}

impl OpContext {
    pub fn detached() -> Self {
        Self {
            progress: crate::install::ignore_progress(),
            cancel: CancellationToken::new(),
            step: Arc::new(|_| {}),
            log_file: None,
            settings: None,
        }
    }
}

/// Filters for [`Store::install_info`].
#[derive(Debug, Clone, Default)]
pub struct InstallInfoQuery {
    pub branch: Option<String>,
    pub build: Option<String>,
    pub language: Option<String>,
}

#[async_trait]
pub trait Store: Send + Sync + 'static {
    fn runner(&self) -> Runner;

    /// Load cached catalogue and credentials from disk. No network; must be fast
    /// because it gates the first library render.
    async fn init(&self) -> Result<()>;

    // ---- account -------------------------------------------------------

    /// Logged-in user, if any.
    fn user(&self) -> Option<UserInfo>;

    fn is_logged_in(&self) -> bool {
        self.user().is_some()
    }

    fn login_flow(&self) -> LoginFlow;

    /// Complete login with what the user pasted (code, JSON or redirect URL).
    async fn login(&self, input: &str) -> Result<UserInfo>;

    async fn logout(&self) -> Result<()>;

    /// Every account: the signed-in ones, and signed-out ones that games
    /// installed with them still belong to. Stores with one login list
    /// [`Store::user`].
    fn accounts(&self) -> Vec<StoreAccount> {
        let Some(user) = self.user() else {
            return Vec::new();
        };
        let games = self.games();
        let owned = || games.iter().filter(|g| !g.is_dlc);
        vec![StoreAccount {
            slot: MAIN_ACCOUNT.to_string(),
            account_id: user.account_id,
            display_name: user.display_name,
            signed_in: true,
            games: owned().count(),
            installed: owned().filter(|g| g.is_installed).count(),
            expired: None,
        }]
    }

    /// Whether another account can log in beside the current ones.
    fn can_add_account(&self) -> bool {
        false
    }

    /// How to log in to the account in `slot` again, or to a new account
    /// when `slot` is `None`.
    fn account_login_flow(&self, _slot: Option<&str>) -> LoginFlow {
        self.login_flow()
    }

    /// Complete [`Store::account_login_flow`] with what the user pasted.
    async fn account_login(&self, _slot: Option<&str>, input: &str) -> Result<UserInfo> {
        self.login(input).await
    }

    /// Log out of one account; the others stay.
    async fn account_logout(&self, _slot: &str) -> Result<()> {
        self.logout().await
    }

    /// Re-fetch one account's games.
    async fn refresh_account(&self, _slot: &str, full: bool) -> Result<()> {
        self.refresh(full).await
    }

    /// The account a game belongs to (the one it was installed with, else
    /// the one owning it), when the store has more than one account.
    fn game_account(&self, _app_name: &str) -> Option<UserInfo> {
        None
    }

    /// Account ids of the signed-in accounts owning each game, by app name,
    /// in account order. Empty unless more than one account is signed in.
    fn game_owners(&self) -> std::collections::HashMap<String, Vec<String>> {
        std::collections::HashMap::new()
    }

    // ---- catalogue -----------------------------------------------------

    /// Re-fetch the owned games. `full` also refreshes per-game metadata that
    /// is otherwise cached.
    async fn refresh(&self, full: bool) -> Result<()>;

    /// All library entries (DLCs included, flagged with `is_dlc`).
    fn games(&self) -> Vec<Arc<GameInfo>>;

    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>>;

    /// Store page details (description, requirements, changelog).
    async fn extra_info(&self, app_name: &str) -> Result<ExtraInfo>;

    /// Sizes and options for the install dialog.
    async fn install_info(
        &self,
        app_name: &str,
        platform: InstallPlatform,
        query: &InstallInfoQuery,
    ) -> Result<InstallInfo>;

    /// App names of installed games with an update available.
    async fn updatable_games(&self) -> Result<Vec<String>>;

    // ---- install lifecycle ---------------------------------------------

    async fn install(
        &self,
        app_name: &str,
        request: &InstallRequest,
        ctx: OpContext,
    ) -> Result<Outcome>;

    async fn update(
        &self,
        app_name: &str,
        request: &UpdateRequest,
        ctx: OpContext,
    ) -> Result<Outcome>;

    /// Verify files and redownload broken ones.
    async fn repair(&self, app_name: &str, ctx: OpContext) -> Result<Outcome>;

    async fn uninstall(&self, app_name: &str, args: RemoveArgs) -> Result<()>;

    /// Register an existing installation found at `path`.
    async fn import(&self, app_name: &str, path: &Path, platform: InstallPlatform) -> Result<()>;

    /// Record that the game's files now live at `path` (the services layer
    /// moves the files).
    async fn set_install_path(&self, app_name: &str, path: &Path) -> Result<()>;

    /// Forget an install without touching files (for installs whose files vanished).
    async fn forget_install(&self, app_name: &str) -> Result<()> {
        self.uninstall(
            app_name,
            RemoveArgs {
                delete_files: false,
                ..Default::default()
            },
        )
        .await
    }

    /// Keep an installed game at its current build: no update checks and no
    /// update prompt at launch (GOG rollback, Epic and ZOOM version pins).
    async fn set_version_pinned(&self, _app_name: &str, _pinned: bool) -> Result<()> {
        Ok(())
    }

    /// Whether the installed files are present (e.g. external drive mounted).
    async fn is_available(&self, app_name: &str) -> bool {
        self.game(app_name)
            .and_then(|g| g.install_path())
            .is_some_and(|p| p.exists())
    }

    // ---- cloud saves ---------------------------------------------------

    fn supports_cloud_saves(&self) -> bool {
        false
    }

    /// Resolve the default save location (expanding store variables inside the prefix).
    async fn default_save_path(&self, _app_name: &str, _prefix: Option<&Path>) -> Result<String> {
        bail!("cloud saves are not supported by this store")
    }

    async fn sync_saves(
        &self,
        _app_name: &str,
        _direction: SyncDirection,
        _save_path: &str,
        _ctx: OpContext,
    ) -> Result<String> {
        bail!("cloud saves are not supported by this store")
    }

    // ---- launching -----------------------------------------------------

    async fn launch_options(&self, _app_name: &str) -> Result<Vec<LaunchOption>> {
        Ok(Vec::new())
    }

    /// Build the command that starts the game. May perform store-specific
    /// preparation (redistributables, overlays, version checks) first.
    async fn launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        ctx: OpContext,
    ) -> Result<LaunchPlan>;

    /// Called after the game process exits (playtime upload, presence…).
    async fn after_exit(&self, _app_name: &str, _played_secs: u64) -> Result<()> {
        Ok(())
    }

    async fn achievements(&self, _app_name: &str, _language: &str) -> Result<Vec<Achievement>> {
        Ok(Vec::new())
    }

    /// Whole minutes played per app name as the store itself recorded them
    /// (its own launcher's sessions included). Empty for stores that keep no
    /// playtime or when syncing it is off.
    async fn remote_playtime(&self) -> Result<std::collections::HashMap<String, u64>> {
        Ok(std::collections::HashMap::new())
    }

    /// Files offered beside the game: offline installers, patches and
    /// goodies. Empty for stores without any.
    async fn extras(&self, _app_name: &str) -> Result<Vec<ExtraFile>> {
        Ok(Vec::new())
    }

    /// Where to download an extra file from right now (addresses expire),
    /// and the file name to save it under.
    async fn extra_url(&self, _app_name: &str, _file: &ExtraFile) -> Result<(String, String)> {
        anyhow::bail!("this store offers no extra files")
    }
}

pub type DynStore = Arc<dyn Store>;

/// Read access to current application settings, handed to stores at
/// construction (worker counts, speed limits, install paths).
pub type SettingsFn = Arc<dyn Fn() -> Arc<crate::settings::AppSettings> + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context as _;

    #[test]
    fn finds_an_expired_login_under_context() {
        let e = anyhow::Error::new(SessionExpired::new(Runner::Gog, "GOG answered 401"))
            .context("Could not refresh the GOG library");
        let found = SessionExpired::of(&e).expect("still visible under context");
        assert_eq!(found.runner, Runner::Gog);
        let other: anyhow::Result<()> = Err(anyhow::anyhow!("timed out")).context("refreshing");
        assert!(SessionExpired::of(&other.unwrap_err()).is_none());
    }
}
