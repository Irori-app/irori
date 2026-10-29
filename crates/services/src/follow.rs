//! Following a game that another program starts. The launcher's process is
//! not the game: Battle.net keeps running after the game ends, and Steam or
//! some launchers hand over and exit before it starts. While such a game
//! runs, its session lasts while a probe sees it (for third-party launchers,
//! processes running from its install folder), checked every few seconds
//! and only then.

use std::process::ExitStatus;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::process::Child;

/// How often the game's processes are looked for.
const POLL: Duration = Duration::from_secs(3);
/// How long the launcher gets to start the game before the session is the
/// launcher's own.
const START_GRACE: Duration = Duration::from_secs(180);

/// Whether any process runs from `folder`. Blocking.
fn game_running(folder: &str) -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_exe(UpdateKind::Always),
    );
    sys.processes().values().any(|p| {
        let cmd: Vec<String> = p
            .cmd()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        irori_core::process::runs_from(folder, p.exe(), &cmd)
    })
}

/// Whether the followed game runs right now. Blocking.
pub(crate) type Probe = Arc<dyn Fn() -> bool + Send + Sync>;

/// A probe for the game whose processes run from `folder`.
pub(crate) fn folder_probe(folder: String) -> Probe {
    Arc::new(move || game_running(&folder))
}

/// Wait for the followed game to end: once `probe` saw it and no longer
/// does, or — when it never shows up — once the launcher exits or the grace
/// period passes. Returns the launcher's exit status when it ended.
pub(crate) async fn follow(child: &mut Child, probe: Probe) -> Option<ExitStatus> {
    let started = Instant::now();
    let mut seen = false;
    let mut launcher: Option<Option<ExitStatus>> = None;
    loop {
        tokio::select! {
            status = child.wait(), if launcher.is_none() => launcher = Some(status.ok()),
            _ = tokio::time::sleep(POLL) => {}
        }
        let p = probe.clone();
        let running = tokio::task::spawn_blocking(move || p())
            .await
            .unwrap_or(false);
        if running {
            seen = true;
            continue;
        }
        if seen {
            return launcher.flatten();
        }
        if let Some(status) = launcher
            && started.elapsed() >= POLL * 2
        {
            // The launcher closed without starting the game: let the
            // failed-start checks judge its exit.
            if started.elapsed() >= START_GRACE || status.is_some_and(|s| !s.success()) {
                return status;
            }
        }
        if started.elapsed() >= START_GRACE {
            return launcher.flatten();
        }
    }
}
