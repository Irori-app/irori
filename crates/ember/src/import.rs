//! Launcher-level part of the Heroic import (settings, favourites,
//! categories, playtime, overrides). Store credentials and installs are
//! imported by each store crate first, so their games resolve here.

use std::collections::HashMap;
use std::path::Path;

use ember_core::Runner;
use ember_services::Backend;
use ember_ui::hooks::ImportSummary;

pub async fn apply_launcher_data(
    backend: &Backend,
    heroic_dir: &Path,
    summary: &mut ImportSummary,
) -> anyhow::Result<()> {
    let mut index: HashMap<String, Runner> =
        ember_extras::heroic_import::heroic_runner_index(heroic_dir);
    for store in backend.stores() {
        for g in store.games() {
            index.insert(g.app_name.clone(), store.runner());
        }
    }
    let data = ember_extras::heroic_import::import(heroic_dir.to_path_buf(), move |app| {
        index.get(app).copied()
    })
    .await?;
    for w in &data.warnings {
        tracing::warn!("Heroic import: {w}");
    }
    if !data.unresolved.is_empty() {
        tracing::info!(
            "Heroic import skipped {} unknown games",
            data.unresolved.len()
        );
    }

    if let Some(mut settings) = data.settings {
        // Presentation preferences are Ember's own.
        settings.ui = backend.settings.app().ui.clone();
        backend.settings.replace_app(settings);
    }
    summary.games_with_settings = data.game_settings.len();
    for (id, overrides) in data.game_settings {
        backend.settings.replace_overrides(&id, overrides);
    }

    summary.favourites = data.favourites.len();
    summary.categories = data.categories.len();
    backend.userdata.update(None, |u| {
        u.favourites
            .extend(data.favourites.iter().map(|id| id.key()));
        u.hidden.extend(data.hidden.iter().map(|id| id.key()));
        for id in data.recent.iter().rev() {
            let key = id.key();
            if !u.recent.contains(&key) {
                u.recent.insert(0, key);
            }
        }
        for (name, ids) in &data.categories {
            u.categories
                .entry(name.clone())
                .or_default()
                .extend(ids.iter().map(|id| id.key()));
        }
        for (id, rec) in &data.playtime {
            let entry = u.playtime.entry(id.key()).or_default();
            entry.total_minutes = entry.total_minutes.max(rec.total_minutes);
            if entry.last_played.is_none() {
                entry.last_played = rec.last_played.clone();
            }
            if entry.first_played.is_none() {
                entry.first_played = rec.first_played.clone();
            }
        }
        for (id, o) in &data.overrides {
            u.overrides.entry(id.key()).or_insert_with(|| o.clone());
        }
    });
    Ok(())
}
