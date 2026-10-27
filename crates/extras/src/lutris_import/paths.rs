//! Where a Lutris installation keeps its data: the native XDG folders and the
//! Flatpak sandbox (`net.lutris.Lutris`). Discovery only looks at the
//! filesystem and never creates anything.

use std::path::{Path, PathBuf};

/// Flatpak application id of Lutris.
const FLATPAK_ID: &str = "net.lutris.Lutris";

/// The folders of one Lutris installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LutrisDirs {
    /// True for the Flatpak's sandboxed folders.
    pub flatpak: bool,
    /// The user's home folder, used to expand `~` in Lutris paths.
    pub home: PathBuf,
    /// `DATA_DIR`: `<data>/lutris` (banners, cover art, default `pga.db`).
    pub data_dir: PathBuf,
    /// `CONFIG_DIR`: `<config>/lutris` when that folder exists, else
    /// `data_dir`, as Lutris decides. Holds `lutris.conf` and the YAML files.
    pub config_dir: PathBuf,
    /// `CACHE_DIR`: `<cache>/lutris` (newer Lutris versions keep cover art
    /// here).
    pub cache_dir: PathBuf,
    /// The game database: `pga_path` from `lutris.conf`, else
    /// `data_dir/pga.db`.
    pub pga_path: PathBuf,
}

/// The three XDG base folders of one root.
struct XdgRoots {
    data: PathBuf,
    config: PathBuf,
    cache: PathBuf,
}

impl LutrisDirs {
    /// Every Lutris installation of the current user that has a game
    /// database: the native one first (honouring `XDG_DATA_HOME`,
    /// `XDG_CONFIG_HOME` and `XDG_CACHE_HOME`), then the Flatpak.
    pub fn discover() -> Vec<LutrisDirs> {
        let Some(home) = dirs::home_dir() else {
            return Vec::new();
        };
        let xdg = |var: &str, default: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(default))
        };
        let native = XdgRoots {
            data: xdg("XDG_DATA_HOME", ".local/share"),
            config: xdg("XDG_CONFIG_HOME", ".config"),
            cache: xdg("XDG_CACHE_HOME", ".cache"),
        };
        candidates(&home, native)
    }

    /// Like [`discover`](Self::discover), with the default XDG folders
    /// under `home`.
    pub fn from_home(home: &Path) -> Vec<LutrisDirs> {
        let native = XdgRoots {
            data: home.join(".local/share"),
            config: home.join(".config"),
            cache: home.join(".cache"),
        };
        candidates(home, native)
    }

    /// Folders that may hold Lutris's YAML files, most likely first. Lutris
    /// reads only `config_dir`, but which folder that is depends on whether
    /// `~/.config/lutris` existed when it ran, so both are checked.
    pub fn yaml_dirs(&self) -> Vec<&Path> {
        let mut out = vec![self.config_dir.as_path()];
        if self.data_dir != self.config_dir {
            out.push(self.data_dir.as_path());
        }
        out
    }
}

fn candidates(home: &Path, native: XdgRoots) -> Vec<LutrisDirs> {
    let sandbox = home.join(".var/app").join(FLATPAK_ID);
    let flatpak = XdgRoots {
        data: sandbox.join("data"),
        config: sandbox.join("config"),
        cache: sandbox.join("cache"),
    };
    [(false, native), (true, flatpak)]
        .into_iter()
        .map(|(is_flatpak, roots)| resolve(home, is_flatpak, roots))
        .filter(|d| d.pga_path.is_file())
        .collect()
}

fn resolve(home: &Path, flatpak: bool, roots: XdgRoots) -> LutrisDirs {
    let data_dir = roots.data.join("lutris");
    let config = roots.config.join("lutris");
    let config_dir = if config.is_dir() {
        config
    } else {
        data_dir.clone()
    };
    let pga_path = std::fs::read_to_string(config_dir.join("lutris.conf"))
        .ok()
        .and_then(|text| ini_value(&text, "lutris", "pga_path"))
        .map(|p| expand_home(&p, home))
        .unwrap_or_else(|| data_dir.join("pga.db"));
    LutrisDirs {
        flatpak,
        home: home.to_path_buf(),
        data_dir,
        config_dir,
        cache_dir: roots.cache.join("lutris"),
        pga_path,
    }
}

/// Expand a leading `~` the way Python's `os.path.expanduser` does.
pub(crate) fn expand_home(raw: &str, home: &Path) -> PathBuf {
    if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

/// Read `key` from `[section]` of a Python `configparser` file: `key = value`
/// or `key: value`, `#` and `;` comments, case-insensitive keys. Empty values
/// count as unset.
pub(crate) fn ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = name.trim() == section;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some(split) = line.find(['=', ':']) else {
            continue;
        };
        let (k, v) = line.split_at(split);
        if k.trim().eq_ignore_ascii_case(key) {
            let v = v[1..].trim();
            return (!v.is_empty()).then(|| v.to_string());
        }
    }
    None
}
