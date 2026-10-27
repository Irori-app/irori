//! How a download adapts to the connection: how many requests run at once
//! ([`Gate`] sized by [`Tuner`]), which server each request goes to
//! ([`Hosts`]), when a connection counts as stalled ([`Watchdog`]) and what
//! holds the download back ([`classify`]).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use irori_core::Bottleneck;
use tokio::sync::Notify;

/// Requests at once when the user set no cap.
pub(crate) const DEFAULT_MAX: usize = 32;
/// Requests at once at the start.
const START: usize = 4;
/// Fewest requests at once the tuner goes down to.
const MIN: usize = 2;
/// A change counts as faster above this factor.
const GAIN: f64 = 1.05;
/// Windows to stay put after more requests did not help.
const SETTLE: u32 = 10;
/// Windows to stay put after a server failed requests.
const COOL_DOWN: u32 = 5;
/// Bytes per second below which a connection counts as stalled.
const FLOOR: f64 = 4096.0;
/// Every this many picks, the second-best server gets the request.
const EXPLORE_EVERY: u64 = 16;
/// First time a failing server is passed over; doubles per failure.
const BENCH: Duration = Duration::from_secs(15);
const MAX_BENCH: Duration = Duration::from_secs(300);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A semaphore whose size can change while requests wait on it.
pub(crate) struct Gate {
    state: Mutex<GateState>,
    notify: Notify,
}

struct GateState {
    limit: usize,
    active: usize,
    /// Whether a request had to wait since the last [`Gate::take_contended`].
    contended: bool,
}

pub(crate) struct Permit<'a>(&'a Gate);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        lock(&self.0.state).active -= 1;
        self.0.notify.notify_one();
    }
}

impl Gate {
    pub fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(GateState {
                limit: limit.max(1),
                active: 0,
                contended: false,
            }),
            notify: Notify::new(),
        }
    }

    pub async fn acquire(&self) -> Permit<'_> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // Registered before checking, so a release in between is not missed.
            notified.as_mut().enable();
            {
                let mut s = lock(&self.state);
                if s.active < s.limit {
                    s.active += 1;
                    return Permit(self);
                }
                s.contended = true;
            }
            notified.await;
        }
    }

    pub fn set_limit(&self, limit: usize) {
        let added = {
            let mut s = lock(&self.state);
            let added = limit.saturating_sub(s.limit);
            s.limit = limit.max(1);
            added
        };
        for _ in 0..added {
            self.notify.notify_one();
        }
    }

    /// Whether another request could start right away.
    pub fn has_spare(&self) -> bool {
        let s = lock(&self.state);
        s.active < s.limit
    }

    /// Whether requests had to wait since the last call.
    pub fn take_contended(&self) -> bool {
        std::mem::take(&mut lock(&self.state).contended)
    }
}

/// What happened during one tuning window.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Window {
    /// Network bytes per second.
    pub rate: f64,
    /// Requests that failed for reasons a retry may fix.
    pub failures: u64,
    /// Requests had to wait for a free slot (more slots could be used).
    pub contended: bool,
    /// The speed limit, not the connection, set the pace.
    pub throttled: bool,
}

/// Hill-climbs the number of requests at once: more while that makes the
/// download faster, back one step when it stops helping, and down quickly
/// when servers start failing requests.
#[derive(Debug)]
pub(crate) struct Tuner {
    limit: usize,
    min: usize,
    max: usize,
    prev_rate: f64,
    /// Requests added by the last step (0: the last step added none).
    grew: usize,
    /// Windows left before growing again.
    hold: u32,
    /// More requests stopped helping: the connection is full.
    settled: bool,
}

