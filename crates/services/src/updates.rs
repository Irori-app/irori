//! Updates of what games run on: Wine and Proton builds from the Wine
//! manager, DXVK, VKD3D-Proton and DXVK-NVAPI, the umu and anticheat runtimes,
//! emulators, RetroArch cores, and (shown, never installed) emulators from
//! Flatpak. Game updates stay with the stores and the
//! download queue; this module also decides what a failed store check leaves
//! of them, and when automatic updates of either kind may start.
//!
//! A check runs once a session and then daily, on lists cached for a day, so
//! it stays within GitHub's limits for anonymous requests. Nothing installs
//! unless asked, or unless `iroriAutoUpdateComponents` is on. A skipped
//! version stays skipped until a newer one is out. The runtimes alone also
//! update during a launch that needs them, because the game can't start
//! without them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result};
use irori_core::process::CancellationToken;
use irori_core::process::CommandSpec;
use irori_core::{
    AppSettings, Event, GameId, InstallProgress, Notice, NoticeLevel, OpContext, Paths, Runner,
    json_store, util,
};
use irori_engines::discovery::Origin;
use irori_engines::{releases, retroarch};
use irori_runtime::{DxTool, discovery, manager, runtimes, tools};
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
    /// umu and the anticheat runtimes.
    Runtime,
    Emulator,
    /// RetroArch's cores, as one entry.
    Cores,
}

/// Why automatic updates are waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoUpdateHold {
    /// Outside the hours set, as `(from, until)` hours.
    Window(u32, u32),
    /// On a metered connection.
    Metered,
}

/// A build from the Wine manager that an update replaced, kept to go back to.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviousBuild {
    pub version: String,
    release: Box<manager::Release>,
}

#[derive(Debug, Clone, PartialEq)]
enum Target {
    Tool(DxTool),
    Wine(Box<manager::Release>),
    Engine(String, Box<releases::Release>),
    Runtime(runtimes::RuntimeName),
    Cores(Vec<String>),
    /// Something else installs it; Irori only tells.
    External,
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
    /// Flatpak updates this: Irori shows it and leaves it alone.
    pub external: bool,
    /// The new build replaces the installed one, under the same name.
    pub in_place: bool,
    /// The release's notes, when the source publishes a page.
    pub notes_url: Option<String>,
    /// What a skip is remembered by: `latest`, unless that changes too often
    /// to mean a version.
    skip_key: String,
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
    /// How far along each running update is, in percent.
    pub progress: BTreeMap<String, f32>,
    /// Wine builds with an earlier version kept.
    pub previous: Vec<PreviousBuild>,
    /// What automatic updates are waiting for, while they are on.
    pub auto_hold: Option<AutoUpdateHold>,
    /// Why the last update of an id failed.
    pub failed: BTreeMap<String, String>,
    /// Sources the last check couldn't read.
    pub check_errors: Vec<String>,
}

impl ComponentReport {
    /// Updates the user hasn't skipped.
    pub fn pending(&self) -> impl Iterator<Item = &ComponentUpdate> {
        self.updates.iter().filter(|u| !u.skipped && !u.external)
    }
}

/// `state/updates.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Saved {
    checked_at: Option<u64>,
    /// Id → the skip key of what the user skipped.
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

fn is_skipped(skipped: &BTreeMap<String, String>, id: &str, key: &str) -> bool {
    skipped.get(id).is_some_and(|v| v == key)
}

/// Whether `hour` is inside the `from`..`until` hours (`until` excluded), past
/// midnight when `until` is earlier. No limit unless both are valid hours.
fn in_update_window(hour: u32, from: Option<u32>, until: Option<u32>) -> bool {
    match (from, until) {
        (Some(from), Some(until)) if from < until => (from..until).contains(&hour),
        (Some(from), Some(until)) if from > until => hour >= from || hour < until,
        _ => true,
    }
}

fn update_hour(value: &str) -> Option<u32> {
    value.trim().parse().ok().filter(|h| *h < 24)
}

