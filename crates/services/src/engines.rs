//! Emulators for the UI: the engines Ember knows, their installations,
//! downloadable versions, installs, updates and removals, RetroArch cores,
//! each system's default emulator and BIOS status. Every change emits
//! [`Event::EnginesChanged`].

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use ember_core::{Event, OpContext};
use ember_engines::bios::{self, BiosIndex, SystemBios};
use ember_engines::releases::{self, Release};
use ember_engines::state::{CoreInstall, ManagedInstall};
use ember_engines::{EngineManifest, EngineRef, Installation, discovery, install, retroarch};

use crate::Backend;

/// One engine with what this computer has of it.
#[derive(Debug, Clone)]
pub struct EngineStatus {
    pub manifest: Arc<EngineManifest>,
    /// Best first.
    pub installations: Vec<Installation>,
    /// The installation used unless a game says otherwise.
    pub default: Option<String>,
    /// Installed cores, by id (RetroArch).
    pub cores: Vec<(String, CoreInstall)>,
}

impl EngineStatus {
    pub fn is_installed(&self) -> bool {
        !self.installations.is_empty()
    }

    /// The installation games use by default.
    pub fn default_installation(&self) -> Option<&Installation> {
        self.default
            .as_deref()
            .and_then(|d| self.installations.iter().find(|i| i.id() == d))
            .or_else(|| self.installations.first())
    }

    /// The newest managed version installed.
    pub fn newest_managed(&self) -> Option<&str> {
        self.installations
            .iter()
            .filter_map(|i| i.version.as_deref())
            .max_by(|a, b| releases::compare_versions(a, b))
    }

    pub fn has_core(&self, core: &str) -> bool {
        self.cores.iter().any(|(id, _)| id == core)
    }

    /// Whether `choice` can play right now: the engine is installed and,
    /// for RetroArch, so is the core.
    pub fn plays(&self, choice: &EngineRef) -> bool {
        self.is_installed() && choice.core.as_deref().is_none_or(|c| self.has_core(c))
    }
}

impl Backend {
    /// Every engine with its installations (discovery reads the disk).
    pub async fn engine_statuses(&self) -> Vec<EngineStatus> {
        self.spawn_blocking(|| {
            let state = ember_engines::state::load();
            let cores: Vec<(String, CoreInstall)> = state
                .cores
                .iter()
                .filter(|(_, c)| c.path.exists())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            ember_engines::engines()
                .iter()
                .map(|m| EngineStatus {
                    installations: discovery::find(m, &state),
                    default: state.preferred.get(&m.id).cloned(),
                    cores: if m.kind == ember_engines::EngineKind::Retroarch {
                        cores.clone()
                    } else {
                        Vec::new()
                    },
                    manifest: m.clone(),
                })
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    /// Downloadable versions of `engine`, newest first.
    pub async fn engine_releases(&self, engine: &str, refresh: bool) -> Result<Vec<Release>> {
        let manifest = ember_engines::engine(engine).context("unknown emulator")?;
        releases::list(&manifest, refresh).await
    }

    /// Install `release` (the newest when `None`). With `make_default`, games
    /// that don't pin a version switch to it (the Update button).
    pub async fn install_engine(
        &self,
        engine: &str,
        release: Option<Release>,
        make_default: bool,
        ctx: &OpContext,
    ) -> Result<Option<ManagedInstall>> {
        let manifest = ember_engines::engine(engine).context("unknown emulator")?;
        let release = match release {
            Some(r) => r,
            None => releases::list(&manifest, false)
                .await?
                .into_iter()
                .next()
                .with_context(|| format!("{} has no download for this computer", manifest.name))?,
        };
        let installed = install::install(&manifest, &release, ctx).await;
        if let Ok(Some(i)) = &installed
            && make_default
        {
            let (id, version) = (engine.to_string(), format!("managed:{}", i.version));
            self.spawn_blocking(move || install::set_preferred(&id, &version))
                .await
                .context("saving the default")??;
        }
        self.bus.emit(Event::EnginesChanged);
        installed
    }

    /// Delete a managed version of `engine`.
    pub async fn remove_engine_version(&self, engine: &str, version: &str) -> Result<()> {
        let result = install::remove(engine, version, &self.settings.app()).await;
        self.bus.emit(Event::EnginesChanged);
        result
    }

    /// Use `installation` (an [`Installation::id`]) for games of `engine`
    /// that don't pin one.
    pub async fn set_engine_default(&self, engine: &str, installation: &str) -> Result<()> {
        let (e, i) = (engine.to_string(), installation.to_string());
        self.spawn_blocking(move || install::set_preferred(&e, &i))
            .await
            .context("saving the default")??;
        self.bus.emit(Event::EnginesChanged);
        Ok(())
    }

    /// Use the program at `path` for `engine`, or stop using one.
    pub async fn set_engine_program(&self, engine: &str, path: Option<PathBuf>) -> Result<()> {
        if let Some(p) = &path {
            anyhow::ensure!(p.exists(), "{} does not exist", p.display());
        }
        let e = engine.to_string();
        self.spawn_blocking(move || install::set_custom(&e, path.as_deref()))
            .await
            .context("saving the program")??;
        self.bus.emit(Event::EnginesChanged);
        Ok(())
    }

    /// Download a RetroArch core.
    pub async fn install_core(&self, core: &str, ctx: &OpContext) -> Result<Option<CoreInstall>> {
        let result = retroarch::install_core(core, ctx).await;
        self.bus.emit(Event::EnginesChanged);
        result
    }

    pub async fn remove_core(&self, core: &str) -> Result<()> {
        let result = retroarch::remove_core(core).await;
        self.bus.emit(Event::EnginesChanged);
        result
    }

    /// The emulator a game of `system` plays with when it names none: the
    /// user's choice for the system, else the first recommended emulator
    /// that is installed, else the first recommended one.
    pub fn system_engine(&self, system: &str, statuses: &[EngineStatus]) -> Option<EngineRef> {
        let app = self.settings.app();
        if let Some(chosen) = app
            .system_engines
            .get(system)
            .and_then(|s| EngineRef::parse(s))
        {
            return Some(chosen);
        }
        let choices = ember_engines::choices_for(system);
        choices
            .iter()
            .find(|c| {
                statuses
                    .iter()
                    .any(|s| s.manifest.id == c.engine && s.plays(c))
            })
            .or_else(|| choices.first())
            .cloned()
    }

    /// Make `choice` the default emulator of `system` (`None`: Ember's
    /// recommendation).
    pub fn set_system_engine(&self, system: &str, choice: Option<&EngineRef>) {
        self.settings.update_app(|app| match choice {
            Some(c) => {
                app.system_engines.insert(system.to_string(), c.to_string());
            }
            None => {
                app.system_engines.remove(system);
            }
        });
        self.bus.emit(Event::EnginesChanged);
    }

    /// BIOS status of every system from the saved index.
    pub async fn bios_statuses(&self) -> Vec<SystemBios> {
        self.spawn_blocking(|| {
            let index = bios::cached_index();
            ember_engines::systems::all()
                .iter()
                .map(|s| bios::status(s, &index))
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    /// Index the BIOS folder `root` again (only new or changed files are
    /// hashed).
    pub async fn rescan_bios(&self, root: PathBuf) -> Result<BiosIndex> {
        let index = self
            .spawn_blocking(move || {
                let previous = bios::cached_index();
                bios::scan(&root, &previous)
            })
            .await
            .context("scanning stopped")??;
        self.bus.emit(Event::EnginesChanged);
        Ok(index)
    }
}