impl Tuner {
    pub fn new(max: usize) -> Self {
        let max = max.max(1);
        Self {
            limit: START.min(max),
            min: MIN.min(max),
            max,
            prev_rate: 0.0,
            grew: 0,
            hold: 0,
            settled: false,
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// At the cap while more requests still made it faster: each
    /// connection is slow, so the servers set the pace.
    pub fn capped(&self) -> bool {
        self.limit >= self.max && !self.settled
    }

    /// The number of requests for the next window.
    pub fn step(&mut self, w: Window) -> usize {
        if w.failures > 0 {
            let cut = (self.limit / 4).max(1);
            self.limit = self.limit.saturating_sub(cut).max(self.min);
            self.grew = 0;
            self.hold = COOL_DOWN;
        } else if !w.contended || w.throttled {
            // Requests are not what limits the download right now.
            self.grew = 0;
        } else if self.grew > 0 && w.rate < self.prev_rate * GAIN {
            self.limit -= self.grew;
            self.grew = 0;
            self.hold = SETTLE;
            self.settled = true;
        } else if self.hold > 0 {
            self.hold -= 1;
        } else if self.limit < self.max {
            if self.grew > 0 {
                // The last step helped.
                self.settled = false;
            }
            let grow = (self.limit / 4).clamp(1, self.max - self.limit);
            self.limit += grow;
            self.grew = grow;
        } else {
            self.grew = 0;
        }
        self.prev_rate = w.rate;
        self.limit
    }
}

/// Speed and reliability of each download server, by host name.
#[derive(Default)]
pub(crate) struct Hosts {
    scores: Mutex<HashMap<String, Score>>,
    picks: AtomicU64,
    /// Typical seconds per chunk request.
    chunk_secs: Mutex<Option<f64>>,
}

#[derive(Debug, Default)]
struct Score {
    /// Bytes per second of one request.
    rate: Option<f64>,
    failures: u32,
    /// Passed over until then.
    benched_until: Option<Instant>,
}

/// The host part of `url`.
pub(crate) fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?']).next().unwrap_or(rest)
}

fn ewma(old: Option<f64>, new: f64) -> f64 {
    old.map_or(new, |o| o * 0.7 + new * 0.3)
}

impl Hosts {
    /// Indices of `urls` in the order to try them: working servers before
    /// failing ones, fastest first, untried ones before measured ones.
    pub fn order(&self, urls: &[String]) -> Vec<usize> {
        let now = Instant::now();
        let scores = lock(&self.scores);
        let key = |i: usize| {
            let s = scores.get(host(&urls[i]));
            let benched = s.and_then(|s| s.benched_until).is_some_and(|u| u > now);
            (benched, s.and_then(|s| s.rate).unwrap_or(f64::INFINITY))
        };
        let mut order: Vec<usize> = (0..urls.len()).collect();
        order.sort_by(|&a, &b| {
            let ((ba, ra), (bb, rb)) = (key(a), key(b));
            ba.cmp(&bb).then(rb.total_cmp(&ra))
        });
        // Now and then the runner-up gets a turn, so a server that got
        // faster is noticed.
        let pick = self.picks.fetch_add(1, Ordering::Relaxed);
        if order.len() > 1
            && pick % EXPLORE_EVERY == EXPLORE_EVERY - 1
            && !key(order[1]).0
            && host(&urls[order[0]]) != host(&urls[order[1]])
        {
            order.swap(0, 1);
        }
        order
    }

    pub fn record_ok(&self, url: &str, bytes: u64, elapsed: Duration) {
        let secs = elapsed.as_secs_f64().max(0.001);
        {
            let mut scores = lock(&self.scores);
            let s = scores.entry(host(url).to_string()).or_default();
            s.rate = Some(ewma(s.rate, bytes as f64 / secs));
            s.failures = 0;
            s.benched_until = None;
        }
        let mut chunk = lock(&self.chunk_secs);
        *chunk = Some(ewma(*chunk, secs));
    }

    pub fn record_failure(&self, url: &str) {
        let mut scores = lock(&self.scores);
        let s = scores.entry(host(url).to_string()).or_default();
        s.failures += 1;
        let bench = BENCH
            .saturating_mul(1 << (s.failures - 1).min(8))
            .min(MAX_BENCH);
        s.benched_until = Some(Instant::now() + bench);
    }