/// Overall percent of update `index` of `total` when it is `percent` done.
fn combined_percent(index: usize, total: usize, percent: f32) -> f32 {
    (index as f32 + percent / 100.0) / total.max(1) as f32 * 100.0
}

/// `(application, version)` per line of `flatpak remote-ls --updates`.
fn parse_flatpak_updates(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut columns = line.split('\t').map(str::trim);
            let app = columns.next().filter(|a| a.contains('.'))?;
            Some((app.to_string(), columns.next().unwrap_or("").to_string()))
        })
        .collect()
}

/// Apps Flatpak has updates for. Empty where it can't say: not installed, or
/// Irori itself runs inside a Flatpak.
async fn flatpak_updates() -> HashMap<String, String> {
    let spec = CommandSpec::new("flatpak").args([
        "remote-ls",
        "--updates",
        "--app",
        "--columns=application,version",
    ]);
    match discovery::run_with_timeout(&spec, std::time::Duration::from_secs(30)).await {
        Some(result) if result.success() => parse_flatpak_updates(&result.stdout),
        _ => HashMap::new(),
    }
}

fn previous_builds() -> Vec<PreviousBuild> {
    manager::rollbacks()
        .into_iter()
        .map(|release| PreviousBuild {
            version: release.version.clone(),
            release: Box::new(release),
        })
        .collect()
}

