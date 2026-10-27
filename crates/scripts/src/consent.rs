//! The consent plan: everything a script will do that the user must see
//! before it runs, built from the parsed script alone. Shell commands are
//! listed verbatim and need their own acknowledgement; writes outside the
//! game folder, the cache and the prefix are refused unless listed here and
//! approved.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::context::{Context, base_vars};
use crate::fetch::file_name_for;
use crate::fsops::lexical;
use crate::model::*;
use crate::vars::{Mode, Vars, references};

/// A file the script downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub file_id: String,
    pub url: String,
    pub host: String,
    /// Bytes, when the script or the server says.
    pub size: Option<u64>,
}

/// A file the user is asked to pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFile {
    pub file_id: String,
    pub prompt: Option<String>,
}

/// Files taken from an installed Steam game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamFile {
    pub file_id: String,
    pub appid: String,
    pub path: String,
}

/// A write outside the game folder, the cache and the prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutsideWrite {
    /// `None` for the Wine prefix named by the game section.
    pub step: Option<usize>,
    pub command: String,
    /// The path as the user should read it (choices not made yet appear as
    /// `<…>`).
    pub path: String,
    /// The exact path, when it does not depend on a later choice. Only these
    /// can be approved.
    pub resolved: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramKind {
    /// A program run directly (`execute` with `file`).
    Native,
    Wine,
    Dosbox,
}

/// A program the script runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramRun {
    pub step: usize,
    pub kind: ProgramKind,
    pub program: String,
    pub args: String,
}

/// A shell command line, exactly as the script wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    pub step: usize,
    /// Verbatim script text.
    pub command: String,
    /// With Irori's variables filled in.
    pub resolved: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryEdit {
    pub step: usize,
    pub path: String,
    pub key: String,
    pub value: String,
    pub kind: RegType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WinetricksUse {
    pub step: usize,
    pub verbs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutosetupUse {
    pub step: usize,
    pub store: AutosetupStore,
    pub id: String,
}

/// What the consent screen lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Consent {
    pub downloads: Vec<Download>,
    pub user_files: Vec<UserFile>,
    pub steam_files: Vec<SteamFile>,
    pub outside_writes: Vec<OutsideWrite>,
    pub programs: Vec<ProgramRun>,
    pub shell_commands: Vec<ShellCommand>,
    pub registry: Vec<RegistryEdit>,
    pub winetricks: Vec<WinetricksUse>,
    pub autosetups: Vec<AutosetupUse>,
}

impl Consent {
    /// Shell commands need an explicit "I understand" on top of consent.
    pub fn needs_shell_ack(&self) -> bool {
        !self.shell_commands.is_empty()
    }

    /// Fill in download sizes the script does not state by asking each
    /// server (HEAD requests).
    pub async fn probe_sizes(&mut self) {
        for d in self.downloads.iter_mut().filter(|d| d.size.is_none()) {
            d.size = crate::fetch::probe_size(&d.url, None).await;
        }
    }
}

/// What the user approved on the consent screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Approvals {
    /// The user accepted the plan.
    pub accepted: bool,
    /// The user ticked "I understand" for the shell commands.
    pub shell_acknowledged: bool,
    /// Outside writes the user allowed (their [`OutsideWrite::resolved`]
    /// paths); writes inside these folders are allowed too.
    pub outside_writes: Vec<PathBuf>,
}

impl Approvals {
    /// Approve everything `consent` lists.
    pub fn all(consent: &Consent) -> Self {
        Self {
            accepted: true,
            shell_acknowledged: consent.needs_shell_ack(),
            outside_writes: consent
                .outside_writes
                .iter()
                .filter_map(|w| w.resolved.clone())
                .collect(),
        }
    }
}

const PLACEHOLDER_CHOICE: &str = "<your choice>";

