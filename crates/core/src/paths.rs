//! Filesystem layout. Everything lives under XDG directories; `IRORI_HOME`
//! relocates all of it (used by tests and portable installs).

use std::path::{Path, PathBuf};

use once_cell::sync::OnceCell;

use crate::runner::{GameId, Runner};

static PATHS: OnceCell<Paths> = OnceCell::new();

#[derive(Debug, Clone)]
pub struct Paths {
    /// `~/.config/irori`: settings and store credentials.
    pub config: PathBuf,
    /// `~/.local/share/irori`: tools, Wine builds, bundled programs, logs.
    pub data: PathBuf,
    /// `~/.cache/irori`: images, store API caches.
    pub cache: PathBuf,
}

impl Paths {
    /// Process-wide paths, resolved once.
    pub fn get() -> &'static Paths {
        PATHS.get_or_init(Paths::resolve)
    }

    /// Install explicit paths (tests). Returns false when already initialised.
    pub fn init(paths: Paths) -> bool {
        PATHS.set(paths).is_ok()
    }

    pub fn under(root: &Path) -> Paths {
        Paths {
            config: root.join("config"),
            data: root.join("data"),
            cache: root.join("cache"),
        }
    }

    fn resolve() -> Paths {
        if let Some(root) = std::env::var_os("IRORI_HOME") {
            return Paths::under(Path::new(&root));
        }
        let home = dirs::home_dir().unwrap_or_else(std::env::temp_dir);
        let paths = Paths {
            config: dirs::config_dir()
                .unwrap_or_else(|| home.join(".config"))
                .join("irori"),
            data: dirs::data_local_dir()
                .unwrap_or_else(|| home.join(".local/share"))
                .join("irori"),
            cache: dirs::cache_dir()
                .unwrap_or_else(|| home.join(".cache"))
                .join("irori"),
        };
        paths.adopt_legacy_dirs();
        paths
    }

    /// Irori was called Ember before its first release. Move Ember's folders
    /// over once, so settings, logins and installs survive the rename. Another
    /// program's `ember` folder (the WorldForge client has one) is left alone:
    /// only a config folder holding Ember's own `settings.json` and `state`
    /// counts.
    fn adopt_legacy_dirs(&self) {
        let legacy = |dir: &Path| dir.with_file_name("ember");
        let config = legacy(&self.config);
        if self.config.exists()
            || !config.join("settings.json").is_file()
            || !config.join("state").is_dir()
        {
            return;
        }
        for dir in [&self.config, &self.data, &self.cache] {
            let old = legacy(dir);
            if dir.exists() || !old.is_dir() {
                continue;
            }
            if let Err(e) = std::fs::rename(&old, dir) {
                tracing::warn!("could not move {} to {}: {e}", old.display(), dir.display());
            }
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    /// Sparse per-game settings file.
    pub fn game_settings_file(&self, id: &GameId) -> PathBuf {
        self.config
            .join("games")
            .join(id.runner.id())
            .join(format!("{}.json", sanitize(&id.app_name)))
    }

    /// Launcher-owned state files (download queue, playtime, favourites…).
    pub fn state_dir(&self) -> PathBuf {
        self.config.join("state")
    }

    /// A store's credentials, install records and other state.
    pub fn store_config_dir(&self, runner: Runner) -> PathBuf {
        self.config.join("stores").join(runner.id())
    }

    /// Cached store API responses.
    pub fn store_cache_dir(&self, runner: Runner) -> PathBuf {
        self.cache.join("stores").join(runner.id())
    }

    /// Credentials and installs of a store account other than the first
    /// (see `irori_services::accounts`).
    pub fn account_config_dir(&self, runner: Runner, slot: &str) -> PathBuf {
        self.store_config_dir(runner)
            .join("accounts")
            .join(sanitize(slot))
    }

    /// Caches of a store account other than the first. Kept apart from
    /// [`Paths::store_cache_dir`], which stores clear when logging out.
    pub fn account_cache_dir(&self, runner: Runner, slot: &str) -> PathBuf {
        self.cache
            .join("accounts")
            .join(runner.id())
            .join(sanitize(slot))
    }

    pub fn images_dir(&self) -> PathBuf {
        self.cache.join("images")
    }

    pub fn tools_dir(&self) -> PathBuf {
        self.data.join("tools")
    }

    /// Downloaded Wine builds.
    pub fn wine_dir(&self) -> PathBuf {
        self.tools_dir().join("wine")
    }

    /// Downloaded Proton builds.
    pub fn proton_dir(&self) -> PathBuf {
        self.tools_dir().join("proton")
    }

    /// Programs Irori places in Wine prefixes (`EpicGamesLauncher.exe`,
    /// `GalaxyCommunication.exe`).
    pub fn bin_dir(&self) -> PathBuf {
        self.data.join("bin")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data.join("logs")
    }

    pub fn game_log_file(&self, id: &GameId) -> PathBuf {
        self.logs_dir().join("games").join(format!(
            "{}-{}.log",
            id.runner.id(),
            sanitize(&id.app_name)
        ))
    }

    /// Output of installs, updates and repairs of a game.
    pub fn operation_log_file(&self, id: &GameId) -> PathBuf {
        self.logs_dir().join("games").join(format!(
            "{}-{}.install.log",
            id.runner.id(),
            sanitize(&id.app_name)
        ))
    }

    pub fn launcher_log_file(&self) -> PathBuf {
        self.logs_dir().join("irori.log")
    }
}

/// Replace characters that are unsafe in file names.
pub fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c => c,
        })
        .collect()
}

/// Expand a leading `~` to the home directory.
pub fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// Heroic's config directory, for import. Checks the native and Flatpak locations.
pub fn heroic_config_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    [
        dirs::config_dir().map(|c| c.join("heroic")),
        Some(home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic")),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.join("config.json").is_file())
}
