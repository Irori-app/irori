//! Lutris install scripts run with Ember's runtime (`docs/design/engines.md`
//! section 8): lutris.net search and script lists on request, the Wine
//! build a script gets, the [`ScriptHost`] that runs Wine, DOSBox and
//! programs for [`ember_scripts::run`], and what a finished install becomes
//! in the library.
//!
//! The UI drives an install through [`ScriptRun`]: progress to poll, the
//! script's questions to answer, a cancel token and the result. Applying the
//! result needs the sideload and ROM library stores, so services only
//! describe it ([`ScriptGame`]) and the caller adds it.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use ember_core::process::{self, CancellationToken, CommandSpec, RunOptions};
use ember_core::{
    AppSettings, EnvVar, GameId, GameSettings, LutrisScriptRecord, OpContext, Paths, Runner,
    WineInstallation, WineKind, async_trait,
};
use ember_runtime::Cancelled;
use ember_runtime::prefix::{self, WineCommand, WineContext};
use ember_scripts::model::{AutosetupStore, RegType};
use ember_scripts::{
    Approvals, AutosetupRequest, Context, DiscRequest, DosExecRequest, ExecRequest, FileRequest,
    GameResult, InstallerListing, LutrisClient, MenuRequest, Outcome, PrefixRequest, Progress,
    ProgressFn, RegistryRequest, Script, ScriptHost, ScriptRecord, ShellRequest, WineExecRequest,
    WineKillRequest, WineMatch, WinetricksRequest, match_wine_version,
};
use futures::channel::{mpsc, oneshot};
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::Backend;

/// Lines of the install log kept in memory for "Show details".
const LOG_LINES: usize = 400;

/// A script install the user approved.
#[derive(Debug, Clone)]
pub struct ScriptInstall {
    pub script: Arc<Script>,
    /// `$GAMEDIR`.
    pub game_dir: PathBuf,
    pub approvals: Approvals,
    /// Files the user chose before the install started, by file id.
    pub picked: BTreeMap<String, PathBuf>,
    /// The script's lutris.net page or local file.
    pub source_url: Option<String>,
    /// The desktop resolution, for `$RESOLUTION`.
    pub resolution: Option<(u32, u32)>,
}

/// A question the running script asks the user. Answering `None`, or
/// dropping the sender, cancels the install.
#[derive(Debug)]
pub enum ScriptPrompt {
    /// Choose a file the script needs (`N/A:<prompt>`).
    File {
        request: FileRequest,
        reply: oneshot::Sender<Option<PathBuf>>,
    },
    /// Choose one of the script's options (`input_menu`).
    Menu {
        request: MenuRequest,
        reply: oneshot::Sender<Option<String>>,
    },
    /// Insert a disc or choose the folder holding its files
    /// (`insert_disc`). `retry` is set when the last folder lacked
    /// `request.requires`.
    Disc {
        request: DiscRequest,
        retry: bool,
        reply: oneshot::Sender<Option<PathBuf>>,
    },
}

/// One step of the script, as it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptStep {
    pub number: usize,
    pub total: usize,
    pub command: &'static str,
    pub description: Option<String>,
}

/// A file being downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptDownload {
    pub file_id: String,
    pub done: u64,
    pub total: Option<u64>,
    pub per_sec: u64,
}

/// Where a running install is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScriptProgress {
    pub step: Option<ScriptStep>,
    pub download: Option<ScriptDownload>,
    /// The install failed or was cancelled and is being undone.
    pub rolling_back: bool,
    /// The newest lines of the install log.
    pub log: VecDeque<String>,
    /// The Wine build the install uses, when it uses one.
    pub wine: Option<WinePick>,
}

/// A running install.
pub struct ScriptRun {
    /// Read it to draw progress; the install updates it.
    pub progress: Arc<Mutex<ScriptProgress>>,
    /// The script's questions, in order.
    pub prompts: mpsc::UnboundedReceiver<ScriptPrompt>,
    /// Cancel the install: the game folder is rolled back, downloads kept.
    pub cancel: CancellationToken,
    /// The outcome, or why the install stopped. An
    /// [`ember_scripts::UnfinishedInstall`] keeps the files and carries the
    /// outcome so the user can choose the game's program.
    pub result: oneshot::Receiver<Result<Outcome>>,
    /// The full log, with the output of every program the script ran.
    pub log_file: PathBuf,
}

/// The Wine build a script install uses.
#[derive(Debug, Clone, PartialEq)]
pub struct WinePick {
    pub build: WineInstallation,
    /// How the script's own build mapped; `None` when it names none.
    pub matched: Option<WineMatch>,
}

/// The build for a script asking for `requested`: the same or a close
/// build from `available`, else `default`, else the recommended one.
/// CrossOver builds are left out: they run bottles, not the script's prefix.
pub fn pick_wine(
    requested: Option<&str>,
    available: &[WineInstallation],
    default: Option<&WineInstallation>,
) -> Option<WinePick> {
    let usable: Vec<WineInstallation> = available
        .iter()
        .filter(|w| w.kind != WineKind::Crossover)
        .cloned()
        .collect();
    let fallback = || {
        default
            .filter(|d| d.kind != WineKind::Crossover && d.is_set())
            .cloned()
            .or_else(|| ember_runtime::recommended(&usable).cloned())
    };
    let Some(requested) = requested.map(str::trim).filter(|r| !r.is_empty()) else {
        return fallback().map(|build| WinePick {
            build,
            matched: None,
        });
    };
    let names: Vec<&str> = usable.iter().map(|w| w.name.as_str()).collect();
    let matched = match_wine_version(requested, &names);
    let named = match &matched {
        WineMatch::Exact(name)
        | WineMatch::Close {
            available: name, ..
        } => usable.iter().find(|w| &w.name == name).cloned(),
        WineMatch::NotAvailable { .. } => None,
    };
    named.or_else(fallback).map(|build| WinePick {
        build,
        matched: Some(matched),
    })
}

