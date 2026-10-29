//! Following a Steam game after `steam://rungameid` hands it to the client.
//! The opener exits at once, so the game is found by its processes: on Linux
//! Steam starts every game under a "reaper" whose command line names the app
//! (`reaper SteamLaunch AppId=620 -- …`), on every system the game's own
//! processes run from its install folder, and while Steam updates or
//! validates the game before it starts, its manifest says so.
//!
//! Once processes were found, later checks look at those processes only and
//! fall back to a full scan when they are gone, so a long session costs one
//! lookup per check rather than a walk over every process on the computer.

use std::path::PathBuf;

use irori_core::GameWatch;
use parking_lot::Mutex;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind};

use crate::scan::{self, state};

/// Watch over one Steam game.
pub(crate) struct SteamGameWatch {
    app_id: String,
    install_path: String,
    manifest: PathBuf,
    /// The game's processes found by the last check.
    pids: Mutex<Vec<Pid>>,
}

impl SteamGameWatch {
    pub(crate) fn new(app_id: String, install_path: String, manifest: PathBuf) -> Self {
        Self {
            app_id,
            install_path,
            manifest,
            pids: Mutex::new(Vec::new()),
        }
    }

    /// Whether the process with this command line and program belongs to
    /// the game.
    fn is_game_process(&self, exe: Option<&std::path::Path>, cmd: &[String]) -> bool {
        is_reaper_for(&self.app_id, cmd)
            || irori_core::process::runs_from(&self.install_path, exe, cmd)
    }

    /// The game's processes now: the remembered ones when any of them still
    /// is the game's, else whatever a full scan finds.
    fn find(&self) -> Vec<Pid> {
        let known = self.pids.lock().clone();
        let refresh = ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_exe(UpdateKind::Always);
        let mut sys = System::new();
        if !known.is_empty() {
            sys.refresh_processes_specifics(ProcessesToUpdate::Some(&known), true, refresh);
            // A PID can be reused by an unrelated process, so each one is
            // checked again rather than only for existence.
            let alive = self.matching(&sys);
            if !alive.is_empty() {
                *self.pids.lock() = alive.clone();
                return alive;
            }
        }
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
        let found = self.matching(&sys);
        *self.pids.lock() = found.clone();
        found
    }

    fn matching(&self, sys: &System) -> Vec<Pid> {
        sys.processes()
            .iter()
            .filter(|(_, p)| {
                let cmd: Vec<String> = p
                    .cmd()
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                self.is_game_process(p.exe(), &cmd)
            })
            .map(|(pid, _)| *pid)
            .collect()
    }
}

impl GameWatch for SteamGameWatch {
    fn running(&self) -> bool {
        // Cheap first: a launch-time update or validation shows in the
        // manifest before any game process exists. Flatpak Steam never sets
        // these flags, which the process checks cover.
        let preparing = scan::manifest_flags(&self.manifest)
            .is_some_and(|flags| flags & state::LAUNCHING_OR_RUNNING != 0);
        preparing || !self.find().is_empty()
    }

    /// Ends the reaper and the game's processes. The Steam client itself
    /// never matches, so it keeps running.
    fn stop(&self, force: bool) {
        let pids = self.find();
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            true,
            ProcessRefreshKind::nothing(),
        );
        for pid in pids {
            if let Some(process) = sys.process(pid) {
                // Windows has no polite signal; terminating is the only stop.
                let sent = (!force)
                    .then(|| process.kill_with(Signal::Term))
                    .flatten()
                    .unwrap_or(false);
                if !sent {
                    process.kill();
                }
            }
        }
    }
}

/// Whether `cmd` is the command line of the reaper Steam runs a game under
/// (Linux): `reaper SteamLaunch AppId=<app_id> -- …`, matched on the whole
/// ID so app 440 doesn't match 4400.
fn is_reaper_for(app_id: &str, cmd: &[String]) -> bool {
    let is_reaper = cmd.first().is_some_and(|program| {
        std::path::Path::new(program)
            .file_name()
            .is_some_and(|name| name == "reaper")
    });
    if !is_reaper || !cmd.iter().any(|a| a == "SteamLaunch") {
        return false;
    }
    let wanted = format!("AppId={app_id}");
    cmd.iter().take_while(|a| *a != "--").any(|a| *a == wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split(' ').map(String::from).collect()
    }

    #[test]
    fn recognises_the_reaper_of_one_app() {
        let reaper = args(
            "/home/u/.local/share/Steam/ubuntu12_32/reaper SteamLaunch AppId=440 -- /home/u/.local/share/Steam/ubuntu12_32/steam-launch-wrapper -- hl2.sh",
        );
        assert!(is_reaper_for("440", &reaper));
        assert!(!is_reaper_for("4400", &reaper));
        assert!(!is_reaper_for("44", &reaper));
        // The game's own arguments after `--` don't count.
        assert!(!is_reaper_for(
            "620",
            &args("reaper SteamLaunch AppId=440 -- game AppId=620")
        ));
        assert!(!is_reaper_for("440", &args("steam SteamLaunch AppId=440")));
    }

    #[test]
    fn follows_the_manifest_while_steam_prepares_the_launch() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("appmanifest_620.acf");
        let watch = SteamGameWatch::new(
            "620".into(),
            dir.path()
                .join("no such game folder")
                .to_string_lossy()
                .into_owned(),
            manifest.clone(),
        );
        let write = |flags: u64| {
            std::fs::write(
                &manifest,
                format!("\"AppState\" {{ \"appid\" \"620\" \"StateFlags\" \"{flags}\" }}"),
            )
            .unwrap()
        };
        write(state::FULLY_INSTALLED | state::UPDATE_RUNNING);
        assert!(watch.running());
        write(state::FULLY_INSTALLED | state::APP_RUNNING);
        assert!(watch.running());
        // A pending update alone is not a launch.
        write(state::FULLY_INSTALLED | 2);
        assert!(!watch.running());
    }
}
