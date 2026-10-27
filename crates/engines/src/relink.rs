//! Engines that convert a game into a program of this computer before it
//! plays ([`EngineKind::Relinker`](crate::EngineKind::Relinker), AnyPS5).
//!
//! The converted program lives in Ember's data folder with links to the
//! game's own files beside it (`app0/`), so the user's game folder is never
//! written. A game is converted again only when its program, its modules or
//! the engine change.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ember_core::process::{CommandSpec, RunOptions};
use ember_core::{OpContext, Paths, json_store};
use md5::{Digest as _, Md5};
use serde::{Deserialize, Serialize};

use crate::discovery::Installation;
use crate::launch::{EngineUnavailable, GameFile, Unavailable};
use crate::manifest::EngineManifest;

/// Folders beside a game's program that hold its own modules. The relinker
/// writes their converted copies into the output's `app0/`.
const MODULE_DIRS: [&str; 3] = ["sce_module", "sce_modules", "prx"];
/// The engine's system libraries, beside its program.
const LIBS: &str = "libs";
const STAMP: &str = "ember-relink.json";

/// Where a game's converted program lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub dir: PathBuf,
    pub program: PathBuf,
}

/// The converted program of `game` for `engine`.
pub fn output(engine: &str, game: &GameFile) -> Output {
    let folder = game_folder(game);
    let key = hex::encode(&Md5::digest(folder.to_string_lossy().as_bytes())[..8]);
    let dir = root().join(engine).join(key);
    let program = dir.join(if cfg!(windows) { "app.exe" } else { "app.elf" });
    Output { dir, program }
}

fn root() -> PathBuf {
    Paths::get().data.join("engines").join("relinked")
}

/// The game's folder: the parent of its program (`eboot.bin`).
fn game_folder(game: &GameFile) -> PathBuf {
    if game.path.is_dir() {
        game.path.clone()
    } else {
        game.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }
}

/// What a conversion was made from; a change means converting again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stamp {
    engine: FileStamp,
    program: FileStamp,
    modules: Vec<(String, FileStamp)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileStamp {
    path: PathBuf,
    size: u64,
    mtime: u64,
}

impl FileStamp {
    fn of(path: &Path) -> Result<Self> {
        let meta =
            std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        Ok(Self {
            path: path.to_path_buf(),
            size: meta.len(),
            mtime,
        })
    }
}

fn stamp_of(installation: &Installation, game: &GameFile) -> Result<Stamp> {
    let folder = game_folder(game);
    let mut modules = Vec::new();
    for dir in MODULE_DIRS.iter().map(|d| folder.join(d)) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_file()) {
                let name = entry.path().strip_prefix(&folder).map_or_else(
                    |_| entry.file_name().to_string_lossy().into_owned(),
                    |p| p.to_string_lossy().into_owned(),
                );
                modules.push((name, FileStamp::of(&entry.path())?));
            }
        }
    }
    modules.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Stamp {
        engine: FileStamp::of(&installation.program)?,
        program: FileStamp::of(&game.path)?,
        modules,
    })
}

/// Whether `game` was converted with `installation` and has not changed
/// since. Blocking.
pub fn is_current(installation: &Installation, game: &GameFile, out: &Output) -> bool {
    let Ok(Some(saved)) = json_store::read::<Stamp>(&out.dir.join(STAMP)) else {
        return false;
    };
    out.program.is_file() && stamp_of(installation, game).is_ok_and(|now| now == saved)
}

/// Refuse a game this computer cannot run before converting it: a program
/// that is still encrypted, or a processor without what the engine's games
/// use. Blocking.
pub fn check(manifest: &EngineManifest, game: &GameFile) -> Result<()> {
    let missing = missing_cpu_features(&manifest.cpu_features);
    if !missing.is_empty() {
        return Err(EngineUnavailable {
            engine: manifest.id.clone(),
            name: manifest.name.clone(),
            reason: Unavailable::Processor { missing },
        }
        .into());
    }
    let mut magic = [0u8; 4];
    let read = std::fs::File::open(&game.path)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic));
    read.with_context(|| format!("Could not read {}", game.path.display()))?;
    if &magic != b"\x7fELF" {
        bail!(
            "{} is not a decrypted program, so {} cannot convert it. Use a dump whose eboot.bin is a plain ELF file.",
            game.path.display(),
            manifest.name
        );
    }
    Ok(())
}

/// The processor features of `wanted` this computer lacks. Every one is
/// missing on a processor that isn't x86-64.
pub fn missing_cpu_features(wanted: &[String]) -> Vec<String> {
    wanted
        .iter()
        .filter(|f| !has_cpu_feature(f))
        .cloned()
        .collect()
}

