//! Integrations beyond the stores: desktop/menu/Steam shortcuts, the
//! `ember://` / `heroic://` URL protocol, extra game information from wikis,
//! AreWeAntiCheatYet data, Heroic's known fixes, SteamGridDB, store discounts,
//! log helpers and importing launcher-level data from Heroic and Lutris.
//!
//! The function signatures in [`shortcuts`] (`ShortcutTargets`, `add`,
//! `remove`) are the contract used by `ember-services`; keep them stable.
//!
//! Every module keeps its caches on disk under `Paths::get().cache` and never
//! writes into other services' state: functions return typed values that the
//! caller applies.

pub mod anticheat;
pub mod discounts;
pub mod heroic_import;
pub mod known_fixes;
pub mod logs;
pub mod lutris_import;
pub mod protocol;
pub mod releases;
pub mod shortcuts;
pub mod steamgrid;
pub mod wiki;

mod util;
