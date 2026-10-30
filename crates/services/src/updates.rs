//! Updates of what games run on: Wine and Proton builds from the Wine
//! manager, DXVK, VKD3D-Proton and DXVK-NVAPI, and emulators from the
//! Emulators page. Game updates stay with the stores and the download queue;
//! this module also decides what a failed store check leaves of them.
//!
//! A check runs once a session and then daily, on lists cached for a day, so
//! it stays within GitHub's limits for anonymous requests. Nothing installs
//! unless asked, or unless `iroriAutoUpdateComponents` is on. A skipped
//! version stays skipped until a newer one is out.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result};
use irori_core::process::CancellationToken;
use irori_core::{Event, GameId, Notice, NoticeLevel, OpContext, Paths, Runner, json_store, util};
use irori_engines::releases;
use irori_runtime::{DxTool, manager, tools};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use crate::Backend;

/// Seconds between two automatic checks.
const CHECK_EVERY: u64 = 24 * 60 * 60;

/// What a component is, for its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    Wine,
    /// DXVK, VKD3D-Proton, DXVK-NVAPI.
    Translation,
    Emulator,
}

#[derive(Debug, Clone, PartialEq)]
enum Target {
    Tool(DxTool),
    Wine(Box<manager::Release>),
    Engine(String, Box<releases::Release>),
}

/// A component with a newer build available.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentUpdate {
    /// Stable across checks (`dxvk`, `wine:GE-Proton`, `engine:duckstation`).
    pub id: String,
    pub kind: ComponentKind,
    pub name: String,
    pub installed: String,
    pub latest: String,
    /// The user skipped this version.
    pub skipped: bool,
    /// The release's notes, when the source publishes a page.
    pub notes_url: Option<String>,
    target: Target,
}

/// What the update manager knows about components.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComponentReport {
    pub updates: Vec<ComponentUpdate>,
    /// When the last check ended, in Unix seconds.
    pub checked_at: Option<u64>,
    pub checking: bool,
    /// Ids being updated.
    pub running: BTreeSet<String>,
    /// Why the last update of an id failed.
    pub failed: BTreeMap<String, String>,
    /// Sources the last check couldn't read.
    pub check_errors: Vec<String>,
}

impl ComponentReport {
    /// Updates the user hasn't skipped.
    pub fn pending(&self) -> impl Iterator<Item = &ComponentUpdate> {
        self.updates.iter().filter(|u| !u.skipped)
    }
}

/// `state/updates.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Saved {
    checked_at: Option<u64>,
    /// Id → the version the user skipped.
    skipped: BTreeMap<String, String>,
}

fn saved_path() -> std::path::PathBuf {
    Paths::get().state_dir().join("updates.json")
}

pub(crate) struct ComponentUpdates {
    report: RwLock<Arc<ComponentReport>>,
    saved: Mutex<Saved>,
    cancels: Mutex<HashMap<String, CancellationToken>>,
    /// A check ran this session, so the list isn't empty just for being new.
    checked: AtomicBool,
}

impl ComponentUpdates {
    pub(crate) fn load() -> Self {
        let saved: Saved = json_store::read_or_default(&saved_path());
        let report = ComponentReport {
            checked_at: saved.checked_at,
            ..Default::default()
        };
        Self {
            report: RwLock::new(Arc::new(report)),
            saved: Mutex::new(saved),
            cancels: Mutex::new(HashMap::new()),
            checked: AtomicBool::new(false),
        }
    }

    fn update(&self, change: impl FnOnce(&mut ComponentReport)) {
        let mut report = (**self.report.read()).clone();
        change(&mut report);
        *self.report.write() = Arc::new(report);
    }
}

fn is_skipped(skipped: &BTreeMap<String, String>, id: &str, latest: &str) -> bool {
    skipped.get(id).is_some_and(|v| v == latest)
}

/// The update set after a check. `answers` holds each store's games, or
/// `None` when its check failed: what was known of that store stands, so a
/// passing network error doesn't clear its updates and announce them anew.
pub(crate) fn merge_game_updates(
    old: &HashSet<GameId>,
    answers: Vec<(Runner, Option<Vec<GameId>>)>,
) -> HashSet<GameId> {
    let mut out = HashSet::new();
    for (runner, games) in answers {
        match games {
            Some(games) => out.extend(games),
            None => out.extend(old.iter().filter(|id| id.runner == runner).cloned()),
        }
    }
    out
}

impl Backend {
    /// What the update manager knows about components.
    pub fn component_updates(&self) -> Arc<ComponentReport> {
        self.components.report.read().clone()
    }

    fn set_components(&self, change: impl FnOnce(&mut ComponentReport)) {
        self.components.update(change);
        self.bus.emit(Event::ComponentUpdatesChanged);
    }

