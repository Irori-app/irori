//! Maps Lutris emulator runners and libretro cores to Irori's system and
//! engine ids (`assets/engines/systems.json`, `docs/design/engines.md`
//! section 3.2). Some systems (`ps2`, `3ds`, `arcade`) and engines (`pcsx2`,
//! `mgba`, `melonds`, `azahar`, `mame`) arrive in a later phase; they are
//! emitted anyway so the import is ready for them.

use std::path::Path;

use super::SkipReason;

/// How one emulator game maps to Irori.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmulatorTarget {
    pub system: &'static str,
    pub engine: String,
}

/// Inputs that pick the system when a runner plays more than one.
pub(crate) struct Hints<'a> {
    pub rom: &'a Path,
    /// The `platform` column ("Nintendo Wii").
    pub platform: Option<&'a str>,
    /// Dolphin's game option `platform`: `0` GameCube, `1` Wii.
    pub dolphin_platform: Option<&'a str>,
    /// The libretro `core`.
    pub core: Option<&'a str>,
}

/// Whether `runner` is an emulator runner this module maps.
pub(crate) fn is_emulator(runner: &str) -> bool {
    matches!(
        runner,
        "libretro"
            | "duckstation"
            | "ppsspp"
            | "dolphin"
            | "dosbox"
            | "scummvm"
            | "pcsx2"
            | "mgba"
            | "melonds"
            | "azahar"
            | "citra"
            | "mame"
    )
}

/// The Irori system and engine for a Lutris emulator game.
pub(crate) fn map(runner: &str, hints: &Hints<'_>) -> Result<EmulatorTarget, SkipReason> {
    let ext = extension(hints.rom);
    let standalone = |system: &'static str, engine: &str| EmulatorTarget {
        system,
        engine: engine.to_string(),
    };
    Ok(match runner {
        "libretro" => {
            let core = hints.core.ok_or(SkipReason::MissingCore)?;
            let system =
                core_system(core, &ext).ok_or_else(|| SkipReason::UnknownCore(core.to_string()))?;
            EmulatorTarget {
                system,
                engine: format!("retroarch:{core}"),
            }
        }
        "duckstation" => standalone("psx", "duckstation"),
        "ppsspp" => standalone("psp", "ppsspp"),
        "dolphin" => standalone(dolphin_system(hints, &ext), "dolphin"),
        "dosbox" => standalone("dos", "dosbox-staging"),
        "scummvm" => standalone("scummvm", "scummvm"),
        "pcsx2" => standalone("ps2", "pcsx2"),
        "mgba" => standalone(game_boy_family(&ext, "gba"), "mgba"),
        "melonds" => standalone("nds", "melonds"),
        "azahar" | "citra" => standalone("3ds", "azahar"),
        "mame" => standalone("arcade", "mame"),
        other => return Err(SkipReason::UnknownRunner(other.to_string())),
    })
}

/// The system a libretro core plays, refined by the ROM's extension for cores
/// that cover a family of machines.
fn core_system(core: &str, ext: &str) -> Option<&'static str> {
    Some(match core {
        "mesen-s" => "snes",
        c if c.starts_with("snes9x") || c.starts_with("bsnes") => "snes",
        "mesen" | "nestopia" | "fceumm" | "quicknes" => "nes",
        "mupen64plus_next" | "mupen64plus" | "parallel_n64" => "n64",
        "gambatte" | "sameboy" | "gearboy" | "tgbdual" => game_boy_family(ext, "gb"),
        "mgba" | "vba_next" | "vbam" | "gpsp" => game_boy_family(ext, "gba"),
        "melonds" | "melondsds" | "desmume" | "desmume2015" => "nds",
        "genesis_plus_gx" | "genesis_plus_gx_wide" | "picodrive" | "blastem" => match ext {
            "sms" => "sms",
            "gg" => "gamegear",
            "cue" | "chd" | "iso" | "m3u" => "segacd",
            _ => "genesis",
        },
        "mednafen_pce_fast" | "mednafen_pce" | "mednafen_supergrafx" => "pce",
        "swanstation" | "mednafen_psx_hw" | "mednafen_psx" | "pcsx_rearmed" | "duckstation" => {
            "psx"
        }
        "ppsspp" => "psp",
        "dosbox_pure" | "dosbox_core" => "dos",
        "scummvm" => "scummvm",
        _ => return None,
    })
}

/// Game Boy cores read all three handhelds; the extension says which.
fn game_boy_family(ext: &str, default: &'static str) -> &'static str {
    match ext {
        "gb" => "gb",
        "gbc" => "gbc",
        "gba" => "gba",
        _ => default,
    }
}

fn dolphin_system(hints: &Hints<'_>, ext: &str) -> &'static str {
    match hints.dolphin_platform {
        Some("1") => return "wii",
        Some("0") => return "gamecube",
        _ => {}
    }
    if let Some(platform) = hints.platform.map(str::to_ascii_lowercase) {
        if platform.contains("wii") {
            return "wii";
        }
        if platform.contains("gamecube") {
            return "gamecube";
        }
    }
    match ext {
        "wbfs" | "wad" => "wii",
        _ => "gamecube",
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}
