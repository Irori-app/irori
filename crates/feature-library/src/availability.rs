//! Which installed games have their files missing (an unmounted drive, a
//! deleted folder). Checked in the background once per library revision for
//! every installed game at once, instead of one check per visible tile.

use std::collections::HashSet;
use std::sync::Arc;

use irori_core::GameId;
use irori_services::LibrarySnapshot;

/// Results of the last availability check.
#[derive(Debug, Default)]
pub struct Availability {
    /// Library revision `missing` was computed for.
    revision: u64,
    missing: Arc<HashSet<GameId>>,
    /// Revision a running check was started for.
    pending: Option<u64>,
}

impl Availability {
    /// Installed games whose files were not found. A new `Arc` is published
    /// only when the set changes.
    pub fn missing(&self) -> &Arc<HashSet<GameId>> {
        &self.missing
    }

    /// Whether a check for `revision` should start; marks it as running.
    pub fn start(&mut self, revision: u64) -> bool {
        if self.revision == revision || self.pending == Some(revision) {
            return false;
        }
        self.pending = Some(revision);
        true
    }

    /// Record the result of a check. Results for an older revision than the
    /// current one are dropped. Returns whether the missing set changed.
    pub fn finish(&mut self, revision: u64, missing: HashSet<GameId>) -> bool {
        if self.pending == Some(revision) {
            self.pending = None;
        }
        if revision < self.revision {
            return false;
        }
        self.revision = revision;
        if *self.missing == missing {
            return false;
        }
        self.missing = Arc::new(missing);
        true
    }
}

/// Games worth checking: installed, with an install folder, not a web app and
/// not managed by another launcher.
pub fn candidates(library: &LibrarySnapshot) -> Vec<GameId> {
    library
        .games
        .iter()
        .filter(|g| {
            g.is_installed
                && g.browser_url.is_none()
                && g.third_party_managed_app.is_none()
                && g.install_path().is_some()
        })
        .map(|g| g.id())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::tests::{game, snapshot};
    use irori_core::Runner;

    #[test]
    fn checks_once_per_revision() {
        let mut a = Availability::default();
        // Revision 0 is the empty library: nothing to check.
        assert!(!a.start(0));
        assert!(a.start(3));
        assert!(!a.start(3));
        let id = GameId::new(Runner::Gog, "1");
        let first = a.missing().clone();
        assert!(a.finish(3, [id.clone()].into()));
        assert!(!Arc::ptr_eq(&first, a.missing()));
        assert!(!a.start(3));
        // Same result for a newer revision keeps the published set.
        assert!(a.start(4));
        let before = a.missing().clone();
        assert!(!a.finish(4, [id].into()));
        assert!(Arc::ptr_eq(&before, a.missing()));
        // A late result for an older revision is ignored.
        assert!(!a.finish(2, HashSet::new()));
        assert_eq!(a.missing().len(), 1);
    }

    #[test]
    fn only_installed_local_games_are_checked() {
        let installed = game(Runner::Gog, "1", "A", true);
        let mut with_path = (*installed).clone();
        with_path.install = Some(irori_core::InstalledInfo {
            install_path: "/games/a".into(),
            ..Default::default()
        });
        let lib = snapshot(vec![
            installed,
            Arc::new(with_path),
            game(Runner::Gog, "2", "B", false),
        ]);
        assert_eq!(candidates(&lib), vec![GameId::new(Runner::Gog, "1")]);
    }
}
