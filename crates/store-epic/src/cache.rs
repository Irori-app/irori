//! Small on-disk key/value caches with an expiry, for store API responses.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use irori_core::{json_store, util};
use parking_lot::Mutex;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

/// A JSON file `{key: {"ts": unix_secs, "value": ...}}`. Entries older than
/// the lifespan are ignored and dropped on the next write. All file access
/// happens on the blocking pool.
#[derive(Clone)]
pub(crate) struct TimedCache {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    lifespan: Option<Duration>,
    lock: Mutex<()>,
}

impl TimedCache {
    pub fn new(path: PathBuf, lifespan: Option<Duration>) -> Self {
        Self {
            inner: Arc::new(Inner {
                path,
                lifespan,
                lock: Mutex::new(()),
            }),
        }
    }

    fn fresh(&self, entry: &Value, now: u64) -> bool {
        match self.inner.lifespan {
            None => true,
            Some(life) => entry
                .get("ts")
                .and_then(Value::as_u64)
                .is_some_and(|ts| now.saturating_sub(ts) < life.as_secs()),
        }
    }

    fn get_blocking<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let _g = self.inner.lock.lock();
        let map: Map<String, Value> = json_store::read_or_default(&self.inner.path);
        let entry = map.get(key)?;
        if !self.fresh(entry, util::unix_now()) {
            return None;
        }
        serde_json::from_value(entry.get("value")?.clone()).ok()
    }

    fn set_blocking(&self, key: &str, value: Value) {
        let _g = self.inner.lock.lock();
        let mut map: Map<String, Value> = json_store::read_or_default(&self.inner.path);
        let now = util::unix_now();
        map.retain(|_, v| self.fresh(v, now));
        map.insert(key.to_string(), json!({"ts": now, "value": value}));
        if let Err(e) = json_store::write(&self.inner.path, &map) {
            tracing::warn!("writing cache {}: {e:#}", self.inner.path.display());
        }
    }

    /// Cached value of `key`, if present and not expired.
    pub async fn get<T: DeserializeOwned + Send + 'static>(&self, key: &str) -> Option<T> {
        let this = self.clone();
        let key = key.to_string();
        tokio::task::spawn_blocking(move || this.get_blocking(&key))
            .await
            .ok()
            .flatten()
    }

    /// Store `value` under `key`.
    pub async fn set<T: Serialize>(&self, key: &str, value: &T) {
        let Ok(value) = serde_json::to_value(value) else {
            return;
        };
        let this = self.clone();
        let key = key.to_string();
        let _ = tokio::task::spawn_blocking(move || this.set_blocking(&key, value)).await;
    }

    /// Delete every entry.
    pub async fn clear(&self) {
        let this = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _g = this.inner.lock.lock();
            let _ = std::fs::remove_file(&this.inner.path);
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stores_and_expires() {
        let dir = tempfile::tempdir().unwrap();
        let cache = TimedCache::new(dir.path().join("c.json"), Some(Duration::from_secs(60)));
        assert_eq!(cache.get::<u32>("a").await, None);
        cache.set("a", &5u32).await;
        assert_eq!(cache.get::<u32>("a").await, Some(5));

        let stale = TimedCache::new(dir.path().join("s.json"), Some(Duration::from_secs(60)));
        json_store::write(
            &dir.path().join("s.json"),
            &json!({"b": {"ts": 1, "value": 7}}),
        )
        .unwrap();
        assert_eq!(stale.get::<u32>("b").await, None);
        cache.clear().await;
        assert_eq!(cache.get::<u32>("a").await, None);
    }
}
