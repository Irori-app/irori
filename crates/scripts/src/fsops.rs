//! Confined file operations. Every path a script writes, moves or deletes
//! must resolve inside the game folder, the install cache, the Wine prefix
//! or a path the user approved; symlinks are resolved before the check so
//! a link cannot lead a write elsewhere. Everything created inside the game
//! folder is recorded so a failed or cancelled install can be rolled back.
//!
//! All functions here block; the executor calls them from `spawn_blocking`.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};

use crate::error::ScriptError;

/// `path` with `.` and `..` resolved without touching the file system.
/// `..` never climbs above the root.
pub(crate) fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
        }
    }
    out
}

/// Where `path` really points: its deepest existing ancestor with every
/// symlink resolved, plus the components that do not exist yet. A dangling
/// symlink is an error, since writing through it could create its target
/// anywhere.
pub(crate) fn resolve(path: &Path) -> io::Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut rest: Vec<OsString> = Vec::new();
    loop {
        match fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name() else {
                    break;
                };
                rest.push(name.to_os_string());
                if !existing.pop() {
                    break;
                }
            }
            Err(e) => return Err(e),
        }
    }
    let mut out = fs::canonicalize(&existing)?;
    out.extend(rest.iter().rev());
    Ok(out)
}

/// The folders a script may write to.
#[derive(Debug, Clone)]
pub(crate) struct Sandbox {
    /// Resolved roots.
    allowed: Vec<PathBuf>,
}

impl Sandbox {
    pub fn new(roots: &[PathBuf]) -> io::Result<Self> {
        let allowed = roots
            .iter()
            .map(|r| resolve(&lexical(r)))
            .collect::<io::Result<_>>()?;
        Ok(Self { allowed })
    }

    /// `path` (absolute, lexically normalised) if writing there stays inside
    /// an allowed folder.
    pub fn check(&self, path: &Path, location: &str) -> Result<PathBuf, ScriptError> {
        let refuse = || ScriptError::OutsideAllowedFolders {
            location: location.into(),
            path: path.to_path_buf(),
        };
        let path = lexical(path);
        if !path.is_absolute() {
            return Err(refuse());
        }
        let real = resolve(&path).map_err(|_| refuse())?;
        if self.allowed.iter().any(|root| real.starts_with(root)) {
            Ok(path)
        } else {
            Err(refuse())
        }
    }

    pub fn contains(&self, path: &Path) -> bool {
        resolve(&lexical(path)).is_ok_and(|real| self.allowed.iter().any(|r| real.starts_with(r)))
    }
}

/// What an install created inside the game folder.
#[derive(Debug, Clone)]
pub(crate) struct Journal {
    game_dir: PathBuf,
    /// The install created the game folder itself, so rollback removes it.
    pub game_dir_created: bool,
    created: Vec<PathBuf>,
}

impl Journal {
    pub fn new(game_dir: &Path) -> Self {
        Self {
            game_dir: lexical(game_dir),
            game_dir_created: false,
            created: Vec::new(),
        }
    }

    /// Remember `path` if it is inside the game folder (only those are ever
    /// rolled back).
    pub fn record(&mut self, path: &Path) {
        let path = lexical(path);
        if path.starts_with(&self.game_dir) && path != self.game_dir {
            self.created.push(path);
        }
    }

    /// `create_dir_all`, recording every folder it creates.
    pub fn create_dir_all(&mut self, dir: &Path) -> io::Result<()> {
        let dir = lexical(dir);
        let mut missing = Vec::new();
        let mut cur = dir.clone();
        while fs::symlink_metadata(&cur).is_err() {
            missing.push(cur.clone());
            if !cur.pop() {
                break;
            }
        }
        fs::create_dir_all(&dir)?;
        for d in missing.iter().rev() {
            if *d == self.game_dir {
                self.game_dir_created = true;
            } else {
                self.record(d);
            }
        }
        Ok(())
    }

