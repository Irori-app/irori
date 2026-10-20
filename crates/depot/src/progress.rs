//! Progress accounting shared by the download workers.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A progress report. Byte counts are in file (disk) bytes so they are
/// comparable across compressed and uncompressed stores.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress {
    /// 0.0 ..= 100.0
    pub percent: f32,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub eta_secs: Option<u64>,
    /// Network bytes per second.
    pub download_speed: u64,
    /// Disk bytes written per second.
    pub disk_speed: u64,
}

/// Receives progress reports (from a timer task, about twice a second).
pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// A sink that drops reports.
pub fn ignore() -> ProgressFn {
    Arc::new(|_| {})
}

const WINDOW: Duration = Duration::from_secs(5);
pub(crate) const INTERVAL: Duration = Duration::from_millis(500);

/// Counters updated by workers.
#[derive(Debug, Default)]
pub(crate) struct Stats {
    pub total: u64,
    /// File bytes finished (written, or found intact on disk).
    pub done: AtomicU64,
    /// Bytes received from the network.
    pub received: AtomicU64,
    /// Bytes written to disk.
    pub written: AtomicU64,
}

impl Stats {
    pub fn new(total: u64) -> Self {
        Self {
            total,
            ..Default::default()
        }
    }

    pub fn add_done(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    /// Undo `n` done bytes (a file is started over).
    pub fn sub_done(&self, n: u64) {
        let _ = self
            .done
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |d| {
                Some(d.saturating_sub(n))
            });
    }

    pub fn add_received(&self, n: u64) {
        self.received.fetch_add(n, Ordering::Relaxed);
    }

    pub fn add_written(&self, n: u64) {
        self.written.fetch_add(n, Ordering::Relaxed);
    }
}

/// Turns counter snapshots into rates over a sliding window.
pub(crate) struct Meter {
    stats: Arc<Stats>,
    samples: VecDeque<(Instant, u64, u64, u64)>,
}

impl Meter {
    pub fn new(stats: Arc<Stats>) -> Self {
        Self {
            stats,
            samples: VecDeque::new(),
        }
    }

    pub fn sample(&mut self) -> Progress {
        let now = Instant::now();
        let done = self
            .stats
            .done
            .load(Ordering::Relaxed)
            .min(self.stats.total);
        let received = self.stats.received.load(Ordering::Relaxed);
        let written = self.stats.written.load(Ordering::Relaxed);
        self.samples.push_back((now, done, received, written));
        while self.samples.len() > 2
            && self
                .samples
                .front()
                .is_some_and(|(t, ..)| now.duration_since(*t) > WINDOW)
        {
            self.samples.pop_front();
        }
        let (t0, done0, received0, written0) = self.samples[0];
        let secs = now.duration_since(t0).as_secs_f64();
        let rate = |a: u64, b: u64| {
            if secs > 0.0 {
                (a.saturating_sub(b) as f64 / secs) as u64
            } else {
                0
            }
        };
        let done_rate = rate(done, done0);
        let total = self.stats.total;
        Progress {
            percent: if total == 0 {
                100.0
            } else {
                (done as f64 * 100.0 / total as f64) as f32
            },
            bytes_done: done,
            bytes_total: total,
            eta_secs: (done_rate > 0).then(|| (total - done) / done_rate),
            download_speed: rate(received, received0),
            disk_speed: rate(written, written0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_totals() {
        let stats = Arc::new(Stats::new(200));
        let mut meter = Meter::new(stats.clone());
        assert_eq!(meter.sample().percent, 0.0);
        stats.add_done(50);
        stats.add_done(500);
        let p = meter.sample();
        assert_eq!((p.bytes_done, p.bytes_total, p.percent), (200, 200, 100.0));
        stats.sub_done(10_000);
        assert_eq!(meter.sample().bytes_done, 0);
        assert_eq!(Meter::new(Arc::new(Stats::new(0))).sample().percent, 100.0);
    }
}
