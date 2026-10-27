//! PCGamingWiki: page lookup (by GOG ID, else search) and wikitext scraping.

use std::time::Duration;

use anyhow::Result;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::util;

const API: &str = "https://www.pcgamingwiki.com/w/api.php";
const TIMEOUT: Duration = Duration::from_secs(10);

/// A review score with its site-specific ID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameScore {
    pub score: String,
    pub url_id: String,
}

/// Data scraped from a PCGamingWiki page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PcGamingWikiInfo {
    pub page_id: u64,
    pub steam_id: String,
    pub how_long_to_beat_id: String,
    pub metacritic: GameScore,
    pub opencritic: GameScore,
    pub igdb: GameScore,
    pub direct3d_versions: Vec<String>,
    pub genres: Vec<String>,
    /// `"<platform>: <date>"` entries.
    pub release_dates: Vec<String>,
}

impl PcGamingWikiInfo {
    /// Link to the wiki page.
    pub fn page_url(&self) -> String {
        format!("https://www.pcgamingwiki.com/wiki/?curid={}", self.page_id)
    }
}

static METACRITIC: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)game/row/reception\|Metacritic\|(\S+)\|([0-9]+)").unwrap());
static OPENCRITIC: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)game/row/reception\|OpenCritic\|(\S+)\|([0-9]+)").unwrap());
static IGDB: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)game/row/reception\|IGDB\|(\S+)\|([0-9]+)").unwrap());
static STEAM_ID: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)steam appid {2}= ([0-9]+)").unwrap());
static HLTB_ID: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)hltb {9}= ([0-9]+)").unwrap());
static D3D: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)direct3d versions {6}= (.+)").unwrap());
static GENRES: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"game/row/taxonomy/genres\s*\|\s*(.*?)\s*\}\}").unwrap());
static RELEASE: Lazy<Regex> = Lazy::new(|| Regex::new(r"game/row/date\|([^|]+)\|([^}]*)").unwrap());

fn score(re: &Regex, text: &str) -> GameScore {
    re.captures(text)
        .map(|c| GameScore {
            url_id: c[1].to_string(),
            score: c[2].to_string(),
        })
        .unwrap_or_default()
}

