//! The optional row above the grid: "Continue playing" or favourites, chosen
//! by the `libraryTopSection` setting.

use std::sync::Arc;

use gpui_kit::SharedString;
use irori_core::{GameId, GameInfo, LibraryTopSection, Runner};
use irori_services::{LibrarySnapshot, UserData};
use irori_ui::i18n::t;

/// What the top row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopKind {
    /// Recently played games as large landscape cards with a Play button.
    ContinuePlaying,
    Favourites,
}

impl TopKind {
    pub fn title(self) -> SharedString {
        t(match self {
            TopKind::ContinuePlaying => "library.top.continue_playing",
            TopKind::Favourites => "library.top.favourites",
        })
    }
}

/// Parse a `GameId::key()` string.
pub fn parse_key(key: &str) -> Option<GameId> {
    let (runner, app) = key.split_once(':')?;
    Some(GameId::new(Runner::from_id(runner)?, app))
}

/// Games of the top row. Hidden games are left out unless `show_hidden`;
/// recently played games keep most-recent-first order, favourites are sorted
/// by title.
pub fn top_games(
    setting: LibraryTopSection,
    max_recent: usize,
    show_hidden: bool,
    user: &UserData,
    lookup: &dyn Fn(&GameId) -> Option<Arc<GameInfo>>,
) -> Option<(TopKind, Vec<Arc<GameInfo>>)> {
    let visible = |g: &Arc<GameInfo>| show_hidden || !user.hidden.contains(&g.id().key());
    let (kind, games) = match setting {
        LibraryTopSection::Disabled => return None,
        LibraryTopSection::RecentlyPlayed | LibraryTopSection::RecentlyPlayedInstalled => {
            let installed_only = setting == LibraryTopSection::RecentlyPlayedInstalled;
            // Without Irori's own history (such as right after importing from
            // Heroic), recorded playtime still says what was played last.
            let keys: Vec<String> = if user.recent.is_empty() {
                let mut played: Vec<(&String, &str)> = user
                    .playtime
                    .iter()
                    .filter_map(|(k, r)| Some((k, r.last_played.as_deref()?)))
                    .collect();
                // RFC 3339 timestamps in one offset sort chronologically as text.
                played.sort_by(|a, b| b.1.cmp(a.1));
                played.into_iter().map(|(k, _)| k.clone()).collect()
            } else {
                user.recent.clone()
            };
            let games: Vec<_> = keys
                .iter()
                .filter_map(|k| parse_key(k))
                .filter_map(|id| lookup(&id))
                .filter(|g| !installed_only || g.is_installed)
                .filter(visible)
                .take(max_recent.max(1))
                .collect();
            (TopKind::ContinuePlaying, games)
        }
        LibraryTopSection::Favourites => {
            let mut games: Vec<_> = user
                .favourites
                .iter()
                .filter_map(|k| parse_key(k))
                .filter_map(|id| lookup(&id))
                .filter(visible)
                .collect();
            games.sort_by(|a, b| a.sort_title.cmp(&b.sort_title));
            (TopKind::Favourites, games)
        }
    };
    (!games.is_empty()).then_some((kind, games))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    revision: u64,
    user: usize,
    setting: LibraryTopSection,
    max: usize,
    show_hidden: bool,
}

/// [`top_games`] cached by library revision, user-data identity and settings.
#[derive(Default)]
pub struct TopCache {
    key: Option<Key>,
    value: Option<(TopKind, Arc<[Arc<GameInfo>]>)>,
}

