//! Installed games by drive, for the stats page: how much of each drive
//! Irori's games take, what else is on it, and how much room is left.

use std::path::{Path, PathBuf};

use irori_core::GameId;

/// A mounted drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    /// The volume's name; may be empty or a device path.
    pub name: String,
    pub mount: PathBuf,
    pub total: u64,
    pub free: u64,
}

impl Drive {
    /// Name to show: the volume name when it reads like one, else where it
    /// is mounted.
    pub fn label(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() || name.starts_with('/') || name.starts_with("\\\\") {
            self.mount.display().to_string()
        } else {
            format!("{name} ({})", self.mount.display())
        }
    }
}

/// An installed game: where it is and its size as the store reported it
/// (0 when unknown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledGame {
    pub id: GameId,
    pub path: PathBuf,
    pub bytes: u64,
}

/// One drive's share of the installed games.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveUsage {
    /// `None` for games on no known drive (a disconnected disk).
    pub drive: Option<Drive>,
    /// Largest first.
    pub games: Vec<(GameId, u64)>,
    /// Sum of the games' known sizes.
    pub used: u64,
}

/// Group `games` by the drive holding them (the longest mount point their
/// path starts with). Drives holding no games are left out; drives are
/// ordered by how much the games take, the unknown group last.
pub fn group(games: &[InstalledGame], drives: &[Drive]) -> Vec<DriveUsage> {
    let mut out: Vec<DriveUsage> = Vec::new();
    for game in games {
        let drive = drives
            .iter()
            .filter(|d| game.path.starts_with(&d.mount))
            .max_by_key(|d| d.mount.components().count())
            .cloned();
        let entry = match out.iter().position(|u| u.drive == drive) {
            Some(ix) => &mut out[ix],
            None => {
                out.push(DriveUsage {
                    drive,
                    games: Vec::new(),
                    used: 0,
                });
                out.last_mut().expect("just pushed")
            }
        };
        entry.games.push((game.id.clone(), game.bytes));
        entry.used += game.bytes;
    }
    for usage in &mut out {
        usage
            .games
            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.key().cmp(&b.0.key())));
    }
    out.sort_by(|a, b| {
        a.drive
            .is_none()
            .cmp(&b.drive.is_none())
            .then(b.used.cmp(&a.used))
    });
    out
}

/// The mounted drives. Blocking: run it off the UI thread.
pub fn read_drives() -> Vec<Drive> {
    sysinfo::Disks::new_with_refreshed_list()
        .list()
        .iter()
        .map(|d| Drive {
            name: d.name().to_string_lossy().into_owned(),
            mount: d.mount_point().to_path_buf(),
            total: d.total_space(),
            free: d.available_space(),
        })
        .collect()
}

/// Where `path` really is, following links (a games folder linked to
/// another disk counts for that disk). Blocking.
pub fn resolve(path: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // Windows canonical paths are verbatim (`\\?\C:\…`); drives mount at `C:\`.
    if cfg!(windows)
        && let Some(plain) = resolved.to_str().and_then(|s| s.strip_prefix(r"\\?\"))
    {
        return PathBuf::from(plain);
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_core::Runner;

    fn drive(mount: &str, name: &str) -> Drive {
        Drive {
            name: name.into(),
            mount: mount.into(),
            total: 1000,
            free: 400,
        }
    }

    fn game(app: &str, path: &str, bytes: u64) -> InstalledGame {
        InstalledGame {
            id: GameId::new(Runner::Gog, app),
            path: path.into(),
            bytes,
        }
    }

    #[test]
    fn games_go_to_the_deepest_mount_point() {
        let drives = [drive("/", "root"), drive("/mnt/games", "Games")];
        let games = [
            game("1", "/home/u/Games/A", 10),
            game("2", "/mnt/games/B", 30),
            game("3", "/mnt/games/C", 50),
            game("4", "/mnt/gamesx/D", 5),
        ];
        let usage = group(&games, &drives);
        assert_eq!(usage.len(), 2);
        assert_eq!(
            usage[0].drive.as_ref().unwrap().mount,
            Path::new("/mnt/games")
        );
        assert_eq!(usage[0].used, 80);
        assert_eq!(usage[0].games[0].0.app_name.as_ref(), "3");
        assert_eq!(usage[1].used, 15);
        assert_eq!(usage[1].games.len(), 2);
    }

    #[test]
    fn games_on_unknown_drives_come_last() {
        let drives = [drive("/data", "")];
        let games = [game("1", "/media/usb/A", 500), game("2", "/data/B", 1)];
        let usage = group(&games, &drives);
        assert!(usage[0].drive.is_some());
        assert!(usage[1].drive.is_none());
        assert_eq!(usage[0].drive.as_ref().unwrap().label(), "/data");
        assert_eq!(drive("/mnt/g", "Games").label(), "Games (/mnt/g)");
        assert_eq!(drive("/", "/dev/nvme0n1p2").label(), "/");
    }
}