fn list(re: &Regex, text: &str) -> Vec<String> {
    re.captures(text)
        .map(|c| {
            c[1].split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Extract the fields Irori shows from page wikitext.
pub fn parse_wikitext(page_id: u64, text: &str) -> PcGamingWikiInfo {
    let group = |re: &Regex| {
        re.captures(text)
            .map(|c| c[1].to_string())
            .unwrap_or_default()
    };
    PcGamingWikiInfo {
        page_id,
        steam_id: group(&STEAM_ID),
        how_long_to_beat_id: group(&HLTB_ID),
        metacritic: score(&METACRITIC, text),
        opencritic: score(&OPENCRITIC, text),
        igdb: score(&IGDB, text),
        direct3d_versions: list(&D3D, text),
        genres: list(&GENRES, text),
        release_dates: RELEASE
            .captures_iter(text)
            .map(|c| format!("{}: {}", &c[1], &c[2]))
            .collect(),
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    query: Option<SearchQuery>,
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    search: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct SearchHit {
    pageid: u64,
    #[serde(default)]
    title: String,
}

/// Page ID from a search response: the exact (case-insensitive) title match,
/// else the first hit.
pub fn pick_search_result(json: &serde_json::Value, title: &str) -> Option<u64> {
    let resp: SearchResponse = serde_json::from_value(json.clone()).ok()?;
    let hits = resp.query?.search;
    hits.iter()
        .find(|h| h.title.to_lowercase() == title.to_lowercase())
        .or(hits.first())
        .map(|h| h.pageid)
}

/// Page ID from a Cargo query by GOG ID.
pub fn parse_cargo_page_id(json: &serde_json::Value) -> Option<u64> {
    let v = json
        .get("cargoquery")?
        .get(0)?
        .get("title")?
        .get("pageID")?;
    match v {
        serde_json::Value::String(s) => s.parse().ok(),
        serde_json::Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

/// Look the game up. `Ok(None)` when the wiki has no page for it.
pub async fn fetch(title: &str, gog_id: Option<&str>) -> Result<Option<PcGamingWikiInfo>> {
    let title = title.replacen(" -", ":", 1);
    let mut page_id = None;
    if let Some(gog) = gog_id.filter(|s| !s.is_empty()) {
        let url = format!(
            "{API}?action=cargoquery&tables=Infobox_game&fields=Infobox_game._pageID%3DpageID%2C&where=Infobox_game.GOGcom_ID%20HOLDS%20{}&format=json",
            util::encode_component(gog)
        );
        let json: serde_json::Value = util::get_json(&url, TIMEOUT).await?;
        page_id = parse_cargo_page_id(&json);
    }
    if page_id.is_none() {
        let url = format!(
            "{API}?action=query&list=search&srsearch={}&format=json",
            util::encode_component(&title)
        );
        let json: serde_json::Value = util::get_json(&url, TIMEOUT).await?;
        page_id = pick_search_result(&json, &title);
    }
    let Some(page_id) = page_id else {
        return Ok(None);
    };
    let url =
        format!("{API}?action=parse&format=json&pageid={page_id}&redirects=true&prop=wikitext");
    let json: serde_json::Value = util::get_json(&url, TIMEOUT).await?;
    let text = json
        .pointer("/parse/wikitext/*")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(parse_wikitext(page_id, text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIKITEXT: &str = "{{Infobox game\n|steam appid  = 1091500\n|hltb         = 2127\n\
|direct3d versions      = 12, 11\n}}\n{{Infobox game/row/reception|Metacritic|cyberpunk-2077|86}}\n\
{{Infobox game/row/reception|OpenCritic|8525/cyberpunk-2077|89}}\n{{Infobox game/row/reception|IGDB|cyberpunk-2077|84}}\n\
{{Infobox game/row/taxonomy/genres | Action, RPG, Open world }}\n\
{{Infobox game/row/date|Windows|December 10, 2020}}\n{{Infobox game/row/date|OS X|TBA}}\n";

    #[test]
    fn parses_wikitext() {
        let info = parse_wikitext(7, WIKITEXT);
        assert_eq!(info.steam_id, "1091500");
        assert_eq!(info.how_long_to_beat_id, "2127");
        assert_eq!(info.direct3d_versions, ["12", "11"]);
        assert_eq!(info.genres, ["Action", "RPG", "Open world"]);
        assert_eq!(
            info.metacritic,
            GameScore {
                score: "86".into(),
                url_id: "cyberpunk-2077".into()
            }
        );
        assert_eq!(info.opencritic.url_id, "8525/cyberpunk-2077");
        assert_eq!(info.igdb.score, "84");
        assert_eq!(
            info.release_dates,
            ["Windows: December 10, 2020", "OS X: TBA"]
        );
        assert!(info.page_url().ends_with("curid=7"));
    }

    #[test]
    fn empty_wikitext_gives_empty_lists() {
        let info = parse_wikitext(1, "nothing here");
        assert!(info.genres.is_empty());
        assert!(info.direct3d_versions.is_empty());
        assert_eq!(info.steam_id, "");
    }

    #[test]
    fn search_and_cargo() {
        let json = serde_json::json!({"query": {"search": [
            {"pageid": 1, "title": "Doom Eternal"}, {"pageid": 2, "title": "DOOM"}]}});
        assert_eq!(pick_search_result(&json, "doom"), Some(2));
        assert_eq!(pick_search_result(&json, "Doom 3"), Some(1));
        assert_eq!(
            pick_search_result(&serde_json::json!({"query": {"search": []}}), "x"),
            None
        );
        let cargo = serde_json::json!({"cargoquery": [{"title": {"pageID": "4242"}}]});
        assert_eq!(parse_cargo_page_id(&cargo), Some(4242));
        assert_eq!(
            parse_cargo_page_id(&serde_json::json!({"cargoquery": []})),
            None
        );
    }
}