    /// Look for new builds of every installed component.
    pub async fn check_components(&self) {
        if self.component_updates().checking {
            return;
        }
        self.set_components(|r| r.checking = true);
        let (found, errors) = self.find_component_updates().await;
        let now = util::unix_now();
        let saved = {
            let mut saved = self.components.saved.lock();
            saved.checked_at = Some(now);
            // Skips of versions no longer on offer have done their job.
            let offered: HashMap<&str, &str> = found
                .iter()
                .map(|u| (u.id.as_str(), u.latest.as_str()))
                .collect();
            saved
                .skipped
                .retain(|id, v| offered.get(id.as_str()) == Some(&v.as_str()));
            serde_json::to_value(&*saved).unwrap_or_default()
        };
        self.save_components(saved).await;
        self.components.checked.store(true, Ordering::Relaxed);
        let skipped = self.components.saved.lock().skipped.clone();
        self.set_components(|r| {
            r.updates = found
                .into_iter()
                .map(|mut u| {
                    u.skipped = is_skipped(&skipped, &u.id, &u.latest);
                    u
                })
                .collect();
            r.checking = false;
            r.checked_at = Some(now);
            r.check_errors = errors;
        });
    }

    async fn save_components(&self, saved: serde_json::Value) {
        let write = self.spawn_blocking(move || json_store::write(&saved_path(), &saved));
        match write.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("saving updates.json: {e:#}"),
            Err(e) => tracing::warn!("saving updates.json: {e}"),
        }
    }

    async fn find_component_updates(&self) -> (Vec<ComponentUpdate>, Vec<String>) {
        let mut found = Vec::new();
        let mut errors = Vec::new();
        for tool in DxTool::ALL {
            match tools::tool_update(tool).await {
                Ok(Some((installed, latest))) => found.push(ComponentUpdate {
                    id: tool.id().to_string(),
                    kind: ComponentKind::Translation,
                    name: tool.label().to_string(),
                    installed,
                    latest,
                    skipped: false,
                    notes_url: None,
                    target: Target::Tool(tool),
                }),
                Ok(None) => {}
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        match manager::check_updates().await {
            Ok(list) => found.extend(list.into_iter().map(|u| {
                ComponentUpdate {
                    id: format!("wine:{}", u.latest.kind),
                    kind: ComponentKind::Wine,
                    name: u.latest.kind.clone(),
                    installed: u.installed.version,
                    latest: u.latest.version.clone(),
                    skipped: false,
                    notes_url: (!u.latest.release_notes_link.is_empty())
                        .then(|| u.latest.release_notes_link.clone()),
                    target: Target::Wine(Box::new(u.latest)),
                }
            })),
            Err(e) => errors.push(format!("{e:#}")),
        }
        for status in self.engine_statuses().await {
            let Some(have) = status.newest_managed().map(str::to_string) else {
                continue;
            };
            let id = status.manifest.id.clone();
            match self.engine_releases(&id, false).await {
                Ok(list) => {
                    let Some(newest) = list
                        .into_iter()
                        .next()
                        .filter(|r| releases::compare_versions(&r.version, &have).is_gt())
                    else {
                        continue;
                    };
                    found.push(ComponentUpdate {
                        id: format!("engine:{id}"),
                        kind: ComponentKind::Emulator,
                        name: status.manifest.name.clone(),
                        installed: have,
                        latest: newest.version.clone(),
                        skipped: false,
                        notes_url: None,
                        target: Target::Engine(id, Box::new(newest)),
                    });
                }
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        if let Err(e) = tools::refresh_winetricks().await {
            errors.push(format!("{e:#}"));
        }
        (found, errors)
    }

    /// The daily check: online, once a session and when a day has passed.
    pub(crate) async fn check_components_when_due(&self) {
        let due = !self.components.checked.load(Ordering::Relaxed)
            || self
                .component_updates()
                .checked_at
                .is_none_or(|at| util::unix_now().saturating_sub(at) >= CHECK_EVERY);
        if !due || !self.is_online() {
            return;
        }
        self.check_components().await;
        if self.settings.app().auto_update_components {
            self.update_all_components().await;
        }
    }

    /// Install the update of `id`. The outcome also reaches the user as a
    /// notice, so it isn't lost when the page is closed.
    pub async fn update_component(&self, id: &str) -> Result<()> {
        let report = self.component_updates();
        let update = report
            .updates
            .iter()
            .find(|u| u.id == id)
            .with_context(|| format!("{id} has no update"))?
            .clone();
        if report.running.contains(id) {
            return Ok(());
        }
        let cancel = CancellationToken::new();
        self.components
            .cancels
            .lock()
            .insert(id.to_string(), cancel.clone());
        self.set_components(|r| {
            r.running.insert(id.to_string());
            r.failed.remove(id);
        });
        let ctx = OpContext {
            cancel: cancel.clone(),
            ..OpContext::detached()
        };
        let result = match &update.target {
            Target::Tool(tool) => tools::update_tool(*tool, &ctx).await.map(|_| ()),
            Target::Wine(release) => manager::install(release, &self.settings.app(), &ctx)
                .await
                .map(|_| ()),
            Target::Engine(engine, release) => self
                .install_engine(engine, Some((**release).clone()), true, &ctx)
                .await
                .map(|_| ()),
        };
        self.components.cancels.lock().remove(id);
        let cancelled = cancel.is_cancelled();
        self.set_components(|r| {
            r.running.remove(id);
            match &result {
                Ok(()) => r.updates.retain(|u| u.id != id),
                Err(_) if cancelled => {}
                Err(e) => {
                    r.failed.insert(id.to_string(), format!("{e:#}"));
                }
            }
        });
        let name = update.name.as_str();
        match result {
            Ok(()) => {
                self.bus.notice(
                    Notice::new(NoticeLevel::Success, format!("{name} updated")).keys(
                        "shell.notice.component_updated",
                        None,
                        &[("name", name), ("version", &update.latest)],
                    ),
                );
                Ok(())
            }
            Err(_) if cancelled => Ok(()),
            Err(e) => {
                self.bus.notice(
                    Notice::new(NoticeLevel::Error, format!("{name} failed to update"))
                        .keys(
                            "shell.notice.component_update_failed",
                            None,
                            &[("name", name)],
                        )
                        .message(format!("{e:#}")),
                );
                Err(e)
            }
        }
    }

    /// Install every update the user hasn't skipped, one after another.
    pub async fn update_all_components(&self) {
        let ids: Vec<String> = self
            .component_updates()
            .pending()
            .map(|u| u.id.clone())
            .collect();
        for id in ids {
            if let Err(e) = self.update_component(&id).await {
                tracing::warn!("updating {id}: {e:#}");
            }
        }
    }

    /// Stop the running update of `id`.
    pub fn cancel_component_update(&self, id: &str) {
        if let Some(cancel) = self.components.cancels.lock().get(id) {
            cancel.cancel();
        }
    }

    /// Skip (or, with `skip` false, offer again) the current version of `id`.
    pub async fn skip_component_update(&self, id: &str, skip: bool) {
        let Some(latest) = self
            .component_updates()
            .updates
            .iter()
            .find(|u| u.id == id)
            .map(|u| u.latest.clone())
        else {
            return;
        };
        let saved = {
            let mut saved = self.components.saved.lock();
            if skip {
                saved.skipped.insert(id.to_string(), latest);
            } else {
                saved.skipped.remove(id);
            }
            serde_json::to_value(&*saved).unwrap_or_default()
        };
        self.save_components(saved).await;
        self.set_components(|r| {
            if let Some(u) = r.updates.iter_mut().find(|u| u.id == id) {
                u.skipped = skip;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(runner: Runner, app: &str) -> GameId {
        GameId::new(runner, app)
    }

    #[test]
    fn a_failed_store_check_keeps_what_was_known() {
        let old: HashSet<GameId> = [game(Runner::Gog, "a"), game(Runner::Legendary, "b")].into();
        let new = merge_game_updates(
            &old,
            vec![
                (Runner::Gog, None),
                (Runner::Legendary, Some(vec![game(Runner::Legendary, "c")])),
            ],
        );
        assert_eq!(
            new,
            [game(Runner::Gog, "a"), game(Runner::Legendary, "c")].into()
        );
    }

    #[test]
    fn a_store_no_longer_asked_drops_out() {
        let old: HashSet<GameId> = [game(Runner::Gog, "a")].into();
        assert!(merge_game_updates(&old, Vec::new()).is_empty());
    }

    #[test]
    fn skipping_holds_only_for_that_version() {
        let skipped = BTreeMap::from([("dxvk".to_string(), "dxvk-2.7".to_string())]);
        assert!(is_skipped(&skipped, "dxvk", "dxvk-2.7"));
        assert!(!is_skipped(&skipped, "dxvk", "dxvk-2.8"));
        assert!(!is_skipped(&skipped, "vkd3d", "dxvk-2.7"));
    }

    #[test]
    fn pending_leaves_out_skipped_updates() {
        let update = |id: &str, skipped| ComponentUpdate {
            id: id.into(),
            kind: ComponentKind::Translation,
            name: id.into(),
            installed: "1".into(),
            latest: "2".into(),
            skipped,
            notes_url: None,
            target: Target::Tool(DxTool::Dxvk),
        };
        let report = ComponentReport {
            updates: vec![update("a", false), update("b", true)],
            ..Default::default()
        };
        let ids: Vec<_> = report.pending().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, ["a"]);
    }
}