/// Whether `script` runs anything with Wine.
pub fn uses_wine(script: &Script) -> bool {
    use ember_scripts::{Command, Task};
    script.info.runner == "wine"
        || script.steps.iter().any(|s| {
            matches!(
                s.command,
                Command::Task(
                    Task::CreatePrefix(_)
                        | Task::WineExec(_)
                        | Task::Winetricks(_)
                        | Task::SetRegedit(_)
                        | Task::WineKill(_)
                )
            )
        })
}

/// Whether Ember can add games of the Lutris runner `runner` to its library.
pub fn runner_supported(runner: &str) -> bool {
    matches!(runner, "wine" | "linux") || ember_extras::lutris_import::is_emulator_runner(runner)
}

/// The store a script's autosetup belongs to, by Ember's name.
pub fn autosetup_store_name(store: AutosetupStore) -> &'static str {
    match store {
        AutosetupStore::Gog => Runner::Gog.store_name(),
        AutosetupStore::Amazon => Runner::Nile.store_name(),
        AutosetupStore::Zoom => Runner::Zoom.store_name(),
    }
}

/// A script tried to install its game through a store Ember has natively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallFromStore(pub AutosetupStore);

impl std::fmt::Display for InstallFromStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let store = autosetup_store_name(self.0);
        write!(
            f,
            "This script sets the game up through {store}. Install it from {store} in Ember instead"
        )
    }
}

impl std::error::Error for InstallFromStore {}

/// What a finished install becomes in the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptLibraryEntry {
    /// An added app: a Windows program (run with Wine outside Windows) or a
    /// native one.
    App {
        title: String,
        program: PathBuf,
        windows: bool,
        args: String,
        /// The game folder; uninstalling with files removes it.
        folder: PathBuf,
    },
    /// A game in the ROM library.
    Emulated {
        title: String,
        file: PathBuf,
        system: String,
    },
}

/// A finished install, ready to add.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptGame {
    pub entry: ScriptLibraryEntry,
    /// Per-game settings to set once the game is added, by settings key.
    pub settings: Vec<(&'static str, Value)>,
}

/// The `emberLutrisScript` record of an install.
pub fn script_record(record: &ScriptRecord) -> LutrisScriptRecord {
    LutrisScriptRecord {
        slug: record.slug.clone(),
        installer_slug: record.installer_slug.clone(),
        version: record.version.clone(),
        revision: record.revision.clone(),
        source_url: record.source_url.clone(),
    }
}

fn env_value(env: &BTreeMap<String, String>) -> Value {
    Value::Array(
        env.iter()
            .map(|(k, v)| json!({"key": k, "value": v}))
            .collect(),
    )
}

/// Describe what `outcome` becomes in the library. `wine` is the build the
/// install used; `program` replaces the game's program (the user's choice
/// after [`ember_scripts::ScriptError::MissingExecutable`]).
pub fn script_game(
    outcome: &Outcome,
    game_dir: &Path,
    wine: Option<&WineInstallation>,
    program: Option<&Path>,
) -> Result<ScriptGame> {
    let game = outcome.game.as_ref().with_context(|| {
        format!(
            "This script adds to “{}” instead of installing a game, which Ember doesn’t support",
            outcome.record.slug
        )
    })?;
    let record = serde_json::to_value(script_record(&outcome.record))?;
    let mut settings = Vec::new();
    let entry = match game.runner.as_str() {
        "wine" | "linux" => {
            let windows = game.runner == "wine";
            let program = program
                .map(Path::to_path_buf)
                .or_else(|| game.exe.clone())
                .or_else(|| game.main_file.clone())
                .ok_or(ember_scripts::ScriptError::MissingExecutable { expected: None })?;
            let mut env = game.env.clone();
            if windows {
                if let Some(prefix) = &game.prefix {
                    settings.push((
                        "winePrefix",
                        Value::String(prefix.to_string_lossy().into_owned()),
                    ));
                }
                if let Some(build) = wine {
                    settings.push(("wineVersion", serde_json::to_value(build)?));
                }
                if game.arch.as_deref() == Some("win32") {
                    env.entry("WINEARCH".into()).or_insert("win32".into());
                }
            }
            if !env.is_empty() {
                settings.push(("enviromentOptions", env_value(&env)));
            }
            ScriptLibraryEntry::App {
                title: game.name.clone(),
                program,
                windows,
                args: game.args.clone().unwrap_or_default(),
                folder: game_dir.to_path_buf(),
            }
        }
        runner => {
            let mut file = program
                .map(Path::to_path_buf)
                .or_else(|| game.rom.clone())
                .or_else(|| game.main_file.clone())
                .or_else(|| game.iso.clone())
                .or_else(|| game.path.clone())
                .or_else(|| game.exe.clone())
                .unwrap_or_else(|| game_dir.to_path_buf());
            // A DOSBox configuration is not a game file; the ROM library
            // finds the program in the game folder.
            if file
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("conf"))
            {
                file = game_dir.to_path_buf();
            }
            let option = |key: &str| {
                game.options.get(key).and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
            };
            let core = option("core");
            let platform = option("platform");
            let (system, engine) = match ember_extras::lutris_import::emulator_target(
                runner,
                &file,
                core.as_deref(),
                platform.as_deref(),
            ) {
                Some(Ok(target)) => target,
                Some(Err(reason)) => {
                    return Err(anyhow!(
                        "Ember can’t tell which system this {runner} game is for ({})",
                        reason.id()
                    ));
                }
                None => {
                    return Err(anyhow!(
                        "Ember can’t add games of the Lutris runner “{runner}” to the library"
                    ));
                }
            };
            anyhow::ensure!(
                ember_engines::systems::get(&system).is_some(),
                "Ember doesn’t support {system} games yet"
            );
            if ember_engines::EngineRef::parse(&engine)
                .is_some_and(|r| ember_engines::engine(&r.engine).is_some())
            {
                settings.push(("emberEngine", Value::String(engine)));
            }
            ScriptLibraryEntry::Emulated {
                title: game.name.clone(),
                file,
                system,
            }
        }
    };
    settings.push(("emberLutrisScript", record));
    Ok(ScriptGame { entry, settings })
}

