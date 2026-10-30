//! The executor: runs an approved script step by step. File commands are
//! carried out here, confined to the game folder, the cache, the prefix and
//! approved paths; Wine, programs, prompts and store installs go through
//! [`ScriptHost`]. A failure or cancel rolls the game folder back and keeps
//! downloads in the cache for a retry.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use irori_core::process::CancellationToken;
use irori_runtime::Cancelled;
use serde_json::{Map, Value};

use crate::archive;
use crate::archive::{glob, single_folder};
use crate::consent::{Approvals, consent};
use crate::context::*;
use crate::error::{ScriptError, UnfinishedInstall, step_loc};
use crate::fetch;
use crate::fsops::{self, Fs, Journal, Sandbox, lexical};
use crate::host::*;
use crate::model::*;
use crate::parse::{check_url, split_words};
use crate::validate::{check_verbs, texts};
use crate::vars::{Mode, Vars, references};

/// Run `script`. Nothing runs unless `approvals` accepts its
/// [`consent`] plan (and acknowledges its shell commands). On failure or
/// cancel the game folder is rolled back and the error returned; a cancel
/// is `irori_runtime::Cancelled` in the error chain. An install that only
/// lacks the game's program is not rolled back: it fails with
/// [`UnfinishedInstall`], which carries the outcome so the user can choose
/// the program.
pub async fn run(
    script: &Script,
    ctx: &Context,
    host: &dyn ScriptHost,
    approvals: &Approvals,
    cancel: &CancellationToken,
    progress: ProgressFn,
) -> Result<Outcome> {
    let plan = consent(script, ctx);
    if !approvals.accepted {
        return Err(ScriptError::NotApproved("the install plan was not accepted".into()).into());
    }
    if plan.needs_shell_ack() && !approvals.shell_acknowledged {
        return Err(
            ScriptError::NotApproved("the shell commands were not acknowledged".into()).into(),
        );
    }
    // Only outside writes the plan listed can be approved.
    let listed: BTreeSet<PathBuf> = plan
        .outside_writes
        .iter()
        .filter_map(|w| w.resolved.clone())
        .collect();
    let mut roots = vec![ctx.game_dir.clone(), ctx.cache_dir.clone()];
    roots.extend(ctx.prefix.clone());
    roots.extend(
        approvals
            .outside_writes
            .iter()
            .map(|p| lexical(p))
            .filter(|p| listed.contains(p)),
    );
    let sandbox = Sandbox::new(&roots).context("Could not check the install folders")?;
    let mut deletable_roots = vec![ctx.game_dir.clone()];
    deletable_roots.extend(ctx.prefix.clone());
    let deletable =
        Sandbox::new(&deletable_roots).context("Could not check the install folders")?;

    let mut runner = Runner {
        script,
        ctx,
        host,
        cancel,
        progress,
        vars: base_vars(script, ctx),
        fs: Arc::new(Mutex::new(Fs {
            sandbox,
            journal: Journal::new(&ctx.game_dir),
            home: ctx.home.clone(),
            protected: ctx.protected.clone(),
        })),
        deletable,
        files: BTreeMap::new(),
        wine_used: false,
    };
    match runner.run().await {
        Ok(outcome) => Ok(outcome),
        Err(e) if UnfinishedInstall::of(&e).is_some() => Err(e),
        Err(e) => {
            runner.rollback().await;
            Err(e)
        }
    }
}

struct Runner<'a> {
    script: &'a Script,
    ctx: &'a Context,
    host: &'a dyn ScriptHost,
    cancel: &'a CancellationToken,
    progress: ProgressFn,
    vars: Vars,
    fs: Arc<Mutex<Fs>>,
    /// Folders whose files `move` may take away (elsewhere it copies).
    deletable: Sandbox,
    files: BTreeMap<String, PathBuf>,
    wine_used: bool,
}

fn lock(fs: &Mutex<Fs>) -> std::sync::MutexGuard<'_, Fs> {
    fs.lock().unwrap_or_else(|e| e.into_inner())
}

