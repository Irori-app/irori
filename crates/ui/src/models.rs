//! Observable mirrors of backend state. Each model is a small entity so views
//! observe only what they render: the library grid re-renders on library and
//! status changes, the settings page only on settings changes.
//!
//! One pump task receives backend events, drains everything queued, and applies
//! the batch in a single update, so a burst of events costs one render.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use ember_core::{
    AppSettings, Event, GameId, GameStatus, Notice, Runner, StatusKind, StoreAccount, UserInfo,
};
use ember_services::{DownloadQueueSnapshot, LibrarySnapshot, UserData};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, Task};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use crate::state::backend;

/// The aggregated library plus the user's own data about it.
pub struct LibraryModel {
    pub snapshot: Arc<LibrarySnapshot>,
    pub user: Arc<UserData>,
    pub updates: Arc<HashSet<GameId>>,
    /// A store refresh is in progress.
    pub refreshing: bool,
}

impl LibraryModel {
    pub fn is_favourite(&self, id: &GameId) -> bool {
        self.user.favourites.contains(&id.key())
    }

    pub fn is_hidden(&self, id: &GameId) -> bool {
        self.user.hidden.contains(&id.key())
    }

    pub fn has_update(&self, id: &GameId) -> bool {
        self.updates.contains(id)
    }

    pub fn playtime_minutes(&self, id: &GameId) -> u64 {
        self.user
            .playtime
            .get(&id.key())
            .map(|p| p.total_minutes)
            .unwrap_or(0)
    }

    pub fn last_played(&self, id: &GameId) -> Option<&str> {
        self.user
            .playtime
            .get(&id.key())
            .and_then(|p| p.last_played.as_deref())
    }

    pub fn categories_of(&self, id: &GameId) -> Vec<String> {
        let key = id.key();
        self.user
            .categories
            .iter()
            .filter(|(_, set)| set.contains(&key))
            .map(|(name, _)| name.clone())
            .collect()
    }
}

/// Live statuses (installing, playing…) of games.
#[derive(Default)]
pub struct StatusModel {
    statuses: HashMap<GameId, GameStatus>,
}

impl StatusModel {
    pub fn get(&self, id: &GameId) -> Option<&GameStatus> {
        self.statuses.get(id)
    }

    pub fn kind(&self, id: &GameId) -> Option<StatusKind> {
        self.statuses.get(id).map(|s| s.kind)
    }

    pub fn is_playing(&self, id: &GameId) -> bool {
        matches!(self.kind(id), Some(StatusKind::Playing))
    }

    pub fn all(&self) -> impl Iterator<Item = &GameStatus> {
        self.statuses.values()
    }

    pub fn playing(&self) -> impl Iterator<Item = &GameId> {
        self.statuses
            .values()
            .filter(|s| s.kind == StatusKind::Playing)
            .map(|s| &s.id)
    }
}

/// The download queue.
#[derive(Default)]
pub struct DownloadsModel {
    pub queue: DownloadQueueSnapshot,
    /// Single files: offline installers, patches, goodies.
    pub files: Vec<ember_services::FileDownload>,
}

/// Accounts of every store and connectivity.
pub struct AccountsModel {
    pub users: BTreeMap<Runner, Option<UserInfo>>,
    /// Every account of each store with accounts, in the store's order.
    pub accounts: BTreeMap<Runner, Vec<StoreAccount>>,
    /// Stores whose saved login was refused, with the store's reason.
    pub expired: BTreeMap<Runner, String>,
    pub online: bool,
}

impl AccountsModel {
    pub fn user(&self, runner: Runner) -> Option<&UserInfo> {
        self.users.get(&runner).and_then(Option::as_ref)
    }

    /// The store's accounts (signed in, or signed out with games installed).
    pub fn accounts(&self, runner: Runner) -> &[StoreAccount] {
        self.accounts.get(&runner).map_or(&[], Vec::as_slice)
    }

    pub fn is_logged_in(&self, runner: Runner) -> bool {
        self.user(runner).is_some()
    }

    /// The store refused the saved login; the user must log in again.
    pub fn login_expired(&self, runner: Runner) -> Option<&str> {
        self.expired.get(&runner).map(String::as_str)
    }

