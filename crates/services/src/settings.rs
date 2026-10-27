//! Application settings and sparse per-game overrides, persisted as JSON.

use std::collections::HashMap;
use std::sync::Arc;

use irori_core::json_store;
use irori_core::settings::{AppSettings, GameSettings, GameSettingsOverrides};
use irori_core::{Event, EventBus, GameId, Paths};
use parking_lot::RwLock;
use serde_json::Value;

pub struct SettingsService {
    app: RwLock<Arc<AppSettings>>,
    /// Loaded lazily per game; absent key = not loaded yet.
    overrides: RwLock<HashMap<GameId, GameSettingsOverrides>>,
    bus: EventBus,
}

impl SettingsService {
    pub fn load(bus: EventBus) -> Self {
        let path = Paths::get().settings_file();
        let app: AppSettings = json_store::read_or_default(&path);
        Self {
            app: RwLock::new(Arc::new(app)),
            overrides: RwLock::new(HashMap::new()),
            bus,
        }
    }

    /// Current application settings (cheap snapshot).
    pub fn app(&self) -> Arc<AppSettings> {
        self.app.read().clone()
    }

    /// Mutate and persist application settings.
    pub fn update_app(&self, f: impl FnOnce(&mut AppSettings)) {
        let snapshot = {
            let mut guard = self.app.write();
            let mut next = (**guard).clone();
            f(&mut next);
            if next == **guard {
                return;
            }
            *guard = Arc::new(next);
            guard.clone()
        };
        if let Err(e) = json_store::write(&Paths::get().settings_file(), &*snapshot) {
            tracing::error!("saving settings: {e:#}");
        }
        self.bus.emit(Event::SettingsChanged);
    }

    /// Replace all settings (import).
    pub fn replace_app(&self, settings: AppSettings) {
        self.update_app(|s| *s = settings);
    }

    pub fn overrides(&self, id: &GameId) -> GameSettingsOverrides {
        if let Some(o) = self.overrides.read().get(id) {
            return o.clone();
        }
        let loaded: GameSettingsOverrides =
            json_store::read_or_default(&Paths::get().game_settings_file(id));
        self.overrides
            .write()
            .entry(id.clone())
            .or_insert(loaded)
            .clone()
    }

    /// Effective settings of a game: global defaults plus its overrides.
    pub fn game(&self, id: &GameId) -> GameSettings {
        self.overrides(id).apply(&self.app().game)
    }

    /// Set (`Some`) or reset to the global default (`None`) one setting key.
    pub fn set_game_value(&self, id: &GameId, key: &str, value: Option<Value>) {
        let mut o = self.overrides(id);
        match value {
            Some(v) => o.set(key, v),
            None => o.reset(key),
        }
        self.store_overrides(id, o);
    }

    /// Store effective settings for a game, keeping only what differs from defaults.
    pub fn set_game(&self, id: &GameId, effective: &GameSettings) {
        let o = GameSettingsOverrides::diff(&self.app().game, effective);
        self.store_overrides(id, o);
    }

    pub fn replace_overrides(&self, id: &GameId, overrides: GameSettingsOverrides) {
        self.store_overrides(id, overrides);
    }

    /// Forget a game's overrides (uninstall with settings removal).
    pub fn remove_game(&self, id: &GameId) {
        self.overrides.write().remove(id);
        let _ = std::fs::remove_file(Paths::get().game_settings_file(id));
        self.bus.emit(Event::GameSettingsChanged(id.clone()));
    }

    /// Drop every game's overrides so all games follow the global defaults.
    pub fn reset_all_games(&self) {
        let ids: Vec<GameId> = self.overrides.write().drain().map(|(id, _)| id).collect();
        let dir = Paths::get().config.join("games");
        if let Err(e) = std::fs::remove_dir_all(&dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::error!("resetting game settings: {e}");
        }
        for id in ids {
            self.bus.emit(Event::GameSettingsChanged(id));
        }
        self.bus.emit(Event::SettingsChanged);
    }

    fn store_overrides(&self, id: &GameId, o: GameSettingsOverrides) {
        let path = Paths::get().game_settings_file(id);
        let res = if o.0.is_empty() {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            }
        } else {
            json_store::write(&path, &o)
        };
        if let Err(e) = res {
            tracing::error!("saving settings of {id}: {e:#}");
        }
        self.overrides.write().insert(id.clone(), o);
        self.bus.emit(Event::GameSettingsChanged(id.clone()));
    }
}