#[cfg(target_arch = "x86_64")]
fn has_cpu_feature(name: &str) -> bool {
    match name {
        "avx" => std::arch::is_x86_feature_detected!("avx"),
        "avx2" => std::arch::is_x86_feature_detected!("avx2"),
        "fma" => std::arch::is_x86_feature_detected!("fma"),
        "f16c" => std::arch::is_x86_feature_detected!("f16c"),
        "bmi1" => std::arch::is_x86_feature_detected!("bmi1"),
        "bmi2" => std::arch::is_x86_feature_detected!("bmi2"),
        "lzcnt" => std::arch::is_x86_feature_detected!("lzcnt"),
        "movbe" => std::arch::is_x86_feature_detected!("movbe"),
        "popcnt" => std::arch::is_x86_feature_detected!("popcnt"),
        "sse4.2" => std::arch::is_x86_feature_detected!("sse4.2"),
        // A feature Ember can't test is left to the engine to report.
        _ => true,
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn has_cpu_feature(_name: &str) -> bool {
    false
}

/// Convert `game` with `installation` unless the last conversion is still
/// current. Returns `None` when cancelled.
pub async fn prepare(
    manifest: &EngineManifest,
    installation: &Installation,
    game: &GameFile,
    ctx: &OpContext,
) -> Result<Option<Output>> {
    let out = output(&manifest.id, game);
    let (m, i, g, o) = (
        manifest.clone(),
        installation.clone(),
        game.clone(),
        out.clone(),
    );
    let plan = tokio::task::spawn_blocking(move || -> Result<Option<(CommandSpec, Stamp)>> {
        check(&m, &g)?;
        if is_current(&i, &g, &o) {
            return Ok(None);
        }
        let stamp = stamp_of(&i, &g)?;
        Ok(Some((start_over(&i, &g, &o)?, stamp)))
    })
    .await
    .context("preparing the conversion stopped")??;
    let Some((command, stamp)) = plan else {
        return Ok(Some(out));
    };

    (ctx.step)("Converting the game");
    let mut options = RunOptions::default().cancel(ctx.cancel.clone());
    options.log_file = ctx.log_file.clone();
    let result = ember_core::process::run(&command, options)
        .await
        .with_context(|| format!("Could not start {}", manifest.name))?;
    if result.aborted || ctx.cancel.is_cancelled() {
        return Ok(None);
    }
    if !result.success() {
        let detail = result
            .stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no details")
            .trim()
            .to_string();
        bail!("{} could not convert this game: {detail}", manifest.name);
    }

    let (g, o) = (game.clone(), out.clone());
    tokio::task::spawn_blocking(move || -> Result<()> {
        link_app0(&game_folder(&g), &o.dir.join("app0"))?;
        json_store::write(&o.dir.join(STAMP), &stamp)
    })
    .await
    .context("finishing the conversion stopped")??;
    Ok(Some(out))
}

/// Empty the output folder and build the relinker's command. Blocking.
fn start_over(installation: &Installation, game: &GameFile, out: &Output) -> Result<CommandSpec> {
    let root = root();
    if !out.dir.starts_with(&root) {
        bail!("{} is not a folder Ember manages", out.dir.display());
    }
    if out.dir.exists() {
        std::fs::remove_dir_all(&out.dir)
            .with_context(|| format!("Could not clear {}", out.dir.display()))?;
    }
    std::fs::create_dir_all(&out.dir)
        .with_context(|| format!("Could not create {}", out.dir.display()))?;

    let engine_dir = installation
        .dir
        .clone()
        .or_else(|| installation.program.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let libs = engine_dir.join(LIBS);
    if !libs.is_dir() {
        bail!(
            "The engine's system libraries are missing from {}. Install the engine again.",
            engine_dir.display()
        );
    }
    let mut spec = CommandSpec::new(&installation.program);
    if cfg!(windows) {
        spec = spec.arg("--windows").arg("--windows-gui");
    }
    // Windows takes only ASCII library paths; elsewhere the libraries are
    // used where the engine keeps them.
    if cfg!(windows) && !libs.to_string_lossy().is_ascii() {
        copy_dir(&libs, &out.dir.join(LIBS))?;
    } else {
        spec = spec.arg("--rpath").arg(&libs);
    }
    // The relinker needs a module folder; a game without modules gets an
    // empty one outside its own folder.
    let folder = game_folder(game);
    if !MODULE_DIRS.iter().any(|d| folder.join(d).is_dir()) {
        let parent = out.dir.join("no-modules");
        std::fs::create_dir_all(parent.join(MODULE_DIRS[0]))
            .with_context(|| format!("Could not create {}", parent.display()))?;
        spec = spec.arg("--sce-module-path").arg(parent);
    }
    spec = spec.arg(&game.path).arg(&out.program);
    spec.cwd = Some(out.dir.clone());
    Ok(spec)
}

/// Link every entry of the game folder into `app0`, except the module
/// folders the relinker filled with converted copies. Blocking.
fn link_app0(game: &Path, app0: &Path) -> Result<()> {
    std::fs::create_dir_all(app0)
        .with_context(|| format!("Could not create {}", app0.display()))?;
    for entry in std::fs::read_dir(game).with_context(|| format!("reading {}", game.display()))? {
        let entry = entry?;
        let to = app0.join(entry.file_name());
        if std::fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        link(&entry.path(), &to).with_context(|| format!("Could not link {}", to.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn link(from: &Path, to: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(from, to)
}

#[cfg(windows)]
fn link(from: &Path, to: &Path) -> std::io::Result<()> {
    // Symbolic links need developer mode on Windows; junctions and hard
    // links don't. A file on another drive is copied.
    if from.is_dir() {
        junction::create(from, to)
    } else {
        std::fs::hard_link(from, to).or_else(|_| std::fs::copy(from, to).map(|_| ()))
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("Could not create {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("Could not copy {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Whether Linux keeps `/dev/udmabuf` from this user. AnyPS5 shares game
/// memory with the GPU through it on drivers other than NVIDIA's, so games
/// start but run badly without access.
pub fn udmabuf_denied() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let path = Path::new("/dev/udmabuf");
    path.exists()
        && std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::Origin;

    fn manifest(cpu: &[&str]) -> EngineManifest {
        EngineManifest::parse(
            &serde_json::json!({
                "id": "relinker-test", "name": "Relinker", "license": "MIT",
                "kind": "relinker", "systems": ["ps5"], "sources": [],
                "cpuFeatures": cpu, "exitHotkey": "Alt+F4"
            })
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn refuses_encrypted_programs_and_reports_missing_cpu_features() {
        let root = crate::test_root("relink-check");
        let game = GameFile {
            path: root.join("eboot.bin"),
            system: "ps5".into(),
        };
        std::fs::write(&game.path, b"SCE\0encrypted").unwrap();
        let e = check(&manifest(&[]), &game).unwrap_err();
        assert!(format!("{e:#}").contains("not a decrypted program"));
        std::fs::write(&game.path, b"\x7fELF....").unwrap();
        check(&manifest(&[]), &game).unwrap();

        // Features Ember can't test are left to the engine.
        assert!(missing_cpu_features(&["made-up".into()]).len() <= 1);
        if cfg!(not(target_arch = "x86_64")) {
            let e = check(&manifest(&["avx2"]), &game).unwrap_err();
            let unavailable = EngineUnavailable::of(&e).unwrap();
            assert_eq!(
                unavailable.reason,
                Unavailable::Processor {
                    missing: vec!["avx2".into()]
                }
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn converts_once_links_the_game_and_converts_again_after_changes() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = crate::test_root("relink-prepare");
        let engine = root.join("engine");
        std::fs::create_dir_all(engine.join(LIBS)).unwrap();
        let calls = root.join("calls.txt");
        // Records its arguments and writes the program and a module folder
        // where the real relinker would.
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nfor last; do :; done\nprintf x > \"$last\"\nmkdir -p \"$(dirname \"$last\")/app0/sce_module\"\n",
            calls.display()
        );
        let program = engine.join("relinker-v0.1");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let installation = Installation {
            engine: "relinker-test".into(),
            origin: Origin::Managed,
            version: Some("0.1".into()),
            program,
            flatpak: None,
            dir: Some(engine.clone()),
            size: 0,
        };

        let folder = root.join("roms/ps5/PPSA00000");
        std::fs::create_dir_all(folder.join("sce_sys")).unwrap();
        std::fs::create_dir_all(folder.join("sce_module")).unwrap();
        std::fs::write(folder.join("eboot.bin"), b"\x7fELF").unwrap();
        std::fs::write(folder.join("sce_module/libgame.prx"), b"\x7fELF").unwrap();
        std::fs::write(folder.join("sce_sys/param.json"), b"{}").unwrap();
        let game = GameFile {
            path: folder.join("eboot.bin"),
            system: "ps5".into(),
        };
        let m = manifest(&[]);

        let out = prepare(&m, &installation, &game, &OpContext::detached())
            .await
            .unwrap()
            .unwrap();
        assert!(out.dir.starts_with(super::root()));
        assert!(out.program.is_file());
        let args = std::fs::read_to_string(&calls).unwrap();
        assert_eq!(
            args.trim(),
            format!(
                "--rpath {} {} {}",
                engine.join(LIBS).display(),
                game.path.display(),
                out.program.display()
            )
        );
        // The game's files are linked, the converted modules kept.
        let app0 = out.dir.join("app0");
        assert_eq!(
            std::fs::read_link(app0.join("sce_sys")).unwrap(),
            folder.join("sce_sys")
        );
        assert_eq!(
            std::fs::read_link(app0.join("eboot.bin")).unwrap(),
            game.path
        );
        assert!(
            !std::fs::symlink_metadata(app0.join("sce_module"))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        // Unchanged: not converted again. A changed module: converted again.
        prepare(&m, &installation, &game, &OpContext::detached())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 1);
        std::fs::write(folder.join("sce_module/libgame.prx"), b"\x7fELF changed").unwrap();
        prepare(&m, &installation, &game, &OpContext::detached())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 2);

        // A game without modules gets an empty module folder outside it.
        std::fs::remove_dir_all(folder.join("sce_module")).unwrap();
        prepare(&m, &installation, &game, &OpContext::detached())
            .await
            .unwrap()
            .unwrap();
        let last = std::fs::read_to_string(&calls).unwrap();
        let last = last.lines().last().unwrap();
        assert!(last.contains(&format!(
            "--sce-module-path {}",
            out.dir.join("no-modules").display()
        )));
        assert!(!folder.join("sce_module").exists());
    }
}
