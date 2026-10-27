//! Steam "non-Steam game" shortcuts: edits `shortcuts.vdf` of every Steam
//! user and places library artwork in their `grid` folders.
//!
//! Steam reads `shortcuts.vdf` at startup and rewrites it when its own
//! shortcuts change, so the file is only edited while Steam is not running.
//! When Steam is running the report says so ([`SteamReport::steam_running`])
//! and nothing is written; the caller asks the user to close Steam.

pub mod art;
pub mod shortcuts_file;
pub mod vdf;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use irori_core::{AppSettings, GameId, GameInfo};

use self::shortcuts_file::{GridIds, NewShortcut, ShortcutEntry, ShortcutsFile};
use super::LauncherCommand;
use super::icon;
use crate::util;

/// One Steam account found under `<steam>/userdata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamUser {
    pub id: String,
    /// `userdata/<id>/config`.
    pub config_dir: PathBuf,
}

impl SteamUser {
    pub fn shortcuts_file(&self) -> PathBuf {
        self.config_dir.join("shortcuts.vdf")
    }

    pub fn grid_dir(&self) -> PathBuf {
        self.config_dir.join("grid")
    }
}

/// Outcome of a Steam shortcut operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SteamReport {
    /// Steam is running, so `shortcuts.vdf` was not touched. Close Steam and
    /// retry; changes appear after Steam starts again.
    pub steam_running: bool,
    /// Number of Steam users whose `shortcuts.vdf` was rewritten.
    pub users_changed: usize,
    /// The game was already present (add) for at least one user.
    pub already_present: bool,
    /// Shortcuts added for the user who received the most (add).
    pub added: usize,
    /// Per-user problems (corrupt files, write errors). Non-fatal as long as
    /// at least one user succeeded.
    pub errors: Vec<String>,
}

/// Steam installation directory: the configured path when it exists, else
/// Flatpak Steam, else `~/.steam/steam`, else `~/.local/share/Steam`.
pub fn steam_path(app: &AppSettings) -> PathBuf {
    let configured = app.default_steam_path.replace('\'', "");
    let configured = configured.trim();
    if !configured.is_empty() {
        let p = irori_core::paths::expand_home(configured);
        if p.join("userdata").is_dir() {
            return p;
        }
    }
    if !cfg!(target_os = "linux") {
        return irori_core::platform::steam_dir();
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let candidates = [
        home.join(".var/app/com.valvesoftware.Steam/.steam/steam"),
        home.join(".steam/steam"),
        home.join(".local/share/Steam"),
    ];
    candidates
        .iter()
        .find(|p| p.join("userdata").is_dir())
        .cloned()
        .unwrap_or_else(|| home.join(".steam/steam"))
}

/// Steam users under `<steam>/userdata` (directories other than `0` and `ac`).
pub async fn users(steam: &Path) -> Result<Vec<SteamUser>> {
    let userdata = steam.join("userdata");
    let mut rd = match tokio::fs::read_dir(&userdata).await {
        Ok(rd) => rd,
        Err(_) => bail!(
            "{} does not exist. Can't add/remove game to/from Steam!",
            userdata.display()
        ),
    };
    let mut out = Vec::new();
    while let Some(entry) = rd.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "0" || name == "ac" {
            continue;
        }
        if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            out.push(SteamUser {
                config_dir: entry.path().join("config"),
                id: name,
            });
        }
    }
    if out.is_empty() {
        bail!(
            "{} does not contain a valid user directory!",
            userdata.display()
        );
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Whether Steam is running: always true in Steam Deck game mode, otherwise a
/// live PID in `<steam>/../steam.pid` or `<steam>/steam.pid`.
pub async fn is_running(steam: &Path) -> bool {
    if std::env::var("XDG_CURRENT_DESKTOP").as_deref() == Ok("gamescope") {
        return true;
    }
    let system = tokio::task::spawn_blocking(|| {
        sysinfo::System::new_with_specifics(
            sysinfo::RefreshKind::nothing().with_processes(sysinfo::ProcessRefreshKind::nothing()),
        )
    })
    .await
    .unwrap_or_default();
    for pid_file in [steam.join("../steam.pid"), steam.join("steam.pid")] {
        let Ok(text) = tokio::fs::read_to_string(&pid_file).await else {
            continue;
        };
        if let Ok(pid) = text.trim().parse::<u32>()
            && pid > 0
            && system.process(sysinfo::Pid::from_u32(pid)).is_some()
        {
            return true;
        }
    }
    system.processes().values().any(|p| {
        matches!(
            p.name().to_string_lossy().to_ascii_lowercase().as_str(),
            "steam" | "steam.exe" | "steam_osx"
        )
    })
}

/// `Exe`, `StartDir` and `LaunchOptions` of Irori's shortcut for a game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryCommand {
    pub exe: String,
    pub start_dir: String,
    pub launch_options: String,
}