    pub fn any_logged_in(&self) -> bool {
        self.users
            .iter()
            .any(|(r, u)| *r != Runner::Sideload && u.is_some())
    }
}

/// Application settings snapshot.
pub struct SettingsModel {
    pub app: Arc<AppSettings>,
    /// Bumped whenever any game's settings change.
    pub game_revision: u64,
    pub last_changed_game: Option<GameId>,
}

/// Notices raised by the backend, drained by the window shell into toasts.
#[derive(Default)]
pub struct NoticeModel {
    pending: VecDeque<Notice>,
}

impl NoticeModel {
    pub fn drain(&mut self) -> Vec<Notice> {
        self.pending.drain(..).collect()
    }

    pub fn push(&mut self, notice: Notice, cx: &mut Context<Self>) {
        self.pending.push_back(notice);
        cx.notify();
    }
}

/// A game session that ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitedGame {
    pub id: GameId,
    pub seconds: u64,
    /// It failed to start: it exited with an error within seconds, unasked.
    pub crashed: bool,
    pub exit_code: Option<i32>,
}

/// Game sessions that ended, for "minimize on launch" restore and help after
/// a failed start. Drained by the shell.
#[derive(Default)]
pub struct SessionModel {
    pub exited: VecDeque<ExitedGame>,
}

/// Handles to every model, installed as a global.
#[derive(Clone)]
pub struct Models {
    pub library: Entity<LibraryModel>,
    pub statuses: Entity<StatusModel>,
    pub downloads: Entity<DownloadsModel>,
    pub accounts: Entity<AccountsModel>,
    pub settings: Entity<SettingsModel>,
    pub notices: Entity<NoticeModel>,
    pub sessions: Entity<SessionModel>,
}

impl Global for Models {}

/// The model handles. Panics if called before [`crate::init`].
pub fn models(cx: &App) -> &Models {
    cx.global::<Models>()
}

type AccountsNow = (
    BTreeMap<Runner, Option<UserInfo>>,
    BTreeMap<Runner, Vec<StoreAccount>>,
);

fn accounts_now(cx: &App) -> AccountsNow {
    let b = backend(cx);
    let users = b.stores().map(|s| (s.runner(), s.user())).collect();
    let accounts = b
        .stores()
        .filter(|s| s.runner().has_account())
        .map(|s| (s.runner(), s.accounts()))
        .collect();
    (users, accounts)
}

pub(crate) fn install(cx: &mut App) {
    let b = backend(cx);
    let library = cx.new(|_| LibraryModel {
        snapshot: b.library(),
        user: b.userdata.snapshot(),
        updates: b.updates(),
        refreshing: false,
    });
    let statuses = cx.new(|_| StatusModel {
        statuses: b
            .statuses
            .all()
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect(),
    });
    let downloads = cx.new(|_| DownloadsModel {
        queue: b.downloads(),
        files: b.file_downloads(),
    });
    let (users, accounts) = accounts_now(cx);
    let accounts = cx.new(|_| AccountsModel {
        users,
        accounts,
        expired: b.expired_logins(),
        online: b.is_online(),
    });
    let settings = cx.new(|_| SettingsModel {
        app: b.settings.app(),
        game_revision: 0,
        last_changed_game: None,
    });
    let notices = cx.new(|_| NoticeModel::default());
    let sessions = cx.new(|_| SessionModel::default());
    cx.set_global(Models {
        library,
        statuses,
        downloads,
        accounts,
        settings,
        notices,
        sessions,
    });
    spawn_pump(cx);
}

// Keep the subscription tied to this app. Reinitialization or shutdown
// cancels the receiver instead of leaving a detached task on an old model set.
struct EventPump {
    _task: Task<()>,
}

impl Global for EventPump {}

fn spawn_pump(cx: &mut App) {
    let mut rx = backend(cx).subscribe();
    let task = cx.spawn(async move |cx| {
        loop {
            let mut batch = Vec::with_capacity(16);
            let mut resync = false;
            match rx.recv().await {
                Ok(e) => batch.push(e),
                Err(RecvError::Lagged(_)) => resync = true,
                Err(RecvError::Closed) => break,
            }
            loop {
                match rx.try_recv() {
                    Ok(e) => batch.push(e),
                    Err(TryRecvError::Lagged(_)) => resync = true,
                    Err(_) => break,
                }
            }
            cx.update(|cx| apply(batch, resync, cx));
        }
    });
    cx.set_global(EventPump { _task: task });
}

