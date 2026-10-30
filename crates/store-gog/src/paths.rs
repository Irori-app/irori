//! On-disk layout of the GOG store.
//!
//! Credentials, install records and support files live in the store config
//! dir (`~/.config/irori/stores/gog`), caches and download journals in the
//! store cache dir and the Galaxy redistributables under Irori's tools dir.

use std::path::{Path, PathBuf};

use irori_core::{Paths, Runner};

/// Every file and directory the GOG store reads or writes.
#[derive(Debug, Clone)]
pub struct GogPaths {
    /// `stores/gog` under Irori's config dir.
    pub config: PathBuf,
    /// `stores/gog` under Irori's cache dir.
    pub cache: PathBuf,
    /// Galaxy common redistributables (`gog-redist`).
    pub redist: PathBuf,
}

impl GogPaths {
    /// Layout under Irori's process-wide [`Paths`].
    pub fn from_irori() -> Self {
        let p = Paths::get();
        Self {
            config: p.store_config_dir(Runner::Gog),
            cache: p.store_cache_dir(Runner::Gog),
            redist: p.tools_dir().join("redist").join("gog"),
        }
    }

    /// Layout rooted at an arbitrary directory (tests, imports).
    pub fn under(root: &Path) -> Self {
        Self {
            config: root.join("config"),
            cache: root.join("cache"),
            redist: root.join("redist"),
        }
    }

    /// Token file (`{"<client id>": {access_token, …}}`).
    pub fn auth_file(&self) -> PathBuf {
        self.config.join("auth.json")
    }

    /// Account details of the logged-in user (no tokens).
    pub fn user_file(&self) -> PathBuf {
        self.config.join("user.json")
    }

    /// Installed games, in Heroic's `installed.json` shape.
    pub fn installed_file(&self) -> PathBuf {
        self.config.join("installed.json")
    }

    /// Downloads that started but have not completed (resume/cleanup info).
    pub fn pending_file(&self) -> PathBuf {
        self.config.join("pending.json")
    }

    /// `{appName: password}` for private beta branches.
    pub fn private_branches_file(&self) -> PathBuf {
        self.config.join("private_branches.json")
    }

    /// `{appName: {locationName: timestamp}}` of the last save sync.
    pub fn save_timestamps_file(&self) -> PathBuf {
        self.config.join("save_timestamps.json")
    }

    /// Playtime sessions that still need to be uploaded.
    pub fn playtime_queue_file(&self) -> PathBuf {
        self.config.join("playtime_queue.json")
    }

    /// Wine contexts remembered from launches (used for uninstallers).
    pub fn wine_contexts_file(&self) -> PathBuf {
        self.config.join("wine_contexts.json")
    }

    /// Which games were set up in which prefixes.
    pub fn prefix_setup_file(&self) -> PathBuf {
        self.config.join("prefix_setup.json")
    }

    /// Build records of installed games.
    pub fn builds_dir(&self) -> PathBuf {
        self.config.join("builds")
    }

    /// What is installed of a game's build (see `build::BuildRecord`).
    pub fn build_file(&self, app_name: &str) -> PathBuf {
        self.builds_dir()
            .join(format!("{}.json", irori_core::paths::sanitize(app_name)))
    }

    pub fn support_root(&self) -> PathBuf {
        self.config.join("support")
    }

    /// Support files (setup scripts) downloaded with a game.
    pub fn support_dir(&self, app_name: &str) -> PathBuf {
        self.support_root()
            .join(irori_core::paths::sanitize(app_name))
    }

    /// Download journal of an interrupted install or update.
    pub fn resume_file(&self, app_name: &str) -> PathBuf {
        self.cache
            .join("resume")
            .join(format!("{}.json", irori_core::paths::sanitize(app_name)))
    }

    /// gogdl's config dir from earlier Irori versions (migrated on startup).
    pub fn legacy_gogdl_dir(&self) -> PathBuf {
        self.config.join("gogdl").join("heroic_gogdl")
    }

    /// Catalogue (GameInfo without install state).
    pub fn library_cache(&self) -> PathBuf {
        self.cache.join("library.json")
    }

    /// Per-game GamesDB responses with their ETag.
    pub fn gamesdb_cache_dir(&self) -> PathBuf {
        self.cache.join("gamesdb")
    }

    pub fn install_info_cache(&self) -> PathBuf {
        self.cache.join("install_info.json")
    }

    pub fn achievements_cache(&self) -> PathBuf {
        self.cache.join("achievements.json")
    }

    pub fn extra_info_cache(&self) -> PathBuf {
        self.cache.join("extra_info.json")
    }

    /// What is downloaded of the redistributables (`redist::RedistRecord`).
    pub fn redist_record(&self) -> PathBuf {
        self.redist.join(".irori-redist.json")
    }
}