impl EntryCommand {
    pub fn new(launcher: &LauncherCommand, id: &GameId) -> Self {
        let mut opts: Vec<String> = launcher
            .prefix_args
            .iter()
            .map(|a| quote_if_needed(a))
            .collect();
        opts.push("--launch".into());
        opts.push(quote(&super::launch_target(id)));
        Self {
            exe: quote(&launcher.program),
            start_dir: quote(&launcher.working_dir.to_string_lossy()),
            launch_options: opts.join(" "),
        }
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\\\""))
}

fn quote_if_needed(s: &str) -> String {
    if s.chars().any(|c| c.is_whitespace() || c == '"') {
        quote(s)
    } else {
        s.to_string()
    }
}

/// Whether a shortcut entry launches this game through Irori (`--launch
/// runner/app` or an `irori://launch/runner/app` URL). Titles are never
/// compared, so renamed entries still match and other tools' entries never do.
pub fn matches_game(entry: ShortcutEntry<'_>, id: &GameId) -> bool {
    let target = super::launch_target(id);
    let encoded = format!(
        "{}/{}",
        id.runner.id(),
        util::encode_component(&id.app_name)
    );
    let opts = entry.launch_options();
    let ends_token = |rest: &str| rest.is_empty() || rest.starts_with(['"', '\'', ' ', '?', '\t']);
    let found = |needle: &str| {
        opts.match_indices(needle)
            .any(|(i, _)| ends_token(&opts[i + needle.len()..]))
    };
    found(&format!("--launch \"{target}\""))
        || found(&format!("--launch {target}"))
        || found(&format!("irori://launch/{target}"))
        || found(&format!("irori://launch/{encoded}"))
}

/// Steam collection every Irori shortcut is filed under, so Irori's games
/// are listed together instead of only under "Non-Steam".
pub const COLLECTION: &str = "Irori";

/// A game to add, with its Steam app ID (from wiki data) when known, which
/// selects official Steam artwork.
#[derive(Debug, Clone, Copy)]
pub struct SteamGame<'a> {
    pub game: &'a GameInfo,
    pub steam_app_id: Option<&'a str>,
}

/// Add the game to every Steam user's shortcuts and set its artwork.
/// `steam_app_id` (from wiki data) selects official Steam artwork when known.
///
/// The game need not be installed: the shortcut starts Irori, which offers
/// to install a game that isn't installed yet.
pub async fn add(
    game: &GameInfo,
    app: &AppSettings,
    steam_app_id: Option<&str>,
) -> Result<SteamReport> {
    let report = add_many(&[SteamGame { game, steam_app_id }], app).await?;
    if report.users_changed == 0 && !report.already_present && !report.steam_running {
        bail!(
            "Adding {} to Steam failed: {}",
            game.title,
            report.errors.join("; ")
        );
    }
    Ok(report)
}

