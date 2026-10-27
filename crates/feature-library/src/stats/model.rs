//! Numbers behind the stats page, computed from the session history and the
//! per-game playtime totals. Pure functions over plain data so they can be
//! unit-tested; dates are local calendar days.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike as _, Days, Local, NaiveDate};
use irori_core::game::PlaytimeRecord;
use irori_core::{GameId, Runner};
use irori_services::Session;

/// Weeks shown in the weekly chart, ending with the current one.
pub const WEEKS: usize = 12;
/// Games listed under "Most played".
pub const TOP_GAMES: usize = 10;
/// A day counts towards a streak once a game ran this long on it.
const STREAK_DAY_SECONDS: u64 = 60;

/// Play time in one calendar week (Monday to Sunday).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Week {
    pub start: NaiveDate,
    pub minutes: u64,
}

/// All-time play time of one store's games.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreTotal {
    pub runner: Runner,
    pub minutes: u64,
}

/// All-time play time of one game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameTotal {
    pub id: GameId,
    pub minutes: u64,
    /// Sessions of it in the recorded history.
    pub sessions: usize,
}

/// Everything the stats page shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// [`WEEKS`] weeks, oldest first, the last one being the current week.
    pub weeks: Vec<Week>,
    /// Consecutive days played up to today (or yesterday, while today is
    /// still open).
    pub current_streak: u32,
    pub longest_streak: u32,
    /// Sessions in the recorded history.
    pub sessions: usize,
    /// Day of the first recorded session.
    pub since: Option<NaiveDate>,
    /// Stores by all-time play time, most first; stores without any left out.
    pub stores: Vec<StoreTotal>,
    /// The [`TOP_GAMES`] most played games of all time, most first.
    pub games: Vec<GameTotal>,
    pub total_minutes: u64,
}

impl Stats {
    /// Minutes played this week and last week.
    pub fn this_and_last_week(&self) -> (u64, u64) {
        let mut recent = self.weeks.iter().rev().map(|w| w.minutes);
        (recent.next().unwrap_or(0), recent.next().unwrap_or(0))
    }

    /// Whether there is anything to show at all.
    pub fn is_empty(&self) -> bool {
        self.total_minutes == 0 && self.sessions == 0
    }
}

/// A session's start as a local calendar day.
pub fn local_day(session: &Session) -> Option<NaiveDate> {
    DateTime::parse_from_rfc3339(&session.started)
        .ok()
        .map(|t| t.with_timezone(&Local).date_naive())
}

/// Monday of the week containing `day`.
fn week_start(day: NaiveDate) -> NaiveDate {
    day - Days::new(u64::from(day.weekday().num_days_from_monday()))
}

