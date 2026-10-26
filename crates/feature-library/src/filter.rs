//! Library view criteria, their persistence, and the cached filtered list.
//!
//! [`ViewFilter`] is what the toolbar and the Filters popover edit. It maps onto
//! [`ember_services::LibraryFilter`] for the heavy lifting and adds what the
//! service filter cannot express (several categories at once, an
//! "Uncategorized" choice, hiding games whose files are missing). The result
//! is cached by library revision, user-data identity, update-set identity,
//! availability identity and filter, so rendering never refilters an
//! unchanged library.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use ember_core::{GameId, GameInfo, InstallPlatform, Paths, Runner, ShownStores, json_store};
use ember_services::{LibraryFilter, LibrarySnapshot, LibrarySort, StoreFilter, UserData};
use ember_ui::i18n::{t, t_args};
use serde::{Deserialize, Serialize};

/// Which install states to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallFilter {
    #[default]
    Any,
    Installed,
    NotInstalled,
}

/// When the shown games came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseWindow {
    #[default]
    Any,
    ThisYear,
    /// This year and the four before it.
    LastFiveYears,
    /// Before the last five years.
    Older,
}

impl ReleaseWindow {
    /// Whether a game released on `date` (`YYYY-MM-DD…`) falls in the window
    /// in `year`. Games with no known date only match [`ReleaseWindow::Any`].
    pub fn contains(self, date: Option<&str>, year: i32) -> bool {
        let released = date.and_then(|d| d.get(..4)?.parse::<i32>().ok());
        match (self, released) {
            (ReleaseWindow::Any, _) => true,
            (_, None) => false,
            (ReleaseWindow::ThisYear, Some(y)) => y == year,
            (ReleaseWindow::LastFiveYears, Some(y)) => (year - 4..=year).contains(&y),
            (ReleaseWindow::Older, Some(y)) => y < year - 4,
        }
    }

    pub fn label(self) -> String {
        t(match self {
            ReleaseWindow::Any => "library.filters.released.any",
            ReleaseWindow::ThisYear => "library.filters.released.this_year",
            ReleaseWindow::LastFiveYears => "library.filters.released.last_five_years",
            ReleaseWindow::Older => "library.filters.released.older",
        })
        .to_string()
    }
}

/// How hidden games are treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiddenMode {
    /// Hidden games are left out (the default).
    #[default]
    Hide,
    /// Hidden games are shown next to the others.
    Show,
    /// Only hidden games are shown (the "Hidden" view).
    Only,
}

/// One selectable entry of the category filter.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryFilter {
    /// Games that belong to no category.
    Uncategorized,
    Named(String),
}

impl CategoryFilter {
    pub fn label(&self) -> String {
        match self {
            CategoryFilter::Uncategorized => t("library.filter.uncategorized").to_string(),
            CategoryFilter::Named(n) => n.clone(),
        }
    }
}

/// Selected categories; empty means every game. Accepts the single-category
/// shape written by earlier versions (`"all"`, `"uncategorized"`,
/// `{"named": …}`).
fn deserialize_categories<'de, D>(d: D) -> Result<BTreeSet<CategoryFilter>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Legacy {
        All,
        Uncategorized,
        Named(String),
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Set(BTreeSet<CategoryFilter>),
        Legacy(Legacy),
    }
    Ok(match Repr::deserialize(d)? {
        Repr::Set(set) => set,
        Repr::Legacy(Legacy::All) => BTreeSet::new(),
        Repr::Legacy(Legacy::Uncategorized) => [CategoryFilter::Uncategorized].into(),
        Repr::Legacy(Legacy::Named(n)) => [CategoryFilter::Named(n)].into(),
    })
}

/// Everything that decides which games the library shows and in which order.
/// Persisted between sessions except for the search query.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewFilter {
    #[serde(skip)]
    pub query: String,
    pub store: StoreFilter,
    pub platform: Option<InstallPlatform>,
    pub install: InstallFilter,
    pub favourites_only: bool,
    pub hidden: HiddenMode,
    pub offline_only: bool,
    pub hide_third_party: bool,
    pub updates_only: bool,
    /// Hide installed games whose files are missing (unmounted drive,
    /// deleted folder).
    pub hide_unavailable: bool,
    /// Games in any of these categories are shown; empty shows every game.
    #[serde(alias = "category", deserialize_with = "deserialize_categories")]
    pub categories: BTreeSet<CategoryFilter>,
    pub released: ReleaseWindow,
    pub sort: LibrarySort,
    pub descending: bool,
}

