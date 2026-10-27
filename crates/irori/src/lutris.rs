//! Importing an existing Lutris setup: reads it with
//! [`irori_extras::lutris_import`] (read-only) and applies the plan. Windows
//! and Linux games become added apps with their prefix, Wine build, arguments
//! and variables; emulated games join the ROM library with their emulator;
//! Steam and store games are matched to Irori's own stores. Favourites,
//! hidden games, categories, playtime and covers come along. Every game that
//! is not imported is reported with the reason.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use irori_core::{GameId, GameOverrides, Runner, Store as _};
use irori_extras::lutris_import::{self, ImportKind, LutrisDirs, PlannedGame};
use irori_services::Backend;
use irori_sideload::{SideloadApp, SideloadPlatform, SideloadStore};
use irori_ui::hooks::{LutrisSkip, LutrisSummary};
use serde_json::Value;

/// The store a Lutris service's games belong to in Irori.
fn service_runner(service: &str) -> Option<Runner> {
    Some(match service {
        "gog" => Runner::Gog,
        "egs" => Runner::Legendary,
        "amazon" => Runner::Nile,
        "itchio" => Runner::Itch,
        "zoom" => Runner::Zoom,
        _ => return None,
    })
}

/// Lutris's name of a Wine build (`lutris-GE-Proton8-26-x86_64`) reduced to
/// what Irori's build names share with it.
fn wine_key(name: &str) -> String {
    let lower = name.to_lowercase();
    let lower = lower.strip_prefix("lutris-").unwrap_or(&lower);
    let lower = lower
        .trim_end_matches("-x86_64")
        .trim_end_matches("-i686")
        .trim_end_matches("-amd64");
    lower.replace(['_', ' '], "-")
}

/// Import every Lutris installation found on this computer.
pub async fn import(
    backend: &Backend,
    sideload: Option<Arc<SideloadStore>>,
    emulation: Option<Arc<irori_emulation::EmulationStore>>,
) -> Result<LutrisSummary> {
    let installs = tokio::task::spawn_blocking(LutrisDirs::discover)
        .await
        .context("looking for Lutris stopped")?;
    anyhow::ensure!(
        !installs.is_empty(),
        "No Lutris library was found on this computer"
    );
    let mut summary = LutrisSummary::default();
    for dirs in installs {
        let plan = lutris_import::plan_async(dirs).await?;
        summary.warnings.extend(plan.warnings);
        for game in plan.games {
            summary.found += 1;
            apply_game(
                backend,
                sideload.as_deref(),
                emulation.as_deref(),
                game,
                &mut summary,
            )
            .await;
        }
    }
    backend.refresh_all(false).await;
    Ok(summary)
}

async fn apply_game(
    backend: &Backend,
    sideload: Option<&SideloadStore>,
    emulation: Option<&irori_emulation::EmulationStore>,
    game: PlannedGame,
    summary: &mut LutrisSummary,
) {
    let skip = |summary: &mut LutrisSummary, reason: &str, detail: Option<String>| {
        summary.skipped.push(LutrisSkip {
            title: game.name.clone(),
            reason: reason.to_string(),
            detail,
        });
    };
    let id: Option<GameId> = match &game.kind {
        ImportKind::Windows {
            exe,
            args,
            working_dir,
            prefix,
            wine_version,
            env,
            ..
        } => {
            let Some(store) = sideload else {
                skip(summary, "unavailable", None);
                return;
            };
            match add_app(
                store,
                &game.name,
                exe,
                SideloadPlatform::Windows,
                args.clone(),
                working_dir.as_deref(),
            )
            .await
            {
                Ok(id) => {
                    if let Some(prefix) = prefix {
                        backend.settings.set_game_value(
                            &id,
                            "winePrefix",
                            Some(Value::String(prefix.display().to_string())),
                        );
                    }
                    if let Some(version) = wine_version {
                        let app = backend.settings.app();
                        let builds = irori_runtime::wine_installations(&app).await;
                        let key = wine_key(version);
                        match builds.iter().find(|b| {
                            let name = wine_key(&b.name);
                            name == key || name.contains(&key) || key.contains(&name)
                        }) {
                            Some(build) => backend.settings.set_game_value(
                                &id,
                                "wineVersion",
                                serde_json::to_value(build).ok(),
                            ),
                            None => summary
                                .wine_missing
                                .push((game.name.clone(), version.clone())),
                        }
                    }
                    set_env(backend, &id, env);
                    Some(id)
                }
                Err(e) => {
                    skip(summary, "failed", Some(format!("{e:#}")));
                    return;
                }
            }
        }
        ImportKind::Native {
            exe,
            args,
            working_dir,
            env,
        } => {
            let Some(store) = sideload else {
                skip(summary, "unavailable", None);
                return;
            };
            let platform = if cfg!(target_os = "macos") {
                SideloadPlatform::Mac
            } else {
                SideloadPlatform::Linux
            };
            match add_app(
                store,
                &game.name,
                exe,
                platform,
                args.clone(),
                working_dir.as_deref(),
            )
            .await
            {
                Ok(id) => {
                    set_env(backend, &id, env);
                    Some(id)
                }
                Err(e) => {
                    skip(summary, "failed", Some(format!("{e:#}")));
                    return;
                }
            }
        }
        ImportKind::Emulated {
            system,
            engine,
            rom,
            ..
        } => {
            let Some(store) = emulation else {
                skip(summary, "unavailable", None);
                return;
            };
            if irori_engines::systems::get(system).is_none() {
                skip(summary, "systemNotSupported", Some(system.clone()));
                return;
            }
            match store
                .add_game(rom, Some(system), Some(game.name.clone()))
                .await
            {
                Ok(id) => {
                    if irori_engines::EngineRef::parse(engine)
                        .is_some_and(|r| irori_engines::engine(&r.engine).is_some())
                    {
                        backend.settings.set_game_value(
                            &id,
                            "iroriEngine",
                            Some(Value::String(engine.clone())),
                        );
                    }
                    Some(id)
                }
                Err(e) => {
                    skip(summary, "failed", Some(format!("{e:#}")));
                    return;
                }
            }
        }
        ImportKind::Steam { app_id } => {
            let id = GameId::new(Runner::Steam, app_id.as_str());
            if backend.library().get(&id).is_none() {
                skip(summary, "steamNotFound", None);
                return;
            }
            summary.matched += 1;
            Some(id)
        }
        ImportKind::StoreGame {
            service,
            service_id,
        } => match service_runner(service) {
            Some(runner) => {
                let id = GameId::new(runner, service_id.as_str());
                if backend.library().get(&id).is_some() {
                    summary.matched += 1;
                    Some(id)
                } else {
                    summary
                        .store_games
                        .push((game.name.clone(), runner.store_name().to_string()));
                    return;
                }
            }
            None => {
                skip(summary, "storeNotSupported", Some(service.clone()));
                return;
            }
        },
        ImportKind::Skipped { reason } => {
            let detail = match reason {
                lutris_import::SkipReason::UnknownRunner(r)
                | lutris_import::SkipReason::UnknownCore(r) => Some(r.clone()),
                lutris_import::SkipReason::FileMissing(p) => Some(p.display().to_string()),
                _ => None,
            };
            skip(summary, reason.id(), detail);
            return;
        }
    };
    let Some(id) = id else { return };
    if !matches!(
        game.kind,
        ImportKind::Steam { .. } | ImportKind::StoreGame { .. }
    ) {
        summary.imported += 1;
    }
    apply_user_data(backend, &id, &game, summary);
}

