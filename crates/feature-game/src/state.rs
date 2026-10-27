//! Pure view-state derivation: which primary action a game shows, and how a
//! DLC toggle maps onto store operations. No views or contexts, so every
//! transition is unit-tested.

use gpui_kit::SharedString;
use irori_core::{GameInfo, Runner, StatusKind};
use irori_services::{DownloadQueueSnapshot, DownloadState};
use irori_ui::i18n::t;

/// Facts about a game that decide its primary action.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActionInputs {
    pub installed: bool,
    /// The store can install it (false for owned-but-uninstallable entries).
    pub installable: bool,
    /// Needs a third-party launcher Irori cannot drive.
    pub unsupported: bool,
    /// Live status from the status registry.
    pub status: Option<StatusKind>,
    /// State of this game's entry in the download queue.
    pub download: Option<DownloadState>,
    /// The whole queue is paused.
    pub queue_paused: bool,
    pub has_update: bool,
    /// Installed files were found (always true while unknown).
    pub files_available: bool,
    /// Play was clicked and the backend has not reported `Launching` yet.
    pub launch_pending: bool,
}

impl ActionInputs {
    /// Derive inputs from the game, its live status and the download queue.
    pub fn from_game(
        game: &GameInfo,
        status: Option<StatusKind>,
        queue: &DownloadQueueSnapshot,
        has_update: bool,
    ) -> Self {
        let id = game.id();
        let download = queue
            .active
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.state);
        Self {
            installed: game.is_installed,
            installable: game.installable || game.runner() == Runner::Sideload,
            unsupported: is_unsupported_third_party(game),
            status,
            download,
            queue_paused: queue.paused,
            has_update,
            files_available: true,
            launch_pending: false,
        }
    }
}

/// Whether a game needs a third-party launcher Irori cannot drive (anything
/// but the EA app and Ubisoft Connect).
pub fn is_unsupported_third_party(game: &GameInfo) -> bool {
    game.runner() != Runner::Sideload
        && game.third_party_managed_app.is_some()
        && !game.is_ea_managed
        && !game.is_ubisoft_managed
}

/// The main button of the game page (and of any other surface that offers
/// one-click play).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryAction {
    /// Open the install sheet.
    Install,
    /// Launch the game.
    Play,
    /// Queue the pending update (Play stays available next to it).
    Update,
    /// The game is running.
    Stop,
    /// The launch is being prepared.
    Launching,
    /// Its download is running; offers pause and cancel.
    Downloading { update: bool },
    /// Its download is paused (or the queue is); offers resume and cancel.
    Resume,
    /// Waiting in the download queue; offers removal.
    Queued,
    /// Another operation runs (verify, move, uninstall, sync…); nothing to do.
    Busy(StatusKind),
    /// Installed, but the files were not found; offers to locate them.
    Missing,
    /// Owned but not installable (e.g. GOG goodies).
    NotInstallable,
    /// Requires a third-party launcher Irori does not support.
    Unsupported,
}

impl PrimaryAction {
    /// Derive the action. Order matters: live operations win over install
    /// state, and a running game wins over everything.
    pub fn from_inputs(i: &ActionInputs) -> Self {
        use StatusKind as S;
        match i.status {
            Some(S::Playing) => return PrimaryAction::Stop,
            Some(S::Launching) => return PrimaryAction::Launching,
            _ => {}
        }
        if i.launch_pending && i.installed {
            return PrimaryAction::Launching;
        }
        match i.download {
            Some(DownloadState::Running) => {
                return PrimaryAction::Downloading {
                    update: i.installed,
                };
            }
            Some(DownloadState::Paused) => return PrimaryAction::Resume,
            Some(DownloadState::Queued) if i.queue_paused => return PrimaryAction::Resume,
            Some(DownloadState::Queued) => return PrimaryAction::Queued,
            _ => {}
        }
        match i.status {
            Some(S::Installing) => {
                return PrimaryAction::Downloading { update: false };
            }
            Some(S::Updating) => return PrimaryAction::Downloading { update: true },
            Some(S::Queued) => return PrimaryAction::Queued,
            Some(kind) if kind.is_busy() => return PrimaryAction::Busy(kind),
            _ => {}
        }
        if !i.installed {
            if i.unsupported {
                return PrimaryAction::Unsupported;
            }
            if !i.installable {
                return PrimaryAction::NotInstallable;
            }
            return PrimaryAction::Install;
        }
        if !i.files_available {
            return PrimaryAction::Missing;
        }
        if i.has_update {
            return PrimaryAction::Update;
        }
        PrimaryAction::Play
    }