/// Preview values: Irori's variables plus placeholders for what is only
/// known later. Returns the names that are placeholders.
fn preview_vars(script: &Script, ctx: &Context) -> (Vars, BTreeSet<String>) {
    let mut vars = base_vars(script, ctx);
    let mut pending = BTreeSet::new();
    let mut placeholder = |vars: &mut Vars, name: &str, value: &str| {
        vars.set(name, value);
        pending.insert(name.to_string());
    };
    placeholder(&mut vars, "DISC", "<disc>");
    placeholder(&mut vars, "INPUT", PLACEHOLDER_CHOICE);
    placeholder(&mut vars, "WINEBIN", "<wine>");
    for s in &script.steps {
        if let Command::InputMenu(InputMenu { id: Some(id), .. }) = &s.command {
            placeholder(&mut vars, &format!("INPUT_{id}"), PLACEHOLDER_CHOICE);
        }
    }
    for f in &script.files {
        match &f.source {
            FileSource::Url(url) => {
                let url = vars
                    .substitute(url, Mode::Strict, "")
                    .unwrap_or_else(|_| url.clone());
                let name = file_name_for(&url, f.filename.as_deref());
                let path = ctx.cache_dir.join(&f.id).join(name);
                vars.set_file(&f.id, &path.to_string_lossy());
            }
            FileSource::UserPick { .. } => {
                vars.set_file(&f.id, "<file you choose>");
                pending.insert(f.id.clone());
            }
            FileSource::Steam { appid, path } => {
                vars.set_file(&f.id, &format!("<Steam app {appid}>/{path}"));
                pending.insert(f.id.clone());
            }
        }
    }
    (vars, pending)
}

struct Preview<'a> {
    vars: Vars,
    pending: BTreeSet<String>,
    ctx: &'a Context,
    roots: Vec<PathBuf>,
}

impl Preview<'_> {
    fn text(&self, raw: &str, mode: Mode) -> String {
        self.vars
            .substitute(raw, mode, "")
            .unwrap_or_else(|_| raw.to_string())
    }

    fn reference(&self, raw: &str) -> String {
        self.vars
            .substitute_ref(raw, "")
            .unwrap_or_else(|_| raw.to_string())
    }

    /// Whether `raw` depends on something only known later.
    fn uncertain(&self, raw: &str) -> bool {
        let id = raw.trim().replace('-', "_");
        if self.pending.contains(&id) {
            return true;
        }
        let known = |n: &str| self.vars.is_known(n);
        references(raw, &known)
            .iter()
            .any(|n| self.pending.contains(n))
    }

    fn path(&self, raw: &str) -> PathBuf {
        let p = PathBuf::from(self.reference(raw));
        lexical(&if p.is_absolute() {
            p
        } else {
            self.ctx.game_dir.join(p)
        })
    }

    fn inside(&self, p: &Path) -> bool {
        self.roots.iter().any(|r| p.starts_with(r))
    }

    /// An [`OutsideWrite`] when writing to `raw` leaves the allowed folders.
    fn outside(&self, step: Option<usize>, command: &str, raw: &str) -> Option<OutsideWrite> {
        let path = self.path(raw);
        // A path that depends on a later choice is checked again when the
        // step runs, which refuses escapes; only paths starting elsewhere
        // are listed.
        if self.inside(&path) {
            return None;
        }
        let uncertain = self.uncertain(raw);
        Some(OutsideWrite {
            step,
            command: command.into(),
            path: path.display().to_string(),
            resolved: (!uncertain).then_some(path),
        })
    }
}

