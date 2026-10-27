//! Turning a game file into the command that plays it: pick the engine's
//! installation, link the system's BIOS files where the engine looks, write
//! RetroArch's settings, and expand the manifest's launch template.
//!
//! The result is a plain [`CommandSpec`]; the services layer spawns and
//! tracks it like any other game, so wrappers, playtime, logs and the
//! failed-start sheet all apply.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use irori_core::process::CommandSpec;
use serde_json::Value;

use crate::bios::BiosIndex;
use crate::discovery::{Installation, Origin};
use crate::host::{Host, Os};
use crate::manifest::{Arg, EngineKind, EngineManifest, Source};

/// The engine (or the core, or the pinned version) a game needs is not
/// installed. The UI offers to install it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct EngineMissing {
    pub engine: String,
    pub name: String,
    pub core: Option<String>,
    /// The version the game is pinned to, when that is what is missing.
    pub pinned: Option<String>,
}

impl fmt::Display for EngineMissing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.core, &self.pinned) {
            (Some(core), _) => write!(f, "The {core} core of {} is not installed", self.name),
            (None, Some(v)) => write!(
                f,
                "This game is set to use {} {v}, which is not installed any more",
                self.name
            ),
            (None, None) => write!(f, "{} is not installed", self.name),
        }
    }
}

impl EngineMissing {
    pub fn of(error: &anyhow::Error) -> Option<&EngineMissing> {
        error.downcast_ref()
    }
}

/// The engine a game needs cannot run on this computer, so installing it
/// would not help. The UI explains why instead of offering an install.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct EngineUnavailable {
    pub engine: String,
    pub name: String,
    pub reason: Unavailable,
}

/// Why an engine can't run here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// Irori has no build of it for this computer. `hosts` are the
    /// computers it has builds for (`linux-x86_64`, …).
    NoBuild { hosts: Vec<String> },
    /// The processor lacks features the engine's games use (`avx2`, …).
    Processor { missing: Vec<String> },
}

impl fmt::Display for EngineUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.reason {
            Unavailable::NoBuild { hosts } if hosts.is_empty() => {
                write!(f, "Irori can't download {} for this computer", self.name)
            }
            Unavailable::NoBuild { hosts } => write!(
                f,
                "{} is not available for this computer; it runs on {}",
                self.name,
                hosts.join(", ")
            ),
            Unavailable::Processor { missing } => write!(
                f,
                "{} games need a processor with {}, which this one lacks",
                self.name,
                missing.join(", ")
            ),
        }
    }
}

impl EngineUnavailable {
    pub fn of(error: &anyhow::Error) -> Option<&EngineUnavailable> {
        error.downcast_ref()
    }

    /// The error for `manifest` when Irori can't download it for `host`.
    pub fn no_build(manifest: &EngineManifest, host: Host) -> Option<Self> {
        if manifest.downloadable_on(host) {
            return None;
        }
        let mut hosts: Vec<String> = manifest
            .sources
            .iter()
            .filter_map(|s| match s {
                Source::Github { assets, .. }
                | Source::Buildbot { assets, .. }
                | Source::Page { assets, .. } => Some(assets.keys().cloned()),
                _ => None,
            })
            .flatten()
            .collect();
        hosts.sort();
        hosts.dedup();
        Some(Self {
            engine: manifest.id.clone(),
            name: manifest.name.clone(),
            reason: Unavailable::NoBuild { hosts },
        })
    }
}

/// A system's games need BIOS files the user hasn't provided. The UI
/// offers to choose the BIOS folder and shows what is needed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{name} games need a startup file from your console (BIOS): {}", files.join(", "))]
pub struct BiosMissing {
    pub system: String,
    pub name: String,
    /// The file names that would do.
    pub files: Vec<String>,
}

impl BiosMissing {
    pub fn of(error: &anyhow::Error) -> Option<&BiosMissing> {
        error.downcast_ref()
    }

    /// The error for `system` when its required BIOS is missing from
    /// `index`.
    pub fn check(system: &crate::System, index: &BiosIndex) -> Result<(), BiosMissing> {
        let status = crate::bios::status(system, index);
        if status.state != crate::bios::SystemState::Missing {
            return Ok(());
        }
        Err(BiosMissing {
            system: system.id.clone(),
            name: system.name.clone(),
            files: status.files.into_iter().map(|f| f.name).collect(),
        })
    }
}

