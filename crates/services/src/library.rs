//! The aggregated library across stores, and its filtering/sorting.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ember_core::{Event, GameId, GameInfo, InstallPlatform, Runner};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use serde::{Deserialize, Serialize};

use crate::Backend;
use crate::userdata::UserData;

static REVISION: AtomicU64 = AtomicU64::new(0);

/// All visible (non-DLC) games of every store, sorted by title.
#[derive(Default)]
pub struct LibrarySnapshot {
    pub revision: u64,
    pub games: Vec<Arc<GameInfo>>,
    index: HashMap<GameId, usize>,
}

impl LibrarySnapshot {
    pub fn get(&self, id: &GameId) -> Option<&Arc<GameInfo>> {
        self.index.get(id).map(|&ix| &self.games[ix])
    }

    pub fn len(&self) -> usize {
        self.games.len()
    }

    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    pub fn count(&self, runner: Runner) -> usize {
        self.games.iter().filter(|g| g.runner() == runner).count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StoreFilter {
    #[default]
    All,
    Store(Runner),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LibrarySort {
    #[default]
    Title,
    LastPlayed,
    Playtime,
    InstallSize,
    /// Installed games first, then by title.
    InstalledFirst,
    /// Newest release first.
    ReleaseDate,
    /// Most recently added to the account first.
    DateAdded,
    /// Highest review score first.
    Score,
}

impl LibrarySort {
    pub const ALL: [LibrarySort; 8] = [
        LibrarySort::Title,
        LibrarySort::LastPlayed,
        LibrarySort::Playtime,
        LibrarySort::DateAdded,
        LibrarySort::ReleaseDate,
        LibrarySort::Score,
        LibrarySort::InstallSize,
        LibrarySort::InstalledFirst,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LibrarySort::Title => "Title",
            LibrarySort::LastPlayed => "Last played",
            LibrarySort::Playtime => "Playtime",
            LibrarySort::InstallSize => "Size on disk",
            LibrarySort::InstalledFirst => "Installed first",
            LibrarySort::ReleaseDate => "Release date",
            LibrarySort::DateAdded => "Date added",
            LibrarySort::Score => "Score",
        }
    }
}

/// Library view criteria. Persisted by the UI between sessions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryFilter {
    pub query: String,
    pub store: StoreFilter,
    pub platform: Option<InstallPlatform>,
    pub installed_only: bool,
    pub not_installed_only: bool,
    pub favourites_only: bool,
    pub show_hidden: bool,
    /// Only hidden games (the "Hidden" view).
    pub hidden_only: bool,
    pub offline_capable_only: bool,
    /// Hide games requiring a third-party launcher (EA app, Ubisoft Connect).
    pub hide_third_party: bool,
    pub category: Option<String>,
    pub updates_only: bool,
    pub sort: LibrarySort,
    pub descending: bool,
}

impl LibraryFilter {
    /// Apply to a snapshot. `updates` holds keys of games with pending updates.
    pub fn apply(
        &self,
        library: &LibrarySnapshot,
        user: &UserData,
        updates: &std::collections::HashSet<GameId>,
    ) -> Vec<Arc<GameInfo>> {
        let category = self.category.as_ref().and_then(|c| user.categories.get(c));
        let mut out: Vec<(i64, Arc<GameInfo>)> = Vec::with_capacity(library.games.len());
        let matcher = (!self.query.trim().is_empty()).then(SkimMatcherV2::default);
        let query = self.query.trim();
        for g in &library.games {
            let key = g.id().key();
            let hidden = user.hidden.contains(&key);
            if self.hidden_only {
                if !hidden {
                    continue;
                }
            } else if hidden && !self.show_hidden {
                continue;
            }
            if let StoreFilter::Store(r) = self.store
                && g.runner() != r
            {
                continue;
            }
            if let Some(p) = self.platform {
                let matches = match g.installed_platform() {
                    Some(installed) => installed == p,
                    None => {
                        g.platforms.contains(&p)
                            || (g.platforms.is_empty() && p == InstallPlatform::Windows)
                    }
                };
                if !matches {
                    continue;
                }
            }
            if self.installed_only && !g.is_installed {
                continue;
            }
            if self.not_installed_only && g.is_installed {
                continue;
            }
            if self.favourites_only && !user.favourites.contains(&key) {
                continue;
            }
            if self.offline_capable_only && !g.can_run_offline {
                continue;
            }
            if self.hide_third_party && g.third_party_managed_app.is_some() {
                continue;
            }
            if let Some(set) = category {
                if !set.contains(&key) {
                    continue;
                }
            } else if self.category.is_some() {
                continue;
            }
            if self.updates_only && !updates.contains(&g.id()) {
                continue;
            }
            let score = match &matcher {
                Some(m) => match m.fuzzy_match(&g.title, query) {
                    Some(s) => s,
                    None => continue,
                },
                None => 0,
            };
            out.push((score, g.clone()));
        }

        if matcher.is_some() {
            out.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| a.1.sort_title.cmp(&b.1.sort_title))
            });
            return out.into_iter().map(|(_, g)| g).collect();
        }

        let played = |g: &GameInfo| {
            user.playtime
                .get(&g.id().key())
                .and_then(|p| p.last_played.as_deref())
                .and_then(ember_core::util::parse_rfc3339)
                .unwrap_or(i64::MIN)
        };
        let minutes = |g: &GameInfo| {
            user.playtime
                .get(&g.id().key())
                .map(|p| p.total_minutes)
                .unwrap_or(0)
        };
        let size = |g: &GameInfo| {
            g.install
                .as_ref()
                .map(|i| i.install_size_bytes)
                .unwrap_or(0)
        };
        // RFC 3339 and plain dates compare as text once cut to one length.
        let added = |g: &GameInfo| {
            g.date_added
                .clone()
                .or_else(|| user.first_seen.get(&g.id().key()).cloned())
                .map(|d| d.chars().take(19).collect::<String>())
        };
        let released = |g: &GameInfo| {
            g.release_date
                .as_deref()
                .map(|d| d.chars().take(10).collect::<String>())
        };
        let mut games: Vec<Arc<GameInfo>> = out.into_iter().map(|(_, g)| g).collect();
        match self.sort {
            LibrarySort::Title => {}
            LibrarySort::LastPlayed => games.sort_by_key(|g| std::cmp::Reverse(played(g))),
            LibrarySort::Playtime => games.sort_by_key(|g| std::cmp::Reverse(minutes(g))),
            LibrarySort::InstallSize => games.sort_by_key(|g| std::cmp::Reverse(size(g))),
            LibrarySort::InstalledFirst => games.sort_by_key(|g| !g.is_installed),
            LibrarySort::ReleaseDate => return known_first(games, self.descending, released),
            LibrarySort::DateAdded => return known_first(games, self.descending, added),
            LibrarySort::Score => return known_first(games, self.descending, |g| g.rating),
        }
        if self.descending {
            games.reverse();
        }
        games
    }
}