    /// Whether the game is idle enough for management actions (verify, move,
    /// uninstall…).
    pub fn is_idle(self) -> bool {
        matches!(
            self,
            PrimaryAction::Install
                | PrimaryAction::Play
                | PrimaryAction::Update
                | PrimaryAction::Missing
                | PrimaryAction::NotInstallable
                | PrimaryAction::Unsupported
        )
    }

    /// Button label, in the interface language.
    pub fn label(self) -> SharedString {
        t(match self {
            PrimaryAction::Install => "game.action.install",
            PrimaryAction::Play => "game.action.play",
            PrimaryAction::Update => "game.action.update",
            PrimaryAction::Stop => "game.action.stop",
            PrimaryAction::Launching => "game.action.launching",
            PrimaryAction::Downloading { update: false } => "game.action.installing",
            PrimaryAction::Downloading { update: true } => "game.action.updating",
            PrimaryAction::Resume => "game.action.resume",
            PrimaryAction::Queued => "game.action.remove_from_queue",
            PrimaryAction::Busy(kind) => return crate::i18n::status_label(kind),
            PrimaryAction::Missing => "game.action.locate_files",
            PrimaryAction::NotInstallable => "game.action.not_installable",
            PrimaryAction::Unsupported => "game.action.not_supported",
        })
    }
}

/// How toggling one DLC is carried out by its store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DlcChange {
    /// The DLC is its own installable entry (Epic): install it next to the base game.
    InstallSeparately,
    /// The DLC is its own installed entry (Epic): uninstall it.
    UninstallSeparately,
    /// The DLC is part of the base install (GOG): update the base game with
    /// this DLC list.
    UpdateBase(Vec<String>),
    /// The store cannot change DLCs after install.
    Unsupported,
}

/// Plan a DLC toggle. `installed` are the DLCs currently installed with the
/// base game; `install` is the requested state of `dlc`.
pub fn plan_dlc_change(
    runner: Runner,
    installed: &[String],
    dlc: &str,
    install: bool,
) -> DlcChange {
    match runner {
        Runner::Legendary if install => DlcChange::InstallSeparately,
        Runner::Legendary => DlcChange::UninstallSeparately,
        Runner::Gog => {
            let mut next: Vec<String> = installed.iter().filter(|d| *d != dlc).cloned().collect();
            if install {
                next.push(dlc.to_string());
            }
            next.sort();
            DlcChange::UpdateBase(next)
        }
        _ => DlcChange::Unsupported,
    }
}