    /// Top-level entries of the game folder, to find what a program created.
    pub fn snapshot(&self) -> BTreeSet<OsString> {
        fs::read_dir(&self.game_dir)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.file_name())).collect())
            .unwrap_or_default()
    }

    /// Record the top-level entries that appeared since `before`.
    pub fn record_new_since(&mut self, before: &BTreeSet<OsString>) {
        if self.game_dir_created {
            return;
        }
        for name in self.snapshot().difference(before) {
            let p = self.game_dir.join(name);
            self.record(&p);
        }
    }

    /// Undo the install inside the game folder. Never touches anything
    /// outside it; the game folder itself goes only when this install
    /// created it and it passes [`irori_core::fs_guard::check_deletable`].
    pub fn rollback(&self, home: Option<&Path>, protected: &[PathBuf]) -> Vec<String> {
        let mut problems = Vec::new();
        if self.game_dir_created {
            let real = fs::canonicalize(&self.game_dir).unwrap_or_else(|_| self.game_dir.clone());
            match irori_core::fs_guard::check_deletable(&real, home, protected) {
                Ok(()) => {
                    if let Err(e) = fs::remove_dir_all(&self.game_dir)
                        && e.kind() != io::ErrorKind::NotFound
                    {
                        problems.push(format!("{}: {e}", self.game_dir.display()));
                    }
                }
                Err(e) => problems.push(format!("{e:#}")),
            }
            return problems;
        }
        for p in self.created.iter().rev() {
            if !p.starts_with(&self.game_dir) {
                continue;
            }
            let result = match fs::symlink_metadata(p) {
                Ok(m) if m.is_dir() => fs::remove_dir_all(p),
                Ok(_) => fs::remove_file(p),
                Err(_) => Ok(()),
            };
            if let Err(e) = result
                && e.kind() != io::ErrorKind::NotFound
            {
                problems.push(format!("{}: {e}", p.display()));
            }
        }
        problems
    }
}

/// The executor's file system access: the sandbox plus the journal.
#[derive(Debug, Clone)]
pub(crate) struct Fs {
    pub sandbox: Sandbox,
    pub journal: Journal,
    pub home: PathBuf,
    /// Folders no delete may reach, whatever the sandbox allows.
    pub protected: Vec<PathBuf>,
}

fn io_err(location: &str, what: &str, path: &Path, e: io::Error) -> anyhow::Error {
    anyhow::anyhow!("{location}: could not {what} {}: {e}", path.display())
}

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

impl Fs {
    /// Check a write target and create its parent folders.
    fn prepare(&mut self, path: &Path, location: &str) -> anyhow::Result<PathBuf> {
        let path = self.sandbox.check(path, location)?;
        if let Some(parent) = path.parent() {
            self.journal
                .create_dir_all(parent)
                .map_err(|e| io_err(location, "create", parent, e))?;
        }
        Ok(path)
    }

    pub fn mkdir(&mut self, path: &Path, location: &str) -> anyhow::Result<()> {
        let path = self.sandbox.check(path, location)?;
        self.journal
            .create_dir_all(&path)
            .map_err(|e| io_err(location, "create", &path, e))
    }