    /// Typical duration of a chunk request, once one finished.
    pub fn chunk_time(&self) -> Option<Duration> {
        lock(&self.chunk_secs).map(Duration::from_secs_f64)
    }
}

/// Notices a connection that stopped delivering: nothing for a whole
/// window, or less than a trickle.
pub(crate) struct Watchdog {
    window: Duration,
    start: Instant,
    bytes: u64,
}

impl Watchdog {
    pub fn new(window: Duration, now: Instant) -> Self {
        Self {
            window,
            start: now,
            bytes: 0,
        }
    }

    /// `n` more bytes arrived; an error when the last window delivered
    /// less than the floor.
    pub fn feed(&mut self, n: u64, now: Instant) -> Result<(), String> {
        self.bytes += n;
        let elapsed = now.saturating_duration_since(self.start);
        if elapsed >= self.window {
            if (self.bytes as f64) < FLOOR * elapsed.as_secs_f64() {
                return Err(format!(
                    "the server sent only {} bytes in {} seconds",
                    self.bytes,
                    elapsed.as_secs()
                ));
            }
            self.start = now;
            self.bytes = 0;
        }
        Ok(())
    }

    /// Time spent waiting on the speed limit does not count.
    pub fn pause(&mut self, d: Duration) {
        self.start += d;
    }
}

/// What the workers saw during one window.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Signals {
    /// Network bytes per second.
    pub rate: f64,
    pub failures: u64,
    /// Requests waiting out a failure right now.
    pub backing_off: u64,
    /// The speed limit in bytes per second.
    pub limit: Option<u64>,
    /// Time writers waited for downloaded data.
    pub net_wait: Duration,
    /// Time writers spent writing.
    pub disk_wait: Duration,
    /// Requests had to wait for a free slot.
    pub contended: bool,
    /// The tuner wants more requests than it may use.
    pub capped: bool,
}