/// Completion of an achievement list, rounded percent.
pub fn completion_percent(unlocked: usize, total: usize) -> u32 {
    if total == 0 {
        0
    } else {
        ((unlocked as f64 / total as f64) * 100.0).round() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_core::GameInfo;
    use irori_services::{DownloadItem, DownloadKind};

    fn base() -> ActionInputs {
        ActionInputs {
            installable: true,
            files_available: true,
            ..Default::default()
        }
    }

    fn action(f: impl FnOnce(&mut ActionInputs)) -> PrimaryAction {
        let mut i = base();
        f(&mut i);
        PrimaryAction::from_inputs(&i)
    }

    #[test]
    fn idle_states() {
        assert_eq!(action(|_| {}), PrimaryAction::Install);
        assert_eq!(action(|i| i.installed = true), PrimaryAction::Play);
        assert_eq!(
            action(|i| {
                i.installed = true;
                i.has_update = true;
            }),
            PrimaryAction::Update
        );
        assert_eq!(
            action(|i| i.installable = false),
            PrimaryAction::NotInstallable
        );
        assert_eq!(action(|i| i.unsupported = true), PrimaryAction::Unsupported);
        assert_eq!(
            action(|i| {
                i.installed = true;
                i.files_available = false;
                i.has_update = true;
            }),
            PrimaryAction::Missing
        );
    }

    #[test]
    fn running_game_wins() {
        assert_eq!(
            action(|i| {
                i.installed = true;
                i.status = Some(StatusKind::Playing);
                i.download = Some(DownloadState::Queued);
            }),
            PrimaryAction::Stop
        );
        assert_eq!(
            action(|i| {
                i.installed = true;
                i.launch_pending = true;
            }),
            PrimaryAction::Launching
        );
        assert_eq!(
            action(|i| i.status = Some(StatusKind::Launching)),
            PrimaryAction::Launching
        );
        // A stale pending flag on an uninstalled game does not block Install.
        assert_eq!(action(|i| i.launch_pending = true), PrimaryAction::Install);
    }

    #[test]
    fn download_queue_states() {
        assert_eq!(
            action(|i| i.download = Some(DownloadState::Queued)),
            PrimaryAction::Queued
        );
        assert_eq!(
            action(|i| {
                i.download = Some(DownloadState::Queued);
                i.queue_paused = true;
            }),
            PrimaryAction::Resume
        );
        assert_eq!(
            action(|i| i.download = Some(DownloadState::Paused)),
            PrimaryAction::Resume
        );
        assert_eq!(
            action(|i| i.download = Some(DownloadState::Running)),
            PrimaryAction::Downloading { update: false }
        );
        assert_eq!(
            action(|i| {
                i.installed = true;
                i.download = Some(DownloadState::Running);
            }),
            PrimaryAction::Downloading { update: true }
        );
        // Status alone (queue snapshot lagging) still shows progress.
        assert_eq!(
            action(|i| {
                i.installed = true;
                i.status = Some(StatusKind::Updating);
            }),
            PrimaryAction::Downloading { update: true }
        );
        // An installed game with a queued update shows the queue state, not Import.
        assert_eq!(
            action(|i| {
                i.installed = true;
                i.status = Some(StatusKind::Queued);
            }),
            PrimaryAction::Queued
        );
    }

    #[test]
    fn busy_operations() {
        for kind in [
            StatusKind::Repairing,
            StatusKind::Moving,
            StatusKind::Uninstalling,
            StatusKind::SyncingSaves,
            StatusKind::Winetricks,
            StatusKind::Redist,
            StatusKind::Importing,
        ] {
            let a = action(|i| {
                i.installed = true;
                i.status = Some(kind);
            });
            assert_eq!(a, PrimaryAction::Busy(kind));
            assert!(!a.is_idle());
        }
        // Finished statuses are idle.
        assert_eq!(
            action(|i| {
                i.installed = true;
                i.status = Some(StatusKind::Done);
            }),
            PrimaryAction::Play
        );
    }

    #[test]
    fn inputs_from_queue() {
        let game = GameInfo {
            runner: Some(Runner::Gog),
            app_name: "1".into(),
            title: "Game".into(),
            installable: true,
            ..Default::default()
        };
        let mut queue = DownloadQueueSnapshot::default();
        queue.active.push(DownloadItem {
            id: game.id(),
            title: game.title.clone(),
            kind: DownloadKind::Install(Default::default()),
            state: DownloadState::Running,
            added_at: 0,
            started_at: None,
            finished_at: None,
            download_size: 0,
            error: None,
            progress: None,
        });
        let i = ActionInputs::from_game(&game, None, &queue, false);
        assert_eq!(i.download, Some(DownloadState::Running));
        assert_eq!(
            PrimaryAction::from_inputs(&i),
            PrimaryAction::Downloading { update: false }
        );
    }

    #[test]
    fn third_party_support() {
        let mut g = GameInfo {
            runner: Some(Runner::Legendary),
            third_party_managed_app: Some("Origin".into()),
            ..Default::default()
        };
        assert!(is_unsupported_third_party(&g));
        g.is_ea_managed = true;
        assert!(!is_unsupported_third_party(&g));
    }

    #[test]
    fn dlc_plans() {
        assert_eq!(
            plan_dlc_change(Runner::Legendary, &[], "d", true),
            DlcChange::InstallSeparately
        );
        assert_eq!(
            plan_dlc_change(Runner::Legendary, &["d".into()], "d", false),
            DlcChange::UninstallSeparately
        );
        assert_eq!(
            plan_dlc_change(Runner::Gog, &["b".into()], "a", true),
            DlcChange::UpdateBase(vec!["a".into(), "b".into()])
        );
        assert_eq!(
            plan_dlc_change(Runner::Gog, &["a".into(), "b".into()], "a", false),
            DlcChange::UpdateBase(vec!["b".into()])
        );
        assert_eq!(
            plan_dlc_change(Runner::Nile, &[], "a", true),
            DlcChange::Unsupported
        );
    }

    #[test]
    fn completion() {
        assert_eq!(completion_percent(0, 0), 0);
        assert_eq!(completion_percent(1, 3), 33);
        assert_eq!(completion_percent(2, 3), 67);
    }
}