/// Sort by `key`, largest first (oldest or lowest first when `ascending`),
/// with games whose key is unknown after all the others either way. Ties
/// keep their title order.
fn known_first<K: Ord>(
    games: Vec<Arc<GameInfo>>,
    ascending: bool,
    key: impl Fn(&GameInfo) -> Option<K>,
) -> Vec<Arc<GameInfo>> {
    let (mut known, unknown): (Vec<_>, Vec<_>) = games
        .into_iter()
        .map(|g| (key(&g), g))
        .partition(|(k, _)| k.is_some());
    if ascending {
        known.sort_by(|a, b| a.0.cmp(&b.0));
    } else {
        known.sort_by(|a, b| b.0.cmp(&a.0));
    }
    known.into_iter().chain(unknown).map(|(_, g)| g).collect()
}

impl Backend {
    /// Current aggregated library.
    pub fn library(&self) -> Arc<LibrarySnapshot> {
        self.library.read().clone()
    }

    /// Look up any game, including DLCs (which the library hides).
    pub fn game(&self, id: &GameId) -> Option<Arc<GameInfo>> {
        if let Some(g) = self.library.read().get(id) {
            return Some(g.clone());
        }
        self.store(id.runner)?.game(&id.app_name)
    }

    /// Recently played games that still exist, most recent first.
    pub fn recent_games(&self, installed_only: bool) -> Vec<Arc<GameInfo>> {
        let lib = self.library();
        let user = self.userdata.snapshot();
        let max = self.settings.app().max_recent_games.max(1) as usize;
        user.recent
            .iter()
            .filter_map(|key| {
                let (runner, app) = key.split_once(':')?;
                lib.get(&GameId::new(Runner::from_id(runner)?, app))
                    .cloned()
            })
            .filter(|g| !installed_only || g.is_installed)
            .take(max)
            .collect()
    }

