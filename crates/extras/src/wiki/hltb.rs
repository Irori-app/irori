//! HowLongToBeat times, from the HLTB game page (by PCGamingWiki's HLTB ID)
//! or, for GOG games, from the GOG store page's HLTB box.

use std::time::Duration;

use anyhow::Result;
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use serde::{Deserialize, Serialize};

/// Completion times in hours (rounded).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HowLongToBeat {
    pub main_story: u32,
    pub main_extra: u32,
    pub completionist: u32,
    pub game_id: Option<u64>,
    pub game_name: Option<String>,
    pub game_image_url: Option<String>,
    pub game_web_link: Option<String>,
}

const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

fn browser_headers(referer: Option<&'static str>) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(USER_AGENT, HeaderValue::from_static(BROWSER_UA));
    h.insert(
        ACCEPT,
        HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8",
        ),
    );
    h.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
    h.insert("Upgrade-Insecure-Requests", HeaderValue::from_static("1"));
    if let Some(r) = referer {
        h.insert(REFERER, HeaderValue::from_static(r));
    }
    h
}

/// GET an HTML page. `Ok(None)` for 4xx responses (page missing or blocked).
async fn get_page(url: &str, referer: Option<&'static str>) -> Result<Option<String>> {
    let resp = ember_core::http::client()
        .get(url)
        .headers(browser_headers(referer))
        .timeout(Duration::from_secs(10))
        .send()
        .await?;
    let status = resp.status();
    if status.is_server_error() {
        anyhow::bail!("{url} returned {status}");
    }
    if !status.is_success() {
        return Ok(None);
    }
    Ok(Some(resp.text().await?))
}

static NEXT_DATA: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?s)<script id="__NEXT_DATA__" type="application/json">(.*?)</script>"#).unwrap()
});

fn hours(seconds: Option<f64>) -> u32 {
    seconds
        .map(|s| (s / 3600.0).round().max(0.0) as u32)
        .unwrap_or(0)
}

/// Parse an HLTB game page (`__NEXT_DATA__` JSON).
pub fn parse_hltb_page(html: &str) -> Option<HowLongToBeat> {
    let json: serde_json::Value =
        serde_json::from_str(NEXT_DATA.captures(html)?.get(1)?.as_str()).ok()?;
    let game = json.pointer("/props/pageProps/game/data/game/0")?;
    let game_id = game.get("game_id")?.as_u64()?;
    let num = |k: &str| game.get(k).and_then(serde_json::Value::as_f64);
    let text = |k: &str| {
        game.get(k)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(HowLongToBeat {
        main_story: hours(num("comp_main")),
        main_extra: hours(num("comp_plus")),
        completionist: hours(num("comp_100")),
        game_id: Some(game_id),
        game_name: text("game_name"),
        game_image_url: text("game_image").map(|i| format!("https://howlongtobeat.com/games/{i}")),
        game_web_link: Some(format!("https://howlongtobeat.com/game/{game_id}")),
    })
}

static GOG_TIME: Lazy<[Regex; 3]> = Lazy::new(|| {
    ["Main", r"Main \+ Sides", "Completionist"].map(|label| {
        Regex::new(&format!(
            r#"<span class="howlongtobeat-box__time"> ?([\d.]+) h ?</span><span class="howlongtobeat-box__name">{label}</span>"#
        ))
        .unwrap()
    })
});
static GOG_ID: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"<a target="_blank" href="https://howlongtobeat.com/game/(\d+)">HowLongToBeat</a>"#,
    )
    .unwrap()
});

/// Parse the HLTB box of a GOG store page. All three times and the link are
/// required.
pub fn parse_gog_store_page(html: &str, title: &str) -> Option<HowLongToBeat> {
    let time = |re: &Regex| -> Option<u32> {
        let v: f64 = re.captures(html)?.get(1)?.as_str().parse().ok()?;
        Some(v.round().max(0.0) as u32)
    };
    let [main, extra, full] = &*GOG_TIME;
    let game_id: u64 = GOG_ID.captures(html)?.get(1)?.as_str().parse().ok()?;
    Some(HowLongToBeat {
        main_story: time(main)?,
        main_extra: time(extra)?,
        completionist: time(full)?,
        game_id: Some(game_id),
        game_name: Some(title.to_string()),
        game_image_url: None,
        game_web_link: Some(format!("https://howlongtobeat.com/game/{game_id}")),
    })
}

/// Times from HLTB by ID.
pub async fn fetch_by_id(hltb_id: &str) -> Result<Option<HowLongToBeat>> {
    let url = format!("https://howlongtobeat.com/game/{hltb_id}");
    Ok(get_page(&url, Some("https://howlongtobeat.com"))
        .await?
        .and_then(|html| parse_hltb_page(&html)))
}

/// Times from a GOG store page.
pub async fn fetch_from_gog_store(store_url: &str, title: &str) -> Result<Option<HowLongToBeat>> {
    Ok(get_page(store_url, None)
        .await?
        .and_then(|html| parse_gog_store_page(&html, title)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hltb_page() {
        let html = r#"<html><script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"game":{"data":{"game":[{"game_id":2127,"game_name":"Cyberpunk 2077","game_image":"x.jpg","comp_main":91800,"comp_plus":217000,"comp_100":0}]}}}}}</script></html>"#;
        let h = parse_hltb_page(html).unwrap();
        assert_eq!((h.main_story, h.main_extra, h.completionist), (26, 60, 0));
        assert_eq!(h.game_id, Some(2127));
        assert_eq!(
            h.game_image_url.as_deref(),
            Some("https://howlongtobeat.com/games/x.jpg")
        );
        assert_eq!(
            h.game_web_link.as_deref(),
            Some("https://howlongtobeat.com/game/2127")
        );
        assert!(parse_hltb_page("<html></html>").is_none());
    }

    #[test]
    fn gog_store_page() {
        let html = r#"<span class="howlongtobeat-box__time">25.5 h</span><span class="howlongtobeat-box__name">Main</span>
<span class="howlongtobeat-box__time"> 60 h </span><span class="howlongtobeat-box__name">Main + Sides</span>
<span class="howlongtobeat-box__time">104 h</span><span class="howlongtobeat-box__name">Completionist</span>
<a target="_blank" href="https://howlongtobeat.com/game/2127">HowLongToBeat</a>"#;
        let h = parse_gog_store_page(html, "Cyberpunk").unwrap();
        assert_eq!((h.main_story, h.main_extra, h.completionist), (26, 60, 104));
        assert_eq!(h.game_name.as_deref(), Some("Cyberpunk"));
        assert!(parse_gog_store_page("<span></span>", "x").is_none());
    }
}
