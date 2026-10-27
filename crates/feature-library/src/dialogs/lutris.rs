//! "Install with a Lutris script": find a game's community install script
//! on lutris.net (or open a script file), pick a version, choose where the
//! game goes and the files it needs, read and approve everything the
//! script will do, then watch it run and answer its questions. Nothing runs
//! before the consent step; shell commands are shown verbatim and need
//! their own acknowledgement. Cancel or failure rolls the game folder back
//! and keeps downloads. A finished install becomes an added app or a
//! console game, with the script recorded for later reinstalls
//! (`docs/design/engines.md` section 8).

mod state;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::oneshot;
use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::link::Link;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, rems,
};
use irori_core::GameId;
use irori_core::process::CancellationToken;
use irori_core::util::format_bytes;
use irori_runtime::is_cancelled;
use irori_scripts::model::AutosetupStore;
use irori_scripts::{
    Consent, DiscRequest, FileRequest, GameResult, MenuRequest, NativeLauncher, Outcome,
    ProgramKind, Script, ScriptError, UnfinishedInstall, WineMatch, consent, script_error,
};
use irori_services::scripts::{
    InstallFromStore, ScriptGame, ScriptLibraryEntry, ScriptProgress, WinePick,
    autosetup_store_name, script_context, script_game,
};
use irori_services::{ScriptInstall, ScriptPrompt};
use irori_sideload::{SideloadApp, SideloadPlatform};
use irori_ui::i18n::{t, t_args};
use irori_ui::{Route, backend, models, navigate};
use parking_lot::Mutex;

use super::{dialog_width, pick_path, text};
use state::{Choice, ConsentChecks, Refusal, Step, files_ready, native_game, refusal};

/// How often a running install repaints.
const TICK: Duration = Duration::from_millis(250);
/// Search results shown; more means the search should be narrower.
const MAX_RESULTS: usize = 50;

/// A question from the running script.
enum Pending {
    Menu {
        request: MenuRequest,
        selected: Option<usize>,
        reply: Option<oneshot::Sender<Option<String>>>,
    },
    File {
        request: FileRequest,
        reply: Option<oneshot::Sender<Option<PathBuf>>>,
    },
    Disc {
        request: DiscRequest,
        retry: bool,
        reply: Option<oneshot::Sender<Option<PathBuf>>>,
    },
}

/// Why the install stopped.
struct Failure {
    message: SharedString,
    cancelled: bool,
    /// The files are in place; only the program is missing.
    unfinished: Option<Box<Outcome>>,
    /// The script wanted a store Irori has natively.
    store: Option<AutosetupStore>,
}

struct Running {
    progress: Arc<Mutex<ScriptProgress>>,
    cancel: CancellationToken,
    log_file: PathBuf,
}

