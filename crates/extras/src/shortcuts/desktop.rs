//! freedesktop `.desktop` entries: per-game launchers on the desktop and in
//! the application menu, and the `x-scheme-handler/irori` URL handler.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use irori_core::process::{CommandSpec, RunOptions};
use irori_core::{GameId, GameInfo, Paths};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::LauncherCommand;
use crate::util;

/// Key in Irori's entries that identifies the game (`runner/app_name`).
const GAME_KEY: &str = "X-Irori-Game";
/// File name of the URL handler entry.
pub const URL_HANDLER_FILE: &str = "irori-url-handler.desktop";

/// `~/.local/share/applications`.
pub fn applications_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".local/share")
        })
        .join("applications")
}

/// The XDG desktop directory, when the user has one.
pub fn desktop_dir() -> Option<PathBuf> {
    dirs::desktop_dir()
}

/// Stable menu entry path of a game (independent of its title).
pub fn menu_file(id: &GameId) -> PathBuf {
    applications_dir().join(format!(
        "irori-{}-{}.desktop",
        id.runner.id(),
        irori_core::paths::sanitize(&id.app_name).replace(' ', "_")
    ))
}

/// Quote one `Exec` argument per the Desktop Entry spec, then apply the
/// string-value escaping (`\` → `\\`) and field-code escaping (`%` → `%%`).
pub fn exec_arg(arg: &str) -> String {
    const RESERVED: &[char] = &[
        ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(',
        ')', '`',
    ];
    let quoted = if arg.is_empty() || arg.contains(RESERVED) {
        let mut s = String::from("\"");
        for c in arg.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                s.push('\\');
            }
            s.push(c);
        }
        s.push('"');
        s
    } else {
        arg.to_string()
    };
    quoted.replace('\\', "\\\\").replace('%', "%%")
}

/// Escape a `string`/`localestring` value (`Name`, `Icon`, …).
pub fn escape_value(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Content of a game launcher entry.
pub fn game_entry(game: &GameInfo, launcher: &LauncherCommand, icon: Option<&Path>) -> String {
    let id = game.id();
    let exec: Vec<String> = launcher
        .launch_args(&id)
        .iter()
        .map(|a| exec_arg(a))
        .collect();
    let mut s = String::from("[Desktop Entry]\n");
    s.push_str("Type=Application\nVersion=1.0\n");
    s.push_str(&format!("Name={}\n", escape_value(&game.title)));
    s.push_str(&format!(
        "Comment=Play {} with Irori\n",
        escape_value(&game.title)
    ));
    s.push_str(&format!("Exec={}\n", exec.join(" ")));
    s.push_str(&format!(
        "Icon={}\n",
        icon.map(|p| escape_value(&p.to_string_lossy()))
            .unwrap_or_else(|| "applications-games".into())
    ));
    s.push_str("Terminal=false\nCategories=Game;\n");
    s.push_str(&format!(
        "{GAME_KEY}={}\n",
        escape_value(&super::launch_target(&id))
    ));
    s
}

/// Content of the `x-scheme-handler/irori` entry.
pub fn url_handler_entry(launcher: &LauncherCommand) -> String {
    let mut exec: Vec<String> = std::iter::once(launcher.program.as_str())
        .chain(launcher.prefix_args.iter().map(String::as_str))
        .map(exec_arg)
        .collect();
    exec.push("%u".into());
    format!(
        "[Desktop Entry]\nType=Application\nName=Irori\nExec={}\nTerminal=false\nNoDisplay=true\nMimeType=x-scheme-handler/irori;\n",
        exec.join(" ")
    )
}

/// The game an existing entry belongs to, if Irori wrote it.
fn entry_game(content: &str) -> Option<&str> {
    content
        .lines()
        .find_map(|l| l.strip_prefix(GAME_KEY)?.strip_prefix('='))
        .map(str::trim)
}

/// Shortcut files written per game, so they are found again after a rename.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Manifest {
    games: BTreeMap<String, Vec<PathBuf>>,
}

