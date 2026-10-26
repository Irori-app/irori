//! Several accounts of one store.
//!
//! [`MultiAccountStore`] runs one instance of a store per account (a *slot*),
//! each with its own config and cache directories, and presents them as one
//! [`Store`]. The first slot ([`MAIN_ACCOUNT`]) uses the store's usual
//! directories, so a single-account setup from earlier versions is that
//! slot, unchanged; further slots live in
//! [`Paths::account_config_dir`]/[`Paths::account_cache_dir`].
//!
//! The library is the union of the slots' games, keyed by app name as
//! before: a game installed with an account comes from that account, else
//! from the first account owning it. Every per-game call goes to that same
//! account, so an installed game keeps launching, updating and syncing
//! saves with the account it was installed with.
//!
//! `accounts.json` beside the main slot's files remembers each slot's
//! account. An account that logs out while games installed with it remain
//! stays listed, signed out, so logging in to it again brings them back.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use ember_core::{
    Achievement, Event, EventBus, ExtraFile, ExtraInfo, GameInfo, InstallInfo, InstallInfoQuery,
    InstallPlatform, InstallRequest, LaunchOption, LaunchPlan, LaunchRequest, LoginFlow,
    MAIN_ACCOUNT, OpContext, Outcome, Paths, RemoveArgs, Runner, SessionExpired, Store,
    StoreAccount, SyncDirection, UpdateRequest, UserInfo, async_trait, json_store,
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

/// Where one account's store instance keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotDirs {
    pub config: PathBuf,
    pub cache: PathBuf,
}

/// Builds the store instance of an extra account.
pub type MakeSlot<S> = Arc<dyn Fn(&SlotDirs) -> Arc<S> + Send + Sync>;

struct Slot<S> {
    id: String,
    store: Arc<S>,
}

/// What `accounts.json` remembers of a slot's account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Remembered {
    account_id: String,
    display_name: String,
    /// Games installed with the account when it logged out.
    installed: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Index {
    accounts: BTreeMap<String, Remembered>,
}

/// A store with one instance per account. See the module docs.
pub struct MultiAccountStore<S: Store> {
    runner: Runner,
    bus: EventBus,
    /// The main slot's config dir: `accounts.json` and `accounts/<slot>`.
    config_root: PathBuf,
    /// Directories of an extra slot.
    dirs_of: Arc<dyn Fn(&str) -> SlotDirs + Send + Sync>,
    make: MakeSlot<S>,
    /// Main slot first, then the others by number. Replaced, never mutated.
    slots: RwLock<Arc<Vec<Arc<Slot<S>>>>>,
    /// The instance a new account is signing in with, between
    /// [`Store::account_login_flow`] and [`Store::account_login`].
    pending: Mutex<Option<Arc<Slot<S>>>>,
    index: Mutex<Index>,
    /// Slots whose saved login the store refused, with its reason.
    expired: RwLock<BTreeMap<String, String>>,
    /// Serialises adding, removing and rediscovering slots.
    ops: tokio::sync::Mutex<()>,
}

impl<S: Store> MultiAccountStore<S> {
    /// Wrap `main`, the store built on its usual directories; `make` builds
    /// the instances of further accounts.
    pub fn new(main: Arc<S>, bus: EventBus, make: MakeSlot<S>) -> Arc<Self> {
        let runner = main.runner();
        let paths = Paths::get();
        Self::build(
            main,
            bus,
            make,
            paths.store_config_dir(runner),
            Arc::new(move |id| SlotDirs {
                config: Paths::get().account_config_dir(runner, id),
                cache: Paths::get().account_cache_dir(runner, id),
            }),
        )
    }

    /// Like [`MultiAccountStore::new`], keeping extra accounts in
    /// `<config_root>/accounts/<slot>` and `<cache_root>/<slot>` (tests).
    pub fn with_roots(
        main: Arc<S>,
        bus: EventBus,
        make: MakeSlot<S>,
        config_root: PathBuf,
        cache_root: PathBuf,
    ) -> Arc<Self> {
        let accounts = config_root.join("accounts");
        Self::build(
            main,
            bus,
            make,
            config_root,
            Arc::new(move |id| SlotDirs {
                config: accounts.join(id),
                cache: cache_root.join(id),
            }),
        )
    }