pub struct LutrisDialog {
    step: Step,
    query: Entity<InputState>,
    searching: bool,
    results: Option<Vec<GameResult>>,
    /// The game whose scripts are listed.
    game: Option<(String, String)>,
    page_url: Option<String>,
    loading: bool,
    choices: Vec<Choice>,
    selected: Option<usize>,
    destination: Entity<InputState>,
    free_space: Option<u64>,
    consent: Option<Consent>,
    probing: bool,
    picked: BTreeMap<String, PathBuf>,
    checks: ConsentChecks,
    wine: Option<WinePick>,
    running: Option<Running>,
    pending: Option<Pending>,
    show_details: bool,
    failure: Option<Failure>,
    finishing: bool,
    done: Option<(GameId, Option<String>)>,
    last_install: Option<ScriptInstall>,
    error: Option<SharedString>,
    _ticker: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LutrisDialog {
    fn new(slug: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t("library.lutris.find.placeholder"))
        });
        let destination = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&query, window, |this, _, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.search(window, cx);
                }
            }),
            cx.subscribe_in(&destination, window, |this, _, event, window, cx| {
                if let InputEvent::Change = event {
                    this.destination_changed(window, cx);
                }
            }),
        ];
        let mut this = Self {
            step: Step::Find,
            query,
            searching: false,
            results: None,
            game: None,
            page_url: None,
            loading: false,
            choices: Vec::new(),
            selected: None,
            destination,
            free_space: None,
            consent: None,
            probing: false,
            picked: BTreeMap::new(),
            checks: ConsentChecks::default(),
            wine: None,
            running: None,
            pending: None,
            show_details: false,
            failure: None,
            finishing: false,
            done: None,
            last_install: None,
            error: None,
            _ticker: None,
            _subscriptions: subscriptions,
        };
        if let Some(slug) = slug.filter(|s| !s.trim().is_empty()) {
            this.open_game(slug.clone(), slug, window, cx);
        }
        this
    }

    fn script(&self) -> Option<Arc<Script>> {
        let choice = self.choices.get(self.selected?)?;
        choice.script.as_ref().ok().cloned()
    }

    fn has_files(&self) -> bool {
        self.consent.as_ref().is_some_and(|c| {
            !c.downloads.is_empty() || !c.user_files.is_empty() || !c.steam_files.is_empty()
        })
    }

    fn game_dir(&self, cx: &App) -> PathBuf {
        PathBuf::from(text(&self.destination, cx))
    }

    // ---- Find ------------------------------------------------------------

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = text(&self.query, cx);
        if query.is_empty() || self.searching {
            return;
        }
        self.searching = true;
        self.error = None;
        cx.notify();
        let b = backend(cx);
        let task = irori_ui::run(cx, async move { b.search_lutris(query).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.searching = false;
                match result {
                    Ok(results) => this.results = Some(results),
                    Err(e) => this.error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// List the scripts lutris.net has for `slug`.
    fn open_game(
        &mut self,
        slug: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = true;
        self.error = None;
        self.step = Step::Version;
        self.game = Some((slug.clone(), name));
        self.page_url = Some(irori_scripts::api::page_url(&slug));
        self.choices.clear();
        self.selected = None;
        cx.notify();
        let b = backend(cx);
        let task = irori_ui::run(cx, async move { b.lutris_installers(slug).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(listings) => {
                        if let Some(first) = listings.first()
                            && let Some(game) = &mut this.game
                        {
                            game.1 = first.name.clone();
                        }
                        let choices = listings.iter().map(Choice::from_listing).collect();
                        this.set_choices(choices, window, cx);
                    }
                    Err(e) => this.error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = pick_path(false, t("library.lutris.find.open_file"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = picked.await else { return };
            let Ok(task) = this.update(cx, |this, cx| {
                this.loading = true;
                this.error = None;
                cx.notify();
                let p = path.clone();
                let b = backend(cx);
                irori_ui::run(cx, async move {
                    let shown = p.display().to_string();
                    let text = b
                        .spawn_blocking(move || std::fs::read_to_string(&p))
                        .await?
                        .map_err(|e| anyhow::anyhow!("{shown}: {e}"))?;
                    Ok(Script::parse_all(&text)?)
                })
            }) else {
                return;
            };
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(scripts) if !scripts.is_empty() => {
                        let info = &scripts[0].info;
                        this.game = Some((info.game_slug.clone(), info.name.clone()));
                        this.page_url = Some(info.page_url());
                        let choices = scripts
                            .into_iter()
                            .map(|s| Choice::from_script(s, &path))
                            .collect();
                        this.step = Step::Version;
                        this.set_choices(choices, window, cx);
                    }
                    Ok(_) => this.error = Some(t("library.lutris.find.empty_file")),
                    Err(e) => this.error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_choices(&mut self, choices: Vec<Choice>, _: &mut Window, _: &mut Context<Self>) {
        self.selected = choices
            .iter()
            .position(|c| c.script.as_ref().is_ok_and(|s| refusal(s).is_none()));
        self.choices = choices;
    }

    // ---- Steps -----------------------------------------------------------

    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        match self.step {
            Step::Version => {
                let Some(script) = self.script() else { return };
                let app = backend(cx).settings.app();
                let dir =
                    state::default_game_dir(&app.default_install_path, &script.info.game_slug);
                let value = dir.to_string_lossy().into_owned();
                self.destination
                    .update(cx, |s, cx| s.set_value(value, window, cx));
                self.step = Step::Destination;
                self.destination_changed(window, cx);
            }
            Step::Destination => {
                let dir = self.game_dir(cx);
                if dir.as_os_str().is_empty() || !dir.is_absolute() {
                    self.error = Some(t("library.lutris.destination.invalid"));
                    cx.notify();
                    return;
                }
                let Some(script) = self.script() else { return };
                let app = backend(cx).settings.app();
                let ctx = script_context(&app, &script, &dir);
                let plan = consent(&script, &ctx);
                self.checks = ConsentChecks::new(&plan);
                self.picked
                    .retain(|id, _| plan.user_files.iter().any(|f| &f.file_id == id));
                self.consent = Some(plan);
                self.step = self.step.next(self.has_files());
                self.probe_sizes(window, cx);
                self.load_wine(window, cx);
            }
            Step::Files => self.step = Step::Consent,
            Step::Consent => self.install(window, cx),
            _ => {}
        }
        cx.notify();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if let Some(step) = self.step.back(self.has_files()) {
            self.step = step;
            self.error = None;
            self.failure = None;
            cx.notify();
        }
    }

    fn destination_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.game_dir(cx);
        let task = irori_ui::state::run_infallible(cx, async move {
            irori_services::scripts::free_space(&dir)
        });
        cx.spawn_in(window, async move |this, cx| {
            let free = task.await.flatten();
            this.update(cx, |this, cx| {
                this.free_space = free;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn browse_destination(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = pick_path(true, t("library.lutris.destination.choose"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(folder) = picked.await else { return };
            this.update_in(cx, |this, window, cx| {
                let slug = this
                    .script()
                    .map(|s| s.info.game_slug.clone())
                    .unwrap_or_default();
                let dir = folder.join(irori_core::paths::sanitize(&slug));
                let value = dir.to_string_lossy().into_owned();
                this.destination
                    .update(cx, |s, cx| s.set_value(value, window, cx));
            })
            .ok();
        })
        .detach();
    }

    fn probe_sizes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self.consent.clone() else {
            return;
        };
        if plan.downloads.iter().all(|d| d.size.is_some()) {
            return;
        }
        self.probing = true;
        let task = irori_ui::state::run_infallible(cx, async move {
            let mut plan = plan;
            plan.probe_sizes().await;
            plan.downloads
        });
        cx.spawn_in(window, async move |this, cx| {
            let downloads = task.await;
            this.update(cx, |this, cx| {
                this.probing = false;
                if let (Some(downloads), Some(plan)) = (downloads, &mut this.consent) {
                    plan.downloads = downloads;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_wine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(script) = self.script() else { return };
        let b = backend(cx);
        let task = irori_ui::state::run_infallible(cx, async move { b.script_wine(&script).await });
        cx.spawn_in(window, async move |this, cx| {
            let wine = task.await.flatten();
            this.update(cx, |this, cx| {
                this.wine = wine;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn pick_user_file(&mut self, file_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let picked = pick_path(false, t("library.lutris.files.choose"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = picked.await else { return };
            this.update(cx, |this, cx| {
                this.picked.insert(file_id, path);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- Install ---------------------------------------------------------

    fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(script), Some(plan)) = (self.script(), self.consent.as_ref()) else {
            return;
        };
        if !self.checks.ready(plan) {
            return;
        }
        let source_url = self
            .choices
            .get(self.selected.unwrap_or(0))
            .map(|c| c.source.clone());
        let resolution = cx.primary_display().map(|d| {
            let size = d.bounds().size;
            (u32::from(size.width), u32::from(size.height))
        });
        let install = ScriptInstall {
            script,
            game_dir: self.game_dir(cx),
            approvals: self.checks.approvals(plan),
            picked: self.picked.clone(),
            source_url,
            resolution,
        };
        self.start(install, window, cx);
    }

    fn start(&mut self, install: ScriptInstall, window: &mut Window, cx: &mut Context<Self>) {
        self.last_install = Some(install.clone());
        let run = backend(cx).start_script_install(install);
        self.step = Step::Progress;
        self.failure = None;
        self.pending = None;
        self.running = Some(Running {
            progress: run.progress.clone(),
            cancel: run.cancel.clone(),
            log_file: run.log_file.clone(),
        });
        let mut prompts = run.prompts;
        cx.spawn_in(window, async move |this, cx| {
            while let Some(prompt) = prompts.next().await {
                let pending = match prompt {
                    ScriptPrompt::File { request, reply } => Pending::File {
                        request,
                        reply: Some(reply),
                    },
                    ScriptPrompt::Menu { request, reply } => Pending::Menu {
                        selected: request
                            .preselect
                            .as_ref()
                            .and_then(|p| request.options.iter().position(|o| &o.value == p)),
                        request,
                        reply: Some(reply),
                    },
                    ScriptPrompt::Disc {
                        request,
                        retry,
                        reply,
                    } => Pending::Disc {
                        request,
                        retry,
                        reply: Some(reply),
                    },
                };
                if this
                    .update(cx, |this, cx| {
                        this.pending = Some(pending);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        let result = run.result;
        cx.spawn_in(window, async move |this, cx| {
            let result = result
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("The install stopped unexpectedly")));
            this.update_in(cx, |this, window, cx| this.finished(result, window, cx))
                .ok();
        })
        .detach();
        self.ensure_ticker(window, cx);
        cx.notify();
    }

    fn ensure_ticker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self._ticker.is_some() {
            return;
        }
        self._ticker = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let Ok(running) = this.update(cx, |this, cx| {
                    cx.notify();
                    this.step == Step::Progress
                }) else {
                    return;
                };
                if !running {
                    let _ = this.update(cx, |this, _| this._ticker = None);
                    return;
                }
            }
        }));
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = &self.running {
            run.cancel.cancel();
        }
        // An open question is answered "no", which also stops the install.
        self.pending = None;
        cx.notify();
    }

    fn finished(
        &mut self,
        result: anyhow::Result<Outcome>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(run) = &self.running {
            self.wine = run.progress.lock().wine.clone().or(self.wine.take());
        }
        self.pending = None;
        match result {
            Ok(outcome) => self.add_to_library(outcome, None, window, cx),
            Err(e) => self.fail(e, cx),
        }
        cx.notify();
    }

    fn fail(&mut self, e: anyhow::Error, cx: &mut Context<Self>) {
        let cancelled = is_cancelled(&e);
        let store = e
            .chain()
            .find_map(|c| c.downcast_ref::<InstallFromStore>())
            .map(|s| s.0);
        let unfinished = UnfinishedInstall::of(&e).map(|u| u.outcome.clone());
        let message: SharedString = if cancelled {
            t("library.lutris.failed.cancelled")
        } else if let Some(store) = store {
            t_args(
                "library.lutris.failed.store",
                &[("store", autosetup_store_name(store))],
            )
        } else if unfinished.is_some() {
            match script_error(&e) {
                Some(ScriptError::MissingExecutable {
                    expected: Some(path),
                }) => t_args(
                    "library.lutris.failed.missing_program",
                    &[("path", &path.display().to_string())],
                ),
                _ => t("library.lutris.failed.no_program"),
            }
        } else {
            format!("{e:#}").into()
        };
        self.failure = Some(Failure {
            message,
            cancelled,
            unfinished,
            store,
        });
        self.step = Step::Failed;
        cx.notify();
    }

    /// Choose the game's program for an install that named none.
    fn choose_program(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(outcome) = self.failure.as_ref().and_then(|f| f.unfinished.clone()) else {
            return;
        };
        let picked = pick_path(false, t("library.lutris.failed.choose_program"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(program) = picked.await else { return };
            this.update_in(cx, |this, window, cx| {
                this.add_to_library(*outcome, Some(program), window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn add_to_library(
        &mut self,
        outcome: Outcome,
        program: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let game_dir = self
            .last_install
            .as_ref()
            .map(|i| i.game_dir.clone())
            .unwrap_or_default();
        let game = match script_game(
            &outcome,
            &game_dir,
            self.wine.as_ref().map(|w| &w.build),
            program.as_deref(),
        ) {
            Ok(game) => game,
            Err(e) => return self.fail(e, cx),
        };
        let (sideload, emulation) = (crate::sideload_store(cx), crate::emulation_store(cx));
        let b = backend(cx);
        self.finishing = true;
        cx.notify();
        let task = irori_ui::run(cx, async move {
            let id = add_game(&game, sideload, emulation).await?;
            for (key, value) in game.settings {
                b.settings.set_game_value(&id, key, Some(value));
            }
            Ok(id)
        });
        let complete = outcome.complete_text.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.finishing = false;
                match result {
                    Ok(id) => {
                        this.done = Some((id, complete));
                        this.failure = None;
                        this.step = Step::Done;
                    }
                    Err(e) => this.fail(e, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(install) = self.last_install.clone() {
            self.start(install, window, cx);
        }
    }

    fn play(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, _)) = self.done.clone() else {
            return;
        };
        window.close_dialog(cx);
        navigate(Route::Game(id.clone()), cx);
        let b = backend(cx);
        let task = irori_ui::run(cx, async move { b.launch(&id, None, Vec::new()).await });
        window
            .spawn(cx, async move |cx| {
                if let Err(e) = task.await {
                    cx.update(|window, cx| {
                        crate::notify_error(t("library.lutris.done.play_failed"), &e, window, cx)
                    })
                    .ok();
                }
            })
            .detach();
    }

    // ---- Prompt answers --------------------------------------------------

    fn answer_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(Pending::Menu {
            request,
            selected: Some(ix),
            reply,
        }) = &mut self.pending
            && let (Some(option), Some(reply)) = (request.options.get(*ix), reply.take())
        {
            let _ = reply.send(Some(option.value.clone()));
            self.pending = None;
            cx.notify();
        }
    }

    fn answer_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let folders = matches!(self.pending, Some(Pending::Disc { .. }));
        let picked = pick_path(folders, t("library.lutris.prompt.choose"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = picked.await else { return };
            this.update(cx, |this, cx| {
                let reply = match &mut this.pending {
                    Some(Pending::File { reply, .. } | Pending::Disc { reply, .. }) => reply.take(),
                    _ => None,
                };
                if let Some(reply) = reply {
                    let _ = reply.send(Some(path));
                }
                this.pending = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// Add a finished install to the library.
async fn add_game(
    game: &ScriptGame,
    sideload: Option<Arc<irori_sideload::SideloadStore>>,
    emulation: Option<Arc<irori_emulation::EmulationStore>>,
) -> anyhow::Result<GameId> {
    match &game.entry {
        ScriptLibraryEntry::App {
            title,
            program,
            windows,
            args,
            folder,
        } => {
            let store =
                sideload.ok_or_else(|| anyhow::anyhow!(t("library.sideload.error.unavailable")))?;
            let platform = if *windows {
                SideloadPlatform::Windows
            } else if cfg!(target_os = "macos") {
                SideloadPlatform::Mac
            } else {
                SideloadPlatform::Linux
            };
            store
                .add_app(SideloadApp {
                    title: title.clone(),
                    executable: Some(program.clone()),
                    platform,
                    install_folder: Some(folder.clone()),
                    launcher_args: args.clone(),
                    ..Default::default()
                })
                .await
        }
        ScriptLibraryEntry::Emulated {
            title,
            file,
            system,
        } => {
            let store =
                emulation.ok_or_else(|| anyhow::anyhow!(t("library.lutris.error.no_emulation")))?;
            store
                .add_game(file, Some(system), Some(title.clone()))
                .await
        }
    }
}

// ---- Rendering --------------------------------------------------------------

fn section(title: SharedString, cx: &App) -> gpui_kit::Div {
    v_flex().gap_2().child(
        div()
            .text_sm()
            .font_semibold()
            .text_color(cx.theme().foreground)
            .child(title),
    )
}

fn muted(text: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .min_w_0()
        .child(text.into())
}

fn mono_block(id: &'static str, lines: Vec<String>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .id(id)
        .w_full()
        .max_h(rems(12.))
        .overflow_y_scroll()
        .p_2()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted)
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .children(lines.into_iter().map(|l| div().min_w_0().child(l)))
}

fn native_launcher_name(launcher: NativeLauncher) -> &'static str {
    match launcher {
        NativeLauncher::EpicGames => "Epic Games Launcher",
        NativeLauncher::EaApp => "EA app",
        NativeLauncher::UbisoftConnect => "Ubisoft Connect",
    }
}

/// Why a script can't be picked, for its row.
fn choice_problem(choice: &Choice) -> Option<SharedString> {
    match &choice.script {
        Err(ScriptError::PreferNative { launcher }) => Some(t_args(
            "library.lutris.version.prefer_native",
            &[("launcher", native_launcher_name(*launcher))],
        )),
        Err(e) => Some(t_args(
            "library.lutris.version.refused",
            &[("error", &e.to_string())],
        )),
        Ok(script) => match refusal(script)? {
            Refusal::NeedsGame(game) => Some(t_args(
                "library.lutris.version.needs_game",
                &[("game", &game)],
            )),
            Refusal::Runner(runner) => Some(t_args(
                "library.lutris.version.runner",
                &[("runner", &runner)],
            )),
        },
    }
}

impl LutrisDialog {
    fn render_find(&self, cx: &mut Context<Self>) -> AnyElement {
        let results = self.results.clone().unwrap_or_default();
        let shown = results.len().min(MAX_RESULTS);
        let rows: Vec<AnyElement> = results
            .into_iter()
            .take(MAX_RESULTS)
            .enumerate()
            .map(|(ix, game)| {
                let detail = {
                    let mut parts: Vec<String> = Vec::new();
                    if let Some(year) = game.year {
                        parts.push(year.to_string());
                    }
                    if !game.platforms.is_empty() {
                        parts.push(game.platforms.join(", "));
                    }
                    parts.push(if game.installers == 1 {
                        t("library.lutris.find.scripts_one").to_string()
                    } else {
                        t_args(
                            "library.lutris.find.scripts_other",
                            &[("count", &game.installers.to_string())],
                        )
                        .to_string()
                    });
                    parts.join(" · ")
                };
                let (slug, name) = (game.slug.clone(), game.name.clone());
                Button::new(ElementId::Name(format!("lutris-result-{ix}").into()))
                    .ghost()
                    .w_full()
                    .h_auto()
                    .py_1()
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .items_start()
                            .child(div().text_sm().truncate().w_full().child(game.name.clone()))
                            .child(muted(detail, cx).truncate().w_full()),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_game(slug.clone(), name.clone(), window, cx)
                    }))
                    .into_any_element()
            })
            .collect();
        let searched = self.results.is_some();
        v_flex()
            .gap_3()
            .child(div().text_sm().child(t("library.lutris.find.intro")))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.query)))
                    .child(
                        Button::new("lutris-search")
                            .primary()
                            .icon(IconName::Search)
                            .label(t("library.lutris.find.search"))
                            .loading(self.searching)
                            .disabled(self.searching)
                            .on_click(cx.listener(|this, _, window, cx| this.search(window, cx))),
                    ),
            )
            .when(searched && rows.is_empty() && !self.searching, |this| {
                this.child(muted(t("library.lutris.find.no_results"), cx))
            })
            .when(!rows.is_empty(), |this| {
                this.child(
                    v_flex()
                        .id("lutris-results")
                        .max_h(rems(18.))
                        .overflow_y_scroll()
                        .children(rows),
                )
            })
            .when(shown == MAX_RESULTS, |this| {
                this.child(muted(t("library.lutris.find.narrow"), cx))
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("lutris-open-file")
                            .outline()
                            .icon(IconName::FolderOpen)
                            .label(t("library.lutris.find.open_file"))
                            .loading(self.loading)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_file(window, cx)),
                            ),
                    )
                    .child(muted(t("library.lutris.find.file_hint"), cx).flex_1()),
            )
            .into_any_element()
    }

    fn render_native_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let script = self.script()?;
        let autosetup = consent(
            &script,
            &script_context(&backend(cx).settings.app(), &script, &PathBuf::from("/")),
        )
        .autosetups
        .first()
        .map(|a| a.store);
        let library = models(cx).library.read(cx).snapshot.clone();
        let native = native_game(&script.info.name, library.games.iter());
        if autosetup.is_none() && native.is_none() {
            return None;
        }
        let store = match (&native, autosetup) {
            (Some(id), _) => id.runner.store_name().to_string(),
            (None, Some(store)) => autosetup_store_name(store).to_string(),
            (None, None) => return None,
        };
        Some(
            v_flex()
                .gap_2()
                .child(
                    Alert::info(
                        "lutris-native",
                        t_args(
                            "library.lutris.native.body",
                            &[("title", &script.info.name), ("store", &store)],
                        ),
                    )
                    .title(t("library.lutris.native.title")),
                )
                .child(
                    h_flex().justify_end().child(
                        Button::new("lutris-native-open")
                            .small()
                            .icon(IconName::ExternalLink)
                            .label(if native.is_some() {
                                t("library.lutris.native.open_game")
                            } else {
                                t("library.lutris.native.open_accounts")
                            })
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                match &native {
                                    Some(id) => navigate(Route::Game(id.clone()), cx),
                                    None => navigate(Route::Accounts, cx),
                                }
                            }),
                    ),
                )
                .into_any_element(),
        )
    }

    fn render_version(&self, cx: &mut Context<Self>) -> AnyElement {
        let name = self.game.as_ref().map(|g| g.1.clone()).unwrap_or_default();
        let page = self.page_url.clone();
        let rows: Vec<AnyElement> = self
            .choices
            .iter()
            .enumerate()
            .map(|(ix, choice)| {
                let problem = choice_problem(choice);
                let selected = self.selected == Some(ix);
                let theme = cx.theme();
                let mut facts = vec![
                    t_args(
                        "library.lutris.version.runner_name",
                        &[("runner", &choice.runner)],
                    )
                    .to_string(),
                ];
                if let Some(author) = &choice.author {
                    facts.push(
                        t_args("library.lutris.version.by", &[("author", author)]).to_string(),
                    );
                }
                if let Some(date) = &choice.date {
                    facts.push(date.clone());
                }
                let enabled = problem.is_none();
                v_flex()
                    .id(ElementId::Name(format!("lutris-choice-{ix}").into()))
                    .gap_1()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(if selected { theme.ring } else { theme.border })
                    .when(enabled, |d| {
                        d.cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = Some(ix);
                                cx.notify();
                            }))
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Checkbox::new(ElementId::Name(format!("lutris-pick-{ix}").into()))
                                    .checked(selected)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if enabled {
                                            this.selected = Some(ix);
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_medium()
                                    .truncate()
                                    .child(choice.version.clone()),
                            )
                            .child(Tag::secondary().xsmall().child(choice.runner.clone())),
                    )
                    .child(muted(facts.join(" · "), cx).truncate())
                    .when_some(choice.description.clone(), |d, text| {
                        d.child(div().text_xs().min_w_0().child(text))
                    })
                    .when_some(choice.notes.clone(), |d, text| d.child(muted(text, cx)))
                    .when_some(problem, |d, problem| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .min_w_0()
                                .child(problem),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_base()
                            .font_semibold()
                            .truncate()
                            .child(name),
                    )
                    .when_some(page, |d, page| {
                        d.child(
                            Link::new("lutris-page")
                                .href(page)
                                .child(t("library.lutris.version.page")),
                        )
                    }),
            )
            .child(muted(t("library.lutris.version.attribution"), cx))
            .children(self.render_native_banner(cx))
            .when(self.loading, |d| {
                d.child(
                    h_flex()
                        .gap_2()
                        .child(Spinner::new().small())
                        .child(muted(t("library.lutris.version.loading"), cx)),
                )
            })
            .when(
                !self.loading && rows.is_empty() && self.error.is_none(),
                |d| d.child(muted(t("library.lutris.version.none"), cx)),
            )
            .child(
                v_flex()
                    .id("lutris-choices")
                    .gap_2()
                    .max_h(rems(22.))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_destination(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_3()
            .child(div().text_sm().child(t("library.lutris.destination.intro")))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.destination)),
                    )
                    .child(
                        Button::new("lutris-destination-browse")
                            .outline()
                            .icon(IconName::FolderOpen)
                            .label(t("library.common.browse"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.browse_destination(window, cx)
                            })),
                    ),
            )
            .when_some(self.free_space, |d, free| {
                d.child(muted(
                    t_args(
                        "library.lutris.destination.free",
                        &[("size", &format_bytes(free))],
                    ),
                    cx,
                ))
            })
            .into_any_element()
    }

    fn render_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(plan) = &self.consent else {
            return div().into_any_element();
        };
        let user_rows: Vec<AnyElement> = plan
            .user_files
            .iter()
            .enumerate()
            .map(|(ix, f)| {
                let chosen = self.picked.get(&f.file_id);
                let id = f.file_id.clone();
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .min_w_0()
                                    .child(f.prompt.clone().unwrap_or_else(|| f.file_id.clone())),
                            )
                            .child(
                                muted(
                                    chosen
                                        .map(|p| SharedString::from(p.display().to_string()))
                                        .unwrap_or_else(|| t("library.lutris.files.not_chosen")),
                                    cx,
                                )
                                .truncate(),
                            ),
                    )
                    .child(
                        Button::new(ElementId::Name(format!("lutris-file-{ix}").into()))
                            .outline()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label(t("library.lutris.files.choose"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.pick_user_file(id.clone(), window, cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let downloads: Vec<AnyElement> = plan
            .downloads
            .iter()
            .map(|d| {
                let size = match d.size {
                    Some(size) => SharedString::from(format_bytes(size)),
                    None if self.probing => t("library.lutris.files.size_checking"),
                    None => t("library.lutris.files.size_unknown"),
                };
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Download)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(d.file_id.clone()))
                            .child(muted(d.host.clone(), cx).truncate()),
                    )
                    .child(muted(size, cx))
                    .into_any_element()
            })
            .collect();
        let steam: Vec<AnyElement> = plan
            .steam_files
            .iter()
            .map(|s| {
                muted(
                    t_args(
                        "library.lutris.files.steam",
                        &[("appid", &s.appid), ("path", &s.path)],
                    ),
                    cx,
                )
                .into_any_element()
            })
            .collect();
        v_flex()
            .gap_4()
            .when(!user_rows.is_empty(), |d| {
                d.child(
                    section(t("library.lutris.files.yours"), cx)
                        .child(muted(t("library.lutris.files.yours_hint"), cx))
                        .children(user_rows),
                )
            })
            .when(!downloads.is_empty(), |d| {
                d.child(section(t("library.lutris.files.downloads"), cx).children(downloads))
            })
            .when(!steam.is_empty(), |d| {
                d.child(section(t("library.lutris.files.steam_title"), cx).children(steam))
            })
            .into_any_element()
    }

    fn render_wine_line(&self, cx: &App) -> Option<AnyElement> {
        let pick = self.wine.as_ref()?;
        let text = match &pick.matched {
            Some(WineMatch::Close { requested, .. }) => t_args(
                "library.lutris.consent.wine_close",
                &[("requested", requested), ("build", &pick.build.name)],
            ),
            Some(WineMatch::NotAvailable { requested }) => t_args(
                "library.lutris.consent.wine_missing",
                &[("requested", requested), ("build", &pick.build.name)],
            ),
            _ => t_args(
                "library.lutris.consent.wine",
                &[("build", &pick.build.name)],
            ),
        };
        Some(muted(text, cx).into_any_element())
    }

    fn render_consent(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(plan) = &self.consent else {
            return div().into_any_element();
        };
        let total: u64 = plan.downloads.iter().filter_map(|d| d.size).sum();
        let mut body = v_flex()
            .gap_4()
            .child(div().text_sm().child(t("library.lutris.consent.intro")));
        if !plan.downloads.is_empty() {
            let hosts: Vec<String> = {
                let mut h: Vec<String> = plan.downloads.iter().map(|d| d.host.clone()).collect();
                h.sort();
                h.dedup();
                h
            };
            body = body.child(
                section(t("library.lutris.consent.downloads"), cx).child(muted(
                    t_args(
                        "library.lutris.consent.downloads_body",
                        &[
                            ("count", &plan.downloads.len().to_string()),
                            ("hosts", &hosts.join(", ")),
                            ("size", &format_bytes(total)),
                        ],
                    ),
                    cx,
                )),
            );
        }
        if !plan.outside_writes.is_empty() {
            let rows: Vec<AnyElement> = plan
                .outside_writes
                .iter()
                .enumerate()
                .map(|(ix, w)| {
                    let label = t_args(
                        "library.lutris.consent.write",
                        &[("path", &w.path), ("command", &w.command)],
                    );
                    if w.resolved.is_some() {
                        Checkbox::new(ElementId::Name(format!("lutris-write-{ix}").into()))
                            .label(label)
                            .checked(self.checks.outside.get(ix).copied().unwrap_or(false))
                            .on_click(cx.listener(move |this, v: &bool, _, cx| {
                                if let Some(b) = this.checks.outside.get_mut(ix) {
                                    *b = *v;
                                }
                                cx.notify();
                            }))
                            .into_any_element()
                    } else {
                        v_flex()
                            .child(div().text_sm().min_w_0().child(label))
                            .child(muted(t("library.lutris.consent.write_later"), cx))
                            .into_any_element()
                    }
                })
                .collect();
            body = body.child(
                section(t("library.lutris.consent.writes"), cx)
                    .child(muted(t("library.lutris.consent.writes_hint"), cx))
                    .children(rows),
            );
        }
        if !plan.programs.is_empty() {
            let lines: Vec<String> = plan
                .programs
                .iter()
                .map(|p| {
                    let kind = match p.kind {
                        ProgramKind::Native => t("library.lutris.consent.kind_program"),
                        ProgramKind::Wine => "Wine".into(),
                        ProgramKind::Dosbox => "DOSBox".into(),
                    };
                    format!("{}. [{kind}] {} {}", p.step, p.program, p.args)
                        .trim_end()
                        .to_string()
                })
                .collect();
            body = body.child(
                section(t("library.lutris.consent.programs"), cx).child(mono_block(
                    "lutris-programs",
                    lines,
                    cx,
                )),
            );
        }
        if !plan.registry.is_empty() {
            let lines = plan
                .registry
                .iter()
                .map(|r| format!("{}\\{} = {} ({})", r.path, r.key, r.value, r.kind.name()))
                .collect();
            body = body.child(
                section(t("library.lutris.consent.registry"), cx).child(mono_block(
                    "lutris-registry",
                    lines,
                    cx,
                )),
            );
        }
        if !plan.winetricks.is_empty() {
            let verbs: Vec<String> = plan
                .winetricks
                .iter()
                .flat_map(|w| w.verbs.clone())
                .collect();
            body = body.child(
                section(t("library.lutris.consent.winetricks"), cx).child(mono_block(
                    "lutris-winetricks",
                    vec![verbs.join(" ")],
                    cx,
                )),
            );
        }
        if let Some(a) = plan.autosetups.first() {
            body = body.child(Alert::warning(
                "lutris-autosetup",
                t_args(
                    "library.lutris.failed.store",
                    &[("store", autosetup_store_name(a.store))],
                ),
            ));
        }
        if plan.needs_shell_ack() {
            let lines = plan
                .shell_commands
                .iter()
                .flat_map(|c| {
                    let mut l = vec![format!(
                        "# {}",
                        t_args(
                            "library.lutris.consent.shell_step",
                            &[("step", &c.step.to_string())]
                        )
                    )];
                    l.extend(c.command.lines().map(str::to_string));
                    l
                })
                .collect();
            body = body.child(
                section(t("library.lutris.consent.shell"), cx)
                    .child(Alert::warning(
                        "lutris-shell-warning",
                        t("library.lutris.consent.shell_warning"),
                    ))
                    .child(mono_block("lutris-shell", lines, cx))
                    .child(
                        Checkbox::new("lutris-shell-ack")
                            .label(t("library.lutris.consent.shell_ack"))
                            .checked(self.checks.shell_acknowledged)
                            .on_click(cx.listener(|this, v: &bool, _, cx| {
                                this.checks.shell_acknowledged = *v;
                                cx.notify();
                            })),
                    ),
            );
        }
        body = body.children(self.render_wine_line(cx));
        v_flex()
            .id("lutris-consent")
            .max_h(rems(30.))
            .overflow_y_scroll()
            .child(body)
            .into_any_element()
    }

    fn render_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pending = self.pending.as_ref()?;
        let body = match pending {
            Pending::Menu {
                request, selected, ..
            } => v_flex()
                .gap_2()
                .child(
                    div().text_sm().font_medium().child(
                        request
                            .description
                            .clone()
                            .map(SharedString::from)
                            .unwrap_or_else(|| t("library.lutris.prompt.menu")),
                    ),
                )
                .child(
                    RadioGroup::vertical("lutris-menu")
                        .children(request.options.iter().map(|o| o.label.clone()))
                        .selected_index(*selected)
                        .on_change(cx.listener(|this, ix: &usize, _, cx| {
                            if let Some(Pending::Menu { selected, .. }) = &mut this.pending {
                                *selected = Some(*ix);
                            }
                            cx.notify();
                        })),
                )
                .child(
                    h_flex().justify_end().child(
                        Button::new("lutris-menu-continue")
                            .primary()
                            .small()
                            .label(t("library.lutris.prompt.continue"))
                            .disabled(selected.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.answer_menu(cx))),
                    ),
                ),
            Pending::File { request, .. } => v_flex()
                .gap_2()
                .child(
                    div().text_sm().font_medium().child(
                        request
                            .prompt
                            .clone()
                            .map(SharedString::from)
                            .unwrap_or_else(|| {
                                t_args("library.lutris.prompt.file", &[("file", &request.file_id)])
                            }),
                    ),
                )
                .when_some(request.filename.clone(), |d, name| {
                    d.child(muted(
                        t_args("library.lutris.prompt.expected", &[("name", &name)]),
                        cx,
                    ))
                })
                .child(self.prompt_buttons(cx)),
            Pending::Disc { request, retry, .. } => v_flex()
                .gap_2()
                .child(
                    div().text_sm().font_medium().child(
                        request
                            .message
                            .clone()
                            .map(SharedString::from)
                            .unwrap_or_else(|| t("library.lutris.prompt.disc")),
                    ),
                )
                .child(muted(
                    t_args(
                        "library.lutris.prompt.disc_needs",
                        &[("file", &request.requires)],
                    ),
                    cx,
                ))
                .when(*retry, |d| {
                    d.child(Alert::warning(
                        "lutris-disc-retry",
                        t_args(
                            "library.lutris.prompt.disc_retry",
                            &[("file", &request.requires)],
                        ),
                    ))
                })
                .child(self.prompt_buttons(cx)),
        };
        Some(
            div()
                .p_3()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().ring)
                .child(body)
                .into_any_element(),
        )
    }

    fn prompt_buttons(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex().justify_end().child(
            Button::new("lutris-prompt-choose")
                .primary()
                .small()
                .icon(IconName::FolderOpen)
                .label(t("library.lutris.prompt.choose"))
                .on_click(cx.listener(|this, _, window, cx| this.answer_path(window, cx))),
        )
    }

    fn render_progress(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(run) = &self.running else {
            return div().into_any_element();
        };
        let p = run.progress.lock().clone();
        let headline: SharedString = if p.rolling_back {
            t("library.lutris.progress.rolling_back")
        } else if self.finishing {
            t("library.lutris.progress.adding")
        } else {
            match &p.step {
                Some(step) => t_args(
                    "library.lutris.progress.step",
                    &[
                        ("step", &step.number.to_string()),
                        ("total", &step.total.to_string()),
                    ],
                ),
                None => t("library.lutris.progress.preparing"),
            }
        };
        let detail = p.step.as_ref().map(|s| {
            s.description
                .clone()
                .unwrap_or_else(|| s.command.to_string())
        });
        let download = p.download.clone();
        let log: Vec<String> = p.log.iter().cloned().collect();
        let log_file = run.log_file.display().to_string();
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(Spinner::new().small())
                    .child(div().text_sm().font_medium().min_w_0().child(headline)),
            )
            .when_some(detail.filter(|_| !p.rolling_back), |d, detail| {
                d.child(muted(detail, cx))
            })
            .when_some(download, |d, dl| {
                let percent = dl
                    .total
                    .filter(|t| *t > 0)
                    .map(|t| (dl.done as f32 / t as f32 * 100.).min(100.));
                let amount = match dl.total {
                    Some(total) => t_args(
                        "library.lutris.progress.download_of",
                        &[
                            ("file", &dl.file_id),
                            ("done", &format_bytes(dl.done)),
                            ("total", &format_bytes(total)),
                        ],
                    ),
                    None => t_args(
                        "library.lutris.progress.download",
                        &[("file", &dl.file_id), ("done", &format_bytes(dl.done))],
                    ),
                };
                d.child(
                    v_flex()
                        .gap_1()
                        .child(
                            Progress::new("lutris-download")
                                .value(percent.unwrap_or(0.))
                                .loading(percent.is_none()),
                        )
                        .child(muted(amount, cx)),
                )
            })
            .children(self.render_prompt(cx))
            .child(
                h_flex().child(
                    Button::new("lutris-details")
                        .ghost()
                        .small()
                        .icon(IconName::ScrollText)
                        .label(if self.show_details {
                            t("library.lutris.progress.hide_details")
                        } else {
                            t("library.lutris.progress.show_details")
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_details = !this.show_details;
                            cx.notify();
                        })),
                ),
            )
            .when(self.show_details, |d| {
                d.child(mono_block("lutris-log", log, cx)).child(muted(
                    t_args("library.lutris.progress.log_file", &[("path", &log_file)]),
                    cx,
                ))
            })
            .into_any_element()
    }

    fn render_done(&self, cx: &mut Context<Self>) -> AnyElement {
        let title = self
            .script()
            .map(|s| s.info.name.clone())
            .unwrap_or_default();
        let complete = self.done.as_ref().and_then(|d| d.1.clone());
        v_flex()
            .gap_2()
            .items_center()
            .py_4()
            .child(
                Icon::new(IconName::CircleCheck)
                    .large()
                    .text_color(cx.theme().success),
            )
            .child(
                div()
                    .text_lg()
                    .font_semibold()
                    .child(t_args("library.lutris.done.title", &[("title", &title)])),
            )
            .when_some(complete, |d, text| d.child(div().text_sm().child(text)))
            .into_any_element()
    }

    fn render_failed(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(failure) = &self.failure else {
            return div().into_any_element();
        };
        let alert = if failure.cancelled {
            Alert::info("lutris-failed", failure.message.clone())
                .title(t("library.lutris.failed.cancelled_title"))
        } else if failure.unfinished.is_some() {
            Alert::warning("lutris-failed", failure.message.clone())
                .title(t("library.lutris.failed.unfinished_title"))
        } else {
            Alert::error("lutris-failed", failure.message.clone())
                .title(t("library.lutris.failed.title"))
        };
        let log = self
            .running
            .as_ref()
            .map(|r| r.log_file.display().to_string());
        v_flex()
            .gap_3()
            .child(alert)
            .when(failure.unfinished.is_some(), |d| {
                d.child(
                    h_flex().child(
                        Button::new("lutris-choose-program")
                            .primary()
                            .icon(IconName::FolderOpen)
                            .label(t("library.lutris.failed.choose_program"))
                            .loading(self.finishing)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_program(window, cx)),
                            ),
                    ),
                )
            })
            .when(failure.store.is_some(), |d| {
                d.child(
                    h_flex().child(
                        Button::new("lutris-go-accounts")
                            .icon(IconName::LogIn)
                            .label(t("library.lutris.native.open_accounts"))
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                                navigate(Route::Accounts, cx);
                            }),
                    ),
                )
            })
            .when_some(log, |d, log| {
                d.child(muted(
                    t_args("library.lutris.progress.log_file", &[("path", &log)]),
                    cx,
                ))
            })
            .into_any_element()
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let step = self.step;
        let back = step.back(self.has_files()).filter(|_| step != Step::Failed);
        let next_label = match step {
            Step::Consent => t("library.lutris.consent.install"),
            _ => t("library.lutris.next"),
        };
        let can_next = match step {
            Step::Version => self.script().is_some_and(|s| refusal(&s).is_none()),
            Step::Destination => !text(&self.destination, cx).is_empty(),
            Step::Files => self
                .consent
                .as_ref()
                .is_some_and(|c| files_ready(c, &self.picked)),
            Step::Consent => self.consent.as_ref().is_some_and(|c| self.checks.ready(c)),
            _ => false,
        };
        h_flex()
            .gap_2()
            .justify_between()
            .child(h_flex().gap_2().when_some(back, |d, _| {
                d.child(
                    Button::new("lutris-back")
                        .ghost()
                        .icon(IconName::ArrowLeft)
                        .label(t("library.lutris.back"))
                        .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                )
            }))
            .child(
                h_flex()
                    .gap_2()
                    .when(step == Step::Progress, |d| {
                        d.child(
                            Button::new("lutris-cancel-run")
                                .danger()
                                .label(t("library.common.cancel"))
                                .disabled(self.finishing)
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                        )
                    })
                    .when(step != Step::Progress, |d| {
                        d.child(
                            Button::new("lutris-close")
                                .ghost()
                                .label(if step == Step::Done {
                                    t("library.lutris.close")
                                } else {
                                    t("library.common.cancel")
                                })
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                    })
                    .when(
                        matches!(
                            step,
                            Step::Version | Step::Destination | Step::Files | Step::Consent
                        ),
                        |d| {
                            d.child(
                                Button::new("lutris-next")
                                    .primary()
                                    .when(step == Step::Consent, |b| b.icon(IconName::Download))
                                    .label(next_label)
                                    .disabled(!can_next)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.next(window, cx)),
                                    ),
                            )
                        },
                    )
                    .when(step == Step::Failed && self.last_install.is_some(), |d| {
                        d.child(
                            Button::new("lutris-review")
                                .outline()
                                .label(t("library.lutris.failed.review"))
                                .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                        )
                        .child(
                            Button::new("lutris-retry")
                                .primary()
                                .icon(IconName::RotateCcw)
                                .label(t("library.lutris.failed.retry"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.retry(window, cx)),
                                ),
                        )
                    })
                    .when(step == Step::Done, |d| {
                        let id = self.done.as_ref().map(|d| d.0.clone());
                        d.child(
                            Button::new("lutris-open-game")
                                .outline()
                                .label(t("library.lutris.done.open"))
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    if let Some(id) = &id {
                                        navigate(Route::Game(id.clone()), cx);
                                    }
                                }),
                        )
                        .child(
                            Button::new("lutris-play")
                                .primary()
                                .icon(IconName::Play)
                                .label(t("library.lutris.done.play"))
                                .on_click(cx.listener(|this, _, window, cx| this.play(window, cx))),
                        )
                    }),
            )
    }
}

