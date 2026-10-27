//! The Lutris installer's view state as plain data: which step follows
//! which, when the consent screen lets the install start, what the user
//! approved, and the checks that keep a script from running at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use irori_core::{GameId, GameInfo, Runner};
use irori_scripts::{Approvals, Consent, InstallerListing, Script, ScriptError};

/// The installer's steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Search lutris.net, or open a script file.
    Find,
    /// Pick one of the game's scripts.
    Version,
    /// Where the game goes.
    Destination,
    /// Files the user provides; what is downloaded.
    Files,
    /// What the script will do, approved by the user.
    Consent,
    Progress,
    Done,
    Failed,
}

impl Step {
    /// The step after this one. The Files step is skipped when the script
    /// needs no files.
    pub fn next(self, has_files: bool) -> Step {
        match self {
            Step::Find => Step::Version,
            Step::Version => Step::Destination,
            Step::Destination if has_files => Step::Files,
            Step::Destination | Step::Files => Step::Consent,
            Step::Consent => Step::Progress,
            Step::Progress | Step::Done | Step::Failed => self,
        }
    }

    /// The step "Back" returns to; `None` where there is no going back.
    pub fn back(self, has_files: bool) -> Option<Step> {
        Some(match self {
            Step::Version => Step::Find,
            Step::Destination => Step::Version,
            Step::Files => Step::Destination,
            Step::Consent if has_files => Step::Files,
            Step::Consent => Step::Destination,
            Step::Failed => Step::Consent,
            Step::Find | Step::Progress | Step::Done => return None,
        })
    }
}

/// One script the user can pick, with its attribution.
#[derive(Debug, Clone)]
pub struct Choice {
    /// The installer's name ("GOG", "Setup"…).
    pub version: String,
    pub runner: String,
    pub author: Option<String>,
    pub date: Option<String>,
    pub description: Option<String>,
    pub notes: Option<String>,
    /// The lutris.net page, or the local file.
    pub source: String,
    /// The script, or why it was refused.
    pub script: Result<Arc<Script>, ScriptError>,
}

impl Choice {
    pub fn from_listing(listing: &InstallerListing) -> Self {
        Self {
            version: listing.version.clone(),
            runner: listing.runner.clone(),
            author: listing.author.clone(),
            date: listing.date().map(day),
            description: listing.description.clone().filter(|d| !d.trim().is_empty()),
            notes: listing.notes.clone().filter(|d| !d.trim().is_empty()),
            source: listing.page_url.clone(),
            script: listing.script().map(Arc::new),
        }
    }

    pub fn from_script(script: Script, file: &Path) -> Self {
        let info = &script.info;
        Self {
            version: info.version.clone(),
            runner: info.runner.clone(),
            author: info.author.clone(),
            date: info
                .updated_at
                .as_deref()
                .or(info.created_at.as_deref())
                .map(day),
            description: info.description.clone().filter(|d| !d.trim().is_empty()),
            notes: info.notes.clone().filter(|d| !d.trim().is_empty()),
            source: file.display().to_string(),
            script: Ok(Arc::new(script)),
        }
    }
}

/// `2021-03-04T05:06:07Z` → `2021-03-04`.
fn day(date: &str) -> String {
    date.split('T').next().unwrap_or(date).to_string()
}

/// Why a parsed script still can't be installed by Irori.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// It adds to another game (`extends`) or installs into one
    /// (`requires`), which Irori doesn't manage for Lutris games.
    NeedsGame(String),
    /// Irori can't add games of this runner to its library.
    Runner(String),
}

/// The reason Irori won't run `script`, if any.
pub fn refusal(script: &Script) -> Option<Refusal> {
    if let Some(base) = &script.extends {
        return Some(Refusal::NeedsGame(base.clone()));
    }
    if let Some(group) = script.requires.first() {
        return Some(Refusal::NeedsGame(group.join(" | ")));
    }
    let runner = script.info.runner.as_str();
    (!irori_services::scripts::runner_supported(runner))
        .then(|| Refusal::Runner(runner.to_string()))
}

/// Letters and digits of a title, lower-cased, for matching titles across
/// stores ("The Witcher 3: Wild Hunt" ≈ "the witcher 3 wild hunt").
pub fn title_key(title: &str) -> String {
    title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Stores whose games Irori installs natively.
const NATIVE: [Runner; 6] = [
    Runner::Legendary,
    Runner::Gog,
    Runner::Nile,
    Runner::Itch,
    Runner::Zoom,
    Runner::Steam,
];

/// A library game of a native store with the same title as `title`.
pub fn native_game<'a>(
    title: &str,
    games: impl IntoIterator<Item = &'a Arc<GameInfo>>,
) -> Option<GameId> {
    let key = title_key(title);
    if key.is_empty() {
        return None;
    }
    games
        .into_iter()
        .find(|g| !g.is_dlc && NATIVE.contains(&g.runner()) && title_key(&g.title) == key)
        .map(|g| g.id())
}

/// The suggested game folder: `<install root>/<slug>`.
pub fn default_game_dir(install_root: &str, slug: &str) -> PathBuf {
    let root = if install_root.trim().is_empty() {
        irori_core::paths::expand_home("~/Games")
    } else {
        irori_core::paths::expand_home(install_root.trim())
    };
    root.join(irori_core::paths::sanitize(slug))
}

/// Whether every file the user must provide has been chosen.
pub fn files_ready(consent: &Consent, picked: &BTreeMap<String, PathBuf>) -> bool {
    consent
        .user_files
        .iter()
        .all(|f| picked.contains_key(&f.file_id))
}