/// The `.reg` file text (without encoding) that sets one value.
pub fn reg_text(request: &RegistryRequest) -> Result<String> {
    let path = request.path.trim().trim_matches(['[', ']', '\\']);
    anyhow::ensure!(
        path.to_ascii_uppercase().starts_with("HKEY_") && !path.contains(['\n', '\r', ']']),
        "“{path}” is not a registry key"
    );
    let quote = |s: &str| -> Result<String> {
        anyhow::ensure!(
            !s.contains(['\n', '\r']),
            "registry values with line breaks are not supported"
        );
        Ok(format!(
            "\"{}\"",
            s.replace('\\', "\\\\").replace('"', "\\\"")
        ))
    };
    let name = if request.key.is_empty() {
        "@".to_string()
    } else {
        quote(&request.key)?
    };
    let value = request.value.trim();
    let hex_number = |max_digits: usize| -> Result<u64> {
        let digits = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
            .unwrap_or(value);
        // Lutris writes the value after `dword:` as it is, so scripts give
        // these numbers in hexadecimal.
        anyhow::ensure!(
            !digits.is_empty()
                && digits.len() <= max_digits
                && digits.bytes().all(|b| b.is_ascii_hexdigit()),
            "“{value}” is not a hexadecimal number"
        );
        Ok(u64::from_str_radix(digits, 16)?)
    };
    let bytes = |b: &[u8]| {
        b.iter()
            .map(|x| format!("{x:02x}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let hex_list = |v: &str| {
        let v = v.replace([' ', ','], "");
        (!v.is_empty() && v.len().is_multiple_of(2) && v.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| {
                v.as_bytes()
                    .chunks(2)
                    .map(|c| String::from_utf8_lossy(c).to_ascii_lowercase())
                    .collect::<Vec<_>>()
                    .join(",")
            })
    };
    let utf16 = |strings: &[&str], double: bool| {
        let mut out: Vec<u8> = Vec::new();
        for s in strings {
            for u in s.encode_utf16().chain([0]) {
                out.extend(u.to_le_bytes());
            }
        }
        if double {
            out.extend([0, 0]);
        }
        bytes(&out)
    };
    let data = match request.kind {
        RegType::Sz => quote(&request.value)?,
        RegType::Dword => format!("dword:{:08x}", hex_number(8)?),
        RegType::Qword => format!("hex(b):{}", bytes(&hex_number(16)?.to_le_bytes())),
        RegType::Binary => format!(
            "hex:{}",
            hex_list(value).with_context(|| format!("“{value}” is not a list of bytes"))?
        ),
        RegType::ExpandSz => format!(
            "hex(2):{}",
            hex_list(value).unwrap_or_else(|| utf16(&[&request.value], false))
        ),
        RegType::MultiSz => format!(
            "hex(7):{}",
            hex_list(value).unwrap_or_else(|| {
                let parts: Vec<&str> = request.value.split('\n').collect();
                utf16(&parts, true)
            })
        ),
    };
    Ok(format!(
        "Windows Registry Editor Version 5.00\r\n\r\n[{path}]\r\n{name}={data}\r\n"
    ))
}

/// A Unix path as Wine's `Z:` drive sees it.
fn wine_path(path: &Path) -> String {
    format!("Z:{}", path.to_string_lossy().replace('/', "\\"))
}

/// The folder under `root` matching `rel` with any letter case.
fn find_any_case(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut cur = root.to_path_buf();
    for part in rel
        .split(['/', '\\'])
        .filter(|p| !p.is_empty() && *p != ".")
    {
        let exact = cur.join(part);
        if exact.exists() {
            cur = exact;
            continue;
        }
        let lower = part.to_lowercase();
        cur = std::fs::read_dir(&cur)
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().to_lowercase() == lower)?
            .path();
    }
    Some(cur)
}

fn push_log(progress: &Mutex<ScriptProgress>, line: &str) {
    let mut p = progress.lock();
    for l in line.lines().filter(|l| !l.trim().is_empty()) {
        if p.log.len() == LOG_LINES {
            p.log.pop_front();
        }
        p.log.push_back(l.to_string());
    }
}

fn append_file(path: &Path, text: &str) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{text}");
    }
}

/// Where an install of `script` into `game_dir` runs: its cache, home and
/// the folders a rollback must never touch. The consent screen builds its
/// plan from the same context, so what it lists is what runs.
pub fn script_context(app: &AppSettings, script: &Script, game_dir: &Path) -> Context {
    let cache_dir = Paths::get()
        .cache
        .join("lutris-installers")
        .join(ember_core::paths::sanitize(&script.info.game_slug));
    let mut ctx = Context::new(game_dir, cache_dir, ember_core::paths::expand_home("~"));
    ctx.protected = ember_core::fs_guard::protected_dirs(app);
    ctx
}

/// Free bytes on the disk that would hold `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    crate::games::free_space(path)
}

static LUTRIS: OnceCell<Arc<LutrisClient>> = OnceCell::new();