static MANIFEST_LOCK: Mutex<()> = Mutex::const_new(());

fn manifest_file() -> PathBuf {
    Paths::get().state_dir().join("shortcuts.json")
}

async fn read_manifest() -> Manifest {
    match tokio::fs::read(manifest_file()).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Manifest::default(),
    }
}

async fn write_manifest(m: &Manifest) -> Result<()> {
    util::write_atomic(&manifest_file(), &serde_json::to_vec_pretty(m)?).await
}

/// Where the desktop file goes: `<Desktop>/<title>.desktop`, or
/// `<title> (<store>).desktop` when another game already uses that name.
async fn desktop_target(dir: &Path, game: &GameInfo) -> PathBuf {
    let base = match util::sanitize_filename(&game.title) {
        s if s.is_empty() => format!("{}-{}", game.runner().id(), game.app_name),
        s => s,
    };
    let target = super::launch_target(&game.id());
    let plain = dir.join(format!("{base}.desktop"));
    match tokio::fs::read_to_string(&plain).await {
        Err(_) => plain,
        Ok(content) if entry_game(&content) == Some(target.as_str()) => plain,
        Ok(_) => dir.join(format!("{} ({}).desktop", base, game.runner().short_name())),
    }
}

async fn write_entry(path: &Path, content: &str, executable: bool) -> Result<()> {
    util::write_atomic(path, content.as_bytes()).await?;
    #[cfg(not(unix))]
    let _ = executable;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .await
            .with_context(|| format!("making {} executable", path.display()))?;
    }
    Ok(())
}

/// Files created by [`create`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreatedEntries {
    pub desktop: Option<PathBuf>,
    pub menu: Option<PathBuf>,
}

/// Write the requested entries for a game and record them.
pub async fn create(
    game: &GameInfo,
    launcher: &LauncherCommand,
    icon: Option<&Path>,
    desktop: bool,
    menu: bool,
) -> Result<CreatedEntries> {
    let content = game_entry(game, launcher, icon);
    let mut created = CreatedEntries::default();
    if menu {
        let path = menu_file(&game.id());
        write_entry(&path, &content, false).await?;
        created.menu = Some(path);
    }
    if desktop {
        let dir = desktop_dir().context("no desktop folder is configured (XDG_DESKTOP_DIR)")?;
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
        let path = desktop_target(&dir, game).await;
        write_entry(&path, &content, true).await?;
        created.desktop = Some(path);
    }
    let _guard = MANIFEST_LOCK.lock().await;
    let mut manifest = read_manifest().await;
    let files = manifest.games.entry(game.id().key()).or_default();
    for p in [&created.desktop, &created.menu].into_iter().flatten() {
        if !files.contains(p) {
            files.push(p.clone());
        }
    }
    write_manifest(&manifest).await?;
    Ok(created)
}

/// Remove every entry Irori wrote for the game: recorded files, the stable
/// menu entry, and a desktop file under the current title that belongs to
/// the game. Returns the removed paths.
pub async fn remove(game: &GameInfo) -> Result<Vec<PathBuf>> {
    let id = game.id();
    let target = super::launch_target(&id);
    let _guard = MANIFEST_LOCK.lock().await;
    let mut manifest = read_manifest().await;
    let mut candidates = manifest.games.remove(&id.key()).unwrap_or_default();
    candidates.push(menu_file(&id));
    if let Some(dir) = desktop_dir() {
        candidates.push(desktop_target(&dir, game).await);
    }
    let mut removed = Vec::new();
    for path in candidates {
        let Ok(content) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        if entry_game(&content) != Some(target.as_str()) {
            continue;
        }
        match tokio::fs::remove_file(&path).await {
            Ok(()) => removed.push(path),
            Err(e) => tracing::warn!("removing {}: {e}", path.display()),
        }
    }
    write_manifest(&manifest).await?;
    Ok(removed)
}

