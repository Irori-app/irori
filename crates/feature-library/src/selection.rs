//! Games picked for bulk actions. Selection mode starts with the toolbar's
//! "Select", a Ctrl-click or a Shift-click and ends with "Done" or Escape;
//! while it lasts, clicking a game picks it instead of opening it.

use std::collections::HashSet;
use std::sync::Arc;

use irori_core::{GameId, GameInfo};

#[derive(Debug, Default)]
pub struct Selection {
    active: bool,
    picked: HashSet<GameId>,
    /// Where the last plain pick happened; Shift-click extends from here.
    anchor: Option<GameId>,
}

impl Selection {
    /// Whether selection mode is on (it can be on with nothing picked).
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn start(&mut self) {
        self.active = true;
    }

    /// Leave selection mode, forgetting the picks.
    pub fn end(&mut self) {
        *self = Self::default();
    }

    pub fn contains(&self, id: &GameId) -> bool {
        self.picked.contains(id)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.picked.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.picked.is_empty()
    }

    /// Pick or unpick one game (a click or Ctrl-click).
    pub fn toggle(&mut self, id: &GameId) {
        self.active = true;
        if !self.picked.remove(id) {
            self.picked.insert(id.clone());
        }
        self.anchor = Some(id.clone());
    }

    /// Pick every game from the anchor to `id` in display order
    /// (Shift-click). Without an anchor this is a plain toggle.
    pub fn extend_to(&mut self, id: &GameId, order: &[Arc<GameInfo>]) {
        let position = |target: &GameId| order.iter().position(|g| &g.id() == target);
        let (Some(from), Some(to)) = (self.anchor.as_ref().and_then(position), position(id)) else {
            self.toggle(id);
            return;
        };
        self.active = true;
        let (lo, hi) = (from.min(to), from.max(to));
        self.picked.extend(order[lo..=hi].iter().map(|g| g.id()));
    }

    /// Pick every shown game.
    pub fn select_all(&mut self, order: &[Arc<GameInfo>]) {
        self.active = true;
        self.picked.extend(order.iter().map(|g| g.id()));
    }

    /// Unpick everything but stay in selection mode.
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.picked.clear();
        self.anchor = None;
    }

    /// Drop picks that are no longer shown (a filter or search changed), so
    /// bulk actions never touch games the user cannot see.
    pub fn keep_shown(&mut self, order: &[Arc<GameInfo>]) {
        if self.picked.is_empty() {
            return;
        }
        let shown: HashSet<GameId> = order.iter().map(|g| g.id()).collect();
        self.picked.retain(|id| shown.contains(id));
    }

    /// The picked games in display order.
    pub fn games(&self, order: &[Arc<GameInfo>]) -> Vec<Arc<GameInfo>> {
        order
            .iter()
            .filter(|g| self.picked.contains(&g.id()))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_core::Runner;

    fn order(n: usize) -> Vec<Arc<GameInfo>> {
        (0..n)
            .map(|i| {
                Arc::new(GameInfo {
                    runner: Some(Runner::Gog),
                    app_name: i.to_string(),
                    title: format!("Game {i}"),
                    ..Default::default()
                })
            })
            .collect()
    }

    fn id(i: usize) -> GameId {
        GameId::new(Runner::Gog, i.to_string())
    }

    fn picked(s: &Selection, order: &[Arc<GameInfo>]) -> Vec<String> {
        s.games(order).iter().map(|g| g.app_name.clone()).collect()
    }

    #[test]
    fn toggles_and_extends_ranges_from_the_anchor() {
        let order = order(6);
        let mut s = Selection::default();
        assert!(!s.is_active());
        s.toggle(&id(1));
        assert!(s.is_active());
        s.extend_to(&id(4), &order);
        assert_eq!(picked(&s, &order), ["1", "2", "3", "4"]);
        // Backwards from the same anchor adds, never removes.
        s.extend_to(&id(0), &order);
        assert_eq!(picked(&s, &order), ["0", "1", "2", "3", "4"]);
        s.toggle(&id(2));
        assert_eq!(picked(&s, &order), ["0", "1", "3", "4"]);
    }

    #[test]
    fn shift_click_without_an_anchor_picks_one() {
        let order = order(3);
        let mut s = Selection::default();
        s.extend_to(&id(2), &order);
        assert_eq!(picked(&s, &order), ["2"]);
    }

    #[test]
    fn hidden_games_drop_out_and_end_resets() {
        let all = order(4);
        let mut s = Selection::default();
        s.select_all(&all);
        assert_eq!(s.len(), 4);
        s.keep_shown(&all[..2]);
        assert_eq!(picked(&s, &all), ["0", "1"]);
        s.clear();
        assert!(s.is_empty() && s.is_active());
        s.end();
        assert!(!s.is_active());
    }
}