    pub(crate) fn rebuild_library(&self) {
        let user = self.userdata.snapshot();
        let mut games: Vec<Arc<GameInfo>> = self
            .stores
            .values()
            .flat_map(|s| s.games())
            .filter(|g| !g.is_dlc)
            .map(|g| match user.overrides.get(&g.id().key()) {
                Some(o) => {
                    let mut g = (*g).clone();
                    if let Some(t) = &o.title {
                        g.title = t.clone();
                    }
                    if let Some(c) = &o.art_cover {
                        g.art_cover = Some(c.clone());
                    }
                    if let Some(s) = &o.art_square {
                        g.art_square = Some(s.clone());
                    }
                    if let Some(b) = &o.art_background {
                        g.art_background = Some(b.clone());
                    }
                    if let Some(l) = &o.art_logo {
                        g.art_logo = Some(l.clone());
                    }
                    self.facts.complete(&mut g);
                    Arc::new(g.finalize())
                }
                None => {
                    let mut g = (*g).clone();
                    self.facts.complete(&mut g);
                    Arc::new(if g.sort_title.is_empty() {
                        g.finalize()
                    } else {
                        g
                    })
                }
            })
            .collect();
        self.userdata.sync_library(&games);
        // Learn missing release dates and scores in the background; the
        // library is rebuilt once they arrive.
        if self.is_online() {
            let (b, list) = (self.clone(), games.clone());
            self.spawn(async move {
                if b.facts.refresh(&list).await {
                    b.rebuild_library();
                }
            });
        }
        games.sort_by(|a, b| {
            a.sort_title
                .cmp(&b.sort_title)
                .then(a.runner().cmp(&b.runner()))
        });
        let index = games
            .iter()
            .enumerate()
            .map(|(ix, g)| (g.id(), ix))
            .collect();
        let revision = REVISION.fetch_add(1, Ordering::Relaxed) + 1;
        *self.library.write() = Arc::new(LibrarySnapshot {
            revision,
            games,
            index,
        });
        self.bus.emit(Event::LibraryUpdated(revision));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn game(runner: Runner, app: &str, title: &str, installed: bool) -> Arc<GameInfo> {
        Arc::new(
            GameInfo {
                runner: Some(runner),
                app_name: app.into(),
                title: title.into(),
                is_installed: installed,
                ..Default::default()
            }
            .finalize(),
        )
    }

    fn snapshot(games: Vec<Arc<GameInfo>>) -> LibrarySnapshot {
        let index = games.iter().enumerate().map(|(i, g)| (g.id(), i)).collect();
        LibrarySnapshot {
            revision: 1,
            games,
            index,
        }
    }

    #[test]
    fn filters_and_searches() {
        let lib = snapshot(vec![
            game(Runner::Gog, "1", "The Witcher 3", true),
            game(Runner::Legendary, "a", "Alan Wake", false),
            game(Runner::Legendary, "b", "Hades", true),
        ]);
        let mut user = UserData::default();
        user.hidden.insert("legendary:b".into());
        let none = HashSet::new();

        let f = LibraryFilter::default();
        let titles: Vec<_> = f
            .apply(&lib, &user, &none)
            .iter()
            .map(|g| g.title.clone())
            .collect();
        assert_eq!(titles, vec!["The Witcher 3", "Alan Wake"]);

        let f = LibraryFilter {
            store: StoreFilter::Store(Runner::Legendary),
            show_hidden: true,
            installed_only: true,
            ..Default::default()
        };
        assert_eq!(f.apply(&lib, &user, &none)[0].title, "Hades");

        let f = LibraryFilter {
            query: "wtch".into(),
            ..Default::default()
        };
        assert_eq!(f.apply(&lib, &user, &none)[0].title, "The Witcher 3");

        let f = LibraryFilter {
            hidden_only: true,
            ..Default::default()
        };
        assert_eq!(f.apply(&lib, &user, &none).len(), 1);
    }

    #[test]
    fn date_and_score_sorts_put_unknowns_last() {
        let with = |app: &str, title: &str, f: &dyn Fn(&mut GameInfo)| {
            let mut g = GameInfo {
                runner: Some(Runner::Gog),
                app_name: app.into(),
                title: title.into(),
                ..Default::default()
            };
            f(&mut g);
            Arc::new(g.finalize())
        };
        let lib = snapshot(vec![
            with("1", "Alpha", &|g| {
                g.release_date = Some("2015-05-19".into());
                g.rating = Some(92);
            }),
            with("2", "Beta", &|g| {
                g.release_date = Some("2020-12-10T00:00:00+0000".into());
                g.rating = Some(75);
                g.date_added = Some("2021-01-01T10:00:00Z".into());
            }),
            with("3", "Gamma", &|_| {}),
        ]);
        let mut user = UserData::default();
        user.first_seen
            .insert("gog:1".into(), "2026-10-23T10:00:00.123Z".into());
        let none = HashSet::new();
        let titles = |sort: LibrarySort, descending: bool| -> Vec<String> {
            LibraryFilter {
                sort,
                descending,
                ..Default::default()
            }
            .apply(&lib, &user, &none)
            .iter()
            .map(|g| g.title.clone())
            .collect()
        };
        assert_eq!(
            titles(LibrarySort::ReleaseDate, false),
            ["Beta", "Alpha", "Gamma"]
        );
        assert_eq!(
            titles(LibrarySort::ReleaseDate, true),
            ["Alpha", "Beta", "Gamma"]
        );
        assert_eq!(
            titles(LibrarySort::Score, false),
            ["Alpha", "Beta", "Gamma"]
        );
        // The store's date first, the first-seen time otherwise.
        assert_eq!(
            titles(LibrarySort::DateAdded, false),
            ["Alpha", "Beta", "Gamma"]
        );
    }
}