impl ViewFilter {
    /// Number of criteria set in the Filters popover (store and search have
    /// their own visible controls and are not counted).
    pub fn active_count(&self) -> usize {
        [
            self.platform.is_some(),
            self.install != InstallFilter::Any,
            self.favourites_only,
            self.hidden != HiddenMode::Hide,
            self.offline_only,
            self.hide_third_party,
            self.updates_only,
            self.hide_unavailable,
            !self.categories.is_empty(),
            self.released != ReleaseWindow::Any,
        ]
        .into_iter()
        .filter(|b| *b)
        .count()
    }

    /// Reset the Filters popover criteria to their defaults. Sorting, store and
    /// search are kept.
    pub fn reset_filters(&mut self) {
        *self = ViewFilter {
            query: std::mem::take(&mut self.query),
            store: self.store,
            sort: self.sort,
            descending: self.descending,
            ..Default::default()
        };
    }

    /// Reset everything that narrows the list: search, store and filters.
    pub fn clear_all(&mut self) {
        self.reset_filters();
        self.query.clear();
        self.store = StoreFilter::All;
    }

    /// Whether anything narrows the list.
    pub fn is_narrowed(&self) -> bool {
        !self.query.trim().is_empty() || self.store != StoreFilter::All || self.active_count() > 0
    }

    /// Short descriptions of the active criteria, for the "no results" state.
    pub fn describe(&self) -> Vec<String> {
        let text = |key: &str| t(key).to_string();
        let mut out = Vec::new();
        if !self.query.trim().is_empty() {
            let query = self.query.trim();
            out.push(t_args("library.filter.describe.search", &[("query", query)]).to_string());
        }
        if let StoreFilter::Store(r) = self.store {
            out.push(r.store_name().to_string());
        }
        if let Some(p) = self.platform {
            out.push(platform_label(p));
        }
        match self.install {
            InstallFilter::Any => {}
            InstallFilter::Installed => out.push(text("library.filter.describe.installed")),
            InstallFilter::NotInstalled => out.push(text("library.filter.describe.not_installed")),
        }
        if self.favourites_only {
            out.push(text("library.filter.describe.favourites"));
        }
        match self.hidden {
            HiddenMode::Hide => {}
            HiddenMode::Show => out.push(text("library.filter.describe.hidden_shown")),
            HiddenMode::Only => out.push(text("library.filter.describe.hidden_only")),
        }
        if self.offline_only {
            out.push(text("library.filter.describe.offline"));
        }
        if self.hide_third_party {
            out.push(text("library.filter.describe.no_third_party"));
        }
        if self.updates_only {
            out.push(text("library.filter.describe.updates"));
        }
        if self.hide_unavailable {
            out.push(text("library.filter.describe.missing_hidden"));
        }
        if self.released != ReleaseWindow::Any {
            let when = self.released.label();
            out.push(t_args("library.filter.describe.released", &[("when", &when)]).to_string());
        }
        let heading = self.category_heading();
        match self.categories.len() {
            0 => {}
            1 => out.push(
                t_args("library.filter.describe.category", &[("name", &heading)]).to_string(),
            ),
            _ => out.push(
                t_args("library.filter.describe.categories", &[("names", &heading)]).to_string(),
            ),
        }
        out
    }

    /// Add or remove one category from the selection.
    pub fn toggle_category(&mut self, category: CategoryFilter) {
        if !self.categories.remove(&category) {
            self.categories.insert(category);
        }
    }

    /// Follow a category rename in the selection.
    pub fn rename_category(&mut self, from: &str, to: &str) {
        if self
            .categories
            .remove(&CategoryFilter::Named(from.to_string()))
        {
            self.categories
                .insert(CategoryFilter::Named(to.to_string()));
        }
    }