/// Whether a desktop or menu entry exists for the game.
pub async fn exists(game: &GameInfo) -> bool {
    let id = game.id();
    let manifest = read_manifest().await;
    let mut candidates = manifest.games.get(&id.key()).cloned().unwrap_or_default();
    candidates.push(menu_file(&id));
    for p in candidates {
        if tokio::fs::try_exists(&p).await.unwrap_or(false) {
            return true;
        }
    }
    false
}

/// Install `irori-url-handler.desktop` and make Irori the default handler for
/// `irori://` URLs. Does nothing when the entry is already current. Missing
/// `xdg-mime` / `update-desktop-database` tools are tolerated.
pub async fn register_url_scheme(launcher: &LauncherCommand) -> Result<()> {
    let dir = applications_dir();
    let path = dir.join(URL_HANDLER_FILE);
    let content = url_handler_entry(launcher);
    if tokio::fs::read_to_string(&path).await.ok().as_deref() == Some(content.as_str()) {
        return Ok(());
    }
    write_entry(&path, &content, false).await?;
    let tools = [
        CommandSpec::new("xdg-mime").args(["default", URL_HANDLER_FILE, "x-scheme-handler/irori"]),
        CommandSpec::new("update-desktop-database").arg(dir.as_os_str()),
    ];
    for spec in &tools {
        match irori_core::process::run(spec, RunOptions::default()).await {
            Ok(r) if r.success() => {}
            Ok(r) => tracing::debug!("{spec} failed: {}", r.error_summary()),
            Err(e) => tracing::debug!("{spec} unavailable: {e:#}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_core::Runner;

    #[test]
    fn exec_quoting() {
        assert_eq!(exec_arg("/usr/bin/irori"), "/usr/bin/irori");
        assert_eq!(exec_arg("gog/123"), "gog/123");
        assert_eq!(exec_arg("/opt/My Apps/irori"), "\"/opt/My Apps/irori\"");
        assert_eq!(exec_arg("a$b"), "\"a\\\\$b\"");
        assert_eq!(exec_arg("100%"), "100%%");
        assert_eq!(exec_arg(""), "\"\"");
    }

    #[test]
    fn game_entry_content() {
        let game = GameInfo {
            runner: Some(Runner::Gog),
            app_name: "1423049311".into(),
            title: "Cyberpunk 2077\\x".into(),
            ..Default::default()
        };
        let launcher = LauncherCommand {
            program: "/usr/bin/irori".into(),
            prefix_args: vec![],
            working_dir: "/usr/bin".into(),
        };
        let s = game_entry(&game, &launcher, Some(Path::new("/i/gog-1.png")));
        assert_eq!(
            s,
            "[Desktop Entry]\nType=Application\nVersion=1.0\nName=Cyberpunk 2077\\\\x\n\
             Comment=Play Cyberpunk 2077\\\\x with Irori\n\
             Exec=/usr/bin/irori --launch gog/1423049311\nIcon=/i/gog-1.png\n\
             Terminal=false\nCategories=Game;\nX-Irori-Game=gog/1423049311\n"
        );
        assert_eq!(entry_game(&s), Some("gog/1423049311"));
    }

    #[test]
    fn url_handler_content() {
        let launcher = LauncherCommand {
            program: "flatpak".into(),
            prefix_args: vec!["run".into(), "io.github.irori_app.Irori".into()],
            working_dir: "/home/u".into(),
        };
        let s = url_handler_entry(&launcher);
        assert!(s.contains("Exec=flatpak run io.github.irori_app.Irori %u\n"));
        assert!(s.contains("MimeType=x-scheme-handler/irori;\n"));
    }

    #[test]
    fn menu_file_is_stable() {
        let p = menu_file(&GameId::new(Runner::Sideload, "my app/x"));
        assert!(p.ends_with("irori-sideload-my_app_x.desktop"));
    }
}