/// Add an app, or find the one already added for `exe`.
async fn add_app(
    store: &SideloadStore,
    title: &str,
    exe: &Path,
    platform: SideloadPlatform,
    args: Option<String>,
    working_dir: Option<&Path>,
) -> Result<GameId> {
    let exe_text = exe.display().to_string();
    if let Some(existing) = store
        .games()
        .into_iter()
        .find(|g| g.install.as_ref().is_some_and(|i| i.executable == exe_text))
    {
        return Ok(existing.id());
    }
    store
        .add_app(SideloadApp {
            title: title.to_string(),
            executable: Some(exe.to_path_buf()),
            platform,
            install_folder: working_dir.map(Path::to_path_buf),
            launcher_args: args.unwrap_or_default(),
            ..Default::default()
        })
        .await
}

fn set_env(backend: &Backend, id: &GameId, env: &std::collections::BTreeMap<String, String>) {
    if env.is_empty() {
        return;
    }
    let vars: Vec<Value> = env
        .iter()
        .map(|(k, v)| serde_json::json!({"key": k, "value": v}))
        .collect();
    backend
        .settings
        .set_game_value(id, "enviromentOptions", Some(Value::Array(vars)));
}

fn apply_user_data(
    backend: &Backend,
    id: &GameId,
    game: &PlannedGame,
    summary: &mut LutrisSummary,
) {
    let key = id.key();
    if game.favourite {
        summary.favourites += 1;
    }
    if !game.categories.is_empty() {
        summary.categories += game.categories.len();
    }
    backend.userdata.update(Some(id), |d| {
        if game.favourite {
            d.favourites.insert(key.clone());
        }
        if game.hidden {
            d.hidden.insert(key.clone());
        }
        for category in &game.categories {
            d.categories
                .entry(category.clone())
                .or_default()
                .insert(key.clone());
        }
        if game.playtime_minutes > 0 || game.last_played.is_some() {
            let rec = d.playtime.entry(key.clone()).or_default();
            // Keep the larger total: playing in both launchers isn't double counted.
            rec.total_minutes = rec.total_minutes.max(game.playtime_minutes);
            if let Some(last) = &game.last_played
                && rec.last_played.as_deref().is_none_or(|l| l < last.as_str())
            {
                rec.last_played = Some(last.clone());
            }
        }
        if let Some(cover) = &game.cover {
            let o = d
                .overrides
                .entry(key.clone())
                .or_insert_with(GameOverrides::default);
            if o.art_cover.is_none() {
                o.art_cover = Some(format!("file://{}", cover.display()));
            }
        }
    });
    if game.playtime_minutes > 0 {
        summary.playtime_games += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wine_builds_match_by_their_shared_name() {
        assert_eq!(wine_key("lutris-GE-Proton8-26-x86_64"), "ge-proton8-26");
        assert_eq!(wine_key("GE-Proton8-26"), "ge-proton8-26");
        assert_eq!(service_runner("egs"), Some(Runner::Legendary));
        assert_eq!(service_runner("humblebundle"), None);
    }
}
