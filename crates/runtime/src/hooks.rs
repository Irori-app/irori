//! Extension points the services layer plugs in at startup: known-fixes
//! variables, umu database ids and per-prefix setup. The runtime has sensible
//! defaults (no fixes, a cached umu database lookup, no setup), so it works
//! without any registration.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use ember_core::{GameId, GameInfo, OpContext, Paths, Runner};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::Deserialize;

use crate::env::umu_store;

/// Launch-time callbacks implemented outside the runtime.
#[async_trait]
pub trait LaunchHooks: Send + Sync {
    /// Known-fixes environment variables of a game (Heroic's
    /// `knownFixes.envVariables`). They override the user's variables but
    /// not Ember's Wine variables.
    async fn known_fixes_env(&self, _id: &GameId) -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// umu database id (`GAMEID`) of a game, used when umu runs it.
    async fn umu_id(&self, id: &GameId) -> Option<String> {
        lookup_umu_id(id).await
    }

    /// Called once per (prefix, game) right after the game was first
    /// registered in a prefix: install redistributables and known-fixes
    /// winetricks verbs here. Errors are logged, not fatal.
    async fn prefix_initialized(
        &self,
        _id: &GameId,
        _game: &GameInfo,
        _prefix: &Path,
        _ctx: &OpContext,
    ) -> Result<()> {
        Ok(())
    }
}

struct Defaults;

impl LaunchHooks for Defaults {}

static HOOKS: Lazy<RwLock<Arc<dyn LaunchHooks>>> = Lazy::new(|| RwLock::new(Arc::new(Defaults)));

/// Install the process-wide hooks (call once at startup).
pub fn set_hooks(hooks: Arc<dyn LaunchHooks>) {
    *HOOKS.write() = hooks;
}

/// The current hooks.
pub fn hooks() -> Arc<dyn LaunchHooks> {
    HOOKS.read().clone()
}

#[derive(Deserialize)]
struct UmuEntry {
    #[serde(default)]
    umu_id: Option<String>,
}

fn umu_cache_file() -> std::path::PathBuf {
    Paths::get().cache.join("umu-ids.json")
}

/// Query the umu database for a game, caching answers (including "not
/// found") on disk. Network failures are not cached.
pub async fn lookup_umu_id(id: &GameId) -> Option<String> {
    if id.runner == Runner::Sideload {
        return None;
    }
    let store = umu_store(id.runner)?;
    let key = format!("{}_{}", id.runner.id(), id.app_name);
    let file = umu_cache_file();
    let mut cache: BTreeMap<String, Option<String>> =
        ember_core::json_store::read_or_default(&file);
    if let Some(hit) = cache.get(&key) {
        return hit.clone();
    }
    let url = format!(
        "https://umu.openwinecomponents.org/umu_api.php?codename={}&store={store}",
        encode(&id.app_name.to_lowercase())
    );
    let entries: Vec<UmuEntry> = match crate::download::get_json(&url).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("umu id lookup for {id}: {e:#}");
            return None;
        }
    };
    let value = entries.into_iter().next().and_then(|e| e.umu_id);
    cache.insert(key, value.clone());
    if let Err(e) = ember_core::json_store::write(&file, &cache) {
        tracing::debug!("saving umu id cache: {e:#}");
    }
    value
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encoding() {
        assert_eq!(encode("a b&c"), "a%20b%26c");
        assert_eq!(encode("fortnite"), "fortnite");
    }
}
