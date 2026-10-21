//! Store-page details (requirements, changelog, release date) and achievements.

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use ember_core::{Achievement, ExtraInfo, InstallPlatform, Requirement, util};

use crate::api;
use crate::manifest;
use crate::{GogStore, load_json, save_json};
use ember_core::Store as _;

const EXTRA_TTL_SECS: u64 = 6 * 3600;
const ACHIEVEMENTS_TTL_SECS: u64 = 3600;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Cached<T> {
    at: u64,
    value: T,
}

/// System requirements for `os` (`windows`, `linux`, `osx`) from `api.gog.com/v2/games`.
pub fn requirements_from_v2(v: &Value, os: &str) -> Vec<Requirement> {
    let Some(sys) = v
        .pointer("/_embedded/supportedOperatingSystems")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter()
                .find(|s| s.pointer("/operatingSystem/name").and_then(Value::as_str) == Some(os))
        })
        .and_then(|s| s.get("systemRequirements"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let reqs = |i: usize| {
        sys.get(i)
            .and_then(|s| s.get("requirements"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let (minimum, recommended) = (reqs(0), reqs(1));
    minimum
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let min = r.get("description").and_then(Value::as_str)?.trim();
            if min.is_empty() {
                return None;
            }
            let id = r.get("id").and_then(Value::as_str);
            let rec = match id {
                Some(id) => recommended
                    .iter()
                    .find(|x| x.get("id").and_then(Value::as_str) == Some(id)),
                None => recommended.get(i),
            }
            .and_then(|x| x.get("description"))
            .and_then(Value::as_str)
            .unwrap_or("");
            Some(Requirement {
                title: r
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .replace(':', "")
                    .trim()
                    .to_string(),
                minimum: min.to_string(),
                recommended: rec.trim().to_string(),
            })
        })
        .collect()
}

/// Map a raw GOG achievement.
pub fn map_achievement(v: &Value) -> Achievement {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Achievement {
        id: v
            .get("achievement_id")
            .map(api::id_string)
            .unwrap_or_default(),
        key: s("achievement_key"),
        visible: v.get("visible").and_then(Value::as_bool).unwrap_or(true),
        name: s("name"),
        description: s("description"),
        image_unlocked: s("image_url_unlocked"),
        image_locked: s("image_url_locked"),
        rarity: v.get("rarity").and_then(Value::as_f64).unwrap_or(0.0) as f32,
        rarity_label: s("rarity_level_description"),
        unlocked_at: v
            .get("date_unlocked")
            .and_then(Value::as_str)
            .filter(|d| !d.is_empty())
            .map(str::to_string),
    }
}

/// Full-size screenshot URLs of a product (`expand=screenshots`).
fn screenshots(product: &Value) -> Vec<String> {
    product
        .get("screenshots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.get("formatter_template_url").and_then(Value::as_str))
        .filter_map(|template| {
            let (base, _) = template.split_once("_{formatter}")?;
            Some(format!("{base}_ggvgl.jpg"))
        })
        .collect()
}

impl GogStore {
    pub(crate) async fn fetch_extra_info(&self, app_name: &str) -> Result<ExtraInfo> {
        let path = self.paths.extra_info_cache();
        let mut cache: HashMap<String, Cached<ExtraInfo>> = load_json(path.clone()).await;
        if let Some(c) = cache.get(app_name)
            && util::unix_now().saturating_sub(c.at) < EXTRA_TTL_SECS
        {
            return Ok(c.value.clone());
        }
        let game = self.game(app_name);
        let os = match game.as_ref() {
            Some(g)
                if g.installed_platform() == Some(InstallPlatform::Linux)
                    || (g.install.is_none() && g.is_linux_native) =>
            {
                "linux"
            }
            _ => "windows",
        };
        let locale = self.api_locale();
        let (v2, product) = tokio::join!(
            api::games_v2(app_name),
            api::product(
                app_name,
                &["changelog", "description", "screenshots"],
                &locale,
                None
            )
        );
        let v2 = v2.ok().flatten();
        let product = product.ok().flatten();
        let base = game
            .as_ref()
            .and_then(|g| g.extra.clone())
            .unwrap_or_default();
        let info = ExtraInfo {
            description: base.description.or_else(|| {
                product
                    .as_ref()
                    .and_then(|p| p.pointer("/description/full"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
            short_description: base.short_description.or_else(|| {
                product
                    .as_ref()
                    .and_then(|p| p.pointer("/description/lead"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            }),
            requirements: v2
                .as_ref()
                .map(|v| requirements_from_v2(v, os))
                .unwrap_or_default(),
            release_date: v2
                .as_ref()
                .and_then(|v| v.pointer("/_embedded/product/globalReleaseDate"))
                .and_then(Value::as_str)
                .map(|s| s.chars().take(19).collect()),
            store_url: v2
                .as_ref()
                .and_then(|v| v.pointer("/_links/store/href"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| game.as_ref().and_then(|g| g.store_url.clone())),
            changelog: product
                .as_ref()
                .and_then(|p| p.get("changelog"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            genres: base.genres,
            screenshots: product.as_ref().map(screenshots).unwrap_or_default(),
        };
        if v2.is_some() || product.is_some() {
            cache.insert(
                app_name.to_string(),
                Cached {
                    at: util::unix_now(),
                    value: info.clone(),
                },
            );
            let _ = save_json(path, cache).await;
        }
        Ok(info)
    }

    /// Galaxy client id of an installed game (build record, then info file).
    pub(crate) async fn client_id(&self, app_name: &str) -> Option<String> {
        if let Some(id) = self
            .build_record(app_name)
            .await
            .and_then(|b| b.client_id())
        {
            return Some(id);
        }
        let rec = self.record(app_name)?;
        tokio::task::spawn_blocking(move || {
            manifest::read_info_file(&rec.path(), &rec.app_name, rec.platform)
                .map(|i| i.client_id)
                .filter(|c| !c.is_empty())
        })
        .await
        .ok()
        .flatten()
    }

    /// Achievements in `language`, cached per language for an hour.
    pub(crate) async fn game_achievements(
        &self,
        app_name: &str,
        language: &str,
    ) -> Result<Vec<Achievement>> {
        let language = if language.is_empty() {
            "en-US"
        } else {
            language
        };
        let key = format!("{app_name}:{language}");
        let path = self.paths.achievements_cache();
        let mut cache: HashMap<String, Cached<Vec<Achievement>>> = load_json(path.clone()).await;
        if let Some(c) = cache.get(&key)
            && util::unix_now().saturating_sub(c.at) < ACHIEVEMENTS_TTL_SECS
        {
            return Ok(c.value.clone());
        }
        if self.user_data().is_none() {
            return Ok(Vec::new());
        }
        let creds = match self.auth.credentials().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("achievements of {app_name}: {e:#}");
                return Ok(cache.remove(&key).map(|c| c.value).unwrap_or_default());
            }
        };
        let mut items = Vec::new();
        if let Some(client) = self.client_id(app_name).await {
            items = api::achievements(&creds, &client, language)
                .await
                .unwrap_or_default();
        }
        if items.is_empty() {
            items = api::achievements(&creds, app_name, language).await?;
        }
        let list: Vec<Achievement> = items.iter().map(map_achievement).collect();
        cache.insert(
            key,
            Cached {
                at: util::unix_now(),
                value: list.clone(),
            },
        );
        let _ = save_json(path, cache).await;
        Ok(list)
    }

    /// Total minutes GOG recorded for a game (`None` when unavailable or disabled).
    pub async fn gog_playtime(&self, app_name: &str) -> Option<u64> {
        if (self.settings)().disable_playtime_sync {
            return None;
        }
        let creds = self.auth.credentials().await.ok()?;
        api::playtime_minutes(&creds, app_name).await.ok()
    }

    /// Languages of a game's Linux installers (`["en-US"]` when unknown).
    pub async fn linux_installer_languages(&self, app_name: &str) -> Vec<String> {
        match api::product(app_name, &["downloads"], &self.api_locale(), None).await {
            Ok(Some(p)) => {
                let langs = api::linux_installer_languages(&p);
                if langs.is_empty() {
                    vec!["en-US".into()]
                } else {
                    langs
                }
            }
            _ => vec!["en-US".into()],
        }
    }

    /// Forget cached achievements of a game.
    pub async fn clear_achievement_cache(&self, app_name: &str) {
        let path = self.paths.achievements_cache();
        let mut cache: HashMap<String, Cached<Vec<Achievement>>> = load_json(path.clone()).await;
        cache.retain(|k, _| !k.starts_with(&format!("{app_name}:")));
        let _ = save_json(path, cache).await;
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn screenshot_urls() {
        let product = serde_json::json!({"screenshots": [
            {"formatter_template_url": "https://images-1.gog-statics.com/abc_{formatter}.png"},
            {"image_id": "no-template"}
        ]});
        assert_eq!(
            super::screenshots(&product),
            ["https://images-1.gog-statics.com/abc_ggvgl.jpg"]
        );
    }

    use super::*;

    #[test]
    fn maps_requirements() {
        let v = serde_json::json!({"_embedded": {"supportedOperatingSystems": [
            {"operatingSystem": {"name": "windows"}, "systemRequirements": [
                {"type": "minimum", "requirements": [
                    {"id": "system", "name": "System:", "description": "Windows 10"},
                    {"id": "cpu", "name": "Processor:", "description": "i5"},
                    {"id": "notes", "name": "Notes:", "description": ""}]},
                {"type": "recommended", "requirements": [
                    {"id": "cpu", "name": "Processor:", "description": "i7"}]}]}]}});
        let r = requirements_from_v2(&v, "windows");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "System");
        assert_eq!(r[0].recommended, "");
        assert_eq!(r[1].recommended, "i7");
        assert!(requirements_from_v2(&v, "linux").is_empty());
    }

    #[test]
    fn maps_achievements() {
        let v = serde_json::json!({"achievement_id": "38", "achievement_key": "k", "visible": false,
            "name": "N", "description": "D", "image_url_unlocked": "u", "image_url_locked": "l",
            "rarity": 12.5, "date_unlocked": null, "rarity_level_description": "Rare"});
        let a = map_achievement(&v);
        assert_eq!(a.id, "38");
        assert!(!a.visible);
        assert_eq!(a.rarity, 12.5);
        assert_eq!(a.rarity_label, "Rare");
        assert!(a.unlocked_at.is_none());
    }
}
