//! Plan an import of an existing Lutris setup (`docs/design/engines.md`
//! section 7, `docs/parity/lutris.md` sections 2 and 3): its installed
//! games, favourites, hidden games, categories, playtime, last played dates
//! and cover art.
//!
//! This module only reads. [`LutrisDirs::discover`] finds the native and
//! Flatpak installations, and [`plan`] reads `pga.db` (read-only and
//! immutable, so a running Lutris is not disturbed) and the YAML config
//! cascade, and returns a [`LutrisPlan`] saying what each game becomes in
//! Irori and, for every game that is not imported, why. The caller applies
//! the plan to its services.

mod config;
mod db;
mod emulators;
mod paths;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use std::collections::BTreeMap;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use config::{Cascade, resolve_path};
use db::DbGame;
pub use paths::LutrisDirs;

/// Lutris services whose games belong to a store Irori has natively (or, for
/// Humble Bundle, deliberately does not); such games are pointed at the store
/// instead of being imported twice.
const STORE_SERVICES: [&str; 6] = ["gog", "egs", "amazon", "itchio", "zoom", "humblebundle"];

/// Everything one Lutris installation would import.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LutrisPlan {
    /// Every game in the database, in database order, including skipped ones.
    pub games: Vec<PlannedGame>,
    /// Files that could not be read; the plan is still usable.
    pub warnings: Vec<String>,
}

/// One Lutris game and what it becomes in Irori.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedGame {
    /// Lutris's row id.
    pub lutris_id: i64,
    pub name: String,
    pub slug: String,
    /// The Lutris runner id (`wine`, `libretro`, …); empty when unset.
    pub runner: String,
    pub kind: ImportKind,
    pub favourite: bool,
    pub hidden: bool,
    /// User categories, without Lutris's reserved `favorite`, `all` and
    /// dot-prefixed ones.
    pub categories: Vec<String>,
    /// Lutris's playtime (hours) in whole minutes, rounded.
    pub playtime_minutes: u64,
    /// RFC 3339, UTC; `None` when never played.
    pub last_played: Option<String>,
    /// Cover art or, failing that, the banner.
    pub cover: Option<PathBuf>,
}

/// What a Lutris game becomes in Irori.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportKind {
    /// Runner `wine`: a sideloaded Windows app.
    Windows {
        exe: PathBuf,
        args: Option<String>,
        working_dir: Option<PathBuf>,
        prefix: Option<PathBuf>,
        /// Lutris's Wine build name (`wine-ge-8-26-x86_64`); the caller maps
        /// it to a build Irori knows.
        wine_version: Option<String>,
        /// `win32` or `win64`, when set.
        arch: Option<String>,
        env: BTreeMap<String, String>,
    },
    /// Runner `linux`: a sideloaded native app.
    Native {
        exe: PathBuf,
        args: Option<String>,
        working_dir: Option<PathBuf>,
        env: BTreeMap<String, String>,
    },
    /// An emulator runner: an entry in the ROM library.
    Emulated {
        /// Irori system id (`snes`, `psx`, `dos`).
        system: String,
        /// Irori engine reference: `duckstation` or `retroarch:<core>`.
        engine: String,
        /// The ROM, disc image, program or (ScummVM) game folder. For MAME a
        /// bare machine name such as `pacman`.
        rom: PathBuf,
        /// ScummVM's game id, when set.
        game_id: Option<String>,
    },
    /// Runner `steam`, or Steam for Windows started with `-applaunch`: in
    /// Irori's Steam library, matched by app id. `args` are the game's own
    /// arguments, which Irori passes when it starts the game.
    Steam {
        app_id: String,
        args: Option<String>,
    },
    /// A game of a store Irori has natively; the user is pointed at it.
    StoreGame {
        /// Lutris's service id: `gog`, `egs`, `amazon`, `itchio`, `zoom` or
        /// `humblebundle`.
        service: String,
        service_id: String,
    },
    /// Not imported.
    Skipped { reason: SkipReason },
}

/// The app id and the game's own arguments when `exe` is Steam's Windows
/// client started with `-applaunch <id>`. Steam's own flags before it (such
/// as `-no-cef-sandbox`) are left out.
fn steam_for_windows(exe: &str, args: Option<&str>) -> Option<(String, Option<String>)> {
    let name = exe.rsplit(['/', '\\']).next()?;
    if !name.eq_ignore_ascii_case("steam.exe") {
        return None;
    }
    let args = irori_core::process::split_args(args?);
    let at = args
        .iter()
        .position(|a| a.eq_ignore_ascii_case("-applaunch"))?;
    let app_id = args
        .get(at + 1)
        .filter(|id| id.chars().all(|c| c.is_ascii_digit()))?;
    let rest = &args[at + 2..];
    let rest = (!rest.is_empty()).then(|| {
        rest.iter()
            .map(|a| irori_core::process::shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    });
    Some((app_id.clone(), rest))
}

/// Why a game is not imported. Each variant maps to one translatable
/// message; [`SkipReason::id`] is a stable key for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Lutris lists it but it is not installed.
    NotInstalled,
    /// Installed, but no runner is set.
    NoRunner,
    /// A runner Irori has no equivalent for.
    UnknownRunner(String),
    /// The game's config names no main file (or Steam app id).
    MissingEntryPoint,
    /// The main file is named but does not exist.
    FileMissing(PathBuf),
    /// A libretro game without a core.
    MissingCore,
    /// A libretro core Irori cannot place on a system.
    UnknownCore(String),
}

