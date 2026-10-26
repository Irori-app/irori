//! Data the game page shows but cannot fetch itself: compatibility reports
//! from wikis and anticheat databases, and store-specific operations of
//! concrete store crates. The shell may install providers with
//! [`install_hooks`]; without a compatibility provider the page reads
//! `ember_extras` directly, and without a branch-password provider the
//! private-branch field is hidden. Concrete store handles (Epic EOS overlay,
//! GOG REDmod) come from [`install_store_handles`]; without them those
//! sections are hidden.

use std::sync::Arc;

use anyhow::Result;
use ember_core::{AppSettings, GameId, GameInfo};
use ember_epic::EpicStore;
use ember_extras::anticheat::{self, AntiCheatInfo, AntiCheatStatus, InstallGate};
use ember_gog::GogStore;
use ember_services::MultiAccountStore;
use ember_ui::i18n::t;
use gpui_kit::{App, Global, SharedString, Task};

/// Steam Deck verification category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteamDeckStatus {
    Unknown,
    Unsupported,
    Playable,
    Verified,
}

impl SteamDeckStatus {
    /// Label in the interface language.
    pub fn label(self) -> SharedString {
        t(match self {
            SteamDeckStatus::Unknown => "game.compat.deck.unknown",
            SteamDeckStatus::Unsupported => "game.compat.deck.unsupported",
            SteamDeckStatus::Playable => "game.compat.deck.playable",
            SteamDeckStatus::Verified => "game.compat.deck.verified",
        })
    }
}

/// How a game's anticheat behaves on Linux (AreWeAntiCheatYet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnticheatStatus {
    Supported,
    Running,
    Planned,
    Broken,
    Denied,
    Unknown,
}

impl AnticheatStatus {
    /// Label in the interface language.
    pub fn label(self) -> SharedString {
        t(match self {
            AnticheatStatus::Supported => "game.compat.anticheat_status.supported",
            AnticheatStatus::Running => "game.compat.anticheat_status.running",
            AnticheatStatus::Planned => "game.compat.anticheat_status.planned",
            AnticheatStatus::Broken => "game.compat.anticheat_status.broken",
            AnticheatStatus::Denied => "game.compat.anticheat_status.denied",
            AnticheatStatus::Unknown => "game.compat.anticheat_status.unknown",
        })
    }

    /// The `ember_extras` status this one stands for.
    fn extras(self) -> AntiCheatStatus {
        match self {
            AnticheatStatus::Supported => AntiCheatStatus::Supported,
            AnticheatStatus::Running => AntiCheatStatus::Running,
            AnticheatStatus::Planned => AntiCheatStatus::Planned,
            AnticheatStatus::Broken => AntiCheatStatus::Broken,
            AnticheatStatus::Denied => AntiCheatStatus::Denied,
            AnticheatStatus::Unknown => AntiCheatStatus::Unknown,
        }
    }

    /// Multiplayer is expected to fail; installs are gated. Decided by
    /// `ember_extras::anticheat` (the single source of truth).
    pub fn blocks_multiplayer(self) -> bool {
        self.extras().is_broken()
    }
}

/// Anticheat report of one game.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AnticheatReport {
    pub status: AnticheatStatus,
    /// Anticheat products in use (empty when removed).
    pub anticheats: Vec<String>,
    pub notes: Vec<String>,
    /// Latest reference link for the status.
    pub reference: Option<String>,
}

impl AnticheatReport {
    pub fn new(status: AnticheatStatus) -> Self {
        Self {
            status,
            anticheats: Vec::new(),
            notes: Vec::new(),
            reference: None,
        }
    }
}

/// Install gate for a game with this anticheat report, decided by
/// [`ember_extras::anticheat::blocks_install`] so every surface agrees.
pub(crate) fn install_gate(report: Option<&AnticheatReport>, app: &AppSettings) -> InstallGate {
    let info = report.map(|r| AntiCheatInfo {
        status: r.status.extras(),
        anticheats: r.anticheats.clone(),
        ..Default::default()
    });
    anticheat::blocks_install(info.as_ref(), app)
}

/// HowLongToBeat hours.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct HowLongToBeat {
    pub main: Option<f32>,
    pub main_extra: Option<f32>,
    pub completionist: Option<f32>,
    pub url: Option<String>,
}

/// A review score (0–100) with the page it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReviewScore {
    pub source: String,
    pub score: u8,
    pub url: Option<String>,
}

impl ReviewScore {
    pub fn new(source: impl Into<String>, score: u8, url: Option<String>) -> Self {
        Self {
            source: source.into(),
            score,
            url,
        }
    }
}

/// Everything the compatibility panel shows. Every field is optional; the
/// panel renders what is present.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct CompatibilityInfo {
    /// ProtonDB tier ("platinum", "gold"…).
    pub protondb_tier: Option<String>,
    pub protondb_url: Option<String>,
    pub steam_deck: Option<SteamDeckStatus>,
    pub anticheat: Option<AnticheatReport>,
    pub how_long_to_beat: Option<HowLongToBeat>,
    pub pcgamingwiki_url: Option<String>,
    /// Review scores (Metacritic, OpenCritic, IGDB…).
    pub scores: Vec<ReviewScore>,
    /// Known-fix notes applied at launch (winetricks, env vars…), as text.
    pub known_fixes: Vec<String>,
    /// External page with important information about the game.
    pub wiki_link: Option<String>,
}

