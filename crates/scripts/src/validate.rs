//! Checks on a parsed script: every variable it uses exists, built-ins are
//! not redefined, and launchers Ember supports natively are not installed
//! inside Wine.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::error::{NativeLauncher, ScriptError, step_loc};
use crate::model::*;
use crate::parse::invalid;
use crate::vars::{self, Mode};

/// Every piece of script text that is substituted, with where it is and how.
pub(crate) fn texts(script: &Script) -> Vec<(String, &str, Mode)> {
    let mut out: Vec<(String, &str, Mode)> = Vec::new();
    for f in &script.files {
        if let FileSource::Url(u) = &f.source {
            out.push((format!("File “{}”", f.id), u, Mode::Strict));
        }
    }
    for step in &script.steps {
        let loc = step_loc(step.number, step.command.name());
        for (s, m) in command_texts(&step.command) {
            out.push((loc.clone(), s, m));
        }
    }
    let g = &script.game;
    for s in [
        &g.exe,
        &g.main_file,
        &g.iso,
        &g.rom,
        &g.path,
        &g.args,
        &g.working_dir,
        &g.prefix,
        &g.arch,
    ]
    .into_iter()
    .flatten()
    {
        out.push(("Game section".into(), s, Mode::Strict));
    }
    for v in g.options.values() {
        value_texts(v, &mut |s| {
            out.push(("Game section".into(), s, Mode::Strict))
        });
    }
    for v in script.system_env.values() {
        out.push(("System section (env)".into(), v, Mode::Strict));
    }
    out
}

fn value_texts<'a>(v: &'a Value, f: &mut dyn FnMut(&'a str)) {
    match v {
        Value::String(s) => f(s),
        Value::Array(items) => items.iter().for_each(|i| value_texts(i, f)),
        Value::Object(m) => m.values().for_each(|i| value_texts(i, f)),
        _ => {}
    }
}

/// The substituted texts of one command.
pub(crate) fn command_texts(c: &Command) -> Vec<(&str, Mode)> {
    use Mode::{Shell, Strict};
    let mut v: Vec<(&str, Mode)> = Vec::new();
    match c {
        Command::Chmodx { file } => v.push((file, Strict)),
        Command::Extract(e) => {
            v.push((&e.file, Strict));
            v.extend(e.dst.as_deref().map(|d| (d, Strict)));
        }
        Command::Merge(p) | Command::Copy(p) => {
            v.push((&p.src, Strict));
            v.push((&p.dst, Strict));
        }
        Command::Move(p) => {
            v.push((&p.src, Strict));
            v.push((&p.dst, Strict));
        }
        Command::Rename { src, dst } => {
            v.push((src, Strict));
            v.push((dst, Strict));
        }
        Command::Mkdir { path } => v.push((path, Strict)),
        Command::WriteFile { file, content, .. } => {
            v.push((file, Strict));
            v.push((content, Strict));
        }
        Command::WriteJson { file, .. } | Command::SubstituteVars { file } => {
            v.push((file, Strict));
        }
        Command::WriteConfig { file, entries, .. } => {
            v.push((file, Strict));
            v.extend(entries.iter().map(|e| (e.value.as_str(), Strict)));
        }
        Command::InputMenu(m) => {
            v.extend(m.description.as_deref().map(|d| (d, Strict)));
            v.extend(m.preselect.as_deref().map(|d| (d, Strict)));
        }
        Command::InsertDisc { requires, message } => {
            v.push((requires, Strict));
            v.extend(message.as_deref().map(|d| (d, Strict)));
        }
        Command::Execute(e) => {
            match &e.program {
                Program::File(f) => v.push((f, Strict)),
                Program::Shell(s) => v.push((s, Shell)),
            }
            v.push((&e.args, Strict));
            v.extend(e.working_dir.as_deref().map(|d| (d, Strict)));
            v.extend(e.env.values().map(|s| (s.as_str(), Strict)));
        }
        Command::Task(t) => match t {
            Task::CreatePrefix(p) => {
                v.extend(p.prefix.as_deref().map(|d| (d, Strict)));
                v.extend(p.arch.as_deref().map(|d| (d, Strict)));
                v.extend(p.overrides.values().map(|s| (s.as_str(), Strict)));
            }
            Task::WineExec(w) => {
                v.push((&w.executable, Strict));
                v.push((&w.args, Strict));
                for o in [&w.prefix, &w.arch, &w.working_dir] {
                    v.extend(o.as_deref().map(|d| (d, Strict)));
                }
                v.extend(w.env.values().map(|s| (s.as_str(), Strict)));
                v.extend(w.overrides.values().map(|s| (s.as_str(), Strict)));
            }
            Task::Winetricks(w) => {
                v.push((&w.app, Strict));
                for o in [&w.prefix, &w.arch] {
                    v.extend(o.as_deref().map(|d| (d, Strict)));
                }
            }
            Task::SetRegedit(r) => {
                v.push((&r.path, Strict));
                v.push((&r.key, Strict));
                v.push((&r.value, Strict));
                for o in [&r.prefix, &r.arch] {
                    v.extend(o.as_deref().map(|d| (d, Strict)));
                }
            }
            Task::WineKill(k) => {
                for o in [&k.prefix, &k.arch] {
                    v.extend(o.as_deref().map(|d| (d, Strict)));
                }
            }
            Task::DosExec(d) => {
                for o in [&d.config_file, &d.executable, &d.working_dir] {
                    v.extend(o.as_deref().map(|s| (s, Strict)));
                }
                v.push((&d.args, Strict));
            }
        },
        Command::StoreAutosetup(_) => {}
    }
    v
}