/// Compute the stats. `sessions` pairs each recorded session with its local
/// start day; `playtime` holds the all-time totals by game key (they include
/// time played before the history existed, such as imports from Heroic).
///
/// Playtime counts only for what `is_game` accepts: records can name DLC
/// (Epic reports its playtime per DLC too, each repeating the base game's)
/// or games no longer in the library, which would double the totals and show
/// up as raw app names.
pub fn compute(
    sessions: &[(NaiveDate, Session)],
    playtime: &BTreeMap<String, PlaytimeRecord>,
    is_game: impl Fn(&GameId) -> bool,
    today: NaiveDate,
) -> Stats {
    let this_week = week_start(today);
    let first_week = this_week - Days::new(7 * (WEEKS as u64 - 1));
    let mut week_seconds = [0u64; WEEKS];
    let mut day_seconds: BTreeMap<NaiveDate, u64> = BTreeMap::new();
    let mut session_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (day, session) in sessions {
        if session.crashed {
            continue;
        }
        *session_counts.entry(&session.game).or_default() += 1;
        *day_seconds.entry(*day).or_default() += session.seconds;
        let week = week_start(*day);
        if (first_week..=this_week).contains(&week) {
            let ix = (week - first_week).num_days() as usize / 7;
            week_seconds[ix] += session.seconds;
        }
    }
    let weeks = week_seconds
        .iter()
        .enumerate()
        .map(|(ix, seconds)| Week {
            start: first_week + Days::new(7 * ix as u64),
            minutes: seconds / 60,
        })
        .collect();

    let played: BTreeSet<NaiveDate> = day_seconds
        .iter()
        .filter(|(_, s)| **s >= STREAK_DAY_SECONDS)
        .map(|(d, _)| *d)
        .collect();
    let (current_streak, longest_streak) = streaks(&played, today);

    let mut stores: BTreeMap<Runner, u64> = BTreeMap::new();
    let mut games: Vec<GameTotal> = Vec::new();
    for (key, record) in playtime {
        let Some(id) = game_id(key) else { continue };
        if record.total_minutes == 0 || !is_game(&id) {
            continue;
        }
        *stores.entry(id.runner).or_default() += record.total_minutes;
        games.push(GameTotal {
            sessions: session_counts.get(key.as_str()).copied().unwrap_or(0),
            id,
            minutes: record.total_minutes,
        });
    }
    let total_minutes = stores.values().sum();
    let mut stores: Vec<StoreTotal> = stores
        .into_iter()
        .map(|(runner, minutes)| StoreTotal { runner, minutes })
        .collect();
    stores.sort_by_key(|s| std::cmp::Reverse(s.minutes));
    games.sort_by_key(|g| std::cmp::Reverse(g.minutes));
    games.truncate(TOP_GAMES);

    Stats {
        weeks,
        current_streak,
        longest_streak,
        sessions: session_counts.values().sum(),
        since: sessions.iter().map(|(d, _)| *d).min(),
        stores,
        games,
        total_minutes,
    }
}

/// The game a `GameId::key()` names.
fn game_id(key: &str) -> Option<GameId> {
    let (runner, app) = key.split_once(':')?;
    Some(GameId::new(Runner::from_id(runner)?, app))
}