/// The consent screen's boxes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsentChecks {
    /// "I understand what these commands do".
    pub shell_acknowledged: bool,
    /// One per [`Consent::outside_writes`] entry.
    pub outside: Vec<bool>,
}

impl ConsentChecks {
    pub fn new(consent: &Consent) -> Self {
        Self {
            shell_acknowledged: false,
            outside: vec![false; consent.outside_writes.len()],
        }
    }

    /// Whether the user ticked every box the install needs: the shell
    /// acknowledgement when there are shell commands, and every write
    /// outside the game folder that can be approved. (Writes whose path
    /// depends on a later choice are checked when they run instead.)
    pub fn ready(&self, consent: &Consent) -> bool {
        (!consent.needs_shell_ack() || self.shell_acknowledged)
            && consent
                .outside_writes
                .iter()
                .zip(&self.outside)
                .all(|(w, ticked)| w.resolved.is_none() || *ticked)
    }

    /// What the user approved.
    pub fn approvals(&self, consent: &Consent) -> Approvals {
        Approvals {
            accepted: self.ready(consent),
            shell_acknowledged: self.shell_acknowledged,
            outside_writes: consent
                .outside_writes
                .iter()
                .zip(&self.outside)
                .filter(|(_, ticked)| **ticked)
                .filter_map(|(w, _)| w.resolved.clone())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_scripts::{OutsideWrite, ShellCommand, UserFile};

    #[test]
    fn steps_follow_in_order_and_skip_files_when_there_are_none() {
        assert_eq!(Step::Find.next(true), Step::Version);
        assert_eq!(Step::Version.next(true), Step::Destination);
        assert_eq!(Step::Destination.next(true), Step::Files);
        assert_eq!(Step::Destination.next(false), Step::Consent);
        assert_eq!(Step::Files.next(true), Step::Consent);
        assert_eq!(Step::Consent.next(false), Step::Progress);
        assert_eq!(Step::Done.next(true), Step::Done);
        assert_eq!(Step::Consent.back(false), Some(Step::Destination));
        assert_eq!(Step::Consent.back(true), Some(Step::Files));
        assert_eq!(Step::Failed.back(true), Some(Step::Consent));
        assert_eq!(Step::Progress.back(true), None);
    }

    fn plan() -> Consent {
        Consent {
            user_files: vec![UserFile {
                file_id: "setup".into(),
                prompt: None,
            }],
            outside_writes: vec![
                OutsideWrite {
                    step: Some(2),
                    command: "write_file".into(),
                    path: "/home/u/.config/game.ini".into(),
                    resolved: Some(PathBuf::from("/home/u/.config/game.ini")),
                },
                OutsideWrite {
                    step: Some(3),
                    command: "copy".into(),
                    path: "<your choice>/x".into(),
                    resolved: None,
                },
            ],
            shell_commands: vec![ShellCommand {
                step: 4,
                command: "rm -rf $GAMEDIR/tmp".into(),
                resolved: "rm -rf /g/tmp".into(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn install_waits_for_every_required_box() {
        let consent = plan();
        let mut checks = ConsentChecks::new(&consent);
        assert!(!checks.ready(&consent));
        assert!(!checks.approvals(&consent).accepted);
        checks.shell_acknowledged = true;
        assert!(!checks.ready(&consent), "the outside write is not approved");
        checks.outside[0] = true;
        assert!(checks.ready(&consent), "unresolved writes need no box");
        let approvals = checks.approvals(&consent);
        assert!(approvals.accepted && approvals.shell_acknowledged);
        assert_eq!(
            approvals.outside_writes,
            vec![PathBuf::from("/home/u/.config/game.ini")]
        );

        let quiet = Consent::default();
        assert!(ConsentChecks::new(&quiet).ready(&quiet));
    }

    #[test]
    fn user_files_must_be_chosen() {
        let consent = plan();
        let mut picked = BTreeMap::new();
        assert!(!files_ready(&consent, &picked));
        picked.insert("setup".to_string(), PathBuf::from("/d/setup.exe"));
        assert!(files_ready(&consent, &picked));
    }

    fn game(runner: Runner, title: &str, dlc: bool) -> Arc<GameInfo> {
        Arc::new(GameInfo {
            app_name: format!("{runner:?}-{title}"),
            runner: Some(runner),
            title: title.into(),
            is_dlc: dlc,
            ..Default::default()
        })
    }

    #[test]
    fn games_in_native_stores_are_offered_first() {
        let games = vec![
            game(Runner::Sideload, "Hades", false),
            game(Runner::Gog, "The Witcher 3: Wild Hunt", false),
            game(Runner::Legendary, "Hades", true),
        ];
        assert_eq!(
            native_game("the witcher 3 wild hunt", &games),
            Some(games[1].id())
        );
        // Added apps and DLC don't count.
        assert_eq!(native_game("Hades", &games), None);
        assert_eq!(native_game("", &games), None);
    }

    #[test]
    fn scripts_irori_cannot_finish_are_refused() {
        let base = "name: X\ngame_slug: x\nversion: A\nslug: x-a\nrunner: linux\nscript:\n  game:\n    exe: x\n  installer:\n  - mkdir: $GAMEDIR/a\n";
        let script = Script::parse(base).unwrap();
        assert_eq!(refusal(&script), None);
        let steam = Script::parse(&base.replace("runner: linux", "runner: web"));
        if let Ok(s) = steam {
            assert_eq!(refusal(&s), Some(Refusal::Runner("web".into())));
        }
        assert_eq!(
            default_game_dir("/games", "the-witcher-3"),
            PathBuf::from("/games/the-witcher-3")
        );
    }
}