/// Add several games at once, reading and writing each user's
/// `shortcuts.vdf` once. Games a user already has are left alone.
pub async fn add_many(games: &[SteamGame<'_>], app: &AppSettings) -> Result<SteamReport> {
    let steam = steam_path(app);
    let users = users(&steam).await?;
    let mut report = SteamReport::default();

    if is_running(&steam).await {
        report.steam_running = true;
        for g in games {
            if is_added_for(&users, &g.game.id()).await {
                report.already_present = true;
                break;
            }
        }
        return Ok(report);
    }

    let launcher = LauncherCommand::detect();
    let mut prepared = Vec::with_capacity(games.len());
    for g in games {
        let game = g.game;
        let id = game.id();
        let cmd = EntryCommand::new(&launcher, &id);
        let icon = match icon::ensure_icon(game).await {
            Ok(p) => p.map(|p| p.to_string_lossy().into_owned()),
            Err(e) => {
                tracing::warn!("no icon for the Steam shortcut of {}: {e:#}", game.title);
                None
            }
        };
        let entry = NewShortcut {
            app_name: game.title.clone(),
            exe: cmd.exe.clone(),
            start_dir: cmd.start_dir.clone(),
            icon,
            launch_options: cmd.launch_options.clone(),
            last_play_time: i32::try_from(irori_core::util::unix_now()).unwrap_or(i32::MAX),
            tags: vec![COLLECTION.to_string()],
        }
        .to_entry();
        let grid_ids = GridIds::from_exe_name(&cmd.exe, &game.title);
        prepared.push((g, id, entry, grid_ids));
    }

    for user in &users {
        let path = user.shortcuts_file();
        let mut file = match read_file(&path).await {
            Ok(Some(f)) => f,
            Ok(None) => ShortcutsFile::empty(),
            Err(e) => {
                report.errors.push(format!(
                    "Can't add games to Steam user \"{}\". \"{}\" is corrupted! {e:#}",
                    user.id,
                    path.display()
                ));
                continue;
            }
        };
        let entries: Vec<(&GameId, &vdf::Map)> =
            prepared.iter().map(|(_, id, e, _)| (id, e)).collect();
        let added = push_missing(&mut file, &entries);
        if added.len() < prepared.len() {
            report.already_present = true;
        }
        if added.is_empty() {
            continue;
        }
        for &ix in &added {
            let (g, _, _, grid_ids) = &prepared[ix];
            for err in art::prepare(&user.grid_dir(), *grid_ids, g.game, g.steam_app_id).await {
                tracing::warn!("Steam artwork for {}: {err}", g.game.title);
            }
        }
        match util::write_atomic(&path, &file.to_bytes()).await {
            Ok(()) => {
                report.users_changed += 1;
                report.added = report.added.max(added.len());
            }
            Err(e) => report.errors.push(format!("{e:#}")),
        }
    }
    Ok(report)
}

/// Append the entries whose game has no Irori shortcut in `file` yet;
/// returns the indices of those appended.
fn push_missing(file: &mut ShortcutsFile, entries: &[(&GameId, &vdf::Map)]) -> Vec<usize> {
    let mut added = Vec::new();
    for (ix, (id, entry)) in entries.iter().enumerate() {
        if file.entries().any(|e| matches_game(e, id)) {
            continue;
        }
        file.push((*entry).clone());
        added.push(ix);
    }
    added
}

/// Remove the game's shortcut from every Steam user, with its artwork.
pub async fn remove(game: &GameInfo, app: &AppSettings) -> Result<SteamReport> {
    let steam = steam_path(app);
    let users = match users(&steam).await {
        Ok(u) => u,
        Err(e) => {
            tracing::debug!("Steam shortcuts not removed: {e:#}");
            return Ok(SteamReport::default());
        }
    };
    let id = game.id();
    let mut report = SteamReport::default();
    if is_running(&steam).await {
        report.steam_running = is_added_for(&users, &id).await;
        return Ok(report);
    }
    let cmd = EntryCommand::new(&LauncherCommand::detect(), &id);
    for user in &users {
        let path = user.shortcuts_file();
        let mut file = match read_file(&path).await {
            Ok(Some(f)) => f,
            Ok(None) => continue,
            Err(e) => {
                report.errors.push(format!(
                    "Can't remove \"{}\" from Steam user \"{}\". \"{}\" is corrupted! {e:#}",
                    game.title,
                    user.id,
                    path.display()
                ));
                continue;
            }
        };
        let removed = file.remove_where(|e| matches_game(e, &id));
        if removed.is_empty() {
            continue;
        }
        match util::write_atomic(&path, &file.to_bytes()).await {
            Ok(()) => report.users_changed += 1,
            Err(e) => {
                report.errors.push(format!("{e:#}"));
                continue;
            }
        }
        let grid = user.grid_dir();
        for entry in &removed {
            let e = ShortcutEntry(entry);
            let ids = match e.app_id() {
                Some(short) => GridIds::from_short(short),
                None => GridIds::from_exe_name(e.exe(), e.app_name()),
            };
            art::remove(&grid, ids).await;
            art::remove(&grid, GridIds::from_exe_name(e.exe(), e.app_name())).await;
        }
        art::remove(&grid, GridIds::from_exe_name(&cmd.exe, &game.title)).await;
    }
    if !report.errors.is_empty() && report.users_changed == 0 {
        bail!(
            "Removing {} from Steam failed: {}",
            game.title,
            report.errors.join("; ")
        );
    }
    Ok(report)
}

/// Whether any Steam user has an Irori shortcut for the game.
pub async fn is_added(game: &GameInfo, app: &AppSettings) -> bool {
    let steam = steam_path(app);
    match users(&steam).await {
        Ok(users) => is_added_for(&users, &game.id()).await,
        Err(_) => false,
    }
}

async fn is_added_for(users: &[SteamUser], id: &GameId) -> bool {
    for user in users {
        if let Ok(Some(file)) = read_file(&user.shortcuts_file()).await
            && file.entries().any(|e| matches_game(e, id))
        {
            return true;
        }
    }
    false
}

/// Read and validate a `shortcuts.vdf`; `Ok(None)` when it does not exist.
async fn read_file(path: &Path) -> Result<Option<ShortcutsFile>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(ShortcutsFile::parse(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_core::Runner;

    fn launcher() -> LauncherCommand {
        LauncherCommand {
            program: "/usr/bin/irori".into(),
            prefix_args: vec![],
            working_dir: "/usr/bin".into(),
        }
    }

    #[test]
    fn entry_command_and_matching() {
        let id = GameId::new(Runner::Gog, "1423049311");
        let cmd = EntryCommand::new(&launcher(), &id);
        assert_eq!(cmd.exe, "\"/usr/bin/irori\"");
        assert_eq!(cmd.start_dir, "\"/usr/bin\"");
        assert_eq!(cmd.launch_options, "--launch \"gog/1423049311\"");

        let entry = NewShortcut {
            app_name: "Cyberpunk".into(),
            exe: cmd.exe.clone(),
            start_dir: cmd.start_dir.clone(),
            icon: None,
            launch_options: cmd.launch_options.clone(),
            last_play_time: 0,
            tags: Vec::new(),
        }
        .to_entry();
        assert!(matches_game(ShortcutEntry(&entry), &id));
        assert!(!matches_game(
            ShortcutEntry(&entry),
            &GameId::new(Runner::Gog, "142304931")
        ));
        assert!(!matches_game(
            ShortcutEntry(&entry),
            &GameId::new(Runner::Legendary, "1423049311")
        ));

        let url_entry = vdf::Map::new().with(
            "LaunchOptions",
            vdf::Value::String("\"irori://launch/sideload/my%20app\"".into()),
        );
        assert!(matches_game(
            ShortcutEntry(&url_entry),
            &GameId::new(Runner::Sideload, "my app")
        ));
    }

    #[test]
    fn flatpak_launch_options() {
        let l = LauncherCommand {
            program: "flatpak".into(),
            prefix_args: vec!["run".into(), "io.github.irori_app.Irori".into()],
            working_dir: "/home/u".into(),
        };
        let cmd = EntryCommand::new(&l, &GameId::new(Runner::Nile, "amzn1.x"));
        assert_eq!(cmd.exe, "\"flatpak\"");
        assert_eq!(
            cmd.launch_options,
            "run io.github.irori_app.Irori --launch \"nile/amzn1.x\""
        );
    }

    #[test]
    fn adds_only_missing_games_in_the_irori_collection() {
        let entry = |id: &GameId, title: &str| {
            let cmd = EntryCommand::new(&launcher(), id);
            NewShortcut {
                app_name: title.into(),
                exe: cmd.exe,
                start_dir: cmd.start_dir,
                icon: None,
                launch_options: cmd.launch_options,
                last_play_time: 0,
                tags: vec![COLLECTION.into()],
            }
            .to_entry()
        };
        let (a, b) = (
            GameId::new(Runner::Gog, "1"),
            GameId::new(Runner::Legendary, "Fortnite"),
        );
        let (ea, eb) = (entry(&a, "A"), entry(&b, "B"));
        let mut file = ShortcutsFile::empty();
        file.push(ea.clone());
        let added = push_missing(&mut file, &[(&a, &ea), (&b, &eb), (&b, &eb)]);
        assert_eq!(added, [1]);
        assert_eq!(file.entries().count(), 2);
        let tags = file
            .entries()
            .nth(1)
            .and_then(|e| e.0.get_ci("tags"))
            .and_then(vdf::Value::as_map)
            .unwrap();
        assert_eq!(tags.str_ci("0"), COLLECTION);
    }

    #[tokio::test]
    async fn users_and_running_detection() {
        let dir = tempfile::tempdir().unwrap();
        let steam = dir.path().join("steam");
        assert!(users(&steam).await.is_err());
        for d in ["userdata/0", "userdata/ac", "userdata/1234"] {
            std::fs::create_dir_all(steam.join(d)).unwrap();
        }
        std::fs::write(steam.join("userdata/file"), b"").unwrap();
        let u = users(&steam).await.unwrap();
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].id, "1234");
        assert!(
            u[0].shortcuts_file()
                .ends_with("userdata/1234/config/shortcuts.vdf")
        );

        if std::env::var("XDG_CURRENT_DESKTOP").as_deref() != Ok("gamescope") {
            assert!(!is_running(&steam).await);
            std::fs::write(steam.join("steam.pid"), std::process::id().to_string()).unwrap();
            assert!(is_running(&steam).await);
        }
    }

    #[tokio::test]
    async fn add_then_remove_restores_file() {
        let dir = tempfile::tempdir().unwrap();
        let user = SteamUser {
            id: "1".into(),
            config_dir: dir.path().join("config"),
        };
        std::fs::create_dir_all(&user.config_dir).unwrap();
        let mut original = ShortcutsFile::empty();
        original.push(
            NewShortcut {
                app_name: "Other".into(),
                exe: "\"/bin/x\"".into(),
                start_dir: "\"/bin\"".into(),
                icon: None,
                launch_options: String::new(),
                last_play_time: 5,
                tags: Vec::new(),
            }
            .to_entry(),
        );
        let before = original.to_bytes();
        std::fs::write(user.shortcuts_file(), &before).unwrap();

        let id = GameId::new(Runner::Legendary, "Fortnite");
        let cmd = EntryCommand::new(&launcher(), &id);
        let mut f = read_file(&user.shortcuts_file()).await.unwrap().unwrap();
        f.push(
            NewShortcut {
                app_name: "Fortnite".into(),
                exe: cmd.exe,
                start_dir: cmd.start_dir,
                icon: None,
                launch_options: cmd.launch_options,
                last_play_time: 7,
                tags: vec![COLLECTION.into()],
            }
            .to_entry(),
        );
        util::write_atomic(&user.shortcuts_file(), &f.to_bytes())
            .await
            .unwrap();
        assert!(is_added_for(std::slice::from_ref(&user), &id).await);

        let mut f = read_file(&user.shortcuts_file()).await.unwrap().unwrap();
        assert_eq!(f.remove_where(|e| matches_game(e, &id)).len(), 1);
        assert_eq!(f.to_bytes(), before);
    }
}