/// The one lutris.net client, so its rate limit covers every request.
/// Call on the backend runtime.
fn lutris() -> Arc<LutrisClient> {
    LUTRIS
        .get_or_init(|| Arc::new(LutrisClient::new(Paths::get().cache.join("lutris"))))
        .clone()
}

impl Backend {
    /// Games on lutris.net matching `query` that have install scripts.
    /// Fetched only when the user searches; answers are cached for a day.
    pub async fn search_lutris(&self, query: String) -> Result<Vec<GameResult>> {
        lutris()
            .search(&query)
            .await
            .context("Could not search lutris.net")
    }

    /// The install scripts lutris.net has for the game `slug`.
    pub async fn lutris_installers(&self, slug: String) -> Result<Vec<InstallerListing>> {
        lutris()
            .installers(&slug)
            .await
            .context("Could not get the install scripts from lutris.net")
    }

    /// The Wine build an install of `script` would use, when it uses Wine.
    pub async fn script_wine(&self, script: &Script) -> Option<WinePick> {
        if cfg!(windows) || !uses_wine(script) {
            return None;
        }
        let app = self.settings.app();
        let available = ember_runtime::wine_installations(&app).await;
        let default = Some(&app.game.wine_version)
            .filter(|w| ember_runtime::discovery::is_usable(w))
            .cloned();
        pick_wine(script.wine_version(), &available, default.as_ref())
    }

    /// Start an approved install on the backend runtime.
    pub fn start_script_install(&self, install: ScriptInstall) -> ScriptRun {
        let (prompt_tx, prompts) = mpsc::unbounded();
        let (result_tx, result) = oneshot::channel();
        let cancel = CancellationToken::new();
        let progress = Arc::new(Mutex::new(ScriptProgress::default()));
        let log_file = Paths::get().logs_dir().join("lutris").join(format!(
            "{}.log",
            ember_core::paths::sanitize(&install.script.info.game_slug)
        ));
        let backend = self.clone();
        let (p, c, l) = (progress.clone(), cancel.clone(), log_file.clone());
        self.spawn(async move {
            let out = backend.run_script(install, prompt_tx, p, l, c).await;
            let _ = result_tx.send(out);
        });
        ScriptRun {
            progress,
            prompts,
            cancel,
            result,
            log_file,
        }
    }

    async fn run_script(
        &self,
        install: ScriptInstall,
        prompts: mpsc::UnboundedSender<ScriptPrompt>,
        progress: Arc<Mutex<ScriptProgress>>,
        log_file: PathBuf,
        cancel: CancellationToken,
    ) -> Result<Outcome> {
        let script = install.script.clone();
        if let Some(dir) = log_file.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .with_context(|| format!("Could not create {}", dir.display()))?;
        }
        tokio::fs::write(
            &log_file,
            format!(
                "Ember {} — Lutris script “{}” ({}) for {}\nSource: {}\nGame folder: {}\n\n",
                env!("CARGO_PKG_VERSION"),
                script.info.version,
                script.info.runner,
                script.info.name,
                install
                    .source_url
                    .clone()
                    .unwrap_or_else(|| script.info.page_url()),
                install.game_dir.display(),
            ),
        )
        .await
        .with_context(|| format!("Could not write {}", log_file.display()))?;

        let app = self.settings.app();
        let wine = self.script_wine(&script).await;
        progress.lock().wine = wine.clone();
        let mut ctx = script_context(&app, &script, &install.game_dir);
        ctx.source_url = install.source_url.clone();
        ctx.resolution = install.resolution;
        let cache_dir = ctx.cache_dir.clone();

        let host = Host {
            backend: self.clone(),
            defaults: app.game.clone(),
            app,
            wine: wine.map(|w| w.build),
            picked: install.picked.clone(),
            prompts,
            progress: progress.clone(),
            log_file: log_file.clone(),
            cache_dir,
            game_dir: install.game_dir.clone(),
        };
        let report: ProgressFn = {
            let progress = progress.clone();
            let log_file = log_file.clone();
            Arc::new(move |event| {
                let line = match &event {
                    Progress::Step {
                        number,
                        total,
                        command,
                        description,
                    } => Some(format!(
                        "Step {number} of {total}: {}",
                        description.as_deref().unwrap_or(command)
                    )),
                    Progress::RollingBack => Some("Undoing the install".to_string()),
                    Progress::Download { .. } => None,
                };
                {
                    let mut p = progress.lock();
                    match event {
                        Progress::Download {
                            file_id,
                            bytes_done,
                            bytes_total,
                            bytes_per_sec,
                        } => {
                            p.download = Some(ScriptDownload {
                                file_id,
                                done: bytes_done,
                                total: bytes_total,
                                per_sec: bytes_per_sec,
                            })
                        }
                        Progress::Step {
                            number,
                            total,
                            command,
                            description,
                        } => {
                            p.download = None;
                            p.step = Some(ScriptStep {
                                number,
                                total,
                                command,
                                description,
                            });
                        }
                        Progress::RollingBack => {
                            p.download = None;
                            p.rolling_back = true;
                        }
                    }
                }
                if let Some(line) = line {
                    push_log(&progress, &line);
                    append_file(&log_file, &line);
                }
            })
        };
        let result =
            ember_scripts::run(&script, &ctx, &host, &install.approvals, &cancel, report).await;
        if let Err(e) = &result {
            let line = format!("The install stopped: {e:#}");
            push_log(&progress, &line);
            append_file(&log_file, &line);
        }
        result
    }
}