impl TopCache {
    pub fn get(
        &mut self,
        setting: LibraryTopSection,
        max_recent: usize,
        show_hidden: bool,
        library: &LibrarySnapshot,
        user: &Arc<UserData>,
    ) -> Option<(TopKind, Arc<[Arc<GameInfo>]>)> {
        let key = Key {
            revision: library.revision,
            user: Arc::as_ptr(user) as usize,
            setting,
            max: max_recent,
            show_hidden,
        };
        if self.key.as_ref() != Some(&key) {
            self.value = top_games(setting, max_recent, show_hidden, user, &|id| {
                library.get(id).cloned()
            })
            .map(|(k, g)| (k, g.into()));
            self.key = Some(key);
        }
        self.value.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::tests::game;
    use std::collections::HashMap;

    #[test]
    fn recent_falls_back_to_playtime() {
        use irori_core::PlaytimeRecord;
        let games: HashMap<GameId, Arc<GameInfo>> = [
            game(Runner::Gog, "1", "Witcher", true),
            game(Runner::Legendary, "a", "Alan Wake", false),
            game(Runner::Legendary, "b", "Hades", true),
        ]
        .into_iter()
        .map(|g| (g.id(), g))
        .collect();
        let lookup = |id: &GameId| games.get(id).cloned();
        let played = |when: &str| PlaytimeRecord {
            total_minutes: 10,
            last_played: Some(when.into()),
            ..Default::default()
        };
        let mut user = UserData::default();
        user.playtime
            .insert("gog:1".into(), played("2026-10-01T10:00:00Z"));
        user.playtime
            .insert("legendary:b".into(), played("2026-10-20T10:00:00Z"));
        user.playtime.insert(
            "legendary:a".into(),
            PlaytimeRecord {
                total_minutes: 5,
                ..Default::default()
            },
        );
        let (_, recent) =
            top_games(LibraryTopSection::RecentlyPlayed, 8, false, &user, &lookup).unwrap();
        let titles: Vec<_> = recent.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, vec!["Hades", "Witcher"]);
    }

    #[test]
    fn recent_and_favourites() {
        let games: HashMap<GameId, Arc<GameInfo>> = [
            game(Runner::Gog, "1", "Witcher", true),
            game(Runner::Legendary, "a", "Alan Wake", false),
            game(Runner::Legendary, "b", "Hades", true),
        ]
        .into_iter()
        .map(|g| (g.id(), g))
        .collect();
        let lookup = |id: &GameId| games.get(id).cloned();
        let mut user = UserData {
            recent: vec![
                "legendary:a".into(),
                "gone:x".into(),
                "legendary:b".into(),
                "gog:1".into(),
            ],
            ..Default::default()
        };
        user.hidden.insert("gog:1".into());
        user.favourites.insert("legendary:b".into());
        user.favourites.insert("legendary:a".into());

        let (kind, recent) =
            top_games(LibraryTopSection::RecentlyPlayed, 8, false, &user, &lookup).unwrap();
        assert_eq!(kind, TopKind::ContinuePlaying);
        let titles: Vec<_> = recent.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, vec!["Alan Wake", "Hades"]);

        let (_, recent) = top_games(
            LibraryTopSection::RecentlyPlayedInstalled,
            8,
            true,
            &user,
            &lookup,
        )
        .unwrap();
        let titles: Vec<_> = recent.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, vec!["Hades", "Witcher"]);

        let (_, recent) =
            top_games(LibraryTopSection::RecentlyPlayed, 1, false, &user, &lookup).unwrap();
        assert_eq!(recent.len(), 1);

        let (kind, favs) =
            top_games(LibraryTopSection::Favourites, 8, false, &user, &lookup).unwrap();
        assert_eq!(kind, TopKind::Favourites);
        let titles: Vec<_> = favs.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, vec!["Alan Wake", "Hades"]);

        assert!(top_games(LibraryTopSection::Disabled, 8, false, &user, &lookup).is_none());
        user.favourites.clear();
        assert!(top_games(LibraryTopSection::Favourites, 8, false, &user, &lookup).is_none());
    }

    #[test]
    fn parses_keys() {
        assert_eq!(parse_key("gog:123"), Some(GameId::new(Runner::Gog, "123")));
        assert_eq!(
            parse_key("sideload:a:b"),
            Some(GameId::new(Runner::Sideload, "a:b"))
        );
        assert_eq!(parse_key("nope"), None);
        assert_eq!(parse_key("steam:1"), Some(GameId::new(Runner::Steam, "1")));
        assert_eq!(parse_key("uplay:1"), None);
    }
}