impl SkipReason {
    /// Stable id, for translation keys and logs.
    pub fn id(&self) -> &'static str {
        match self {
            SkipReason::NotInstalled => "notInstalled",
            SkipReason::NoRunner => "noRunner",
            SkipReason::UnknownRunner(_) => "unknownRunner",
            SkipReason::MissingEntryPoint => "missingEntryPoint",
            SkipReason::FileMissing(_) => "fileMissing",
            SkipReason::MissingCore => "missingCore",
            SkipReason::UnknownCore(_) => "unknownCore",
        }
    }
}

impl ImportKind {
    fn skipped(reason: SkipReason) -> Self {
        ImportKind::Skipped { reason }
    }

    /// Whether the game is left out.
    pub fn is_skipped(&self) -> bool {
        matches!(self, ImportKind::Skipped { .. })
    }
}

/// Whether `runner` is a Lutris emulator runner Irori maps to an engine.
pub fn is_emulator_runner(runner: &str) -> bool {
    emulators::is_emulator(runner)
}

/// The Irori system and engine (`duckstation`, `retroarch:<core>`) for a
/// game of the Lutris emulator `runner` whose file is `file`. `core` is a
/// libretro game's core and `dolphin_platform` Dolphin's `platform` option
/// (`0` GameCube, `1` Wii). Install scripts use this for the games they
/// produce; `None` means `runner` is not an emulator runner.
pub fn emulator_target(
    runner: &str,
    file: &Path,
    core: Option<&str>,
    dolphin_platform: Option<&str>,
) -> Option<Result<(String, String), SkipReason>> {
    if !emulators::is_emulator(runner) {
        return None;
    }
    let hints = emulators::Hints {
        rom: file,
        platform: None,
        dolphin_platform,
        core,
    };
    Some(emulators::map(runner, &hints).map(|t| (t.system.to_string(), t.engine)))
}