    pub fn write(
        &mut self,
        path: &Path,
        bytes: &[u8],
        append: bool,
        location: &str,
    ) -> anyhow::Result<()> {
        let path = self.prepare(path, location)?;
        let existed = exists(&path);
        let result = if append {
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(bytes))
        } else {
            irori_core::json_store::write_bytes(&path, bytes).map_err(io::Error::other)
        };
        result.map_err(|e| io_err(location, "write", &path, e))?;
        if !existed {
            self.journal.record(&path);
        }
        Ok(())
    }

    pub fn chmodx(&mut self, path: &Path, location: &str) -> anyhow::Result<()> {
        let path = self.sandbox.check(path, location)?;
        if !path.is_file() {
            return Err(ScriptError::NotFound {
                location: location.into(),
                path,
            }
            .into());
        }
        set_executable(&path).map_err(|e| io_err(location, "make executable", &path, e))
    }

    /// Copy the file `src` into the folder `dst`; returns the copy's path.
    pub fn copy_file_into(
        &mut self,
        src: &Path,
        dst: &Path,
        location: &str,
    ) -> anyhow::Result<PathBuf> {
        self.mkdir(dst, location)?;
        let name = src
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("{location}: {} is not a file", src.display()))?;
        let target = lexical(&dst.join(name));
        if lexical(src) == target {
            return Ok(target);
        }
        let target = self.sandbox.check(&target, location)?;
        let existed = exists(&target);
        fs::copy(src, &target).map_err(|e| io_err(location, "copy to", &target, e))?;
        if !existed {
            self.journal.record(&target);
        }
        Ok(target)
    }

    /// Copy the contents of folder `src` into folder `dst`, keeping what is
    /// already there unless the same name is copied.
    pub fn merge_dir(&mut self, src: &Path, dst: &Path, location: &str) -> anyhow::Result<()> {
        self.mkdir(dst, location)?;
        let entries = fs::read_dir(src).map_err(|e| io_err(location, "read", src, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| io_err(location, "read", src, e))?;
            let from = entry.path();
            let to = self.sandbox.check(&dst.join(entry.file_name()), location)?;
            let kind = entry
                .file_type()
                .map_err(|e| io_err(location, "read", &from, e))?;
            if kind.is_dir() {
                self.merge_dir(&from, &to, location)?;
            } else if kind.is_symlink() {
                self.copy_link(&from, &to, location)?;
            } else {
                let existed = exists(&to);
                fs::copy(&from, &to).map_err(|e| io_err(location, "copy to", &to, e))?;
                if !existed {
                    self.journal.record(&to);
                }
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn copy_link(&mut self, from: &Path, to: &Path, location: &str) -> anyhow::Result<()> {
        let target = fs::read_link(from).map_err(|e| io_err(location, "read", from, e))?;
        let existed = exists(to);
        if existed {
            fs::remove_file(to).map_err(|e| io_err(location, "replace", to, e))?;
        }
        std::os::unix::fs::symlink(&target, to).map_err(|e| io_err(location, "link", to, e))?;
        if !existed {
            self.journal.record(to);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn copy_link(&mut self, from: &Path, to: &Path, location: &str) -> anyhow::Result<()> {
        // Windows symlinks need privileges; copy what the link points to.
        if from.is_file() {
            let existed = exists(to);
            fs::copy(from, to).map_err(|e| io_err(location, "copy to", to, e))?;
            if !existed {
                self.journal.record(to);
            }
        }
        Ok(())
    }

    /// Move `src` to `dst` (into it, when `dst` is an existing folder). When
    /// `keep_source`, the source is copied instead (downloads stay in the
    /// cache for a retry; files outside the allowed folders are never
    /// deleted). Returns the new path, or `None` when the target already
    /// existed and nothing was done.
    pub fn move_path(
        &mut self,
        src: &Path,
        dst: &Path,
        keep_source: bool,
        location: &str,
    ) -> anyhow::Result<Option<PathBuf>> {
        let dst = lexical(dst);
        let target = if dst.is_dir() {
            match src.file_name() {
                Some(name) => dst.join(name),
                None => dst.clone(),
            }
        } else {
            dst.clone()
        };
        if lexical(src) == target {
            return Ok(Some(target));
        }
        if src.is_file() && exists(&target) {
            // Lutris skips rather than overwrite here.
            return Ok(None);
        }
        let target = self.prepare(&target, location)?;
        if keep_source {
            if src.is_dir() {
                self.merge_dir(src, &target, location)?;
            } else {
                fs::copy(src, &target).map_err(|e| io_err(location, "copy to", &target, e))?;
                self.journal.record(&target);
            }
            return Ok(Some(target));
        }
        self.sandbox.check(src, location)?;
        self.rename_or_copy(src, &target, location)?;
        Ok(Some(target))
    }

    /// Rename `src` to `dst`; `dst` must not exist (an empty folder is
    /// removed first).
    pub fn rename(&mut self, src: &Path, dst: &Path, location: &str) -> anyhow::Result<()> {
        let src = self.sandbox.check(src, location)?;
        if !exists(&src) {
            return Err(ScriptError::NotFound {
                location: location.into(),
                path: src,
            }
            .into());
        }
        let dst = self.prepare(dst, location)?;
        if dst.is_dir() {
            let _ = fs::remove_dir(&dst);
        }
        if exists(&dst) {
            anyhow::bail!("{location}: {} already exists", dst.display());
        }
        self.rename_or_copy(&src, &dst, location)
    }

    fn rename_or_copy(&mut self, src: &Path, dst: &Path, location: &str) -> anyhow::Result<()> {
        match fs::rename(src, dst) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                if src.is_dir() {
                    self.merge_dir(src, dst, location)?;
                    fs::remove_dir_all(src).map_err(|e| io_err(location, "remove", src, e))?;
                } else {
                    fs::copy(src, dst).map_err(|e| io_err(location, "copy to", dst, e))?;
                    fs::remove_file(src).map_err(|e| io_err(location, "remove", src, e))?;
                }
            }
            Err(e) => return Err(io_err(location, "move to", dst, e)),
        }
        self.journal.record(dst);
        Ok(())
    }

    /// Move a freshly unpacked tree into place, merging folders and
    /// replacing files. Symlinks at the destination are replaced, never
    /// followed.
    pub fn move_tree(&mut self, src: &Path, dst: &Path, location: &str) -> anyhow::Result<()> {
        let src_meta = fs::symlink_metadata(src).map_err(|e| io_err(location, "read", src, e))?;
        match fs::symlink_metadata(dst) {
            Err(_) => {
                let dst = self.sandbox.check(dst, location)?;
                fs::rename(src, &dst).map_err(|e| io_err(location, "move to", &dst, e))?;
                self.journal.record(&dst);
            }
            Ok(m) if m.is_dir() && src_meta.is_dir() => {
                self.sandbox.check(dst, location)?;
                let entries = fs::read_dir(src).map_err(|e| io_err(location, "read", src, e))?;
                for entry in entries {
                    let entry = entry.map_err(|e| io_err(location, "read", src, e))?;
                    self.move_tree(&entry.path(), &dst.join(entry.file_name()), location)?;
                }
                let _ = fs::remove_dir(src);
            }
            Ok(m) if m.is_dir() => {
                anyhow::bail!(
                    "{location}: cannot replace the folder {} with a file",
                    dst.display()
                );
            }
            Ok(_) => {
                let dst = lexical(dst);
                // Check the parent: the old entry may be a link we replace.
                if let Some(parent) = dst.parent() {
                    self.sandbox.check(parent, location)?;
                }
                fs::remove_file(&dst).map_err(|e| io_err(location, "replace", &dst, e))?;
                fs::rename(src, &dst).map_err(|e| io_err(location, "move to", &dst, e))?;
            }
        }
        Ok(())
    }

    /// Remove a folder the script asked to drop (`remove_source`).
    pub fn remove_tree(&mut self, path: &Path, location: &str) -> anyhow::Result<()> {
        let path = self.sandbox.check(path, location)?;
        if path.is_dir() {
            irori_core::fs_guard::check_deletable(&path, Some(&self.home), &self.protected)?;
            fs::remove_dir_all(&path).map_err(|e| io_err(location, "remove", &path, e))?;
        }
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o111);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
pub(crate) fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Find `rel` under `root`, matching each component case-insensitively
/// when the exact name is missing (scripts written on case-insensitive
/// file systems).
pub(crate) fn find_case_insensitive(root: &Path, rel: &Path) -> Option<PathBuf> {
    let mut cur = root.to_path_buf();
    for c in rel.components() {
        match c {
            Component::Normal(name) => {
                let exact = cur.join(name);
                if exists(&exact) {
                    cur = exact;
                    continue;
                }
                let want = name.to_string_lossy().to_lowercase();
                let found = fs::read_dir(&cur)
                    .ok()?
                    .filter_map(Result::ok)
                    .find(|e| e.file_name().to_string_lossy().to_lowercase() == want)?;
                cur = found.path();
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(cur)
}

/// Merge `data` into `base` the way Lutris's `write_json` does: mappings
/// merge recursively, anything else is replaced.
pub(crate) fn merge_json(base: &mut serde_json::Value, data: &serde_json::Value) {
    match (base, data) {
        (serde_json::Value::Object(b), serde_json::Value::Object(d)) => {
            for (k, v) in d {
                match b.get_mut(k) {
                    Some(existing) => merge_json(existing, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, d) => *b = d.clone(),
    }
}

/// Set `section.key = value` in INI text, keeping comments, order and key
/// case; missing sections and keys are appended.
pub(crate) fn set_ini(text: &str, section: &str, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let header = |l: &str| {
        let t = l.trim();
        (t.starts_with('[') && t.ends_with(']')).then(|| t[1..t.len() - 1].trim().to_string())
    };
    let start = lines
        .iter()
        .position(|l| header(l).as_deref() == Some(section));
    let entry = format!("{key}={value}");
    match start {
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[{section}]"));
            lines.push(entry);
        }
        Some(s) => {
            let end = lines[s + 1..]
                .iter()
                .position(|l| header(l).is_some())
                .map_or(lines.len(), |p| s + 1 + p);
            let existing = (s + 1..end).find(|&i| {
                let t = lines[i].trim_start();
                !t.starts_with(['#', ';'])
                    && t.split_once('=').is_some_and(|(k, _)| k.trim() == key)
            });
            match existing {
                Some(i) => lines[i] = entry,
                None => {
                    // After the section's last non-blank line.
                    let mut at = end;
                    while at > s + 1 && lines[at - 1].trim().is_empty() {
                        at -= 1;
                    }
                    lines.insert(at, entry);
                }
            }
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_resolves_dots_without_escaping_root() {
        assert_eq!(lexical(Path::new("/a/b/../c/./d")), PathBuf::from("/a/c/d"));
        assert_eq!(lexical(Path::new("/a/../../..")), PathBuf::from("/"));
    }

    #[test]
    fn ini_updates_in_place_and_appends() {
        let text = "# conf\n[sdl]\nfullscreen=false\n\n[cpu]\ncycles=auto\n";
        let t = set_ini(text, "sdl", "fullscreen", "true");
        assert_eq!(t, "# conf\n[sdl]\nfullscreen=true\n\n[cpu]\ncycles=auto\n");
        let t = set_ini(&t, "sdl", "output", "opengl");
        assert!(t.contains("fullscreen=true\noutput=opengl\n\n[cpu]"));
        let t = set_ini(&t, "autoexec", "mount", "c .");
        assert!(t.ends_with("[autoexec]\nmount=c .\n"));
        assert_eq!(set_ini("", "a", "K", "v"), "[a]\nK=v\n");
    }

    #[test]
    fn json_merges_recursively() {
        let mut base = serde_json::json!({"a": {"x": 1, "y": 2}, "b": 1});
        merge_json(&mut base, &serde_json::json!({"a": {"y": 3}, "c": [1]}));
        assert_eq!(
            base,
            serde_json::json!({"a": {"x": 1, "y": 3}, "b": 1, "c": [1]})
        );
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_refuses_escapes_and_symlinks_out() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("game");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&game).unwrap();
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, game.join("link")).unwrap();
        let sb = Sandbox::new(std::slice::from_ref(&game)).unwrap();
        assert!(sb.check(&game.join("a/b.txt"), "t").is_ok());
        assert!(sb.check(&game.join("../outside/x"), "t").is_err());
        assert!(sb.check(&game.join("link/x"), "t").is_err());
        assert!(sb.check(Path::new("/etc/passwd"), "t").is_err());
        assert!(sb.check(Path::new("relative"), "t").is_err());
    }

    #[test]
    fn finds_paths_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("Game/Bin")).unwrap();
        fs::write(tmp.path().join("Game/Bin/Play.EXE"), b"").unwrap();
        let found = find_case_insensitive(tmp.path(), Path::new("game/bin/play.exe")).unwrap();
        // Case-insensitive file systems (macOS, Windows) find the exact
        // spelling first; elsewhere the real name is returned.
        assert!(found.is_file());
        if !Path::new(&tmp.path().join("GAME")).exists() {
            assert!(found.ends_with("Game/Bin/Play.EXE"));
        }
        assert!(find_case_insensitive(tmp.path(), Path::new("game/nope.exe")).is_none());
    }
}