    fn build(
        main: Arc<S>,
        bus: EventBus,
        make: MakeSlot<S>,
        config_root: PathBuf,
        dirs_of: Arc<dyn Fn(&str) -> SlotDirs + Send + Sync>,
    ) -> Arc<Self> {
        let runner = main.runner();
        Arc::new(Self {
            runner,
            bus,
            config_root,
            dirs_of,
            make,
            slots: RwLock::new(Arc::new(vec![Arc::new(Slot {
                id: MAIN_ACCOUNT.to_string(),
                store: main,
            })])),
            pending: Mutex::new(None),
            index: Mutex::new(Index::default()),
            expired: RwLock::new(BTreeMap::new()),
            ops: tokio::sync::Mutex::new(()),
        })
    }

    /// The instance on the store's usual directories (EOS overlay, EGL
    /// sync and other store-wide tools).
    pub fn main(&self) -> Arc<S> {
        self.slots.read()[0].store.clone()
    }

    /// The instance a game belongs to: the account it was installed with,
    /// else the first account owning it, else the main one.
    pub fn store_for(&self, app_name: &str) -> Arc<S> {
        let slots = self.slots();
        owner(&slots, app_name).unwrap_or(&slots[0]).store.clone()
    }

    fn slots(&self) -> Arc<Vec<Arc<Slot<S>>>> {
        self.slots.read().clone()
    }

    fn slot(&self, id: &str) -> Result<Arc<Slot<S>>> {
        self.slots()
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .with_context(|| format!("This {} account is no longer here", self.runner))
    }

    fn index_file(&self) -> PathBuf {
        self.config_root.join("accounts.json")
    }

    fn accounts_dir(&self) -> PathBuf {
        self.config_root.join("accounts")
    }

    fn dirs(&self, id: &str) -> SlotDirs {
        (self.dirs_of)(id)
    }

    fn signed_in(&self) -> Vec<Arc<Slot<S>>> {
        self.slots()
            .iter()
            .filter(|s| s.store.user().is_some())
            .cloned()
            .collect()
    }

    /// More than one account is listed, so games and errors name theirs.
    /// Cheap enough for render: no game lists are built.
    fn several(&self) -> bool {
        let index = self.index.lock();
        self.slots()
            .iter()
            .filter(|s| s.store.user().is_some() || index.accounts.contains_key(&s.id))
            .count()
            > 1
    }

    fn name_of(&self, slot: &Slot<S>) -> Option<String> {
        slot.store
            .user()
            .map(|u| u.display_name)
            .or_else(|| {
                self.index
                    .lock()
                    .accounts
                    .get(&slot.id)
                    .map(|r| r.display_name.clone())
            })
            .filter(|n| !n.trim().is_empty())
    }

    async fn save_index(&self) {
        let index = self.index.lock().clone();
        let file = self.index_file();
        let written = tokio::task::spawn_blocking(move || json_store::write(&file, &index)).await;
        match written {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("saving the {} accounts: {e:#}", self.runner),
            Err(e) => tracing::warn!("saving the {} accounts: {e}", self.runner),
        }
    }

    /// Remember the accounts signed in now under their slots.
    async fn remember_signed_in(&self) {
        let changed = {
            let mut index = self.index.lock();
            let mut changed = false;
            for slot in self.slots().iter() {
                let Some(user) = slot.store.user() else {
                    continue;
                };
                let entry = Remembered {
                    account_id: user.account_id,
                    display_name: user.display_name,
                    installed: 0,
                };
                if index.accounts.get(&slot.id) != Some(&entry) {
                    index.accounts.insert(slot.id.clone(), entry);
                    changed = true;
                }
            }
            changed
        };
        if changed {
            self.save_index().await;
        }
    }