/// Names a script may reference: built-ins, its variables, its file ids
/// and `INPUT_<id>` for its menus.
pub(crate) fn known_names(script: &Script) -> BTreeSet<String> {
    let mut known: BTreeSet<String> = vars::BUILTINS.iter().map(|s| s.to_string()).collect();
    known.extend(script.variables.keys().cloned());
    known.extend(script.files.iter().map(|f| f.id.clone()));
    for s in &script.steps {
        if let Command::InputMenu(InputMenu { id: Some(id), .. }) = &s.command {
            known.insert(format!("INPUT_{id}"));
        }
    }
    known
}

pub(crate) fn validate(script: &Script) -> Result<(), ScriptError> {
    let mut seen = BTreeSet::new();
    for f in &script.files {
        if !seen.insert(f.id.as_str()) {
            return Err(ScriptError::DuplicateFile { id: f.id.clone() });
        }
        if vars::is_reserved(&f.id) {
            return Err(ScriptError::ShadowedVariable { name: f.id.clone() });
        }
        if script.variables.contains_key(&f.id) {
            return Err(invalid(
                "Script",
                format!("“{}” is both a file and a variable", f.id),
            ));
        }
    }
    if let Some(name) = script.variables.keys().find(|k| vars::is_reserved(k)) {
        return Err(ScriptError::ShadowedVariable { name: name.clone() });
    }
    let known = known_names(script);
    let is_known = |n: &str| known.contains(n);
    for (location, text, mode) in texts(script) {
        for name in vars::references(text, &is_known) {
            if is_known(&name) || (mode == Mode::Shell && vars::is_shell_name(&name)) {
                continue;
            }
            return Err(ScriptError::UnknownVariable { location, name });
        }
    }
    if let Some(launcher) = native_launcher(script) {
        return Err(ScriptError::PreferNative { launcher });
    }
    Ok(())
}

const LAUNCHER_SLUGS: &[(&str, NativeLauncher)] = &[
    ("epic-games-store", NativeLauncher::EpicGames),
    ("epic-games-launcher", NativeLauncher::EpicGames),
    ("epic-games", NativeLauncher::EpicGames),
    ("origin", NativeLauncher::EaApp),
    ("ea-app", NativeLauncher::EaApp),
    ("ea-desktop", NativeLauncher::EaApp),
    ("ea-desktop-app", NativeLauncher::EaApp),
    ("ubisoft-connect", NativeLauncher::UbisoftConnect),
    ("uplay", NativeLauncher::UbisoftConnect),
    ("ubisoft-game-launcher", NativeLauncher::UbisoftConnect),
];

const LAUNCHER_FILES: &[(&str, NativeLauncher)] = &[
    ("epicgameslauncher.exe", NativeLauncher::EpicGames),
    ("epicgameslauncherinstaller.msi", NativeLauncher::EpicGames),
    ("epicinstaller.msi", NativeLauncher::EpicGames),
    ("origin.exe", NativeLauncher::EaApp),
    ("originsetup.exe", NativeLauncher::EaApp),
    ("originthinsetup.exe", NativeLauncher::EaApp),
    ("eadesktop.exe", NativeLauncher::EaApp),
    ("ealauncher.exe", NativeLauncher::EaApp),
    ("eaappinstaller.exe", NativeLauncher::EaApp),
    ("upc.exe", NativeLauncher::UbisoftConnect),
    ("uplay.exe", NativeLauncher::UbisoftConnect),
    ("uplayinstaller.exe", NativeLauncher::UbisoftConnect),
    ("ubisoftconnect.exe", NativeLauncher::UbisoftConnect),
    (
        "ubisoftconnectinstaller.exe",
        NativeLauncher::UbisoftConnect,
    ),
    ("ubisoftgamelauncher.exe", NativeLauncher::UbisoftConnect),
];

fn file_name_of(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .trim()
        .to_ascii_lowercase()
}

/// The launcher a script installs inside Wine, when it is one Ember
/// supports natively: by slug (the script's, or a game it requires), or for
/// Wine scripts by the program it downloads, runs or starts.
pub fn native_launcher(script: &Script) -> Option<NativeLauncher> {
    let by_slug = |slug: &str| {
        let slug = slug.trim().to_ascii_lowercase();
        LAUNCHER_SLUGS
            .iter()
            .find(|(s, _)| *s == slug)
            .map(|(_, l)| *l)
    };
    let slugs = [
        Some(&script.info.game_slug),
        script.info.slug.as_ref(),
        script.info.installer_slug.as_ref(),
        script.extends.as_ref(),
    ]
    .into_iter()
    .flatten()
    .chain(script.requires.iter().flatten());
    for slug in slugs {
        if let Some(l) = by_slug(slug) {
            return Some(l);
        }
    }
    if script.info.runner != "wine" {
        return None;
    }
    let by_file = |name: &str| {
        let name = file_name_of(name);
        LAUNCHER_FILES
            .iter()
            .find(|(f, _)| *f == name)
            .map(|(_, l)| *l)
    };
    let mut candidates: Vec<&str> = Vec::new();
    candidates.extend(script.game.exe.as_deref());
    for f in &script.files {
        candidates.extend(f.filename.as_deref());
        if let FileSource::Url(u) = &f.source {
            candidates.push(u);
        }
    }
    for s in &script.steps {
        if let Command::Task(Task::WineExec(w)) = &s.command {
            candidates.push(&w.executable);
        }
    }
    candidates.into_iter().find_map(by_file)
}
