//! "Pick a random game": opens the page of a game chosen at random, for when
//! the backlog is longer than the evening.

use std::collections::BTreeSet;
use std::hash::{BuildHasher as _, Hasher as _};
use std::sync::Arc;

use ember_core::{GameId, GameInfo};
use ember_ui::i18n::t;
use ember_ui::{Route, models, navigate};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, Window};

/// A game to suggest: an installed one when any is installed, else any game
/// in the library. Hidden games are never picked. `roll` chooses among them.
pub fn pick(games: &[Arc<GameInfo>], hidden: &BTreeSet<String>, roll: u64) -> Option<GameId> {
    let shown: Vec<&Arc<GameInfo>> = games
        .iter()
        .filter(|g| !hidden.contains(&g.id().key()))
        .collect();
    let installed: Vec<&Arc<GameInfo>> = shown.iter().copied().filter(|g| g.is_installed).collect();
    let pool = if installed.is_empty() {
        shown
    } else {
        installed
    };
    if pool.is_empty() {
        return None;
    }
    Some(pool[(roll % pool.len() as u64) as usize].id())
}

/// A random number, different on every call.
fn roll() -> u64 {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    hasher.finish()
}

/// Open the page of a random game, or say there is nothing to pick from.
pub fn open_random_game(window: &mut Window, cx: &mut App) {
    crate::i18n::register();
    let library = models(cx).library.read(cx);
    let picked = pick(&library.snapshot.games, &library.user.hidden, roll());
    match picked {
        Some(id) => navigate(Route::Game(id), cx),
        None => window.push_notification(
            Notification::info(t("library.random.empty")).title(t("library.random.title")),
            cx,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_core::Runner;

    fn game(app: &str, installed: bool) -> Arc<GameInfo> {
        Arc::new(GameInfo {
            runner: Some(Runner::Gog),
            app_name: app.into(),
            is_installed: installed,
            ..Default::default()
        })
    }

    #[test]
    fn prefers_installed_games_and_skips_hidden_ones() {
        let games = [game("1", false), game("2", true), game("3", true)];
        let hidden: BTreeSet<String> = ["gog:3".to_string()].into();
        for roll in 0..10 {
            assert_eq!(
                pick(&games, &hidden, roll),
                Some(GameId::new(Runner::Gog, "2"))
            );
        }
        let none_installed = [game("1", false), game("4", false)];
        let picks: BTreeSet<_> = (0..4)
            .filter_map(|r| pick(&none_installed, &BTreeSet::new(), r))
            .collect();
        assert_eq!(picks.len(), 2);
        assert_eq!(pick(&[], &hidden, 3), None);
        let all_hidden: BTreeSet<String> = ["gog:2".to_string(), "gog:3".to_string()].into();
        assert_eq!(
            pick(&games[1..], &all_hidden, 1),
            None,
            "hidden games are never picked"
        );
    }
}