/// Ember's side of a script install.
struct Host {
    backend: Backend,
    app: Arc<AppSettings>,
    /// The global game settings, the base for every Wine command.
    defaults: GameSettings,
    wine: Option<WineInstallation>,
    picked: BTreeMap<String, PathBuf>,
    prompts: mpsc::UnboundedSender<ScriptPrompt>,
    progress: Arc<Mutex<ScriptProgress>>,
    log_file: PathBuf,
    cache_dir: PathBuf,
    game_dir: PathBuf,
}

impl Host {
    fn log(&self, line: &str) {
        push_log(&self.progress, line);
        append_file(&self.log_file, line);
    }

    fn op(&self, cancel: &CancellationToken) -> OpContext {
        let progress = self.progress.clone();
        OpContext {
            progress: ember_core::ignore_progress(),
            cancel: cancel.clone(),
            step: Arc::new(move |s| push_log(&progress, s)),
            log_file: Some(self.log_file.clone()),
            settings: None,
        }
    }

    fn wine(&self) -> Result<WineInstallation> {
        if cfg!(windows) {
            anyhow::bail!(
                "This script runs Windows programs through Wine, which Ember uses on Linux and macOS only"
            );
        }
        self.wine.clone().context(
            "No Wine or Proton version was found. Download one in the Wine manager, then try again",
        )
    }

    /// The settings a Wine command runs with: the defaults, the chosen
    /// build and prefix, plus the script's variables.
    fn settings_for(
        &self,
        wine: &WineInstallation,
        prefix: &Path,
        env: &BTreeMap<String, String>,
        arch: Option<&str>,
        overrides: &BTreeMap<String, String>,
    ) -> GameSettings {
        let mut s = self.defaults.clone();
        s.wine_version = wine.clone();
        s.wine_prefix = prefix.to_string_lossy().into_owned();
        let mut vars: Vec<(String, String)> =
            env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        if let Some(arch) = arch.filter(|a| !a.is_empty()) {
            vars.push(("WINEARCH".into(), arch.to_string()));
        }
        if !overrides.is_empty() {
            let list: Vec<String> = overrides.iter().map(|(k, v)| format!("{k}={v}")).collect();
            vars.push(("WINEDLLOVERRIDES".into(), list.join(";")));
        }
        s.environment_options
            .retain(|e| !vars.iter().any(|(k, _)| k == &e.key));
        s.environment_options
            .extend(vars.into_iter().map(|(key, value)| EnvVar { key, value }));
        s
    }

    async fn context(
        &self,
        wine: WineInstallation,
        prefix: &Path,
        settings: &GameSettings,
        ctx: &OpContext,
    ) -> WineContext {
        let mut uses_umu = ember_runtime::runtimes::is_umu_supported(settings, false);
        let mut umu = PathBuf::new();
        if uses_umu {
            match ember_runtime::runtimes::ensure_umu(ctx).await {
                Ok(p) => umu = p,
                Err(e) => {
                    self.log(&format!("{e:#}; running Proton directly"));
                    uses_umu = false;
                }
            }
        }
        WineContext::new(wine, prefix.to_path_buf(), uses_umu, umu)
    }

    /// Run `argv` in `prefix` with Wine and wait for it.
    async fn run_wine(
        &self,
        argv: Vec<String>,
        cwd: Option<PathBuf>,
        prefix: &Path,
        settings: GameSettings,
        cancel: &CancellationToken,
    ) -> Result<i32> {
        let wine = self.wine()?;
        let ctx = self.op(cancel);
        let wc = self.context(wine, prefix, &settings, &ctx).await;
        let mut cmd = WineCommand::new(argv);
        cmd.cwd = cwd;
        cmd.wait = true;
        cmd.verb = "waitforexitandrun";
        cmd.game_id = "lutris-script".into();
        let res = prefix::run_wine(&self.app, &settings, &wc, cmd, &ctx).await?;
        if res.aborted {
            return Err(Cancelled.into());
        }
        Ok(res.code.unwrap_or(-1))
    }

    /// Run a program, logging its output.
    async fn run(&self, spec: CommandSpec, cancel: &CancellationToken) -> Result<i32> {
        let progress = self.progress.clone();
        let opts = RunOptions::default()
            .cancel(cancel.clone())
            .log_file(&self.log_file)
            .on_line(move |_, line| push_log(&progress, line));
        let program = spec.program.display().to_string();
        let res = process::run(&spec, opts)
            .await
            .with_context(|| format!("Could not start {program}"))?;
        if res.aborted {
            return Err(Cancelled.into());
        }
        Ok(res.code.unwrap_or(-1))
    }

    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Option<T>>) -> ScriptPrompt,
    ) -> Option<T> {
        let (tx, rx) = oneshot::channel();
        self.prompts.unbounded_send(make(tx)).ok()?;
        rx.await.ok().flatten()
    }
}

#[async_trait]
impl ScriptHost for Host {
    async fn pick_file(&self, request: &FileRequest) -> Result<Option<PathBuf>> {
        if let Some(p) = self.picked.get(&request.file_id) {
            return Ok(Some(p.clone()));
        }
        let request = request.clone();
        Ok(self
            .ask(|reply| ScriptPrompt::File { request, reply })
            .await)
    }

    async fn steam_file(
        &self,
        appid: &str,
        path: &str,
        _cancel: &CancellationToken,
    ) -> Result<PathBuf> {
        let id = GameId::new(Runner::Steam, appid);
        let dir = self
            .backend
            .game(&id)
            .filter(|g| g.is_installed)
            .and_then(|g| g.install_path())
            .with_context(|| {
                format!(
                    "This script needs files from the Steam game {appid}. Install it in Steam, then try again"
                )
            })?;
        let rel = path.trim_matches(['/', '\\']);
        Ok(if rel.is_empty() || rel == "." {
            dir
        } else {
            dir.join(rel)
        })
    }