/// Plan the import of one Lutris installation. Reads files directly; call it
/// off the async runtime (or use [`plan_async`]).
pub fn plan(dirs: &LutrisDirs) -> Result<LutrisPlan> {
    if !dirs.pga_path.is_file() {
        anyhow::bail!(
            "No Lutris game database found at {}",
            dirs.pga_path.display()
        );
    }
    let data = db::read(&dirs.pga_path)?;
    let mut out = LutrisPlan::default();
    for game in &data.games {
        let names = data
            .categories
            .get(&game.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let planned = plan_game(dirs, game, names, &mut out.warnings);
        out.games.push(planned);
    }
    Ok(out)
}

/// [`plan`] on a blocking thread.
pub async fn plan_async(dirs: LutrisDirs) -> Result<LutrisPlan> {
    tokio::task::spawn_blocking(move || plan(&dirs))
        .await
        .context("Lutris import task failed")?
}

fn plan_game(
    dirs: &LutrisDirs,
    game: &DbGame,
    category_names: &[String],
    warnings: &mut Vec<String>,
) -> PlannedGame {
    let slug = game.slug.clone().unwrap_or_default();
    let name = game.name.clone().unwrap_or_else(|| slug.clone());
    let runner = game.runner.clone().unwrap_or_default();
    let favourite = category_names.iter().any(|c| c == "favorite");
    let hidden = game.hidden || category_names.iter().any(|c| c == ".hidden");
    let categories = category_names
        .iter()
        .filter(|c| !matches!(c.as_str(), "favorite" | "all") && !c.starts_with('.'))
        .cloned()
        .collect();
    let kind = classify(dirs, game, &runner, warnings);
    PlannedGame {
        lutris_id: game.id,
        name,
        cover: cover(dirs, &slug),
        slug,
        runner,
        kind,
        favourite,
        hidden,
        categories,
        playtime_minutes: hours_to_minutes(game.playtime.unwrap_or_default()),
        last_played: game.lastplayed.and_then(rfc3339),
    }
}

fn classify(
    dirs: &LutrisDirs,
    game: &DbGame,
    runner: &str,
    warnings: &mut Vec<String>,
) -> ImportKind {
    if !game.installed {
        return ImportKind::skipped(SkipReason::NotInstalled);
    }
    if let (Some(service), Some(service_id)) = (&game.service, &game.service_id)
        && STORE_SERVICES.contains(&service.as_str())
    {
        return ImportKind::StoreGame {
            service: service.clone(),
            service_id: service_id.clone(),
        };
    }
    if runner.is_empty() {
        return ImportKind::skipped(SkipReason::NoRunner);
    }
    if runner != "wine" && runner != "linux" && runner != "steam" && !emulators::is_emulator(runner)
    {
        return ImportKind::skipped(SkipReason::UnknownRunner(runner.to_string()));
    }

    let cascade = Cascade::load(dirs, runner, game.configpath.as_deref(), warnings);
    let directory = game
        .directory
        .as_deref()
        .map(|d| resolve_path(d, None, &dirs.home));
    let resolve = |raw: &str| resolve_path(raw, directory.as_deref(), &dirs.home);

    if runner == "steam" {
        let app_id = cascade.game_str("appid").or_else(|| {
            (game.service.as_deref() == Some("steam"))
                .then(|| game.service_id.clone())
                .flatten()
        });
        return match app_id {
            Some(app_id) => ImportKind::Steam {
                app_id,
                args: cascade.game_str("args").filter(|a| !a.trim().is_empty()),
            },
            None => ImportKind::skipped(SkipReason::MissingEntryPoint),
        };
    }

    let Some(raw_entry) = cascade.entry_point() else {
        return ImportKind::skipped(SkipReason::MissingEntryPoint);
    };
    // MAME names a machine (`pacman`) rather than a file when the main file
    // has no extension (`game.py`); there is nothing on disk to check.
    let machine_name = runner == "mame" && !raw_entry.contains(['.', '/', '\\']);
    let entry = if machine_name {
        PathBuf::from(&raw_entry)
    } else {
        resolve(&raw_entry)
    };
    if !machine_name && !entry.exists() {
        return ImportKind::skipped(SkipReason::FileMissing(entry));
    }

    let args = cascade.game_str("args");
    // Lutris's "Steam for Windows" runs a game through Steam in a Wine
    // prefix; Irori starts the same game through the Steam it lists.
    if runner == "wine"
        && let Some((app_id, args)) = steam_for_windows(&raw_entry, args.as_deref())
    {
        return ImportKind::Steam { app_id, args };
    }
    let working_dir = cascade.game_str("working_dir").map(|w| resolve(&w));
    let env = cascade.env();
    match runner {
        "wine" => ImportKind::Windows {
            exe: entry,
            args,
            working_dir,
            prefix: cascade.game_str("prefix").map(|p| resolve(&p)),
            wine_version: cascade.runner_str("version"),
            arch: cascade
                .game_str("arch")
                .or_else(|| cascade.runner_str("arch")),
            env,
        },
        "linux" => ImportKind::Native {
            exe: entry,
            args,
            working_dir,
            env,
        },
        _ => {
            let core = cascade.game_str("core");
            let dolphin_platform = cascade.game_str("platform");
            let hints = emulators::Hints {
                rom: &entry,
                platform: game.platform.as_deref(),
                dolphin_platform: dolphin_platform.as_deref(),
                core: core.as_deref(),
            };
            match emulators::map(runner, &hints) {
                Ok(target) => ImportKind::Emulated {
                    system: target.system.to_string(),
                    engine: target.engine,
                    rom: entry,
                    game_id: (runner == "scummvm")
                        .then(|| cascade.game_str("game_id"))
                        .flatten(),
                },
                Err(reason) => ImportKind::skipped(reason),
            }
        }
    }
}

/// Lutris stores fractional hours; Irori counts whole minutes.
pub(crate) fn hours_to_minutes(hours: f64) -> u64 {
    if hours.is_finite() && hours > 0.0 {
        (hours * 60.0).round() as u64
    } else {
        0
    }
}

fn rfc3339(unix_seconds: i64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp(unix_seconds)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

/// `coverart/<slug>.*`, then `banners/<slug>.*`, each in the data folder and
/// then the cache folder (where newer Lutris versions keep them).
fn cover(dirs: &LutrisDirs, slug: &str) -> Option<PathBuf> {
    if slug.is_empty() || slug.contains(['/', '\\']) || slug.starts_with('.') {
        return None;
    }
    let roots: [&Path; 2] = [&dirs.data_dir, &dirs.cache_dir];
    ["coverart", "banners"]
        .into_iter()
        .flat_map(|kind| roots.into_iter().map(move |root| root.join(kind)))
        .flat_map(|folder| {
            ["jpg", "png", "webp"]
                .into_iter()
                .map(move |ext| folder.join(format!("{slug}.{ext}")))
        })
        .find(|p| p.is_file())
}
