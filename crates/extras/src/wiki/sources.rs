//! Small JSON sources: ProtonDB tier, Steam Deck compatibility, GOG GamesDB
//! (Steam app ID by store ID) and umu IDs.

use std::time::Duration;

use anyhow::Result;
use ember_core::Runner;
use serde::{Deserialize, Serialize};

use crate::util;

const TIMEOUT: Duration = Duration::from_secs(10);

/// ProtonDB summary tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtonDbTier {
    Native,
    Platinum,
    Gold,
    Silver,
    Bronze,
    Borked,
    Pending,
    #[serde(other)]
    Unknown,
}

impl ProtonDbTier {
    pub fn label(self) -> &'static str {
        match self {
            ProtonDbTier::Native => "Native",
            ProtonDbTier::Platinum => "Platinum",
            ProtonDbTier::Gold => "Gold",
            ProtonDbTier::Silver => "Silver",
            ProtonDbTier::Bronze => "Bronze",
            ProtonDbTier::Borked => "Borked",
            ProtonDbTier::Pending => "Pending",
            ProtonDbTier::Unknown => "Unknown",
        }
    }
}

/// Steam Deck compatibility category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SteamDeckCompat {
    Unknown,
    Unsupported,
    Playable,
    Verified,
}

impl SteamDeckCompat {
    /// Valve's `resolved_category`: 0 unknown, 1 unsupported, 2 playable,
    /// 3 verified.
    pub fn from_category(c: i64) -> Self {
        match c {
            1 => SteamDeckCompat::Unsupported,
            2 => SteamDeckCompat::Playable,
            3 => SteamDeckCompat::Verified,
            _ => SteamDeckCompat::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SteamDeckCompat::Unknown => "Unknown",
            SteamDeckCompat::Unsupported => "Unsupported",
            SteamDeckCompat::Playable => "Playable",
            SteamDeckCompat::Verified => "Verified",
        }
    }
}

/// `tier` of a ProtonDB summary.
pub fn parse_protondb(json: &serde_json::Value) -> Option<ProtonDbTier> {
    let tier = json.get("tier")?.as_str()?;
    serde_json::from_value(serde_json::Value::String(tier.to_ascii_lowercase())).ok()
}

/// `results.resolved_category` of Valve's Deck report.
pub fn parse_deck(json: &serde_json::Value) -> Option<SteamDeckCompat> {
    json.pointer("/results/resolved_category")?
        .as_i64()
        .map(SteamDeckCompat::from_category)
}

/// ProtonDB tier for a Steam app. `Ok(None)` when ProtonDB has no summary.
pub async fn protondb(steam_id: &str) -> Result<Option<ProtonDbTier>> {
    let url = format!("https://www.protondb.com/api/v1/reports/summaries/{steam_id}.json");
    let resp = ember_core::http::client()
        .get(&url)
        .timeout(TIMEOUT)
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let json: serde_json::Value = resp.error_for_status()?.json().await?;
    let tier = parse_protondb(&json);
    if tier.is_none() {
        tracing::debug!("ProtonDB: no tier in response for {steam_id}");
    }
    Ok(tier)
}

/// Steam Deck compatibility for a Steam app.
pub async fn steam_deck(steam_id: &str) -> Result<Option<SteamDeckCompat>> {
    let url = format!(
        "https://store.steampowered.com/saleaction/ajaxgetdeckappcompatibilityreport?nAppID={steam_id}"
    );
    let json: serde_json::Value = util::get_json(&url, TIMEOUT).await?;
    Ok(parse_deck(&json))
}

/// GamesDB platform name of a runner.
pub fn gamesdb_store(runner: Runner) -> Option<&'static str> {
    match runner {
        Runner::Legendary => Some("epic"),
        Runner::Gog => Some("gog"),
        Runner::Nile => Some("amazon"),
        Runner::Zoom => Some("zoom"),
        Runner::Itch => Some("itch"),
        Runner::Steam | Runner::Sideload => None,
    }
}

