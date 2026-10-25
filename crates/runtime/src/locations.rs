//! Where the runtime keeps and finds things on disk: Ember's tools
//! directories, Heroic's (reused read-only when Ember has nothing yet), Steam
//! and the host environment flags.

use std::path::{Path, PathBuf};

use ember_core::{AppSettings, Paths};

/// `<data>/tools`: DXVK, VKD3D, winetricks, runtimes, Wine and Proton builds.
pub fn tools_dir() -> PathBuf {
    Paths::get().tools_dir()
}

/// `<data>/tools/runtimes`: umu, EAC and BattlEye runtimes.
pub fn runtimes_dir() -> PathBuf {
    tools_dir().join("runtimes")
}

/// Where Ember installs umu-run when the system has none.
pub fn default_umu_path() -> PathBuf {
    runtimes_dir().join("umu").join("umu_run.py")
}

/// The winetricks script Ember downloads.
pub fn winetricks_path() -> PathBuf {
    tools_dir().join("winetricks")
}

/// Cached Wine/Proton release list (the Wine manager's catalogue and install
/// state).
pub fn releases_file() -> PathBuf {
    Paths::get().state_dir().join("wine-releases.json")
}

/// Heroic's config directory (`~/.config/heroic` or the Flatpak one), if any.
pub fn heroic_dir() -> Option<PathBuf> {
    ember_core::paths::heroic_config_dir().or_else(|| {
        let dir = dirs::config_dir()?.join("heroic");
        dir.join("tools").is_dir().then_some(dir)
    })
}

/// Heroic's tools directory, when it exists.
pub fn heroic_tools_dir() -> Option<PathBuf> {
    heroic_dir().map(|d| d.join("tools")).filter(|d| d.is_dir())
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Whether Ember runs inside a Flatpak sandbox.
pub fn is_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

/// Steam Deck game mode (gamescope session): MangoHud and gamescope are
/// provided by the session and must not be stacked.
pub fn is_steam_deck_game_mode() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|v| v == "gamescope")
}

/// The user's real home as seen by the host (`~/.var/app/<id>` inside a
/// Flatpak). Used for `PROTON_LOG_DIR`.
///
/// Heroic derives this by removing `/data` from `$XDG_DATA_HOME`, which on a
/// native install with `XDG_DATA_HOME=~/.local/share` yields a wrong path; this
/// only applies the substitution inside a Flatpak.
pub fn flatpak_home() -> PathBuf {
    if is_flatpak()
        && let Ok(data) = std::env::var("XDG_DATA_HOME")
        && let Some(stripped) = data.strip_suffix("/data")
    {
        return PathBuf::from(stripped);
    }
    home()
}

/// The Steam installation: the configured path (quotes removed, `~`
/// expanded), else Flatpak Steam when present, else `~/.steam/steam`.
pub fn steam_path(app: &AppSettings) -> PathBuf {
    let configured = app.default_steam_path.replace('\'', "");
    if !configured.trim().is_empty() {
        let p = ember_core::paths::expand_home(configured.trim());
        if p.exists() {
            return p;
        }
    }
    if !cfg!(target_os = "linux") {
        return ember_core::platform::steam_dir();
    }
    let flatpak = home().join(".var/app/com.valvesoftware.Steam/.steam/steam");
    if flatpak.exists() {
        return flatpak;
    }
    ember_core::platform::steam_dir()
}

/// Value for `STEAM_COMPAT_CLIENT_INSTALL_PATH`.
///
/// Heroic uses `<flatpakHome>/.steam/steam`, which is wrong whenever
/// `XDG_DATA_HOME` is customised; Ember points at the real Steam directory.
pub fn steam_client_install_path(app: &AppSettings) -> PathBuf {
    steam_path(app)
}

/// Pick the first existing path, else the first candidate.
pub fn first_existing(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|p| p.exists())
        .or_else(|| candidates.first())
        .cloned()
}

/// Canonical form for de-duplication (falls back to the input).
pub fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