    /// Next free slot number: extra accounts are numbered from 2, after the
    /// main one.
    fn next_slot_id(&self) -> String {
        let existing = std::fs::read_dir(self.accounts_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()));
        let known = self
            .slots()
            .iter()
            .map(|s| s.id.clone())
            .chain(self.index.lock().accounts.keys().cloned())
            .chain(self.pending.lock().as_ref().map(|p| p.id.clone()))
            .filter_map(|id| id.parse::<u32>().ok())
            .collect::<Vec<_>>();
        let last = existing.chain(known).max().unwrap_or(1);
        (last.max(1) + 1).to_string()
    }

    /// Delete an extra slot's directories (an account that logged out with
    /// nothing installed, or a new login that was refused).
    async fn remove_slot_dirs(&self, id: &str) {
        if id == MAIN_ACCOUNT || id.parse::<u32>().is_err() {
            return;
        }
        let dirs = self.dirs(id);
        for dir in [dirs.config, dirs.cache] {
            let removed =
                tokio::task::spawn_blocking(move || match std::fs::remove_dir_all(&dir) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e).with_context(|| format!("removing {}", dir.display())),
                })
                .await;
            if let Ok(Err(e)) = removed {
                tracing::warn!("{e:#}");
            }
        }
    }

    /// Record how a call to the account in `slot` ended: a refused login is
    /// remembered for its row and named after the account, success clears it.
    fn track<T>(&self, slot: &Slot<S>, result: Result<T>) -> Result<T> {
        match &result {
            Ok(_) => {
                self.expired.write().remove(&slot.id);
                result
            }
            Err(e) => {
                let Some(expired) = SessionExpired::of(e) else {
                    return result;
                };
                self.expired
                    .write()
                    .insert(slot.id.clone(), expired.reason.clone());
                match self.name_of(slot).filter(|_| self.several()) {
                    Some(name) => Err(SessionExpired::new(
                        self.runner,
                        format!("{name}: {}", expired.reason),
                    )
                    .into()),
                    None => result,
                }
            }
        }
    }

    fn changed(&self) {
        self.bus.emit(Event::AccountChanged(self.runner));
        self.bus.emit(Event::LibraryChanged(self.runner));
    }

    /// The instance a new account signs in with: the main one while it
    /// holds no account, else a new slot.
    fn new_account_slot(&self) -> Arc<Slot<S>> {
        if let Some(pending) = self.pending.lock().clone() {
            return pending;
        }
        let main = self.slots()[0].clone();
        if main.store.user().is_none() && !self.index.lock().accounts.contains_key(MAIN_ACCOUNT) {
            return main;
        }
        let id = self.next_slot_id();
        let slot = Arc::new(Slot {
            store: (self.make)(&self.dirs(&id)),
            id,
        });
        *self.pending.lock() = Some(slot.clone());
        slot
    }

    /// Why `user`, just logged in to `slot`, cannot stay there: it is
    /// already listed in another slot.
    fn duplicate_of(&self, slot: &str, user: &UserInfo) -> Option<String> {
        if user.account_id.is_empty() {
            return None;
        }
        let store = self.runner.store_name();
        let name = if user.display_name.trim().is_empty() {
            user.account_id.as_str()
        } else {
            user.display_name.as_str()
        };
        for other in self.slots().iter().filter(|s| s.id != slot) {
            if let Some(u) = other.store.user() {
                if u.account_id == user.account_id {
                    return Some(format!("“{name}” is already logged in to {store}."));
                }
            } else if self
                .index
                .lock()
                .accounts
                .get(&other.id)
                .is_some_and(|r| r.account_id == user.account_id)
            {
                return Some(format!(
                    "“{name}” is listed with {store} as logged out. Use Log in on that account \
                     instead, so the games installed with it come back."
                ));
            }
        }
        None
    }
}

/// The slot a game belongs to: the one it is installed with, else the
/// first one owning it.
fn owner<'a, S: Store>(slots: &'a [Arc<Slot<S>>], app_name: &str) -> Option<&'a Arc<Slot<S>>> {
    if slots.len() == 1 {
        return slots.first();
    }
    let mut first = None;
    for slot in slots {
        if let Some(game) = slot.store.game(app_name) {
            if game.is_installed {
                return Some(slot);
            }
            first.get_or_insert(slot);
        }
    }
    first
}

/// Extra slot ids found on disk, in order.
fn slot_ids_on_disk(dir: &Path) -> Vec<String> {
    let mut ids: Vec<u32> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse().ok()))
        .collect();
    ids.sort_unstable();
    ids.into_iter().map(|n| n.to_string()).collect()
}

#[async_trait]
impl<S: Store> Store for MultiAccountStore<S> {
    fn runner(&self) -> Runner {
        self.runner
    }