    /// Drop a deleted category from the selection. When it was the only one
    /// selected, the library returns to every game.
    pub fn forget_category(&mut self, name: &str) {
        self.categories
            .remove(&CategoryFilter::Named(name.to_string()));
    }

    /// Title for the selected categories: the name of a single one, the
    /// names of up to three, else a count.
    pub fn category_heading(&self) -> String {
        match self.categories.len() {
            0 => String::new(),
            1..=3 => self
                .categories
                .iter()
                .map(CategoryFilter::label)
                .collect::<Vec<_>>()
                .join(&t("library.filter.list_separator")),
            n => t_args(
                "library.filter.category_count",
                &[("count", &n.to_string())],
            )
            .to_string(),
        }
    }

    /// The service filter for these criteria. `stores` are the stores the
    /// store switcher offers; a persisted store that is no longer offered
    /// falls back to all stores.
    pub fn to_library_filter(&self, stores: &[Runner]) -> LibraryFilter {
        let store = match self.store {
            StoreFilter::Store(r) if stores.contains(&r) => StoreFilter::Store(r),
            _ => StoreFilter::All,
        };
        LibraryFilter {
            query: self.query.trim().to_string(),
            store,
            platform: self.platform,
            installed_only: self.install == InstallFilter::Installed,
            not_installed_only: self.install == InstallFilter::NotInstalled,
            favourites_only: self.favourites_only,
            show_hidden: self.hidden == HiddenMode::Show,
            hidden_only: self.hidden == HiddenMode::Only,
            offline_capable_only: self.offline_only,
            hide_third_party: self.hide_third_party,
            category: None,
            updates_only: self.updates_only,
            sort: self.sort,
            descending: self.descending,
        }
    }
}

/// Display name of a platform; operating systems keep their own names.
pub fn platform_label(platform: InstallPlatform) -> String {
    match platform {
        InstallPlatform::Browser => t("library.platform.browser").to_string(),
        other => other.label().to_string(),
    }
}

/// Where the view criteria are stored.
pub fn state_file() -> PathBuf {
    Paths::get().state_dir().join("library-view.json")
}

/// Load persisted criteria (defaults when missing or unreadable).
pub fn load() -> ViewFilter {
    json_store::read_or_default(&state_file())
}

/// Persist criteria (the search query is never written).
pub fn save(filter: &ViewFilter) -> anyhow::Result<()> {
    json_store::write(&state_file(), filter)
}

/// Stores the store switcher offers: stores with games in the library, plus
/// stores the user is logged in to, leaving out stores the user hid.
/// Ordered like [`Runner::ALL`].
pub fn offered_stores(
    library: &LibrarySnapshot,
    logged_in: &dyn Fn(Runner) -> bool,
    shown: &ShownStores,
) -> Vec<Runner> {
    let mut with_games = HashSet::new();
    for g in &library.games {
        with_games.insert(g.runner());
    }
    Runner::ALL
        .into_iter()
        .filter(|r| with_games.contains(r) || (r.has_account() && logged_in(*r)))
        .filter(|r| shown.shows(*r))
        .collect()
}

/// Whether `key` passes the category selection (union of the selected
/// categories; "Uncategorized" matches games in no category).
fn in_categories(selected: &BTreeSet<CategoryFilter>, user: &UserData, key: &str) -> bool {
    selected.iter().any(|c| match c {
        CategoryFilter::Uncategorized => !user.categories.values().any(|set| set.contains(key)),
        CategoryFilter::Named(name) => user.categories.get(name).is_some_and(|s| s.contains(key)),
    })
}