/// Current and longest runs of consecutive days in `played`. The current run
/// ends today, or yesterday when today has no play yet (the day is not over).
fn streaks(played: &BTreeSet<NaiveDate>, today: NaiveDate) -> (u32, u32) {
    let mut longest = 0;
    let mut run = 0;
    let mut previous: Option<NaiveDate> = None;
    for day in played {
        run = match previous {
            Some(p) if p.succ_opt() == Some(*day) => run + 1,
            _ => 1,
        };
        longest = longest.max(run);
        previous = Some(*day);
    }
    let mut day = if played.contains(&today) {
        today
    } else {
        today.pred_opt().unwrap_or(today)
    };
    let mut current = 0;
    while played.contains(&day) {
        current += 1;
        match day.pred_opt() {
            Some(d) => day = d,
            None => break,
        }
    }
    (current, longest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn session(game: &str, d: &str, minutes: u64) -> (NaiveDate, Session) {
        (
            day(d),
            Session {
                game: game.into(),
                started: format!("{d}T12:00:00Z"),
                seconds: minutes * 60,
                crashed: false,
                exit_code: None,
            },
        )
    }

    fn record(minutes: u64) -> PlaytimeRecord {
        PlaytimeRecord {
            total_minutes: minutes,
            ..Default::default()
        }
    }

    #[test]
    fn weeks_start_on_monday_and_end_with_this_week() {
        // 2026-10-23 is a Friday; its week starts on Monday the 19th.
        let today = day("2026-10-23");
        let sessions = [
            session("gog:1", "2026-10-19", 30),
            session("gog:1", "2026-10-23", 15),
            session("gog:1", "2026-10-18", 60),
            // Older than the chart: ignored by it.
            session("gog:1", "2026-01-05", 600),
        ];
        let stats = compute(&sessions, &BTreeMap::new(), |_| true, today);
        assert_eq!(stats.weeks.len(), WEEKS);
        let last = stats.weeks.last().unwrap();
        assert_eq!((last.start, last.minutes), (day("2026-10-19"), 45));
        assert_eq!(stats.this_and_last_week(), (45, 60));
        assert_eq!(stats.weeks[0].start, day("2026-08-03"));
        assert_eq!(stats.since, Some(day("2026-01-05")));
    }

    #[test]
    fn streaks_count_consecutive_days() {
        let today = day("2026-10-23");
        let sessions = [
            session("gog:1", "2026-10-01", 30),
            session("gog:1", "2026-10-02", 30),
            session("gog:1", "2026-10-03", 30),
            session("gog:1", "2026-10-04", 30),
            session("gog:1", "2026-10-21", 30),
            session("gog:1", "2026-10-22", 30),
            // Under a minute: does not count.
            (
                day("2026-10-20"),
                Session {
                    game: "gog:1".into(),
                    started: "2026-10-20T12:00:00Z".into(),
                    seconds: 20,
                    crashed: false,
                    exit_code: None,
                },
            ),
        ];
        let stats = compute(&sessions, &BTreeMap::new(), |_| true, today);
        // Today has no play yet, so yesterday's run is still current.
        assert_eq!((stats.current_streak, stats.longest_streak), (2, 4));
        let tomorrow = day("2026-10-25");
        assert_eq!(
            compute(&sessions, &BTreeMap::new(), |_| true, tomorrow).current_streak,
            0
        );
    }

    #[test]
    fn totals_by_store_and_game_skip_crashes_and_unknown_keys() {
        let mut playtime = BTreeMap::new();
        playtime.insert("gog:1".to_string(), record(120));
        playtime.insert("gog:2".to_string(), record(30));
        playtime.insert("legendary:Fowl".to_string(), record(90));
        playtime.insert("nonsense".to_string(), record(999));
        playtime.insert("gog:3".to_string(), record(0));
        let mut sessions = vec![
            session("gog:1", "2026-10-22", 60),
            session("gog:1", "2026-10-23", 60),
        ];
        sessions.push((
            day("2026-10-23"),
            Session {
                game: "gog:2".into(),
                started: "2026-10-23T12:00:00Z".into(),
                seconds: 2,
                crashed: true,
                exit_code: None,
            },
        ));
        let stats = compute(&sessions, &playtime, |_| true, day("2026-10-23"));
        assert_eq!(stats.total_minutes, 240);
        assert_eq!(
            stats.stores,
            vec![
                StoreTotal {
                    runner: Runner::Gog,
                    minutes: 150
                },
                StoreTotal {
                    runner: Runner::Legendary,
                    minutes: 90
                },
            ]
        );
        let names: Vec<_> = stats.games.iter().map(|g| g.id.key()).collect();
        assert_eq!(names, ["gog:1", "legendary:Fowl", "gog:2"]);
        assert_eq!(stats.games[0].sessions, 2);
        assert_eq!(stats.sessions, 2);
    }

    #[test]
    fn dlc_and_unknown_games_are_left_out() {
        // Epic reports Civilization VI's DLC with the base game's time.
        let mut playtime = BTreeMap::new();
        playtime.insert("legendary:Kinglet".to_string(), record(2283));
        playtime.insert("legendary:KingletAztec".to_string(), record(2283));
        playtime.insert("legendary:KingletNubia".to_string(), record(248));
        let games = ["legendary:Kinglet"];
        let stats = compute(
            &[],
            &playtime,
            |id| games.contains(&id.key().as_str()),
            day("2026-10-23"),
        );
        let names: Vec<_> = stats.games.iter().map(|g| g.id.key()).collect();
        assert_eq!(names, ["legendary:Kinglet"]);
        assert_eq!(stats.total_minutes, 2283);
        assert_eq!(stats.stores[0].minutes, 2283);
    }

    #[test]
    fn nothing_played_is_empty() {
        assert!(compute(&[], &BTreeMap::new(), |_| true, day("2026-10-23")).is_empty());
    }
}