/// Steam app ID in a GamesDB `external_releases` response.
pub fn parse_gamesdb(json: &serde_json::Value) -> Option<String> {
    json.pointer("/game/releases")?
        .as_array()?
        .iter()
        .find(|r| r.get("platform_id").and_then(|p| p.as_str()) == Some("steam"))?
        .get("external_id")
        .and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .filter(|s| !s.is_empty())
}

/// Steam app ID of a store game according to GOG's GamesDB. A Steam game's
/// app name is its Steam app ID.
pub async fn gamesdb_steam_id(runner: Runner, app_name: &str) -> Result<Option<String>> {
    if runner == Runner::Steam {
        return Ok(Some(app_name.to_string()));
    }
    let Some(store) = gamesdb_store(runner) else {
        return Ok(None);
    };
    let url = format!(
        "https://gamesdb.gog.com/platforms/{store}/external_releases/{}",
        util::encode_component(app_name)
    );
    let resp = ember_core::http::client()
        .get(&url)
        .timeout(TIMEOUT)
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let json: serde_json::Value = resp.error_for_status()?.json().await?;
    Ok(parse_gamesdb(&json))
}

/// umu database store name of a runner.
pub fn umu_store(runner: Runner) -> Option<&'static str> {
    match runner {
        Runner::Gog => Some("gog"),
        Runner::Legendary => Some("egs"),
        Runner::Nile => Some("amazon"),
        Runner::Zoom => Some("zoomplatform"),
        Runner::Itch => Some("itchio"),
        Runner::Steam | Runner::Sideload => None,
    }
}

#[derive(Deserialize)]
struct UmuEntry {
    umu_id: Option<String>,
}

/// First `umu_id` of a umu API response.
pub fn parse_umu(json: &serde_json::Value) -> Option<String> {
    let entries: Vec<UmuEntry> = serde_json::from_value(json.clone()).ok()?;
    entries.into_iter().next()?.umu_id.filter(|s| !s.is_empty())
}

/// Query the umu database.
pub async fn umu_lookup(runner: Runner, app_name: &str) -> Result<Option<String>> {
    let Some(store) = umu_store(runner) else {
        return Ok(None);
    };
    let url = format!(
        "https://umu.openwinecomponents.org/umu_api.php?codename={}&store={store}",
        util::encode_component(&app_name.to_lowercase())
    );
    let json: serde_json::Value = util::get_json(&url, TIMEOUT).await?;
    Ok(parse_umu(&json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn protondb_and_deck() {
        assert_eq!(
            parse_protondb(&json!({"tier": "platinum"})),
            Some(ProtonDbTier::Platinum)
        );
        assert_eq!(
            parse_protondb(&json!({"tier": "Gold"})),
            Some(ProtonDbTier::Gold)
        );
        assert_eq!(
            parse_protondb(&json!({"tier": "weird"})),
            Some(ProtonDbTier::Unknown)
        );
        assert_eq!(parse_protondb(&json!({})), None);
        assert_eq!(
            parse_deck(&json!({"success": 1, "results": {"resolved_category": 3}})),
            Some(SteamDeckCompat::Verified)
        );
        assert_eq!(parse_deck(&json!({"results": {}})), None);
    }

    #[test]
    fn gamesdb_and_umu() {
        let j = json!({"game": {"releases": [
            {"platform_id": "gog", "external_id": "1"},
            {"platform_id": "steam", "external_id": "1091500"}]}});
        assert_eq!(parse_gamesdb(&j).as_deref(), Some("1091500"));
        assert_eq!(parse_gamesdb(&json!({"game": {"releases": []}})), None);
        assert_eq!(
            parse_umu(&json!([{"title": "x", "umu_id": "umu-1091500"}])).as_deref(),
            Some("umu-1091500")
        );
        assert_eq!(parse_umu(&json!([])), None);
        assert_eq!(gamesdb_store(Runner::Sideload), None);
        assert_eq!(umu_store(Runner::Legendary), Some("egs"));
    }
}
