//! Following a game that a third-party launcher starts. The launcher's
//! process is not the game: Battle.net keeps running after the game ends,
//! and some launchers hand over and exit before it starts. While such a game
//! runs, its session is the life of the processes that run from its install
//! folder, checked every few seconds (and only then).

use std::path::Path;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use tokio::process::Child;

/// How often the game's processes are looked for.
const POLL: Duration = Duration::from_secs(3);
/// How long the launcher gets to start the game before the session is the
/// launcher's own.
const START_GRACE: Duration = Duration::from_secs(180);

/// Whether a process's command line or program is in the install folder
/// `folder` (a Windows path as the launcher stores it, or a Unix path).
pub(crate) fn runs_from(folder: &str, exe: Option<&Path>, cmd: &[String]) -> bool {
    let normalize = |s: &str| s.replace('\\', "/").to_lowercase();
    let folder = normalize(folder.trim_end_matches(['\\', '/']));
    // `C:/Program Files/X` under Wine appears as `c:/program files/x`; a
    // folder name alone is too short to trust.
    if folder.len() < 6 {
        return false;
    }
    let tail = folder
        .split_once(':')
        .map_or(folder.as_str(), |(_, rest)| rest);
    let hit = |s: &str| {
        let s = normalize(s);
        s.contains(&folder) || (!tail.is_empty() && s.contains(tail))
    };
    exe.is_some_and(|e| hit(&e.to_string_lossy())) || cmd.iter().any(|a| hit(a))
}

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
        runs_from(folder, p.exe(), &cmd)
    })
}

/// Wait for the game in `folder` to end: once its processes were seen and
/// are gone, or — when they never show up — once the launcher exits or the
/// grace period passes. Returns the launcher's exit status when it ended.
pub(crate) async fn follow(child: &mut Child, folder: String) -> Option<ExitStatus> {
    let started = Instant::now();
    let mut seen = false;
    let mut launcher: Option<Option<ExitStatus>> = None;
    loop {
        tokio::select! {
            status = child.wait(), if launcher.is_none() => launcher = Some(status.ok()),
            _ = tokio::time::sleep(POLL) => {}
        }
        let f = folder.clone();
        let running = tokio::task::spawn_blocking(move || game_running(&f))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_wine_and_native_command_lines() {
        let folder = r"C:\Program Files (x86)\Diablo IV";
        assert!(runs_from(
            folder,
            None,
            &[r"C:\Program Files (x86)\Diablo IV\Diablo IV.exe".into()]
        ));
        assert!(runs_from(
            folder,
            Some(Path::new(
                "/home/u/pfx/drive_c/Program Files (x86)/Diablo IV/Diablo IV.exe"
            )),
            &[]
        ));
        assert!(!runs_from(
            folder,
            None,
            &[r"C:\Program Files (x86)\Battle.net\Battle.net.exe".into()]
        ));
        assert!(!runs_from("C:\\", None, &["C:\\anything".into()]));
    }
}