/// Apply the criteria to a library snapshot. `missing` holds installed games
/// whose files were not found.
pub fn apply(
    filter: &ViewFilter,
    stores: &[Runner],
    library: &LibrarySnapshot,
    user: &UserData,
    updates: &HashSet<GameId>,
    missing: &HashSet<GameId>,
) -> Vec<Arc<GameInfo>> {
    let mut games = filter
        .to_library_filter(stores)
        .apply(library, user, updates);
    if !filter.categories.is_empty() {
        games.retain(|g| in_categories(&filter.categories, user, &g.id().key()));
    }
    if filter.hide_unavailable && !missing.is_empty() {
        games.retain(|g| !missing.contains(&g.id()));
    }
    if filter.released != ReleaseWindow::Any {
        let year = chrono::Datelike::year(&chrono::Local::now());
        games.retain(|g| filter.released.contains(g.release_date.as_deref(), year));
    }
    games
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheKey {
    revision: u64,
    user: usize,
    updates: usize,
    missing: usize,
    stores: Vec<Runner>,
    filter: ViewFilter,
}

/// The filtered, sorted game list for one set of inputs, with an index from
/// game identity to position (keyboard focus is kept by identity).
#[derive(Default)]
pub struct FilteredGames {
    key: Option<CacheKey>,
    games: Arc<[Arc<GameInfo>]>,
    positions: HashMap<GameId, usize>,
    recomputes: usize,
}

impl FilteredGames {
    /// Current list for these inputs, recomputed only when one changed.
    /// `user`, `updates` and `missing` are compared by identity: a new `Arc`
    /// is published for every change.
    pub fn get(
        &mut self,
        filter: &ViewFilter,
        stores: &[Runner],
        library: &LibrarySnapshot,
        user: &Arc<UserData>,
        updates: &Arc<HashSet<GameId>>,
        missing: &Arc<HashSet<GameId>>,
    ) -> Arc<[Arc<GameInfo>]> {
        let key = CacheKey {
            revision: library.revision,
            user: Arc::as_ptr(user) as usize,
            updates: Arc::as_ptr(updates) as usize,
            missing: Arc::as_ptr(missing) as usize,
            stores: stores.to_vec(),
            filter: filter.clone(),
        };
        if self.key.as_ref() != Some(&key) {
            let games: Arc<[Arc<GameInfo>]> =
                apply(filter, stores, library, user, updates, missing).into();
            self.positions = games.iter().enumerate().map(|(i, g)| (g.id(), i)).collect();
            self.games = games;
            self.key = Some(key);
            self.recomputes += 1;
        }
        self.games.clone()
    }

    /// Position of a game in the last computed list.
    pub fn position(&self, id: &GameId) -> Option<usize> {
        self.positions.get(id).copied()
    }

    /// How many times the list was rebuilt.
    #[cfg(test)]
    pub fn recomputes(&self) -> usize {
        self.recomputes
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn game(runner: Runner, app: &str, title: &str, installed: bool) -> Arc<GameInfo> {
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

    pub(crate) fn snapshot(games: Vec<Arc<GameInfo>>) -> LibrarySnapshot {
        // LibrarySnapshot's index is private; a snapshot built through Default
        // and filled with games is enough for filtering, which only iterates.
        let mut s = LibrarySnapshot::default();
        s.revision = 1;
        s.games = games;
        s
    }

    fn sample() -> LibrarySnapshot {
        snapshot(vec![
            game(Runner::Gog, "1", "The Witcher 3", true),
            game(Runner::Legendary, "a", "Alan Wake", false),
            game(Runner::Legendary, "b", "Hades", true),
            game(Runner::Sideload, "x", "Itch game", true),
        ])
    }

    fn titles(v: &[Arc<GameInfo>]) -> Vec<&str> {
        v.iter().map(|g| g.title.as_str()).collect()
    }

    #[test]
    fn counts_and_resets_filters() {
        let mut f = ViewFilter {
            query: "hades".into(),
            store: StoreFilter::Store(Runner::Gog),
            platform: Some(InstallPlatform::Linux),
            install: InstallFilter::Installed,
            favourites_only: true,
            hidden: HiddenMode::Only,
            sort: LibrarySort::Playtime,
            descending: true,
            categories: [CategoryFilter::Uncategorized].into(),
            hide_unavailable: true,
            ..Default::default()
        };
        assert_eq!(f.active_count(), 6);
        assert!(f.is_narrowed());
        f.reset_filters();
        assert_eq!(f.active_count(), 0);
        // Reset keeps search, store and sorting, and never turns hidden games on.
        assert_eq!(f.query, "hades");
        assert_eq!(f.store, StoreFilter::Store(Runner::Gog));
        assert_eq!(f.sort, LibrarySort::Playtime);
        assert!(f.descending);
        assert_eq!(f.hidden, HiddenMode::Hide);
        f.clear_all();
        assert!(!f.is_narrowed());
    }

    #[test]
    fn query_is_not_persisted() {
        let f = ViewFilter {
            query: "secret".into(),
            install: InstallFilter::NotInstalled,
            categories: [
                CategoryFilter::Named("RPG".into()),
                CategoryFilter::Uncategorized,
            ]
            .into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(!json.contains("secret"));
        let back: ViewFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(back.install, InstallFilter::NotInstalled);
        assert_eq!(back.categories, f.categories);
        assert!(back.query.is_empty());
        // Unknown or partial files fall back to defaults field by field.
        let partial: ViewFilter = serde_json::from_str(r#"{"favourites_only":true}"#).unwrap();
        assert!(partial.favourites_only);
    }

    #[test]
    fn reads_single_category_files() {
        let read = |json: &str| serde_json::from_str::<ViewFilter>(json).unwrap().categories;
        assert!(read(r#"{"category":"all"}"#).is_empty());
        assert_eq!(
            read(r#"{"category":"uncategorized"}"#),
            [CategoryFilter::Uncategorized].into()
        );
        assert_eq!(
            read(r#"{"category":{"named":"RPG"}}"#),
            [CategoryFilter::Named("RPG".into())].into()
        );
        assert_eq!(
            read(r#"{"categories":["uncategorized",{"named":"RPG"}]}"#).len(),
            2
        );
    }

    #[test]
    fn category_selection_follows_renames_and_deletes() {
        crate::i18n::register();
        let mut f = ViewFilter::default();
        f.toggle_category(CategoryFilter::Named("RPG".into()));
        f.toggle_category(CategoryFilter::Uncategorized);
        assert_eq!(f.category_heading(), "Uncategorized, RPG");
        f.rename_category("RPG", "Role-playing");
        assert!(
            f.categories
                .contains(&CategoryFilter::Named("Role-playing".into()))
        );
        f.toggle_category(CategoryFilter::Uncategorized);
        f.forget_category("Role-playing");
        assert!(f.categories.is_empty());
        for n in ["a", "b", "c", "d"] {
            f.toggle_category(CategoryFilter::Named(n.into()));
        }
        assert_eq!(f.category_heading(), "4 categories");
    }

    #[test]
    fn unavailable_store_falls_back_to_all() {
        let f = ViewFilter {
            store: StoreFilter::Store(Runner::Nile),
            ..Default::default()
        };
        assert_eq!(f.to_library_filter(&[Runner::Gog]).store, StoreFilter::All);
        assert_eq!(
            f.to_library_filter(&[Runner::Nile]).store,
            StoreFilter::Store(Runner::Nile)
        );
    }

    #[test]
    fn uncategorized_and_named_categories() {
        // `sample()` is in snapshot (display) order; filters keep it.
        let lib = sample();
        let mut user = UserData::default();
        user.categories
            .entry("RPG".into())
            .or_default()
            .insert("gog:1".into());
        user.categories
            .entry("Roguelike".into())
            .or_default()
            .insert("legendary:b".into());
        let none = HashSet::new();
        let stores = Runner::ALL.to_vec();
        let with = |c: Vec<CategoryFilter>| ViewFilter {
            categories: c.into_iter().collect(),
            ..Default::default()
        };

        let f = with(vec![CategoryFilter::Uncategorized]);
        assert_eq!(
            titles(&apply(&f, &stores, &lib, &user, &none, &none)),
            vec!["Alan Wake", "Itch game"]
        );
        let f = with(vec![CategoryFilter::Named("RPG".into())]);
        assert_eq!(
            titles(&apply(&f, &stores, &lib, &user, &none, &none)),
            vec!["The Witcher 3"]
        );
        // Several categories show the union.
        let f = with(vec![
            CategoryFilter::Named("RPG".into()),
            CategoryFilter::Named("Roguelike".into()),
        ]);
        assert_eq!(
            titles(&apply(&f, &stores, &lib, &user, &none, &none)),
            vec!["The Witcher 3", "Hades"]
        );
        let f = with(vec![
            CategoryFilter::Named("RPG".into()),
            CategoryFilter::Uncategorized,
        ]);
        assert_eq!(
            titles(&apply(&f, &stores, &lib, &user, &none, &none)),
            vec!["The Witcher 3", "Alan Wake", "Itch game"]
        );
    }

    #[test]
    fn hides_games_with_missing_files() {
        let lib = sample();
        let user = UserData::default();
        let none = HashSet::new();
        let stores = Runner::ALL.to_vec();
        let missing: HashSet<GameId> = [GameId::new(Runner::Legendary, "b")].into();
        let mut f = ViewFilter::default();
        assert_eq!(apply(&f, &stores, &lib, &user, &none, &missing).len(), 4);
        f.hide_unavailable = true;
        assert_eq!(
            titles(&apply(&f, &stores, &lib, &user, &none, &missing)),
            vec!["The Witcher 3", "Alan Wake", "Itch game"]
        );
    }

    #[test]
    fn offered_stores_include_logged_in_and_non_empty() {
        let lib = sample();
        let shown = ShownStores::default();
        let stores = offered_stores(&lib, &|r| r == Runner::Zoom, &shown);
        assert_eq!(
            stores,
            vec![
                Runner::Legendary,
                Runner::Gog,
                Runner::Zoom,
                Runner::Sideload
            ]
        );
        let shown = ShownStores {
            epic: false,
            ..shown
        };
        let stores = offered_stores(&lib, &|r| r != Runner::Gog, &shown);
        assert!(!stores.contains(&Runner::Legendary));
        assert!(stores.contains(&Runner::Nile));
        assert!(stores.contains(&Runner::Gog));
    }

    #[test]
    fn cache_recomputes_only_when_inputs_change() {
        let lib = sample();
        let user = Arc::new(UserData::default());
        let updates = Arc::new(HashSet::new());
        let missing = Arc::new(HashSet::new());
        let stores = Runner::ALL.to_vec();
        let mut cache = FilteredGames::default();
        let mut f = ViewFilter::default();

        let a = cache.get(&f, &stores, &lib, &user, &updates, &missing);
        let b = cache.get(&f, &stores, &lib, &user, &updates, &missing);
        assert_eq!(cache.recomputes(), 1);
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(
            cache.position(&GameId::new(Runner::Legendary, "b")),
            Some(2)
        );

        // A new user-data snapshot (even with equal content) is a new input.
        let user2 = Arc::new(UserData::default());
        cache.get(&f, &stores, &lib, &user2, &updates, &missing);
        assert_eq!(cache.recomputes(), 2);

        f.install = InstallFilter::Installed;
        let c = cache.get(&f, &stores, &lib, &user2, &updates, &missing);
        assert_eq!(cache.recomputes(), 3);

        // New availability results are a new input.
        let missing2 = Arc::new(HashSet::new());
        cache.get(&f, &stores, &lib, &user2, &updates, &missing2);
        assert_eq!(cache.recomputes(), 4);
        assert_eq!(c.len(), 3);
        assert_eq!(cache.position(&GameId::new(Runner::Legendary, "a")), None);
    }

    #[test]
    fn release_windows_count_calendar_years() {
        let (any, this, five, older) = (
            ReleaseWindow::Any,
            ReleaseWindow::ThisYear,
            ReleaseWindow::LastFiveYears,
            ReleaseWindow::Older,
        );
        assert!(any.contains(None, 2026));
        assert!(!this.contains(None, 2026));
        assert!(this.contains(Some("2026-03-01"), 2026));
        assert!(!this.contains(Some("2025-12-31"), 2026));
        assert!(five.contains(Some("2022-01-01T00:00:00+0000"), 2026));
        assert!(!five.contains(Some("2021-12-31"), 2026));
        assert!(older.contains(Some("2021-12-31"), 2026));
        assert!(!older.contains(Some("2022-01-01"), 2026));
        assert!(!older.contains(Some("garbage"), 2026));
    }
}