/// The consent plan for running `script` with `ctx`.
pub fn consent(script: &Script, ctx: &Context) -> Consent {
    let (vars, pending) = preview_vars(script, ctx);
    let mut roots = vec![lexical(&ctx.game_dir), lexical(&ctx.cache_dir)];
    roots.extend(ctx.prefix.as_deref().map(lexical));
    let p = Preview {
        vars,
        pending,
        ctx,
        roots,
    };
    let mut c = Consent::default();

    for f in &script.files {
        match &f.source {
            FileSource::Url(raw) => {
                let url = p.text(raw, Mode::Strict);
                let host = url::Url::parse(&url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_string))
                    .unwrap_or_default();
                c.downloads.push(Download {
                    file_id: f.id.clone(),
                    url,
                    host,
                    size: f.size,
                });
            }
            FileSource::UserPick { prompt } => c.user_files.push(UserFile {
                file_id: f.id.clone(),
                prompt: prompt.clone(),
            }),
            FileSource::Steam { appid, path } => c.steam_files.push(SteamFile {
                file_id: f.id.clone(),
                appid: appid.clone(),
                path: path.clone(),
            }),
        }
    }

    // The default prefix: Irori's, else the game section's, else the game
    // folder.
    let default_prefix = match (&ctx.prefix, &script.game.prefix) {
        (Some(_), _) | (None, None) => None,
        (None, Some(raw)) => Some(raw.as_str()),
    };
    if let Some(raw) = default_prefix
        && let Some(w) = p.outside(None, "Wine prefix", raw)
    {
        c.outside_writes.push(w);
    }

    for step in &script.steps {
        let n = step.number;
        let name = step.command.name();
        let mut write = |raw: &str| {
            if let Some(w) = p.outside(Some(n), name, raw) {
                c.outside_writes.push(w);
            }
        };
        match &step.command {
            Command::Chmodx { file } => write(file),
            Command::Extract(e) => write(e.dst.as_deref().unwrap_or("$GAMEDIR")),
            Command::Merge(cp) | Command::Copy(cp) => {
                write(&cp.dst);
                if cp.remove_source {
                    write(&cp.src);
                }
            }
            Command::Move(m) => write(&m.dst),
            Command::Rename { src, dst } => {
                write(src);
                write(dst);
            }
            Command::Mkdir { path } => write(path),
            Command::WriteFile { file, .. }
            | Command::WriteJson { file, .. }
            | Command::WriteConfig { file, .. }
            | Command::SubstituteVars { file } => write(file),
            Command::Task(Task::CreatePrefix(t)) => {
                if let Some(pfx) = &t.prefix {
                    write(pfx);
                }
            }
            Command::Task(Task::Winetricks(t)) => {
                if let Some(pfx) = &t.prefix {
                    write(pfx);
                }
            }
            Command::Task(Task::SetRegedit(t)) => {
                if let Some(pfx) = &t.prefix {
                    write(pfx);
                }
            }
            Command::Task(Task::WineExec(t)) => {
                if let Some(pfx) = &t.prefix {
                    write(pfx);
                }
            }
            _ => {}
        }
        match &step.command {
            Command::Execute(e) => match &e.program {
                Program::File(f) => c.programs.push(ProgramRun {
                    step: n,
                    kind: ProgramKind::Native,
                    program: p.reference(f),
                    args: p.text(&e.args, Mode::Strict),
                }),
                Program::Shell(cmd) => c.shell_commands.push(ShellCommand {
                    step: n,
                    command: cmd.clone(),
                    resolved: p.text(cmd, Mode::Shell),
                }),
            },
            Command::Task(Task::WineExec(t)) => c.programs.push(ProgramRun {
                step: n,
                kind: ProgramKind::Wine,
                program: p.reference(&t.executable),
                args: p.text(&t.args, Mode::Strict),
            }),
            Command::Task(Task::DosExec(t)) => c.programs.push(ProgramRun {
                step: n,
                kind: ProgramKind::Dosbox,
                program: t
                    .executable
                    .as_deref()
                    .or(t.config_file.as_deref())
                    .map(|s| p.reference(s))
                    .unwrap_or_default(),
                args: p.text(&t.args, Mode::Strict),
            }),
            Command::Task(Task::SetRegedit(t)) => c.registry.push(RegistryEdit {
                step: n,
                path: p.text(&t.path, Mode::Strict),
                key: p.text(&t.key, Mode::Strict),
                value: p.text(&t.value, Mode::Strict),
                kind: t.kind,
            }),
            Command::Task(Task::Winetricks(t)) => c.winetricks.push(WinetricksUse {
                step: n,
                verbs: crate::parse::split_words(&p.text(&t.app, Mode::Strict)),
            }),
            Command::StoreAutosetup(a) => c.autosetups.push(AutosetupUse {
                step: n,
                store: a.store,
                id: a.id.clone(),
            }),
            _ => {}
        }
    }
    c
}