    async fn choose(&self, menu: &MenuRequest) -> Result<Option<String>> {
        let request = menu.clone();
        Ok(self
            .ask(|reply| ScriptPrompt::Menu { request, reply })
            .await)
    }

    async fn insert_disc(&self, request: &DiscRequest) -> Result<Option<PathBuf>> {
        let mut retry = false;
        loop {
            let request_now = request.clone();
            let Some(root) = self
                .ask(|reply| ScriptPrompt::Disc {
                    request: request_now,
                    retry,
                    reply,
                })
                .await
            else {
                return Ok(None);
            };
            let (r, requires) = (root.clone(), request.requires.clone());
            let found = tokio::task::spawn_blocking(move || find_any_case(&r, &requires))
                .await
                .unwrap_or(None);
            if found.is_some() {
                self.log(&format!("Using the disc at {}", root.display()));
                return Ok(Some(root));
            }
            retry = true;
        }
    }

    async fn wine_bin(&self) -> Result<PathBuf> {
        let wine = self.wine()?;
        let bin = if wine.kind == WineKind::Proton {
            prefix::wine_inside_proton(&wine)
                .map(|w| w.bin)
                .context("This Proton build has no Wine program the script can use")?
        } else {
            wine.bin
        };
        Ok(PathBuf::from(bin))
    }

    async fn create_prefix(
        &self,
        request: &PrefixRequest,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let wine = self.wine()?;
        self.log(&format!(
            "Creating the Wine prefix {} with {}",
            request.prefix.display(),
            wine.name
        ));
        let settings = self.settings_for(
            &wine,
            &request.prefix,
            &BTreeMap::new(),
            request.arch.as_deref(),
            &request.overrides,
        );
        let ctx = self.op(cancel);
        let wc = self.context(wine, &request.prefix, &settings, &ctx).await;
        prefix::verify_prefix(&self.app, &settings, &wc, &ctx).await?;
        Ok(())
    }

    async fn wineexec(&self, request: &WineExecRequest, cancel: &CancellationToken) -> Result<i32> {
        let wine = self.wine()?;
        self.log(&format!(
            "Running {} with {}",
            request.executable.display(),
            wine.name
        ));
        let settings = self.settings_for(
            &wine,
            &request.prefix,
            &request.env,
            request.arch.as_deref(),
            &request.overrides,
        );
        let mut argv = vec![request.executable.to_string_lossy().into_owned()];
        argv.extend(request.args.iter().cloned());
        let cwd = request.working_dir.clone().or_else(|| {
            request
                .executable
                .parent()
                .filter(|p| p.is_absolute())
                .map(Path::to_path_buf)
        });
        self.run_wine(argv, cwd, &request.prefix, settings, cancel)
            .await
    }

    async fn winetricks(
        &self,
        request: &WinetricksRequest,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let wine = self.wine()?;
        self.log(&format!("Running winetricks {}", request.verbs.join(" ")));
        let settings = self.settings_for(
            &wine,
            &request.prefix,
            &request.env,
            request.arch.as_deref(),
            &BTreeMap::new(),
        );
        let ctx = self.op(cancel);
        let wc = self.context(wine, &request.prefix, &settings, &ctx).await;
        prefix::verify_prefix(&self.app, &settings, &wc, &ctx).await?;
        let res = ember_runtime::tools::run_winetricks(
            &self.app,
            &settings,
            &wc,
            "lutris-script",
            &request.verbs,
            &ctx,
        )
        .await?;
        if res.aborted {
            return Err(Cancelled.into());
        }
        res.check().map(|_| ())
    }

    async fn set_regedit(
        &self,
        request: &RegistryRequest,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let wine = self.wine()?;
        let text = reg_text(request)?;
        self.log(&format!(
            "Setting {}\\{} in the registry",
            request.path, request.key
        ));
        let mut data = vec![0xff, 0xfe];
        for u in text.encode_utf16() {
            data.extend(u.to_le_bytes());
        }
        tokio::fs::create_dir_all(&self.cache_dir).await?;
        let file = self.cache_dir.join("ember-registry.reg");
        tokio::fs::write(&file, data)
            .await
            .with_context(|| format!("Could not write {}", file.display()))?;
        let settings = self.settings_for(
            &wine,
            &request.prefix,
            &BTreeMap::new(),
            request.arch.as_deref(),
            &BTreeMap::new(),
        );
        let argv = vec!["regedit".into(), "/S".into(), wine_path(&file)];
        let code = self
            .run_wine(argv, None, &request.prefix, settings, cancel)
            .await;
        let _ = tokio::fs::remove_file(&file).await;
        match code? {
            0 => Ok(()),
            code => Err(anyhow!("The registry editor exited with code {code}")),
        }
    }

    async fn winekill(&self, request: &WineKillRequest, _cancel: &CancellationToken) -> Result<()> {
        match &self.wine {
            Some(wine) if !cfg!(windows) => prefix::kill(wine, &request.prefix).await,
            _ => Ok(()),
        }
    }