impl CompatibilityInfo {
    pub fn is_empty(&self) -> bool {
        self == &CompatibilityInfo::default()
    }
}

/// Fetches compatibility data for a game (wikis, anticheat, known fixes).
pub type CompatibilityFn = Arc<dyn Fn(Arc<GameInfo>, &mut App) -> Task<Result<CompatibilityInfo>>>;

/// Stores (or clears, with an empty string) a GOG private-branch password.
pub type BranchPasswordFn = Arc<dyn Fn(GameId, SharedString, &mut App) -> Task<Result<()>>>;

/// Providers the shell wires to concrete crates.
#[derive(Clone, Default)]
pub struct GameHooks {
    compatibility: Option<CompatibilityFn>,
    branch_password: Option<BranchPasswordFn>,
}

impl GameHooks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Provide compatibility data (typically from `ember_extras`).
    pub fn with_compatibility(mut self, f: CompatibilityFn) -> Self {
        self.compatibility = Some(f);
        self
    }

    /// Provide GOG private-branch passwords (typically `GogStore::set_private_branch_password`).
    pub fn with_branch_password(mut self, f: BranchPasswordFn) -> Self {
        self.branch_password = Some(f);
        self
    }
}

impl Global for GameHooks {}

/// Install the providers. Call once at startup; later calls replace them.
pub fn install_hooks(hooks: GameHooks, cx: &mut App) {
    cx.set_global(hooks);
}

/// Concrete store handles for store-specific sections of the game page.
#[derive(Clone, Default)]
struct StoreHandles {
    epic: Option<Arc<EpicStore>>,
    gog: Option<Arc<MultiAccountStore<GogStore>>>,
}

impl Global for StoreHandles {}

/// Give the game page the concrete Epic and GOG stores (the same instances
/// the backend drives; Epic's main account, every GOG account). Enables the
/// EOS overlay section on Epic games and REDmod management for Cyberpunk
/// 2077 on GOG. Call once at startup; later calls replace the handles.
pub fn install_store_handles(
    epic: Option<Arc<EpicStore>>,
    gog: Option<Arc<MultiAccountStore<GogStore>>>,
    cx: &mut App,
) {
    cx.set_global(StoreHandles { epic, gog });
}

pub(crate) fn epic_store(cx: &App) -> Option<Arc<EpicStore>> {
    cx.try_global::<StoreHandles>().and_then(|h| h.epic.clone())
}

/// GOG's main account, for work not tied to a game.
pub(crate) fn gog_store(cx: &App) -> Option<Arc<GogStore>> {
    cx.try_global::<StoreHandles>()
        .and_then(|h| h.gog.as_ref())
        .map(|gog| gog.main())
}

/// The GOG account `app_name` belongs to.
pub(crate) fn gog_store_for(app_name: &str, cx: &App) -> Option<Arc<GogStore>> {
    cx.try_global::<StoreHandles>()
        .and_then(|h| h.gog.as_ref())
        .map(|gog| gog.store_for(app_name))
}

/// Compatibility data of a game: from the installed provider, else from
/// `ember_extras` (wikis, AreWeAntiCheatYet, known fixes).
pub(crate) fn fetch_compatibility(
    game: Arc<GameInfo>,
    cx: &mut App,
) -> Option<Task<Result<CompatibilityInfo>>> {
    if let Some(f) = cx
        .try_global::<GameHooks>()
        .and_then(|h| h.compatibility.clone())
    {
        return Some(f(game, cx));
    }
    Some(ember_ui::run(cx, crate::compat_source::load(game)))
}

/// Whether private-branch passwords can be set.
pub(crate) fn has_branch_password(cx: &App) -> bool {
    cx.try_global::<GameHooks>()
        .is_some_and(|h| h.branch_password.is_some())
}

pub(crate) fn set_branch_password(
    id: GameId,
    password: SharedString,
    cx: &mut App,
) -> Task<Result<()>> {
    match cx
        .try_global::<GameHooks>()
        .and_then(|h| h.branch_password.clone())
    {
        Some(f) => f(id, password, cx),
        None => Task::ready(Err(anyhow::anyhow!(t(
            "game.error.private_branches_unavailable"
        )))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_gate_follows_extras() {
        let mut app = AppSettings {
            allow_installation_broken_anticheat: false,
            ..Default::default()
        };
        let report = |s| AnticheatReport::new(s);
        assert_eq!(install_gate(None, &app), InstallGate::Allowed);
        for s in [
            AnticheatStatus::Supported,
            AnticheatStatus::Running,
            AnticheatStatus::Planned,
        ] {
            assert_eq!(install_gate(Some(&report(s)), &app), InstallGate::Allowed);
            assert!(!s.blocks_multiplayer());
        }
        for s in [
            AnticheatStatus::Broken,
            AnticheatStatus::Denied,
            AnticheatStatus::Unknown,
        ] {
            assert_eq!(
                install_gate(Some(&report(s)), &app),
                if cfg!(windows) {
                    InstallGate::Allowed
                } else {
                    InstallGate::Blocked
                }
            );
            assert!(s.blocks_multiplayer());
        }
        app.allow_installation_broken_anticheat = true;
        assert_eq!(
            install_gate(Some(&report(AnticheatStatus::Denied)), &app),
            if cfg!(windows) {
                InstallGate::Allowed
            } else {
                InstallGate::Confirm
            }
        );
    }
}