impl Render for LutrisDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let heading = match self.step {
            Step::Find => t("library.lutris.step.find"),
            Step::Version => t("library.lutris.step.version"),
            Step::Destination => t("library.lutris.step.destination"),
            Step::Files => t("library.lutris.step.files"),
            Step::Consent => t("library.lutris.step.consent"),
            Step::Progress => t("library.lutris.step.progress"),
            Step::Done => t("library.lutris.step.done"),
            Step::Failed => t("library.lutris.step.failed"),
        };
        let body = match self.step {
            Step::Find => self.render_find(cx),
            Step::Version => self.render_version(cx),
            Step::Destination => self.render_destination(cx),
            Step::Files => self.render_files(cx),
            Step::Consent => self.render_consent(cx),
            Step::Progress => self.render_progress(cx),
            Step::Done => self.render_done(cx),
            Step::Failed => self.render_failed(cx),
        };
        v_flex()
            .id("lutris-dialog")
            .gap_4()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(heading),
            )
            .child(body)
            .when_some(self.error.clone(), |d, e| {
                d.child(Alert::error("lutris-error", e))
            })
            .child(self.render_footer(cx))
    }
}

/// Open the Lutris script installer, at the scripts of the lutris.net game
/// `slug` when given (`irori://lutris/<slug>`, `lutris:<slug>`).
pub fn open_lutris_installer(slug: Option<String>, window: &mut Window, cx: &mut App) {
    crate::init(cx);
    let view = cx.new(|cx| {
        cx.on_release(|this: &mut LutrisDialog, _| {
            // Closing the window stops the install it started.
            if let Some(run) = &this.running
                && this.step == Step::Progress
            {
                run.cancel.cancel();
            }
        })
        .detach();
        LutrisDialog::new(slug, window, cx)
    });
    let width = dialog_width(window, 44.);
    let focus = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if focus.replace(false) {
            let input = view.read(cx).query.clone();
            input.focus_handle(cx).focus(window, cx);
        }
        dialog
            .title(t("library.lutris.title"))
            .w(width)
            .child(view.clone())
    });
}
