//! Filesystem layout. Everything lives under XDG directories; `EMBER_HOME`
//! relocates all of it (used by tests and portable installs).

use std::path::{Path, PathBuf};

use once_cell::sync::OnceCell;

use crate::runner::{GameId, Runner};

static PATHS: OnceCell<Paths> = OnceCell::new();

#[derive(Debug, Clone)]
pub struct Paths {
    /// `~/.config/ember`: settings and store credentials.
    pub config: PathBuf,
    /// `~/.local/share/ember`: tools, Wine builds, bundled programs, logs.
    pub data: PathBuf,
    /// `~/.cache/ember`: images, store API caches.
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
        if let Some(root) = std::env::var_os("EMBER_HOME") {
            return Paths::under(Path::new(&root));
        }
        let home = dirs::home_dir().unwrap_or_else(std::env::temp_dir);
        Paths {
            config: dirs::config_dir()
                .unwrap_or_else(|| home.join(".config"))
                .join("ember"),
            data: dirs::data_local_dir()
                .unwrap_or_else(|| home.join(".local/share"))
                .join("ember"),
            cache: dirs::cache_dir()
                .unwrap_or_else(|| home.join(".cache"))
                .join("ember"),
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
    /// (see `ember_services::accounts`).
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

    /// Programs Ember places in Wine prefixes (`EpicGamesLauncher.exe`,
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
        self.logs_dir().join("ember.log")
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