/// What holds the download back; `None` while nothing meaningful can be
/// said (starting, checking files, finishing).
pub(crate) fn classify(s: Signals) -> Option<Bottleneck> {
    if s.rate < 1.0 {
        return (s.failures > 0 || s.backing_off > 0).then_some(Bottleneck::Reconnecting);
    }
    if s.limit.is_some_and(|l| s.rate >= l as f64 * 0.85) {
        return Some(Bottleneck::SpeedLimit);
    }
    if s.disk_wait > s.net_wait.mul_f64(1.5) && s.disk_wait >= Duration::from_millis(200) {
        return Some(Bottleneck::Disk);
    }
    if s.failures > 0 || s.capped {
        return Some(Bottleneck::Server);
    }
    s.contended.then_some(Bottleneck::Network)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy(rate: f64) -> Window {
        Window {
            rate,
            contended: true,
            ..Default::default()
        }
    }

    #[test]
    fn tuner_climbs_while_it_helps_and_settles() {
        let mut t = Tuner::new(32);
        assert_eq!(t.limit(), 4);
        // Throughput grows with requests up to 10 of them.
        let rate = |n: usize| n.min(10) as f64 * 1e6;
        for _ in 0..20 {
            let n = t.limit();
            t.step(busy(rate(n)));
        }
        assert!((10..=13).contains(&t.limit()), "{}", t.limit());
        assert!(!t.capped());
        // Failures cut the count but never below the minimum.
        for _ in 0..20 {
            t.step(Window {
                failures: 3,
                ..busy(1e6)
            });
        }
        assert_eq!(t.limit(), 2);
    }

    #[test]
    fn tuner_holds_when_requests_are_not_the_limit() {
        let mut t = Tuner::new(32);
        for _ in 0..10 {
            t.step(Window {
                rate: 5e6,
                ..Default::default()
            });
        }
        assert_eq!(t.limit(), 4);
        for i in 0..10 {
            t.step(Window {
                throttled: true,
                ..busy(1e6 * i as f64)
            });
        }
        assert_eq!(t.limit(), 4);
    }

    #[test]
    fn tuner_reports_slow_servers_at_the_cap() {
        let mut t = Tuner::new(8);
        let mut rate = 1e6;
        for _ in 0..10 {
            rate *= 1.5;
            t.step(busy(rate));
        }
        assert_eq!(t.limit(), 8);
        assert!(t.capped());
        assert_eq!(Tuner::new(1).limit(), 1);
    }

    #[tokio::test]
    async fn gate_resizes_while_requests_wait() {
        let gate = std::sync::Arc::new(Gate::new(1));
        let first = gate.acquire().await;
        assert!(!gate.has_spare());
        let g = gate.clone();
        let waiter = tokio::spawn(async move {
            let _p = g.acquire().await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(gate.take_contended());
        gate.set_limit(2);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        drop(first);
        assert!(gate.has_spare());
        assert!(!gate.take_contended());
    }

    #[test]
    fn hosts_prefer_fast_working_servers() {
        let urls: Vec<String> = ["https://a.cdn/x", "https://b.cdn/x", "https://c.cdn/x"]
            .map(String::from)
            .into();
        let hosts = Hosts::default();
        assert_eq!(hosts.order(&urls)[0], 0);
        hosts.record_ok(&urls[0], 1_000_000, Duration::from_secs(1));
        // Untried servers get a turn before measured ones.
        assert_eq!(hosts.order(&urls)[0], 1);
        hosts.record_ok(&urls[1], 4_000_000, Duration::from_secs(1));
        hosts.record_ok(&urls[2], 2_000_000, Duration::from_secs(1));
        assert_eq!(hosts.order(&urls)[..3], [1, 2, 0]);
        hosts.record_failure(&urls[1]);
        assert_eq!(hosts.order(&urls)[..3], [2, 0, 1]);
        assert_eq!(host("http://h.example:8080/p?q"), "h.example:8080");
        assert!(hosts.chunk_time().is_some());
    }

    #[test]
    fn watchdog_catches_stalls_and_trickles() {
        let t0 = Instant::now();
        let s = Duration::from_secs(8);
        let mut w = Watchdog::new(s, t0);
        assert!(w.feed(1 << 20, t0 + Duration::from_secs(1)).is_ok());
        assert!(w.feed(0, t0 + s).is_ok());
        // 1 KiB/s for a whole window.
        let mut w = Watchdog::new(s, t0);
        assert!(w.feed(4096, t0 + Duration::from_secs(4)).is_ok());
        assert!(w.feed(4096, t0 + s).is_err());
        // Waiting on the speed limit is not the server's fault.
        let mut w = Watchdog::new(s, t0);
        w.pause(Duration::from_secs(60));
        assert!(w.feed(10, t0 + Duration::from_secs(30)).is_ok());
    }

    #[test]
    fn classifies_bottlenecks() {
        let base = Signals {
            rate: 10e6,
            contended: true,
            net_wait: Duration::from_secs(2),
            disk_wait: Duration::from_millis(100),
            ..Default::default()
        };
        assert_eq!(classify(base), Some(Bottleneck::Network));
        assert_eq!(
            classify(Signals {
                limit: Some(10_000_000),
                ..base
            }),
            Some(Bottleneck::SpeedLimit)
        );
        assert_eq!(
            classify(Signals {
                disk_wait: Duration::from_secs(5),
                ..base
            }),
            Some(Bottleneck::Disk)
        );
        assert_eq!(
            classify(Signals {
                capped: true,
                ..base
            }),
            Some(Bottleneck::Server)
        );
        assert_eq!(
            classify(Signals {
                rate: 0.0,
                backing_off: 2,
                ..base
            }),
            Some(Bottleneck::Reconnecting)
        );
        assert_eq!(classify(Signals::default()), None);
        assert_eq!(
            classify(Signals {
                contended: false,
                ..base
            }),
            None
        );
    }
}