    async fn dosexec(&self, request: &DosExecRequest, cancel: &CancellationToken) -> Result<i32> {
        const DOSBOX: &str = "dosbox-staging";
        let manifest = ember_engines::engine(DOSBOX).context("DOSBox Staging is not available")?;
        let state = tokio::task::spawn_blocking(ember_engines::state::load)
            .await
            .context("looking for DOSBox stopped")?;
        let m = manifest.clone();
        let installs = tokio::task::spawn_blocking(move || {
            ember_engines::discovery::find(&m, &ember_engines::state::load())
        })
        .await
        .context("looking for DOSBox stopped")?;
        let install = ember_engines::launch::resolve(
            &manifest,
            &installs,
            None,
            state.preferred.get(DOSBOX).map(String::as_str),
        )
        .map_err(anyhow::Error::from)
        .context("Install DOSBox Staging on the Emulators page, then try again")?;

        let mut args: Vec<String> = Vec::new();
        if let Some(conf) = &request.config_file {
            args.extend(["-conf".into(), conf.to_string_lossy().into_owned()]);
        }
        if let Some(exe) = &request.executable {
            args.push(exe.to_string_lossy().into_owned());
        }
        args.extend(request.args.iter().cloned());
        if request.exit {
            args.push("-exit".into());
        }
        let cwd = request
            .working_dir
            .clone()
            .or_else(|| {
                request
                    .executable
                    .as_deref()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
            })
            .unwrap_or_else(|| self.game_dir.clone());
        let spec = match &install.flatpak {
            Some(id) => CommandSpec::new("flatpak")
                .arg("run")
                .arg(format!("--filesystem={}", self.game_dir.display()))
                .arg(format!("--filesystem={}", cwd.display()))
                .arg(id)
                .args(args),
            None if install.program.extension().is_some_and(|e| e == "app") => {
                CommandSpec::new(ember_core::platform::bundle_executable(&install.program)?)
                    .args(args)
            }
            None => CommandSpec::new(&install.program).args(args),
        }
        .cwd(cwd);
        let shown: Vec<String> = spec
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        self.log(&format!("Running DOSBox Staging {}", shown.join(" ")));
        self.run(spec, cancel).await
    }

    async fn execute(&self, request: &ExecRequest, cancel: &CancellationToken) -> Result<i32> {
        self.log(&format!("Running {}", request.program.display()));
        let spec = CommandSpec::new(&request.program)
            .args(request.args.iter().cloned())
            .envs(request.env.clone())
            .cwd(&request.working_dir);
        self.run(spec, cancel).await
    }

    async fn run_shell(&self, request: &ShellRequest, cancel: &CancellationToken) -> Result<i32> {
        self.log(&format!("Running the shell command: {}", request.command));
        let spec = if cfg!(windows) {
            CommandSpec::new("cmd").args(["/C", request.command.as_str()])
        } else {
            CommandSpec::new("sh").args(["-c", request.command.as_str()])
        }
        .envs(request.env.clone())
        .cwd(&request.working_dir);
        self.run(spec, cancel).await
    }