    /// Find the accounts on disk and load each one.
    async fn init(&self) -> Result<()> {
        let _ops = self.ops.lock().await;
        let file = self.index_file();
        let index: Index =
            tokio::task::spawn_blocking(move || json_store::read_or_default(&file)).await?;
        *self.index.lock() = index;
        let dir = self.accounts_dir();
        let ids = tokio::task::spawn_blocking(move || slot_ids_on_disk(&dir)).await?;
        let current = self.slots();
        let mut slots = vec![current[0].clone()];
        for id in ids {
            let slot = current.iter().find(|s| s.id == id).cloned();
            slots.push(slot.unwrap_or_else(|| {
                Arc::new(Slot {
                    store: (self.make)(&self.dirs(&id)),
                    id,
                })
            }));
        }
        let results = futures::future::join_all(slots.iter().map(|s| s.store.init())).await;
        *self.slots.write() = Arc::new(slots.clone());
        self.remember_signed_in().await;
        let mut first_error = None;
        for (slot, result) in slots.iter().zip(results) {
            if let Err(e) = result {
                if slot.id == MAIN_ACCOUNT {
                    first_error.get_or_insert(e);
                } else {
                    tracing::error!("loading {} account {}: {e:#}", self.runner, slot.id);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn user(&self) -> Option<UserInfo> {
        self.slots().iter().find_map(|s| s.store.user())
    }

    fn login_flow(&self) -> LoginFlow {
        self.account_login_flow(None)
    }

    async fn login(&self, input: &str) -> Result<UserInfo> {
        self.account_login(None, input).await
    }

    /// Log out of every account.
    async fn logout(&self) -> Result<()> {
        for slot in self.slots().iter() {
            if slot.store.user().is_some() {
                self.account_logout(&slot.id).await?;
            }
        }
        Ok(())
    }

    fn accounts(&self) -> Vec<StoreAccount> {
        let index = self.index.lock().clone();
        let expired = self.expired.read().clone();
        self.slots()
            .iter()
            .filter_map(|slot| {
                if let Some(user) = slot.store.user() {
                    let games = slot.store.games();
                    let owned = || games.iter().filter(|g| !g.is_dlc);
                    return Some(StoreAccount {
                        slot: slot.id.clone(),
                        account_id: user.account_id,
                        display_name: user.display_name,
                        signed_in: true,
                        games: owned().count(),
                        installed: owned().filter(|g| g.is_installed).count(),
                        expired: expired.get(&slot.id).cloned(),
                    });
                }
                let remembered = index.accounts.get(&slot.id)?;
                Some(StoreAccount {
                    slot: slot.id.clone(),
                    account_id: remembered.account_id.clone(),
                    display_name: remembered.display_name.clone(),
                    signed_in: false,
                    games: 0,
                    installed: remembered.installed,
                    expired: None,
                })
            })
            .collect()
    }

    fn can_add_account(&self) -> bool {
        true
    }

    fn account_login_flow(&self, slot: Option<&str>) -> LoginFlow {
        match slot {
            Some(id) => match self.slot(id) {
                Ok(slot) => slot.store.login_flow(),
                Err(_) => self.slots()[0].store.login_flow(),
            },
            None => self.new_account_slot().store.login_flow(),
        }
    }

    async fn account_login(&self, slot: Option<&str>, input: &str) -> Result<UserInfo> {
        let _ops = self.ops.lock().await;
        let target = match slot {
            Some(id) => self.slot(id)?,
            None => self.new_account_slot(),
        };
        let added = !self.slots().iter().any(|s| Arc::ptr_eq(s, &target));
        let user = target.store.login(input).await?;
        if let Some(why) = self.duplicate_of(&target.id, &user) {
            if let Err(e) = target.store.logout().await {
                tracing::warn!("{e:#}");
            }
            if added {
                *self.pending.lock() = None;
                self.remove_slot_dirs(&target.id).await;
            }
            bail!(why);
        }
        if added {
            *self.pending.lock() = None;
            let mut slots = (*self.slots()).clone();
            slots.push(target.clone());
            *self.slots.write() = Arc::new(slots);
        }
        self.expired.write().remove(&target.id);
        self.remember_signed_in().await;
        self.changed();
        Ok(user)
    }

    /// Log out of one account. Its games installed here stay; the account
    /// stays listed, signed out, while there are any.
    async fn account_logout(&self, slot: &str) -> Result<()> {
        let _ops = self.ops.lock().await;
        let target = self.slot(slot)?;
        let installed = target
            .store
            .games()
            .iter()
            .filter(|g| g.is_installed && !g.is_dlc)
            .count();
        let user = target.store.user();
        target.store.logout().await?;
        self.expired.write().remove(slot);
        if installed > 0 {
            let mut index = self.index.lock();
            let entry = index.accounts.entry(slot.to_string()).or_default();
            if let Some(user) = user {
                entry.account_id = user.account_id;
                entry.display_name = user.display_name;
            }
            entry.installed = installed;
        } else {
            self.index.lock().accounts.remove(slot);
            if slot != MAIN_ACCOUNT {
                let slots = self
                    .slots()
                    .iter()
                    .filter(|s| s.id != slot)
                    .cloned()
                    .collect();
                *self.slots.write() = Arc::new(slots);
                self.remove_slot_dirs(slot).await;
            }
        }
        self.save_index().await;
        self.changed();
        Ok(())
    }

    async fn refresh_account(&self, slot: &str, full: bool) -> Result<()> {
        let target = self.slot(slot)?;
        let result = target.store.refresh(full).await;
        self.track(&target, result)
    }

    fn game_account(&self, app_name: &str) -> Option<UserInfo> {
        if !self.several() {
            return None;
        }
        let slots = self.slots();
        let slot = owner(&slots, app_name)?;
        slot.store.user().or_else(|| {
            self.index.lock().accounts.get(&slot.id).map(|r| UserInfo {
                account_id: r.account_id.clone(),
                display_name: r.display_name.clone(),
            })
        })
    }

    fn game_owners(&self) -> HashMap<String, Vec<String>> {
        let slots = self.signed_in();
        let mut owners: HashMap<String, Vec<String>> = HashMap::new();
        if slots.len() < 2 {
            return owners;
        }
        for slot in &slots {
            let Some(user) = slot.store.user() else {
                continue;
            };
            for game in slot.store.games() {
                owners
                    .entry(game.app_name.clone())
                    .or_default()
                    .push(user.account_id.clone());
            }
        }
        owners
    }

    /// Refresh every signed-in account. A refused login wins over other
    /// errors, so the user is asked to log in again.
    async fn refresh(&self, full: bool) -> Result<()> {
        let slots = self.signed_in();
        let results = futures::future::join_all(slots.iter().map(|s| s.store.refresh(full))).await;
        let mut first_error: Option<anyhow::Error> = None;
        for (slot, result) in slots.iter().zip(results) {
            let Err(e) = self.track(slot, result) else {
                continue;
            };
            let e = match self.name_of(slot).filter(|_| self.several()) {
                Some(name) if SessionExpired::of(&e).is_none() => {
                    e.context(format!("Could not refresh the games of “{name}”"))
                }
                _ => e,
            };
            let replaces = match &first_error {
                None => true,
                Some(old) => SessionExpired::of(old).is_none() && SessionExpired::of(&e).is_some(),
            };
            if replaces {
                first_error = Some(e);
            } else {
                tracing::warn!("refreshing {}: {e:#}", self.runner);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn games(&self) -> Vec<Arc<GameInfo>> {
        let slots = self.slots();
        if slots.len() == 1 {
            return slots[0].store.games();
        }
        let mut by_app: HashMap<String, Arc<GameInfo>> = HashMap::new();
        for slot in slots.iter() {
            for game in slot.store.games() {
                match by_app.get(&game.app_name) {
                    Some(kept) if kept.is_installed || !game.is_installed => {}
                    _ => {
                        by_app.insert(game.app_name.clone(), game);
                    }
                }
            }
        }
        by_app.into_values().collect()
    }

    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        self.store_for(app_name).game(app_name)
    }

    async fn extra_info(&self, app_name: &str) -> Result<ExtraInfo> {
        self.store_for(app_name).extra_info(app_name).await
    }

    async fn install_info(
        &self,
        app_name: &str,
        platform: InstallPlatform,
        query: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        self.store_for(app_name)
            .install_info(app_name, platform, query)
            .await
    }

    async fn updatable_games(&self) -> Result<Vec<String>> {
        let slots = self.signed_in();
        let results =
            futures::future::join_all(slots.iter().map(|s| s.store.updatable_games())).await;
        let mut apps = Vec::new();
        let mut first_error = None;
        for (slot, result) in slots.iter().zip(results) {
            match self.track(slot, result) {
                Ok(found) => apps.extend(found),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        match first_error {
            Some(e) if apps.is_empty() => Err(e),
            Some(e) => {
                tracing::warn!("update check for {}: {e:#}", self.runner);
                Ok(apps)
            }
            None => {
                apps.sort();
                apps.dedup();
                Ok(apps)
            }
        }
    }

    /// Installs with the account the request names when it owns the game
    /// and the game is not installed with another one.
    async fn install(
        &self,
        app_name: &str,
        request: &InstallRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        let slots = self.slots();
        let owning = owner(&slots, app_name).unwrap_or(&slots[0]);
        let installed = owning.store.game(app_name).is_some_and(|g| g.is_installed);
        let chosen = request
            .account
            .as_deref()
            .filter(|_| !installed)
            .and_then(|id| {
                slots.iter().find(|s| {
                    s.store.user().is_some_and(|u| u.account_id == id)
                        && s.store.game(app_name).is_some()
                })
            });
        let slot = chosen.unwrap_or(owning);
        let result = slot.store.install(app_name, request, ctx).await;
        self.track(slot, result)
    }

    async fn update(
        &self,
        app_name: &str,
        request: &UpdateRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        let slots = self.slots();
        let slot = owner(&slots, app_name).unwrap_or(&slots[0]);
        let result = slot.store.update(app_name, request, ctx).await;
        self.track(slot, result)
    }

    async fn repair(&self, app_name: &str, ctx: OpContext) -> Result<Outcome> {
        let slots = self.slots();
        let slot = owner(&slots, app_name).unwrap_or(&slots[0]);
        let result = slot.store.repair(app_name, ctx).await;
        self.track(slot, result)
    }

    async fn uninstall(&self, app_name: &str, args: RemoveArgs) -> Result<()> {
        self.store_for(app_name).uninstall(app_name, args).await
    }

    async fn import(&self, app_name: &str, path: &Path, platform: InstallPlatform) -> Result<()> {
        self.store_for(app_name)
            .import(app_name, path, platform)
            .await
    }

    async fn set_install_path(&self, app_name: &str, path: &Path) -> Result<()> {
        self.store_for(app_name)
            .set_install_path(app_name, path)
            .await
    }

    async fn forget_install(&self, app_name: &str) -> Result<()> {
        self.store_for(app_name).forget_install(app_name).await
    }

    async fn set_version_pinned(&self, app_name: &str, pinned: bool) -> Result<()> {
        self.store_for(app_name)
            .set_version_pinned(app_name, pinned)
            .await
    }

    async fn is_available(&self, app_name: &str) -> bool {
        self.store_for(app_name).is_available(app_name).await
    }

    fn supports_cloud_saves(&self) -> bool {
        self.main().supports_cloud_saves()
    }

    async fn default_save_path(&self, app_name: &str, prefix: Option<&Path>) -> Result<String> {
        self.store_for(app_name)
            .default_save_path(app_name, prefix)
            .await
    }

    async fn sync_saves(
        &self,
        app_name: &str,
        direction: SyncDirection,
        save_path: &str,
        ctx: OpContext,
    ) -> Result<String> {
        let slots = self.slots();
        let slot = owner(&slots, app_name).unwrap_or(&slots[0]);
        let result = slot
            .store
            .sync_saves(app_name, direction, save_path, ctx)
            .await;
        self.track(slot, result)
    }

    async fn launch_options(&self, app_name: &str) -> Result<Vec<LaunchOption>> {
        self.store_for(app_name).launch_options(app_name).await
    }

    async fn launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        ctx: OpContext,
    ) -> Result<LaunchPlan> {
        let slots = self.slots();
        let slot = owner(&slots, app_name).unwrap_or(&slots[0]);
        let result = slot.store.launch(app_name, request, ctx).await;
        self.track(slot, result)
    }

    async fn after_exit(&self, app_name: &str, played_secs: u64) -> Result<()> {
        self.store_for(app_name)
            .after_exit(app_name, played_secs)
            .await
    }

    async fn achievements(&self, app_name: &str, language: &str) -> Result<Vec<Achievement>> {
        self.store_for(app_name)
            .achievements(app_name, language)
            .await
    }

    /// Each game's longest total across the accounts.
    async fn remote_playtime(&self) -> Result<HashMap<String, u64>> {
        let slots = self.signed_in();
        let results =
            futures::future::join_all(slots.iter().map(|s| s.store.remote_playtime())).await;
        let mut totals: HashMap<String, u64> = HashMap::new();
        let mut first_error = None;
        for result in results {
            match result {
                Ok(found) => {
                    for (app, minutes) in found {
                        let total = totals.entry(app).or_default();
                        *total = (*total).max(minutes);
                    }
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        match first_error {
            Some(e) if totals.is_empty() => Err(e),
            _ => Ok(totals),
        }
    }

    async fn extras(&self, app_name: &str) -> Result<Vec<ExtraFile>> {
        self.store_for(app_name).extras(app_name).await
    }

    async fn extra_url(&self, app_name: &str, file: &ExtraFile) -> Result<(String, String)> {
        self.store_for(app_name).extra_url(app_name, file).await
    }
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;