/// The installation to use: the game's pin when set, else the engine's
/// default, else the best one found.
pub fn resolve<'a>(
    manifest: &EngineManifest,
    installs: &'a [Installation],
    pin: Option<&str>,
    preferred: Option<&str>,
) -> Result<&'a Installation, EngineMissing> {
    let missing = |pinned: Option<String>| EngineMissing {
        engine: manifest.id.clone(),
        name: manifest.name.clone(),
        core: None,
        pinned,
    };
    if let Some(pin) = pin.filter(|p| !p.is_empty()) {
        return installs.iter().find(|i| i.id() == pin).ok_or_else(|| {
            missing(Some(
                pin.strip_prefix("managed:").unwrap_or(pin).to_string(),
            ))
        });
    }
    preferred
        .and_then(|p| installs.iter().find(|i| i.id() == p))
        .or_else(|| installs.first())
        .ok_or_else(|| missing(None))
}

/// A game an engine plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameFile {
    /// The file to open (a ROM, a disc's `.cue`/`.m3u`, a DOS program) or,
    /// for folder games, the game's folder.
    pub path: PathBuf,
    pub system: String,
}

/// Everything [`command`] needs.
pub struct LaunchInput<'a> {
    pub manifest: &'a EngineManifest,
    pub installation: &'a Installation,
    pub game: &'a GameFile,
    /// The libretro core, for RetroArch.
    pub core: Option<&'a str>,
    /// Option values (see [`EngineManifest::option_values`]).
    pub values: &'a BTreeMap<String, Value>,
    pub bios: &'a BiosIndex,
    /// Use the engine's own settings instead of the ones Irori manages.
    pub own_settings: bool,
}

