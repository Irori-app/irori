//! Ring buffer of transfer-rate samples of the current download, for the
//! sparkline on the downloads page.

use std::collections::VecDeque;

use ember_core::{GameId, InstallProgress};

/// Samples kept; at the backend's 250 ms progress cadence this is the last
/// half minute or so.
pub(crate) const SPEED_SAMPLES: usize = 120;

/// One rate sample in bytes per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SpeedSample {
    pub download: u64,
    pub disk: u64,
}

/// Rate history of one download. Resets when the tracked download changes.
#[derive(Debug, Clone)]
pub(crate) struct SpeedHistory {
    capacity: usize,
    samples: VecDeque<SpeedSample>,
    tracking: Option<GameId>,
    last: Option<InstallProgress>,
}

impl SpeedHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(2),
            samples: VecDeque::with_capacity(capacity),
            tracking: None,
            last: None,
        }
    }

    /// Record the latest progress of the download `id` (or `None` when
    /// nothing downloads). A sample is added only when progress changed, so
    /// repeated notifications for unrelated games do not stretch the graph.
    /// Returns whether the history changed.
    pub fn record(&mut self, id: Option<&GameId>, progress: Option<InstallProgress>) -> bool {
        if self.tracking.as_ref() != id {
            let had = !self.samples.is_empty();
            self.samples.clear();
            self.last = None;
            self.tracking = id.cloned();
            if id.is_none() {
                return had;
            }
        }
        let Some(progress) = progress else {
            return false;
        };
        if self.last == Some(progress) {
            return false;
        }
        self.last = Some(progress);
        if progress.download_speed.is_none() && progress.disk_speed.is_none() {
            return false;
        }
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(SpeedSample {
            download: progress.download_speed.unwrap_or(0),
            disk: progress.disk_speed.unwrap_or(0),
        });
        true
    }

    pub fn samples(&self) -> impl ExactSizeIterator<Item = &SpeedSample> {
        self.samples.iter()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_core::Runner;

    fn p(down: u64, disk: u64) -> Option<InstallProgress> {
        Some(InstallProgress {
            download_speed: Some(down),
            disk_speed: Some(disk),
            ..Default::default()
        })
    }

    #[test]
    fn keeps_a_bounded_window() {
        let id = GameId::new(Runner::Gog, "a");
        let mut h = SpeedHistory::new(3);
        for i in 1..=5 {
            assert!(h.record(Some(&id), p(i * 10, i)));
        }
        assert_eq!(h.len(), 3);
        let downs: Vec<u64> = h.samples().map(|s| s.download).collect();
        assert_eq!(downs, vec![30, 40, 50]);
    }

    #[test]
    fn ignores_repeats_and_resets_on_new_download() {
        let a = GameId::new(Runner::Gog, "a");
        let b = GameId::new(Runner::Zoom, "b");
        let mut h = SpeedHistory::new(10);
        assert!(h.record(Some(&a), p(5, 5)));
        assert!(!h.record(Some(&a), p(5, 5)));
        assert!(!h.record(Some(&a), None));
        assert_eq!(h.len(), 1);
        assert!(h.record(Some(&b), p(7, 1)));
        assert_eq!(h.len(), 1);
        assert_eq!(h.samples().next().unwrap().download, 7);
        assert!(h.record(None, None));
        assert_eq!(h.len(), 0);
        assert!(!h.record(None, None));
    }

    #[test]
    fn progress_without_rates_adds_nothing() {
        let a = GameId::new(Runner::Gog, "a");
        let mut h = SpeedHistory::new(10);
        let pct = Some(InstallProgress {
            percent: Some(10.0),
            ..Default::default()
        });
        assert!(!h.record(Some(&a), pct));
        assert_eq!(h.len(), 0);
    }
}