/// Apply a batch of events, notifying each affected model once.
fn apply(batch: Vec<Event>, resync: bool, cx: &mut App) {
    let b = backend(cx);
    let m = models(cx).clone();
    let mut library = resync;
    let mut downloads = resync;
    let mut accounts = resync;
    let mut settings = resync;
    let mut statuses: Vec<Event> = Vec::new();
    let mut changed_game = None;

    for event in batch {
        match event {
            Event::LibraryUpdated(_) => {
                library = true;
                // Each account's game counts.
                accounts = true;
            }
            Event::GameChanged(_) => library = true,
            Event::LibraryChanged(_) => {}
            Event::Status(_) | Event::StatusCleared(_) => statuses.push(event),
            Event::AccountChanged(_) => {
                accounts = true;
            }
            Event::DownloadQueueChanged => downloads = true,
            Event::SettingsChanged => {
                settings = true;
                library = true;
            }
            Event::GameSettingsChanged(id) => changed_game = Some(id),
            Event::Connectivity(online) => {
                m.accounts.update(cx, |a, cx| {
                    a.online = online;
                    cx.notify();
                });
            }
            Event::Notice(n) => {
                crate::system_notify::notice(&n, cx);
                m.notices.update(cx, |model, cx| model.push(n, cx));
            }
            Event::GameExited {
                id,
                seconds,
                crashed,
                exit_code,
            } => {
                if crashed && let Some(game) = b.game(&id) {
                    crate::system_notify::failed_start(&id, &game.title, cx);
                }
                m.sessions.update(cx, |s, cx| {
                    s.exited.push_back(ExitedGame {
                        id,
                        seconds,
                        crashed,
                        exit_code,
                    });
                    cx.notify();
                });
            }
            Event::WineVersionsChanged => settings = true,
        }
    }

    if resync {
        let all = b.statuses.all();
        m.statuses.update(cx, |s, cx| {
            s.statuses = all.into_iter().map(|s| (s.id.clone(), s)).collect();
            cx.notify();
        });
    } else if !statuses.is_empty() {
        m.statuses.update(cx, |s, cx| {
            for e in statuses {
                match e {
                    Event::Status(st) => {
                        s.statuses.insert(st.id.clone(), st);
                    }
                    Event::StatusCleared(id) => {
                        s.statuses.remove(&id);
                    }
                    _ => {}
                }
            }
            cx.notify();
        });
    }
    if library {
        let (snapshot, user, updates) = (b.library(), b.userdata.snapshot(), b.updates());
        m.library.update(cx, |l, cx| {
            l.snapshot = snapshot;
            l.user = user;
            l.updates = updates;
            cx.notify();
        });
    }
    if downloads {
        let (queue, files) = (b.downloads(), b.file_downloads());
        m.downloads.update(cx, |d, cx| {
            d.queue = queue;
            d.files = files;
            cx.notify();
        });
    }
    if accounts {
        let (users, accounts) = accounts_now(cx);
        let expired = b.expired_logins();
        m.accounts.update(cx, |a, cx| {
            if a.users != users || a.accounts != accounts || a.expired != expired {
                a.users = users;
                a.accounts = accounts;
                a.expired = expired;
                cx.notify();
            }
        });
    }
    if settings || changed_game.is_some() {
        let app = b.settings.app();
        m.settings.update(cx, |s, cx| {
            s.app = app;
            if changed_game.is_some() {
                s.game_revision += 1;
                s.last_changed_game = changed_game;
            }
            cx.notify();
        });
    }
}

/// Mark the library as refreshing while `refresh_all` runs.
pub fn refresh_library(full: bool, cx: &mut App) {
    let m = models(cx).library.clone();
    if m.read(cx).refreshing {
        return;
    }
    m.update(cx, |l, cx| {
        l.refreshing = true;
        cx.notify();
    });
    let b = backend(cx);
    let task = crate::state::run_infallible(cx, async move { b.refresh_all(full).await });
    cx.spawn(async move |cx| {
        task.await;
        m.update(cx, |l, cx| {
            l.refreshing = false;
            cx.notify();
        });
    })
    .detach();
}