/// Build the command. Blocking: links files and writes settings.
pub fn command(input: &LaunchInput) -> Result<CommandSpec> {
    let LaunchInput {
        manifest,
        installation,
        game,
        core,
        values,
        bios,
        own_settings,
    } = *input;
    let os = Os::current();
    let rom_dir = if game.path.is_dir() {
        game.path.clone()
    } else {
        game.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    };
    let home = crate::dirs::home(&manifest.id);
    let engine_dir = installation
        .dir
        .clone()
        .or_else(|| installation.program.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let vars = Vars {
        rom: game.path.display().to_string(),
        rom_dir: rom_dir.display().to_string(),
        home: dirs::home_dir().unwrap_or_default().display().to_string(),
        engine_home: home.display().to_string(),
        engine_dir: engine_dir.display().to_string(),
        xdg_data: std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_default()
                    .join(".local/share")
                    .display()
                    .to_string()
            }),
    };

    // Folders the Flatpak sandbox must see.
    let mut shared: Vec<PathBuf> = vec![rom_dir.clone()];
    let system = crate::systems::get(&game.system)
        .with_context(|| format!("Irori does not know the system {}", game.system))?;

    let mut args: Vec<OsString> = Vec::new();
    match manifest.kind {
        EngineKind::Retroarch => {
            let core = core.context("RetroArch needs a core for this system")?;
            let core_path =
                crate::retroarch::installed_core(core).ok_or_else(|| EngineMissing {
                    engine: manifest.id.clone(),
                    name: manifest.name.clone(),
                    core: Some(core.to_string()),
                    pinned: None,
                })?;
            crate::bios::link_into(system, bios, &crate::dirs::system())?;
            let config = home.join("retroarch.cfg");
            let settings = crate::retroarch::managed_settings(values);
            if own_settings {
                // Only what Irori must decide: where BIOS files and cores are.
                let essentials: Vec<_> = settings
                    .into_iter()
                    .filter(|(k, _)| matches!(*k, "system_directory" | "libretro_directory"))
                    .collect();
                let append = home.join("irori-append.cfg");
                crate::retroarch::write_config(&append, &essentials)?;
                args.extend(["--appendconfig".into(), append.into_os_string()]);
            } else {
                crate::retroarch::write_config(&config, &settings)?;
                args.extend(["--config".into(), config.into_os_string()]);
            }
            args.extend(["-L".into(), core_path.clone().into_os_string()]);
            shared.extend([
                home.clone(),
                crate::dirs::system(),
                crate::retroarch::cores_dir(),
            ]);
            if let Ok(target) = std::fs::canonicalize(bios.root.as_path()) {
                shared.push(target);
            }
        }
        _ => {
            if let Some(template) = manifest.bios_dir.get(os) {
                let dir = PathBuf::from(vars.expand(template));
                if !dir.as_os_str().is_empty()
                    && (installation.origin == Origin::Managed
                        || !template.contains("{engine_dir}"))
                {
                    crate::bios::link_into(system, bios, &dir)?;
                    shared.push(bios.root.clone());
                }
            }
            if installation.origin == Origin::Managed
                && let Some(marker) = manifest.portable_marker.get(os)
            {
                let marker = engine_dir.join(marker);
                if !own_settings && !marker.exists() {
                    std::fs::write(&marker, b"")
                        .with_context(|| format!("creating {}", marker.display()))?;
                } else if own_settings && marker.exists() {
                    let _ = std::fs::remove_file(&marker);
                }
            }
        }
    }

    expand_args(&manifest.launch.args, values, &vars, &mut args);
    let has_rom = manifest
        .launch
        .args
        .iter()
        .any(|a| matches!(a, Arg::Plain(s) if s.contains("{rom")));
    match manifest.kind {
        EngineKind::Dosbox => {
            let ext = game
                .path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase());
            if ext.as_deref() == Some("conf") {
                args.extend(["--conf".into(), game.path.clone().into_os_string()]);
            } else {
                args.push(game.path.clone().into_os_string());
                if flag(values, "exit_on_quit") && !game.path.is_dir() {
                    args.push("-exit".into());
                }
            }
        }
        EngineKind::Scummvm => {
            args.push(format!("--path={}", rom_dir.display()).into());
            args.push("--auto-detect".into());
        }
        EngineKind::Mame => {
            let machine = game
                .path
                .file_stem()
                .context("the game file has no name")?
                .to_os_string();
            args.extend(["-rompath".into(), rom_dir.clone().into_os_string(), machine]);
        }
        EngineKind::Retroarch | EngineKind::Standalone => {
            if !has_rom {
                args.push(game.path.clone().into_os_string());
            }
        }
        // The converted program is the game; it takes no game file.
        EngineKind::Relinker => {}
    }

    if manifest.kind == EngineKind::Relinker {
        let out = crate::relink::output(&manifest.id, game);
        if !out.program.is_file() {
            bail!("{} has not converted this game yet", manifest.name);
        }
        let mut spec = CommandSpec::new(&out.program).args(args);
        for (k, v) in &manifest.launch.env {
            spec = spec.env(k.clone(), vars.expand(v));
        }
        spec.cwd = Some(out.dir);
        return Ok(spec);
    }

    let mut spec = match installation.origin {
        Origin::Flatpak => {
            let id = installation
                .flatpak
                .clone()
                .context("the Flatpak has no application id")?;
            let mut spec = CommandSpec::new("flatpak").arg("run");
            shared.sort();
            shared.dedup();
            for dir in shared.iter().filter(|d| !d.as_os_str().is_empty()) {
                spec = spec.arg(format!("--filesystem={}", dir.display()));
            }
            spec.arg(id).args(args)
        }
        _ => CommandSpec::new(program_of(&installation.program)?).args(args),
    };
    for (k, v) in &manifest.launch.env {
        spec = spec.env(k.clone(), vars.expand(v));
    }
    std::fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;
    spec.cwd = Some(if game.path.is_dir() {
        game.path.clone()
    } else {
        rom_dir
    });
    Ok(spec)
}

/// The executable of `program`: itself, or the binary of an app bundle.
fn program_of(program: &Path) -> Result<PathBuf> {
    if program.extension().is_some_and(|e| e == "app") {
        if cfg!(target_os = "macos") {
            return irori_core::platform::bundle_executable(program);
        }
        bail!("{} is a macOS app", program.display());
    }
    Ok(program.to_path_buf())
}

fn flag(values: &BTreeMap<String, Value>, id: &str) -> bool {
    values.get(id).and_then(Value::as_bool).unwrap_or(false)
}

struct Vars {
    rom: String,
    rom_dir: String,
    home: String,
    engine_home: String,
    engine_dir: String,
    xdg_data: String,
}