    async fn store_autosetup(
        &self,
        request: &AutosetupRequest,
        _cancel: &CancellationToken,
    ) -> Result<()> {
        Err(InstallFromStore(request.store).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_scripts::{GameEntry, Script};
    use serde_json::Map;

    fn build(name: &str, kind: WineKind) -> WineInstallation {
        WineInstallation {
            bin: format!("/w/{name}/bin/wine"),
            name: name.into(),
            kind,
            ..Default::default()
        }
    }

    #[test]
    fn wine_builds_follow_the_script_then_the_default() {
        let available = vec![
            build("GE-Proton8-26", WineKind::Proton),
            build("Wine 9.0", WineKind::Wine),
            build("CrossOver 24", WineKind::Crossover),
        ];
        let default = build("Wine 9.0", WineKind::Wine);
        let pick = pick_wine(Some("wine-ge-8-26"), &available, Some(&default)).unwrap();
        assert_eq!(pick.build.name, "GE-Proton8-26");
        assert!(matches!(pick.matched, Some(WineMatch::Close { .. })));

        let pick = pick_wine(Some("lutris-7.2-x86_64"), &available, Some(&default)).unwrap();
        assert_eq!(pick.build.name, "Wine 9.0");
        assert!(matches!(pick.matched, Some(WineMatch::NotAvailable { .. })));

        let pick = pick_wine(None, &available, Some(&default)).unwrap();
        assert_eq!((pick.build.name.as_str(), pick.matched), ("Wine 9.0", None));

        // CrossOver runs bottles, never the script's prefix.
        let cx = [build("CrossOver 24", WineKind::Crossover)];
        assert_eq!(pick_wine(Some("CrossOver 24"), &cx, cx.first()), None);
    }

    fn outcome(runner: &str, f: impl FnOnce(&mut GameEntry)) -> Outcome {
        let mut game = GameEntry {
            name: "Example".into(),
            game_slug: "example".into(),
            runner: runner.into(),
            exe: None,
            main_file: None,
            iso: None,
            rom: None,
            path: None,
            args: None,
            working_dir: None,
            prefix: None,
            arch: None,
            wine_version: None,
            env: BTreeMap::new(),
            options: Map::new(),
            runner_options: Map::new(),
        };
        f(&mut game);
        Outcome {
            game: Some(game),
            record: ScriptRecord {
                slug: "example".into(),
                installer_slug: Some("example-setup".into()),
                version: "Setup".into(),
                revision: Some("1234".into()),
                updated_at: None,
                source_url: Some("https://lutris.net/games/example/".into()),
            },
            complete_text: None,
            files: BTreeMap::new(),
        }
    }

    fn setting<'a>(game: &'a ScriptGame, key: &str) -> Option<&'a Value> {
        game.settings
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    #[test]
    fn wine_games_become_windows_apps_with_their_prefix_and_build() {
        let dir = Path::new("/games/example");
        let out = outcome("wine", |g| {
            g.exe = Some(dir.join("drive_c/game.exe"));
            g.prefix = Some(dir.to_path_buf());
            g.args = Some("-windowed".into());
            g.arch = Some("win32".into());
            g.env.insert("DXVK_HUD".into(), "fps".into());
        });
        let wine = build("Wine 9.0", WineKind::Wine);
        let game = script_game(&out, dir, Some(&wine), None).unwrap();
        assert_eq!(
            game.entry,
            ScriptLibraryEntry::App {
                title: "Example".into(),
                program: dir.join("drive_c/game.exe"),
                windows: true,
                args: "-windowed".into(),
                folder: dir.to_path_buf(),
            }
        );
        assert_eq!(setting(&game, "winePrefix"), Some(&json!("/games/example")));
        assert_eq!(
            setting(&game, "wineVersion").and_then(|v| v.get("name")),
            Some(&json!("Wine 9.0"))
        );
        assert_eq!(
            setting(&game, "enviromentOptions"),
            Some(&json!([
                {"key": "DXVK_HUD", "value": "fps"},
                {"key": "WINEARCH", "value": "win32"}
            ]))
        );
        let record = setting(&game, "emberLutrisScript").unwrap();
        assert_eq!(
            record,
            &json!({
                "slug": "example",
                "installerSlug": "example-setup",
                "version": "Setup",
                "revision": "1234",
                "sourceUrl": "https://lutris.net/games/example/"
            })
        );
        // The record reads back as the setting it is stored as.
        let parsed: LutrisScriptRecord = serde_json::from_value(record.clone()).unwrap();
        assert_eq!(parsed.revision.as_deref(), Some("1234"));
    }

    #[test]
    fn a_chosen_program_finishes_an_install_that_named_none() {
        let dir = Path::new("/games/open");
        let out = outcome("linux", |_| {});
        assert!(script_game(&out, dir, None, None).is_err());
        let game = script_game(&out, dir, None, Some(&dir.join("start.sh"))).unwrap();
        assert!(matches!(
            game.entry,
            ScriptLibraryEntry::App { windows: false, ref program, .. } if program == &dir.join("start.sh")
        ));
        assert_eq!(setting(&game, "winePrefix"), None);
    }

    #[test]
    fn emulator_games_join_the_rom_library_with_their_engine() {
        let dir = Path::new("/games/quake");
        let out = outcome("dosbox", |g| g.main_file = Some(dir.join("dosbox.conf")));
        let game = script_game(&out, dir, None, None).unwrap();
        assert_eq!(
            game.entry,
            ScriptLibraryEntry::Emulated {
                title: "Example".into(),
                file: dir.to_path_buf(),
                system: "dos".into(),
            }
        );
        assert_eq!(
            setting(&game, "emberEngine"),
            Some(&json!("dosbox-staging"))
        );

        let out = outcome("libretro", |g| {
            g.main_file = Some(PathBuf::from("/roms/game.sfc"));
            g.options.insert("core".into(), json!("snes9x"));
        });
        let game = script_game(&out, Path::new("/roms"), None, None).unwrap();
        assert!(matches!(
            game.entry,
            ScriptLibraryEntry::Emulated { ref system, .. } if system == "snes"
        ));

        let out = outcome("steam", |g| g.exe = Some(PathBuf::from("/x")));
        assert!(script_game(&out, dir, None, None).is_err());
        assert!(!runner_supported("steam"));
        assert!(runner_supported("dosbox") && runner_supported("wine"));
    }

    fn reg(kind: RegType, value: &str) -> Result<String> {
        reg_text(&RegistryRequest {
            path: "HKEY_CURRENT_USER\\Software\\Wine\\Direct3D".into(),
            key: "MaxVersionGL".into(),
            value: value.into(),
            kind,
            prefix: PathBuf::from("/p"),
            arch: None,
        })
    }

    #[test]
    fn registry_values_are_written_as_regedit_reads_them() {
        let text = reg(RegType::Dword, "30002").unwrap();
        assert_eq!(
            text,
            "Windows Registry Editor Version 5.00\r\n\r\n[HKEY_CURRENT_USER\\Software\\Wine\\Direct3D]\r\n\"MaxVersionGL\"=dword:00030002\r\n"
        );
        assert!(
            reg(RegType::Sz, r#"C:\a "b""#)
                .unwrap()
                .contains(r#""MaxVersionGL"="C:\\a \"b\"""#)
        );
        assert!(
            reg(RegType::Binary, "01 ff")
                .unwrap()
                .contains("=hex:01,ff")
        );
        assert!(
            reg(RegType::ExpandSz, "A")
                .unwrap()
                .contains("=hex(2):41,00,00,00")
        );
        assert!(
            reg(RegType::Qword, "1")
                .unwrap()
                .contains("=hex(b):01,00,00,00,00,00,00,00")
        );
        assert!(reg(RegType::Dword, "zz").is_err());
        assert!(reg(RegType::Sz, "a\nb").is_err());
        let bad = reg_text(&RegistryRequest {
            path: "Software\\x".into(),
            key: String::new(),
            value: "1".into(),
            kind: RegType::Sz,
            prefix: PathBuf::from("/p"),
            arch: None,
        });
        assert!(bad.is_err());
        assert_eq!(wine_path(Path::new("/tmp/a.reg")), "Z:\\tmp\\a.reg");
    }

    #[test]
    fn wine_scripts_are_told_apart() {
        let text = "name: X\ngame_slug: x\nversion: Linux\nslug: x-linux\nrunner: linux\nscript:\n  game:\n    exe: x\n  installer:\n  - mkdir: $GAMEDIR/a\n";
        let script = Script::parse(text).unwrap();
        assert!(!uses_wine(&script));
        let script = Script::parse(&text.replace("runner: linux", "runner: wine")).unwrap();
        assert!(uses_wine(&script));
    }

    #[test]
    fn discs_are_found_in_any_letter_case() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("SETUP")).unwrap();
        std::fs::write(tmp.path().join("SETUP/Install.EXE"), b"").unwrap();
        assert!(find_any_case(tmp.path(), "setup/install.exe").is_some());
        assert!(find_any_case(tmp.path(), "setup/missing.exe").is_none());
    }
}