impl Runner<'_> {
    /// Run a blocking file operation with the confined file system.
    async fn with_fs<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Fs) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let fs = self.fs.clone();
        tokio::task::spawn_blocking(move || f(&mut lock(&fs)))
            .await
            .context("A file operation stopped unexpectedly")?
    }

    fn check_cancel(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        Ok(())
    }

    fn text(&self, raw: &str, loc: &str) -> Result<String> {
        Ok(self.vars.substitute(raw, Mode::Strict, loc)?)
    }

    /// A path parameter: a file id or substituted text, relative paths
    /// taken from the game folder.
    fn path(&self, raw: &str, loc: &str) -> Result<PathBuf> {
        let s = self.vars.substitute_ref(raw, loc)?;
        let s = s.trim_end_matches(['/', '\\']);
        let p = PathBuf::from(if s.is_empty() { "/" } else { s });
        Ok(lexical(&if p.is_absolute() {
            p
        } else {
            self.ctx.game_dir.join(p)
        }))
    }

    fn opt_path(&self, raw: Option<&str>, loc: &str) -> Result<Option<PathBuf>> {
        raw.map(|r| self.path(r, loc)).transpose()
    }

    fn is_file_id(&self, raw: &str) -> bool {
        self.files.contains_key(&raw.trim().replace('-', "_"))
    }

    fn set_file(&mut self, id: &str, path: PathBuf) {
        self.vars.set_file(id, &path.to_string_lossy());
        self.files.insert(id.to_string(), path);
    }

    fn env(&self, extra: &BTreeMap<String, String>, loc: &str) -> Result<BTreeMap<String, String>> {
        let mut env = BTreeMap::new();
        for (k, v) in self.script.system_env.iter().chain(extra) {
            env.insert(k.clone(), self.text(v, loc)?);
        }
        Ok(env)
    }

    fn args(&self, raw: &str, loc: &str) -> Result<Vec<String>> {
        split_words(raw).iter().map(|a| self.text(a, loc)).collect()
    }

    fn check_code(code: i32, accepted: Option<i32>, loc: &str) -> Result<()> {
        if code == 0 || Some(code) == accepted {
            Ok(())
        } else {
            Err(ScriptError::ExitCode {
                location: loc.into(),
                code,
            }
            .into())
        }
    }

    /// The prefix a Wine task uses: its own, else Irori's, else the game
    /// section's, else the game folder. Checked as a write target.
    async fn prefix(&self, own: Option<&str>, loc: &str) -> Result<PathBuf> {
        let p = match (own, &self.ctx.prefix, &self.script.game.prefix) {
            (Some(raw), _, _) => self.path(raw, loc)?,
            (None, Some(p), _) => lexical(p),
            (None, None, Some(raw)) => self.path(raw, loc)?,
            (None, None, None) => lexical(&self.ctx.game_dir),
        };
        let loc = loc.to_string();
        self.with_fs(move |fs| Ok(fs.sandbox.check(&p, &loc)?))
            .await
    }

    fn arch(&self, own: Option<&str>, loc: &str) -> Result<Option<String>> {
        own.or(self.script.game.arch.as_deref())
            .map(|a| self.text(a, loc))
            .transpose()
    }

    /// Run a host step, recording what it creates in the game folder.
    async fn tracked<T>(&self, fut: impl std::future::Future<Output = Result<T>>) -> Result<T> {
        let before = self.with_fs(|fs| Ok(fs.journal.snapshot())).await?;
        let out = fut.await;
        self.with_fs(move |fs| {
            fs.journal.record_new_since(&before);
            Ok(())
        })
        .await?;
        out
    }

    async fn run(&mut self) -> Result<Outcome> {
        let game_dir = self.ctx.game_dir.clone();
        self.with_fs(move |fs| {
            fs.journal
                .create_dir_all(&game_dir)
                .with_context(|| format!("Could not create {}", game_dir.display()))
        })
        .await?;
        tokio::fs::create_dir_all(&self.ctx.cache_dir)
            .await
            .with_context(|| format!("Could not create {}", self.ctx.cache_dir.display()))?;

        let known = |n: &str| self.vars.is_known(n);
        let wants_wine = texts(self.script)
            .iter()
            .any(|(_, t, _)| references(t, &known).iter().any(|n| n == "WINEBIN"));
        if wants_wine {
            let bin = self.host.wine_bin().await?;
            self.vars.set("WINEBIN", bin.to_string_lossy());
        }

        self.fetch_files().await?;

        let total = self.script.steps.len();
        for step in &self.script.steps {
            self.check_cancel()?;
            (self.progress)(Progress::Step {
                number: step.number,
                total,
                command: step.command.name(),
                description: step.description.clone(),
            });
            self.step(step).await?;
        }
        self.check_cancel()?;
        let (game, missing) = self.game_entry().await?;
        let info = &self.script.info;
        let outcome = Outcome {
            game,
            record: ScriptRecord {
                slug: info.game_slug.clone(),
                installer_slug: info.installer_slug().map(str::to_string),
                version: info.version.clone(),
                revision: info
                    .revision
                    .clone()
                    .or_else(|| info.id.map(|i| i.to_string())),
                updated_at: info.updated_at.clone().or_else(|| info.created_at.clone()),
                source_url: self
                    .ctx
                    .source_url
                    .clone()
                    .or_else(|| Some(info.page_url())),
            },
            complete_text: self.script.install_complete_text.clone(),
            files: self.files.clone(),
        };
        match missing {
            Some(error) => Err(UnfinishedInstall {
                outcome: Box::new(outcome),
                error,
            }
            .into()),
            None => Ok(outcome),
        }
    }

    async fn fetch_files(&mut self) -> Result<()> {
        for f in &self.script.files {
            self.check_cancel()?;
            let loc = format!("File “{}”", f.id);
            let path = match &f.source {
                FileSource::Url(raw) => {
                    let url = self.text(raw, &loc)?;
                    check_url(&f.id, &url)?;
                    let name = fetch::file_name_for(&url, f.filename.as_deref());
                    crate::parse::check_filename(&loc, &name)?;
                    let path = fetch::download(
                        &f.id,
                        &url,
                        &name,
                        f.referer.as_deref(),
                        &self.ctx.cache_dir,
                        self.cancel,
                        &self.progress,
                    )
                    .await?;
                    if let Some(c) = &f.checksum
                        && let Err(e) = fetch::verify(&path, c, &name).await
                    {
                        // A damaged download is useless for a retry.
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(e);
                    }
                    path
                }
                FileSource::UserPick { prompt } => {
                    let request = FileRequest {
                        file_id: f.id.clone(),
                        prompt: prompt.clone(),
                        filename: f.filename.clone(),
                        checksum: f.checksum.clone(),
                    };
                    let Some(path) = self.host.pick_file(&request).await? else {
                        return Err(Cancelled.into());
                    };
                    if let Some(c) = &f.checksum {
                        let label = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| f.id.clone());
                        fetch::verify(&path, c, &label).await?;
                    }
                    path
                }
                FileSource::Steam { appid, path } => {
                    self.host.steam_file(appid, path, self.cancel).await?
                }
            };
            self.set_file(&f.id, path);
        }
        Ok(())
    }

    async fn step(&mut self, step: &Step) -> Result<()> {
        let loc = step_loc(step.number, step.command.name());
        match &step.command {
            Command::Chmodx { file } => {
                let p = self.path(file, &loc)?;
                self.with_fs(move |fs| fs.chmodx(&p, &loc)).await
            }
            Command::Extract(e) => self.extract(e, &loc).await,
            Command::Merge(c) | Command::Copy(c) => self.copy(c, &loc).await,
            Command::Move(m) => {
                let src = self.path(&m.src, &loc)?;
                let dst = self.path(&m.dst, &loc)?;
                if tokio::fs::symlink_metadata(&src).await.is_err() {
                    return self.missing(m.optional, src, &loc);
                }
                let keep = !self.deletable.contains(&src);
                let l = loc.clone();
                let s = src.clone();
                let moved = self
                    .with_fs(move |fs| fs.move_path(&s, &dst, keep, &l))
                    .await?;
                if let Some(new) = moved
                    && self.is_file_id(&m.src)
                {
                    self.set_file(&m.src.trim().replace('-', "_"), new);
                }
                Ok(())
            }
            Command::Rename { src, dst } => {
                let (src, dst) = (self.path(src, &loc)?, self.path(dst, &loc)?);
                self.with_fs(move |fs| fs.rename(&src, &dst, &loc)).await
            }
            Command::Mkdir { path } => {
                let p = self.path(path, &loc)?;
                self.with_fs(move |fs| fs.mkdir(&p, &loc)).await
            }
            Command::WriteFile {
                file,
                content,
                append,
            } => {
                let p = self.path(file, &loc)?;
                let content = self.text(content, &loc)?;
                let append = *append;
                self.with_fs(move |fs| fs.write(&p, content.as_bytes(), append, &loc))
                    .await
            }
            Command::WriteJson { file, data, merge } => {
                let p = self.path(file, &loc)?;
                let (data, merge) = (data.clone(), *merge);
                self.with_fs(move |fs| {
                    let p = fs.sandbox.check(&p, &loc)?;
                    let mut base = if merge {
                        std::fs::read_to_string(&p)
                            .ok()
                            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                            .unwrap_or_else(|| Value::Object(Map::new()))
                    } else {
                        Value::Object(Map::new())
                    };
                    fsops::merge_json(&mut base, &data);
                    let text = serde_json::to_string_pretty(&base)?;
                    fs.write(&p, text.as_bytes(), false, &loc)
                })
                .await
            }
            Command::WriteConfig {
                file,
                entries,
                merge,
            } => {
                let p = self.path(file, &loc)?;
                let entries: Vec<ConfigEntry> = entries
                    .iter()
                    .map(|e| {
                        Ok(ConfigEntry {
                            value: self.text(&e.value, &loc)?,
                            ..e.clone()
                        })
                    })
                    .collect::<Result<_>>()?;
                let merge = *merge;
                self.with_fs(move |fs| {
                    let p = fs.sandbox.check(&p, &loc)?;
                    let mut text = if merge {
                        std::fs::read_to_string(&p).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    for e in &entries {
                        text = fsops::set_ini(&text, &e.section, &e.key, &e.value);
                    }
                    fs.write(&p, text.as_bytes(), false, &loc)
                })
                .await
            }
            Command::SubstituteVars { file } => {
                let p = self.path(file, &loc)?;
                let vars = self.vars.clone();
                self.with_fs(move |fs| {
                    let p = fs.sandbox.check(&p, &loc)?;
                    let text = std::fs::read_to_string(&p)
                        .with_context(|| format!("{loc}: could not read {}", p.display()))?;
                    let out = vars.substitute(&text, Mode::Lenient, &loc)?;
                    fs.write(&p, out.as_bytes(), false, &loc)
                })
                .await
            }
            Command::InputMenu(menu) => {
                let request = MenuRequest {
                    id: menu.id.clone(),
                    description: menu
                        .description
                        .as_deref()
                        .map(|d| self.text(d, &loc))
                        .transpose()?,
                    options: menu.options.clone(),
                    preselect: menu
                        .preselect
                        .as_deref()
                        .map(|d| self.text(d, &loc))
                        .transpose()?,
                };
                let Some(choice) = self.host.choose(&request).await? else {
                    return Err(Cancelled.into());
                };
                if !menu.options.iter().any(|o| o.value == choice) {
                    return Err(ScriptError::Invalid {
                        location: loc,
                        message: format!("“{choice}” is not one of the options"),
                    }
                    .into());
                }
                if let Some(id) = &menu.id {
                    self.vars.set(&format!("INPUT_{id}"), choice.clone());
                }
                self.vars.set("INPUT", choice);
                Ok(())
            }
            Command::InsertDisc { requires, message } => {
                let request = DiscRequest {
                    requires: self.text(requires, &loc)?,
                    message: message.as_deref().map(|m| self.text(m, &loc)).transpose()?,
                };
                let Some(root) = self.host.insert_disc(&request).await? else {
                    return Err(Cancelled.into());
                };
                let rel = PathBuf::from(request.requires.trim_start_matches(['/', '\\']));
                let r = root.clone();
                let found =
                    tokio::task::spawn_blocking(move || fsops::find_case_insensitive(&r, &rel))
                        .await?;
                if found.is_none() {
                    return Err(ScriptError::NotFound {
                        location: loc,
                        path: root.join(&request.requires),
                    }
                    .into());
                }
                self.vars.set("DISC", root.to_string_lossy());
                Ok(())
            }
            Command::Execute(e) => self.execute(e, &loc).await,
            Command::Task(t) => self.task(t, &loc).await,
            Command::StoreAutosetup(a) => {
                let request = AutosetupRequest {
                    store: a.store,
                    command: a.command,
                    id: a.id.clone(),
                    file: a.file.as_ref().and_then(|f| self.files.get(f).cloned()),
                    game_dir: self.ctx.game_dir.clone(),
                    silent: a.silent,
                    params: a.params.clone(),
                };
                self.tracked(self.host.store_autosetup(&request, self.cancel))
                    .await
            }
        }
    }

    fn missing(&self, optional: bool, path: PathBuf, loc: &str) -> Result<()> {
        if optional {
            return Ok(());
        }
        Err(ScriptError::NotFound {
            location: loc.into(),
            path,
        }
        .into())
    }

    async fn copy(&mut self, c: &CopyParams, loc: &str) -> Result<()> {
        let src = self.path(&c.src, loc)?;
        let dst = self.path(&c.dst, loc)?;
        let meta = match tokio::fs::metadata(&src).await {
            Ok(m) => m,
            Err(_) => return self.missing(c.optional, src, loc),
        };
        let l = loc.to_string();
        if meta.is_file() {
            let s = src.clone();
            let new = self
                .with_fs(move |fs| fs.copy_file_into(&s, &dst, &l))
                .await?;
            if self.is_file_id(&c.src) {
                self.set_file(&c.src.trim().replace('-', "_"), new);
            }
            return Ok(());
        }
        let remove = c.remove_source;
        self.with_fs(move |fs| {
            fs.merge_dir(&src, &dst, &l)?;
            if remove {
                fs.remove_tree(&src, &l)?;
            }
            Ok(())
        })
        .await
    }

    async fn extract(&mut self, e: &Extract, loc: &str) -> Result<()> {
        let src = self.path(&e.file, loc)?;
        let archives = tokio::task::spawn_blocking(move || glob(&src)).await?;
        if archives.is_empty() {
            let src = self.path(&e.file, loc)?;
            return Err(ScriptError::NotFound {
                location: loc.into(),
                path: src,
            }
            .into());
        }
        let dst = self.path(e.dst.as_deref().unwrap_or("$GAMEDIR"), loc)?;
        for (i, archive) in archives.into_iter().enumerate() {
            self.check_cancel()?;
            let staging = dst.join(format!(".irori-extract-{i}"));
            let (l, d, s) = (loc.to_string(), dst.clone(), staging.clone());
            self.with_fs(move |fs| {
                fs.mkdir(&d, &l)?;
                let _ = std::fs::remove_dir_all(&s);
                fs.mkdir(&s, &l)
            })
            .await?;
            let result = self.unpack(&archive, &staging, e.format.clone()).await;
            if let Err(err) = result {
                let s = staging.clone();
                let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(s)).await;
                return Err(err.context(format!(
                    "{loc}: could not extract {}",
                    archive
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                )));
            }
            let (l, d, merge_single) = (loc.to_string(), dst.clone(), e.merge_single);
            self.with_fs(move |fs| {
                let source = single_folder(&staging)
                    .filter(|_| merge_single)
                    .unwrap_or_else(|| staging.clone());
                let entries = std::fs::read_dir(&source)
                    .with_context(|| format!("{l}: could not read {}", source.display()))?;
                for entry in entries {
                    let entry = entry?;
                    fs.move_tree(&entry.path(), &d.join(entry.file_name()), &l)?;
                }
                let _ = std::fs::remove_dir_all(&staging);
                Ok(())
            })
            .await?;
        }
        Ok(())
    }

    async fn unpack(
        &self,
        archive: &Path,
        staging: &Path,
        format: Option<ArchiveFormat>,
    ) -> Result<()> {
        let format = match format {
            Some(f) => f,
            None => {
                let a = archive.to_path_buf();
                tokio::task::spawn_blocking(move || archive::detect(&a))
                    .await?
                    .with_context(|| format!("Could not read {}", archive.display()))?
            }
        };
        let (a, s, cancel) = (
            archive.to_path_buf(),
            staging.to_path_buf(),
            self.cancel.clone(),
        );
        match format {
            ArchiveFormat::Tar => {
                tokio::task::spawn_blocking(move || archive::extract_tar(&a, &s, &cancel)).await?
            }
            ArchiveFormat::Zip => {
                tokio::task::spawn_blocking(move || archive::extract_zip(&a, &s, &cancel)).await?
            }
            ArchiveFormat::Gzip => {
                tokio::task::spawn_blocking(move || archive::gunzip(&a, &s)).await?
            }
            ArchiveFormat::External(format) => {
                let request = ExtractRequest {
                    archive: a,
                    dst: s,
                    format,
                };
                self.host.extract_external(&request, self.cancel).await
            }
        }
    }

    async fn execute(&mut self, e: &Execute, loc: &str) -> Result<()> {
        let env = self.env(&e.env, loc)?;
        let working_dir = match self.opt_path(e.working_dir.as_deref(), loc)? {
            Some(d) if tokio::fs::metadata(&d).await.is_ok_and(|m| m.is_dir()) => d,
            _ => self.ctx.game_dir.clone(),
        };
        match &e.program {
            Program::Shell(cmd) => {
                let request = ShellRequest {
                    command: self.vars.substitute(cmd, Mode::Shell, loc)?,
                    working_dir,
                    env,
                    terminal: e.terminal,
                };
                let code = self
                    .tracked(self.host.run_shell(&request, self.cancel))
                    .await?;
                Self::check_code(code, e.return_code, loc)
            }
            Program::File(f) => {
                let raw = self.vars.substitute_ref(f, loc)?;
                let program = if self.is_file_id(f) || raw.contains(['/', '\\']) {
                    let p = self.path(f, loc)?;
                    if self.deletable.contains(&p) && p.is_file() {
                        let target = p.clone();
                        let l = loc.to_string();
                        self.with_fs(move |fs| fs.chmodx(&target, &l)).await?;
                    }
                    p
                } else {
                    PathBuf::from(raw)
                };
                let request = ExecRequest {
                    program,
                    args: self.args(&e.args, loc)?,
                    env,
                    working_dir,
                    terminal: e.terminal,
                    include_processes: e.include_processes.clone(),
                    exclude_processes: e.exclude_processes.clone(),
                    disable_runtime: e.disable_runtime,
                };
                let code = self
                    .tracked(self.host.execute(&request, self.cancel))
                    .await?;
                Self::check_code(code, e.return_code, loc)
            }
        }
    }

    async fn task(&mut self, t: &Task, loc: &str) -> Result<()> {
        match t {
            Task::CreatePrefix(p) => {
                self.wine_used = true;
                let request = PrefixRequest {
                    prefix: self.prefix(p.prefix.as_deref(), loc).await?,
                    arch: self.arch(p.arch.as_deref(), loc)?,
                    overrides: self.env_values(&p.overrides, loc)?,
                    install_gecko: p.install_gecko,
                    install_mono: p.install_mono,
                };
                self.tracked(self.host.create_prefix(&request, self.cancel))
                    .await
            }
            Task::WineExec(w) => {
                self.wine_used = true;
                let raw = self.vars.substitute_ref(&w.executable, loc)?;
                let executable = if self.is_file_id(&w.executable) || Path::new(&raw).is_absolute()
                {
                    self.path(&w.executable, loc)?
                } else {
                    PathBuf::from(raw)
                };
                let request = WineExecRequest {
                    executable,
                    args: self.args(&w.args, loc)?,
                    prefix: self.prefix(w.prefix.as_deref(), loc).await?,
                    arch: self.arch(w.arch.as_deref(), loc)?,
                    working_dir: self.opt_path(w.working_dir.as_deref(), loc)?,
                    blocking: w.blocking,
                    env: self.env(&w.env, loc)?,
                    overrides: self.env_values(&w.overrides, loc)?,
                    include_processes: w.include_processes.clone(),
                    exclude_processes: w.exclude_processes.clone(),
                    disable_runtime: w.disable_runtime,
                };
                let code = self
                    .tracked(self.host.wineexec(&request, self.cancel))
                    .await?;
                Self::check_code(code, w.return_code, loc)
            }
            Task::Winetricks(w) => {
                self.wine_used = true;
                let verbs = split_words(&self.text(&w.app, loc)?);
                check_verbs(&verbs, loc)?;
                let request = WinetricksRequest {
                    verbs,
                    prefix: self.prefix(w.prefix.as_deref(), loc).await?,
                    arch: self.arch(w.arch.as_deref(), loc)?,
                    silent: w.silent,
                    env: self.env(&BTreeMap::new(), loc)?,
                };
                self.tracked(self.host.winetricks(&request, self.cancel))
                    .await
            }
            Task::SetRegedit(r) => {
                self.wine_used = true;
                let request = RegistryRequest {
                    path: self.text(&r.path, loc)?,
                    key: self.text(&r.key, loc)?,
                    value: self.text(&r.value, loc)?,
                    kind: r.kind,
                    prefix: self.prefix(r.prefix.as_deref(), loc).await?,
                    arch: self.arch(r.arch.as_deref(), loc)?,
                };
                self.tracked(self.host.set_regedit(&request, self.cancel))
                    .await
            }
            Task::WineKill(k) => {
                let request = WineKillRequest {
                    prefix: self.prefix(k.prefix.as_deref(), loc).await?,
                    arch: self.arch(k.arch.as_deref(), loc)?,
                };
                self.host.winekill(&request, self.cancel).await
            }
            Task::DosExec(d) => {
                let request = DosExecRequest {
                    config_file: self.opt_path(d.config_file.as_deref(), loc)?,
                    executable: self.opt_path(d.executable.as_deref(), loc)?,
                    args: self.args(&d.args, loc)?,
                    exit: d.exit,
                    working_dir: self.opt_path(d.working_dir.as_deref(), loc)?,
                };
                let code = self
                    .tracked(self.host.dosexec(&request, self.cancel))
                    .await?;
                Self::check_code(code, None, loc)
            }
        }
    }

    fn env_values(
        &self,
        m: &BTreeMap<String, String>,
        loc: &str,
    ) -> Result<BTreeMap<String, String>> {
        m.iter()
            .map(|(k, v)| Ok((k.clone(), self.text(v, loc)?)))
            .collect()
    }

    /// What Play starts stays among what the install put in place: `exe:
    /// /bin/sh` would make a script's arguments a command run on every Play.
    fn check_game_program(&self, program: &Path) -> Result<()> {
        let program = lexical(program);
        let mut roots = vec![lexical(&self.ctx.game_dir), lexical(&self.ctx.cache_dir)];
        roots.extend(self.ctx.prefix.as_deref().map(lexical));
        if let Some(raw) = &self.script.game.prefix {
            roots.push(lexical(&self.path(raw, "Game section")?));
        }
        let inside = roots.iter().any(|r| program.starts_with(r))
            || self.files.values().any(|f| lexical(f) == program);
        if !inside {
            return Err(ScriptError::Invalid {
                location: "Game section".into(),
                message: format!(
                    "{} is outside the game folder, so the script was refused",
                    program.display()
                ),
            }
            .into());
        }
        Ok(())
    }

    /// The game section, resolved and checked: every entry point it names
    /// must exist. A missing one is returned beside the entry (left empty
    /// there) instead of failing, so the files are kept for the user to
    /// choose the program.
    async fn game_entry(&self) -> Result<(Option<GameEntry>, Option<ScriptError>)> {
        if self.script.extends.is_some() {
            return Ok((None, None));
        }
        let g = &self.script.game;
        let loc = "Game section";
        let mut resolved: [Option<PathBuf>; 5] = Default::default();
        let mut any = false;
        let mut missing = None;
        for (i, (slot, raw)) in resolved
            .iter_mut()
            .zip([&g.exe, &g.main_file, &g.iso, &g.rom, &g.path])
            .enumerate()
        {
            let Some(raw) = raw else { continue };
            any = true;
            if AUTO_EXE_MARKERS.contains(&raw.trim()) {
                missing.get_or_insert(ScriptError::MissingExecutable { expected: None });
                continue;
            }
            let text = self.vars.substitute_ref(raw, loc)?;
            if text.starts_with("http://") || text.starts_with("https://") {
                *slot = Some(PathBuf::from(text));
                continue;
            }
            let path = self.path(raw, loc)?;
            let game_dir = lexical(&self.ctx.game_dir);
            let found = tokio::task::spawn_blocking({
                let path = path.clone();
                move || {
                    if std::fs::symlink_metadata(&path).is_ok() {
                        return Some(path);
                    }
                    let rel = path.strip_prefix(&game_dir).ok()?;
                    fsops::find_case_insensitive(&game_dir, rel)
                }
            })
            .await?;
            match found {
                Some(p) => {
                    if i == 0 {
                        self.check_game_program(&p)?;
                    }
                    *slot = Some(p);
                }
                None => {
                    missing.get_or_insert(ScriptError::MissingExecutable {
                        expected: Some(path),
                    });
                }
            }
        }
        let runner = self.script.info.runner.clone();
        if !any && matches!(runner.as_str(), "wine" | "linux") {
            missing.get_or_insert(ScriptError::MissingExecutable { expected: None });
        }
        let mut options = g.options.clone();
        for v in options.values_mut() {
            self.substitute_value(v, loc)?;
        }
        let [exe, main_file, iso, rom, path] = resolved;
        let is_wine = runner == "wine";
        let entry = GameEntry {
            name: self.script.info.name.clone(),
            game_slug: self.script.info.game_slug.clone(),
            exe,
            main_file,
            iso,
            rom,
            path,
            args: g.args.as_deref().map(|a| self.text(a, loc)).transpose()?,
            working_dir: self.opt_path(g.working_dir.as_deref(), loc)?,
            prefix: if is_wine {
                Some(match (&self.ctx.prefix, &g.prefix) {
                    (Some(p), _) => p.clone(),
                    (None, Some(raw)) => self.path(raw, loc)?,
                    (None, None) => self.ctx.game_dir.clone(),
                })
            } else {
                None
            },
            arch: g.arch.as_deref().map(|a| self.text(a, loc)).transpose()?,
            wine_version: self.script.wine_version().map(str::to_string),
            env: self.env(&BTreeMap::new(), loc)?,
            options,
            runner_options: self
                .script
                .runner_sections
                .get(&runner)
                .cloned()
                .unwrap_or_default(),
            runner,
        };
        Ok((Some(entry), missing))
    }

    fn substitute_value(&self, v: &mut Value, loc: &str) -> Result<()> {
        match v {
            Value::String(s) => *s = self.text(s, loc)?,
            Value::Array(items) => {
                for i in items {
                    self.substitute_value(i, loc)?;
                }
            }
            Value::Object(m) => {
                for i in m.values_mut() {
                    self.substitute_value(i, loc)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn rollback(&self) {
        (self.progress)(Progress::RollingBack);
        if self.wine_used && self.script.info.runner == "wine" {
            // Wine processes would keep files in the prefix open.
            let prefix = self
                .ctx
                .prefix
                .clone()
                .unwrap_or_else(|| self.ctx.game_dir.clone());
            let request = WineKillRequest {
                prefix,
                arch: self.script.game.arch.clone(),
            };
            if let Err(e) = self
                .host
                .winekill(&request, &CancellationToken::new())
                .await
            {
                tracing::warn!("stopping Wine before the rollback failed: {e:#}");
            }
        }
        let fs = self.fs.clone();
        let home = self.ctx.home.clone();
        let protected = self.ctx.protected.clone();
        let problems = tokio::task::spawn_blocking(move || {
            lock(&fs).journal.rollback(Some(&home), &protected)
        })
        .await
        .unwrap_or_default();
        for p in problems {
            tracing::warn!("rolling back the install: {p}");
        }
    }
}