impl Vars {
    fn expand(&self, s: &str) -> String {
        s.replace("{rom_dir}", &self.rom_dir)
            .replace("{rom}", &self.rom)
            .replace("{engine_home}", &self.engine_home)
            .replace("{engine_dir}", &self.engine_dir)
            .replace("{xdg_data}", &self.xdg_data)
            .replace("{home}", &self.home)
    }
}

fn expand_args(
    template: &[Arg],
    values: &BTreeMap<String, Value>,
    vars: &Vars,
    out: &mut Vec<OsString>,
) {
    for arg in template {
        match arg {
            Arg::Plain(s) => {
                let mut text = vars.expand(s);
                for (id, value) in values {
                    let key = format!("{{opt.{id}}}");
                    if text.contains(&key) {
                        let v = match value {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        text = text.replace(&key, &v);
                    }
                }
                out.push(text.into());
            }
            Arg::When { when, args } if flag(values, when) => expand_args(args, values, vars, out),
            Arg::Unless { unless, args } if !flag(values, unless) => {
                expand_args(args, values, vars, out)
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_without_a_build_here_say_where_they_run() {
        let _root = crate::test_root("no-build");
        let anyps5 = crate::engine("anyps5").unwrap();
        let mac = Host {
            os: Os::Macos,
            arch: "aarch64",
        };
        let e = EngineUnavailable::no_build(&anyps5, mac).unwrap();
        assert_eq!(
            e.reason,
            Unavailable::NoBuild {
                hosts: vec!["linux-x86_64".into(), "windows-x86_64".into()]
            }
        );
        assert!(e.to_string().contains("runs on linux-x86_64"));
        let linux = Host {
            os: Os::Linux,
            arch: "x86_64",
        };
        assert!(EngineUnavailable::no_build(&anyps5, linux).is_none());
        // MAME is only detected: Irori downloads it nowhere.
        let mame = crate::engine("mame").unwrap();
        let e = EngineUnavailable::no_build(&mame, linux).unwrap();
        assert_eq!(e.reason, Unavailable::NoBuild { hosts: Vec::new() });
    }

    fn installation(engine: &str, origin: Origin, program: &Path) -> Installation {
        Installation {
            engine: engine.into(),
            origin,
            version: (origin == Origin::Managed).then(|| "1.0".to_string()),
            program: program.to_path_buf(),
            flatpak: (origin == Origin::Flatpak).then(|| format!("org.example.{engine}")),
            dir: None,
            size: 0,
        }
    }

    #[test]
    fn resolves_pins_defaults_and_reports_missing_versions() {
        let _ = crate::test_root("resolve");
        let m = crate::engine("duckstation").unwrap();
        let a = installation("duckstation", Origin::Managed, Path::new("/a"));
        let b = installation("duckstation", Origin::System, Path::new("/b"));
        let installs = vec![a, b];
        assert_eq!(
            resolve(&m, &installs, None, None).unwrap().id(),
            "managed:1.0"
        );
        assert_eq!(
            resolve(&m, &installs, None, Some("system")).unwrap().id(),
            "system"
        );
        assert_eq!(
            resolve(&m, &installs, Some("system"), Some("managed:1.0"))
                .unwrap()
                .id(),
            "system"
        );
        let e = resolve(&m, &installs, Some("managed:0.9"), None).unwrap_err();
        assert_eq!(e.pinned.as_deref(), Some("0.9"));
        assert!(e.to_string().contains("DuckStation 0.9"));
        assert!(resolve(&m, &[], None, None).is_err());
    }

    #[test]
    fn standalone_command_follows_the_template() {
        let root = crate::test_root("launch-standalone");
        let rom = root.join("psx/Game (USA).cue");
        std::fs::create_dir_all(rom.parent().unwrap()).unwrap();
        std::fs::write(&rom, b"FILE").unwrap();
        let m = crate::engine("duckstation").unwrap();
        let program = root.join("duckstation");
        let inst = installation("duckstation", Origin::System, &program);
        let values = m.option_values(&BTreeMap::new());
        let spec = command(&LaunchInput {
            manifest: &m,
            installation: &inst,
            game: &GameFile {
                path: rom.clone(),
                system: "psx".into(),
            },
            core: None,
            values: &values,
            bios: &BiosIndex::default(),
            own_settings: false,
        })
        .unwrap();
        assert_eq!(spec.program, program);
        let args: Vec<String> = spec
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["-batch", "-fullscreen", "--", rom.to_str().unwrap()]);
        assert_eq!(spec.cwd.as_deref(), rom.parent());

        let mut off = values.clone();
        off.insert("fullscreen".into(), Value::Bool(false));
        let spec = command(&LaunchInput {
            manifest: &m,
            installation: &inst,
            game: &GameFile {
                path: rom.clone(),
                system: "psx".into(),
            },
            core: None,
            values: &off,
            bios: &BiosIndex::default(),
            own_settings: false,
        })
        .unwrap();
        assert!(!spec.args.iter().any(|a| a == "-fullscreen"));
    }

    #[test]
    fn retroarch_needs_its_core_and_flatpaks_see_the_rom_folder() {
        let root = crate::test_root("launch-retroarch");
        let rom = root.join("roms/snes/Game.sfc");
        std::fs::create_dir_all(rom.parent().unwrap()).unwrap();
        std::fs::write(&rom, b"x").unwrap();
        let m = crate::engine("retroarch").unwrap();
        let inst = installation("retroarch", Origin::Flatpak, Path::new("flatpak"));
        let values = m.option_values(&BTreeMap::new());
        let game = GameFile {
            path: rom.clone(),
            system: "snes".into(),
        };
        let input = LaunchInput {
            manifest: &m,
            installation: &inst,
            game: &game,
            core: Some("snes9x"),
            values: &values,
            bios: &BiosIndex::default(),
            own_settings: false,
        };
        let err = command(&input).unwrap_err();
        let missing = EngineMissing::of(&err).expect("typed");
        assert_eq!(missing.core.as_deref(), Some("snes9x"));

        let Some(file) = crate::retroarch::core_file("snes9x") else {
            return;
        };
        let cores = crate::retroarch::cores_dir();
        std::fs::create_dir_all(&cores).unwrap();
        std::fs::write(cores.join(file), b"core").unwrap();
        let spec = command(&input).unwrap();
        assert_eq!(spec.program, PathBuf::from("flatpak"));
        let args: Vec<String> = spec
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "run");
        assert!(args.contains(&format!("--filesystem={}", rom.parent().unwrap().display())));
        let id = args
            .iter()
            .position(|a| a == "org.example.retroarch")
            .unwrap();
        assert_eq!(args[id + 1], "--config");
        assert!(args.contains(&"-L".to_string()));
        assert!(args.contains(&"--fullscreen".to_string()));
        assert_eq!(args.last().unwrap(), rom.to_str().unwrap());
        let cfg = std::fs::read_to_string(crate::retroarch::home().join("retroarch.cfg")).unwrap();
        assert!(cfg.contains("system_directory = "));
    }

    #[test]
    fn dosbox_runs_the_program_and_scummvm_detects_the_folder() {
        let root = crate::test_root("launch-folders");
        let game = root.join("dos/KEEN");
        std::fs::create_dir_all(&game).unwrap();
        let exe = game.join("KEEN1.EXE");
        std::fs::write(&exe, b"MZ").unwrap();
        let m = crate::engine("dosbox-staging").unwrap();
        let inst = installation(
            "dosbox-staging",
            Origin::System,
            Path::new("/usr/bin/dosbox"),
        );
        let values = m.option_values(&BTreeMap::new());
        let spec = command(&LaunchInput {
            manifest: &m,
            installation: &inst,
            game: &GameFile {
                path: exe.clone(),
                system: "dos".into(),
            },
            core: None,
            values: &values,
            bios: &BiosIndex::default(),
            own_settings: false,
        })
        .unwrap();
        let args: Vec<String> = spec
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["--fullscreen", exe.to_str().unwrap(), "-exit"]);

        let m = crate::engine("scummvm").unwrap();
        let inst = installation("scummvm", Origin::System, Path::new("/usr/bin/scummvm"));
        let values = m.option_values(&BTreeMap::new());
        let spec = command(&LaunchInput {
            manifest: &m,
            installation: &inst,
            game: &GameFile {
                path: game.clone(),
                system: "scummvm".into(),
            },
            core: None,
            values: &values,
            bios: &BiosIndex::default(),
            own_settings: false,
        })
        .unwrap();
        let args: Vec<String> = spec
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args.last().unwrap(), "--auto-detect");
        assert_eq!(args[args.len() - 2], format!("--path={}", game.display()));
        assert_eq!(spec.cwd.as_deref(), Some(game.as_path()));
    }
}