/// A release date as the page shows it (`2026-10-30T12:00:00Z` → `2026-10-30`).
fn date_part(value: &str) -> String {
    value.chars().take(10).collect()
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
                .map(|u| (u.id.as_str(), u.skip_key.as_str()))
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
                    u.skipped = is_skipped(&skipped, &u.id, &u.skip_key);
                    u
                })
                .collect();
            r.previous = previous_builds();
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
                    skip_key: latest.clone(),
                    latest,
                    skipped: false,
                    external: false,
                    in_place: false,
                    notes_url: None,
                    target: Target::Tool(tool),
                }),
                Ok(None) => {}
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        match manager::check_updates().await {
            Ok(list) => found.extend(list.into_iter().map(|u| {
                let in_place = u.installed.version == u.latest.version;
                ComponentUpdate {
                    id: format!("wine:{}", u.latest.kind),
                    kind: ComponentKind::Wine,
                    name: u.latest.kind.clone(),
                    installed: u.installed.version,
                    latest: if in_place {
                        u.latest.date.clone()
                    } else {
                        u.latest.version.clone()
                    },
                    in_place,
                    skip_key: format!("{}:{}", u.latest.version, u.latest.date),
                    skipped: false,
                    external: false,
                    notes_url: (!u.latest.release_notes_link.is_empty())
                        .then(|| u.latest.release_notes_link.clone()),
                    target: Target::Wine(Box::new(u.latest)),
                }
            })),
            Err(e) => errors.push(format!("{e:#}")),
        }
        match runtimes::check_updates().await {
            Ok(list) => {
                found.extend(
                    list.into_iter()
                        .map(|(name, installed, latest)| ComponentUpdate {
                            id: format!("runtime:{}", name.id()),
                            kind: ComponentKind::Runtime,
                            name: name.label().to_string(),
                            installed: date_part(&installed),
                            latest: date_part(&latest),
                            skip_key: latest,
                            skipped: false,
                            external: false,
                            in_place: false,
                            notes_url: None,
                            target: Target::Runtime(name),
                        }),
                )
            }
            Err(e) => errors.push(format!("{e:#}")),
        }
        let engines = self.engine_statuses().await;
        let from_flatpak = engines
            .iter()
            .flat_map(|s| &s.installations)
            .any(|i| i.origin == Origin::Flatpak);
        let flatpak = if from_flatpak {
            flatpak_updates().await
        } else {
            HashMap::new()
        };
        for status in engines {
            for install in status
                .installations
                .iter()
                .filter(|i| i.origin == Origin::Flatpak)
            {
                let Some(app) = install.flatpak.as_deref() else {
                    continue;
                };
                let Some(version) = flatpak.get(app) else {
                    continue;
                };
                found.push(ComponentUpdate {
                    id: format!("flatpak:{app}"),
                    kind: ComponentKind::Emulator,
                    name: status.manifest.name.clone(),
                    installed: String::new(),
                    latest: version.clone(),
                    skip_key: version.clone(),
                    skipped: false,
                    external: true,
                    in_place: false,
                    notes_url: None,
                    target: Target::External,
                });
            }
            match retroarch::check_core_updates(&status.cores).await {
                Ok(Some(update)) => found.push(ComponentUpdate {
                    id: "retroarch-cores".to_string(),
                    kind: ComponentKind::Cores,
                    name: status.manifest.name.clone(),
                    installed: update.oldest,
                    latest: update.newest,
                    // Nightly builds move the date daily; the set of cores
                    // says whether this is still what was skipped.
                    skip_key: update.cores.join(","),
                    skipped: false,
                    external: false,
                    in_place: false,
                    notes_url: None,
                    target: Target::Cores(update.cores),
                }),
                Ok(None) => {}
                Err(e) => errors.push(format!("{e:#}")),
            }
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
                        skip_key: newest.version.clone(),
                        skipped: false,
                        external: false,
                        in_place: false,
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
    }

    /// What holds automatic updates back right now.
    async fn auto_update_hold(&self, app: &AppSettings) -> Option<AutoUpdateHold> {
        use chrono::Timelike as _;
        let (from, until) = (
            update_hour(&app.auto_update_from),
            update_hour(&app.auto_update_until),
        );
        let hour = chrono::Local::now().hour();
        if !in_update_window(hour, from, until) {
            return Some(AutoUpdateHold::Window(from?, until?));
        }
        (app.auto_update_unmetered && crate::network::is_metered().await == Some(true))
            .then_some(AutoUpdateHold::Metered)
    }

    /// Start the updates the user wants done unattended, when the hours and
    /// the connection allow. Runs with each refresh, so updates held back
    /// start soon after the window opens. A component that failed waits for
    /// the user, to not repeat the failure every few minutes.
    pub(crate) async fn run_automatic_updates(&self) {
        let app = self.settings.app();
        let wanted = app.auto_update_games || app.auto_update_components;
        let hold = if wanted {
            self.auto_update_hold(&app).await
        } else {
            None
        };
        if self.component_updates().auto_hold != hold {
            let hold = hold.clone();
            self.set_components(|r| r.auto_hold = hold);
        }
        if !wanted || hold.is_some() || !self.is_online() {
            return;
        }
        if app.auto_update_games {
            self.enqueue_all_updates().await;
        }
        if app.auto_update_components {
            let report = self.component_updates();
            let ids: Vec<String> = report
                .pending()
                .filter(|u| !report.failed.contains_key(&u.id))
                .map(|u| u.id.clone())
                .collect();
            self.update_components(ids).await;
        }
    }

    /// The progress sink of the update of `id`, reporting whole percents so
    /// the page redraws a hundred times at most.
    fn component_progress(&self, id: &str) -> Arc<dyn Fn(InstallProgress) + Send + Sync> {
        let (b, id) = (self.clone(), id.to_string());
        let last = Mutex::new(-1.0_f32);
        Arc::new(move |p: InstallProgress| {
            let Some(percent) = p.percent else { return };
            let percent = percent.floor();
            let mut seen = last.lock();
            if *seen != percent {
                *seen = percent;
                drop(seen);
                b.set_components(|r| {
                    r.progress.insert(id.clone(), percent);
                });
            }
        })
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
        let progress = self.component_progress(id);
        let ctx = OpContext {
            cancel: cancel.clone(),
            progress: progress.clone(),
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
            Target::Runtime(name) => runtimes::download(*name, &ctx).await,
            Target::External => Ok(()),
            Target::Cores(cores) => self.update_cores(cores, &progress, &cancel).await,
        };
        self.components.cancels.lock().remove(id);
        let cancelled = cancel.is_cancelled();
        self.set_components(|r| {
            r.running.remove(id);
            r.progress.remove(id);
            r.previous = previous_builds();
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

    /// Download every core in `cores`, with one progress over all of them.
    async fn update_cores(
        &self,
        cores: &[String],
        progress: &Arc<dyn Fn(InstallProgress) + Send + Sync>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        for (index, core) in cores.iter().enumerate() {
            if cancel.is_cancelled() {
                break;
            }
            let (overall, total) = (progress.clone(), cores.len());
            let ctx = OpContext {
                cancel: cancel.clone(),
                progress: Arc::new(move |mut p: InstallProgress| {
                    p.percent = p.percent.map(|v| combined_percent(index, total, v));
                    overall(p);
                }),
                ..OpContext::detached()
            };
            self.install_core(core, &ctx).await?;
        }
        Ok(())
    }

    /// Install every update the user hasn't skipped, one after another.
    pub async fn update_all_components(&self) {
        let ids: Vec<String> = self
            .component_updates()
            .pending()
            .map(|u| u.id.clone())
            .collect();
        self.update_components(ids).await;
    }

    async fn update_components(&self, ids: Vec<String>) {
        for id in ids {
            if let Err(e) = self.update_component(&id).await {
                tracing::warn!("updating {id}: {e:#}");
            }
        }
    }

    /// Bring back the build an update of `version` replaced.
    pub async fn roll_back_build(&self, version: &str) -> Result<()> {
        let build = self
            .component_updates()
            .previous
            .iter()
            .find(|p| p.version == version)
            .cloned()
            .with_context(|| format!("No earlier {version} is kept"))?;
        manager::rollback(&build.release).await?;
        self.bus.emit(Event::WineVersionsChanged);
        self.check_components().await;
        Ok(())
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
            .map(|u| u.skip_key.clone())
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
    fn reads_flatpak_updates() {
        let found =
            parse_flatpak_updates("org.DolphinEmu.dolphin-emu\t2412\nnet.pcsx2.PCSX2\t\n\n");
        assert_eq!(found["org.DolphinEmu.dolphin-emu"], "2412");
        assert_eq!(found["net.pcsx2.PCSX2"], "");
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn update_windows_cover_their_hours() {
        assert!(in_update_window(3, None, None));
        assert!(in_update_window(3, Some(22), None));
        assert!(in_update_window(3, Some(4), Some(4)));
        assert!(in_update_window(4, Some(4), Some(8)));
        assert!(!in_update_window(8, Some(4), Some(8)));
        assert!(in_update_window(23, Some(22), Some(6)));
        assert!(in_update_window(2, Some(22), Some(6)));
        assert!(!in_update_window(6, Some(22), Some(6)));
        assert!(!in_update_window(12, Some(22), Some(6)));
        assert_eq!(update_hour(" 7 "), Some(7));
        assert_eq!(update_hour("24"), None);
        assert_eq!(update_hour(""), None);
    }

    #[test]
    fn progress_spans_all_cores() {
        assert_eq!(combined_percent(0, 4, 0.0), 0.0);
        assert_eq!(combined_percent(1, 4, 50.0), 37.5);
        assert_eq!(combined_percent(3, 4, 100.0), 100.0);
        assert_eq!(combined_percent(0, 0, 50.0), 50.0);
    }

    #[test]
    fn release_dates_lose_their_time() {
        assert_eq!(date_part("2026-10-30T12:00:00Z"), "2026-10-30");
        assert_eq!(date_part("2026-10-30"), "2026-10-30");
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
            skip_key: "2".into(),
            skipped,
            external: false,
            in_place: false,
            notes_url: None,
            target: Target::Tool(DxTool::Dxvk),
        };
        let report = ComponentReport {
            updates: vec![
                update("a", false),
                update("b", true),
                ComponentUpdate {
                    external: true,
                    in_place: false,
                    ..update("c", false)
                },
            ],
            ..Default::default()
        };
        let ids: Vec<_> = report.pending().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, ["a"]);
    }
}
