//! Store-page details (description, requirements, release date) from Epic's
//! web APIs, and the Epic service status check.

use anyhow::Result;
use irori_core::{ExtraInfo, Requirement, http};
use serde_json::{Value, json};

const GRAPHQL_URL: &str = "https://launcher.store.epicgames.com/graphql";
const STORE_CONTENT_URL: &str = "https://store-content.ak.epicgames.com/api";
const STATUS_URL: &str = "https://status.epicgames.com/api/v2/components.json";
/// User agent Epic's launcher APIs expect.
pub(crate) const EGL_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
AppleWebKit/537.36 (KHTML, like Gecko) EpicGamesLauncher";

pub(crate) fn escape_gql(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) async fn graphql(query: String) -> Result<Value> {
    Ok(http::client()
        .post(GRAPHQL_URL)
        .header(reqwest::header::USER_AGENT, EGL_USER_AGENT)
        .json(&json!({ "query": query }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

/// [`graphql`] as the signed-in player, for queries about their own data.
pub(crate) async fn graphql_signed(
    query: String,
    session: &irori_epic_api::Session,
) -> Result<Value> {
    Ok(http::client()
        .post(GRAPHQL_URL)
        .header(reqwest::header::USER_AGENT, EGL_USER_AGENT)
        .bearer_auth(&session.access_token)
        .json(&json!({ "query": query }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

/// Slug of a title when Epic's catalogue has no mapping.
pub(crate) fn slug_from_title(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || *c == ' ')
        .collect::<String>()
        .replace(' ', "-")
}

/// `productHome` page slug from the catalogue mapping response.
pub(crate) fn parse_slug(v: &Value) -> Option<String> {
    v.pointer("/data/Catalog/catalogNs/mappings")?
        .as_array()?
        .iter()
        .find(|m| m.get("pageType").and_then(Value::as_str) == Some("productHome"))?
        .get("pageSlug")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn product_url(slug: &str) -> String {
    format!("https://www.epicgames.com/store/product/{slug}")
}

fn requirements(v: Option<&Value>) -> Vec<Requirement> {
    v.and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|r| Requirement {
                    title: str_field(r, "title"),
                    minimum: str_field(r, "minimum"),
                    recommended: str_field(r, "recommended"),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Map the store-content product response.
pub(crate) fn parse_store_content(v: &Value, slug: &str) -> Option<ExtraInfo> {
    let page = v
        .get("pages")?
        .as_array()?
        .iter()
        .find(|p| p.get("type").and_then(Value::as_str) == Some("productHome"))?;
    let data = page.get("data")?;
    let about = data.get("about");
    Some(ExtraInfo {
        description: about.map(|a| str_field(a, "description")),
        short_description: about.map(|a| str_field(a, "shortDescription")),
        requirements: requirements(data.pointer("/requirements/systems/0/details")),
        release_date: data
            .pointer("/meta/releaseDate")
            .and_then(Value::as_str)
            .map(|d| d.chars().take(19).collect()),
        store_url: Some(product_url(slug)),
        screenshots: gallery(data),
        ..Default::default()
    })
}

/// Screenshot URLs of a store page's gallery.
fn gallery(data: &Value) -> Vec<String> {
    data.pointer("/gallery/galleryImages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|i| i.get("src").and_then(Value::as_str))
        .filter(|s| s.starts_with("https://"))
        .map(str::to_string)
        .collect()
}

/// Map the GraphQL product configuration response (fallback).
pub(crate) fn parse_product_config(v: &Value, slug: &str) -> Option<ExtraInfo> {
    let configs = v.pointer("/data/Product/sandbox/configuration/0/configs")?;
    let reqs = configs.pointer("/technicalRequirements/windows")?;
    if reqs.is_null() {
        return None;
    }
    Some(ExtraInfo {
        description: Some(str_field(configs, "shortDescription")),
        short_description: Some(String::new()),
        requirements: requirements(Some(reqs)),
        store_url: Some(product_url(slug)),
        ..Default::default()
    })
}

/// Epic's store locale for Irori's UI language.
pub(crate) fn store_locale(ui_language: &str) -> String {
    match ui_language {
        "" => "en-US".into(),
        "pt" => "pt-BR".into(),
        "zh_Hans" => "zh-CN".into(),
        "es" => "es-ES".into(),
        other => other.into(),
    }
}

async fn product_slug(namespace: &str, title: &str) -> String {
    let query = format!(
        "{{ Catalog {{ catalogNs(namespace: \"{}\") {{ mappings (pageType: \"productHome\") {{ pageSlug pageType }} }} }} }}",
        escape_gql(namespace)
    );
    match graphql(query).await {
        Ok(v) => parse_slug(&v).unwrap_or_else(|| slug_from_title(title)),
        Err(e) => {
            tracing::warn!("Epic product slug lookup failed: {e:#}");
            slug_from_title(title)
        }
    }
}

/// Fetch store page details: store-content API first, GraphQL configuration
/// as fallback. `None` when neither has data.
pub(crate) async fn fetch_extra_info(
    namespace: &str,
    title: &str,
    ui_language: &str,
) -> Option<ExtraInfo> {
    let slug = product_slug(namespace, title).await;
    let url = format!(
        "{STORE_CONTENT_URL}/{}/content/products/{slug}",
        store_locale(ui_language)
    );
    match http::get_json::<Value>(&url).await {
        Ok(v) => {
            if let Some(info) = parse_store_content(&v, &slug) {
                return Some(info);
            }
        }
        Err(e) => tracing::warn!("Failed to get info from Epic API: {e:#}"),
    }
    let query = format!(
        "{{ Product {{ sandbox(sandboxId: \"{}\") {{ configuration {{ ... on StoreConfiguration {{ configs {{ \
         shortDescription technicalRequirements {{ macos {{ minimum recommended title }} \
         windows {{ minimum recommended title }} }} }} }} }} }} }} }}",
        escape_gql(namespace)
    );
    match graphql(query).await {
        Ok(v) => parse_product_config(&v, &slug),
        Err(e) => {
            tracing::warn!("Epic GraphQL product lookup failed: {e:#}");
            None
        }
    }
}

/// Whether Epic's status page reports a major outage of the store. Heroic
/// compared against `"major"`, which the status page never reports; the
/// statuspage value is `"major_outage"`. Request failures count as online.
pub(crate) fn parse_status_offline(v: &Value) -> bool {
    v.get("components")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter()
                .find(|c| c.get("name").and_then(Value::as_str) == Some("Epic Games Store"))
        })
        .and_then(|c| c.get("status").and_then(Value::as_str))
        .is_some_and(|s| s == "major_outage" || s == "major")
}

pub(crate) async fn fetch_service_offline() -> bool {
    let req = http::client()
        .get(STATUS_URL)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await;
    match req {
        Ok(resp) => match resp.json::<Value>().await {
            Ok(v) => parse_status_offline(&v),
            Err(_) => false,
        },
        Err(e) => {
            tracing::warn!("Failed to get Epic service status: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        let v = json!({"data":{"Catalog":{"catalogNs":{"mappings":[
            {"pageSlug":"other","pageType":"offer"},
            {"pageSlug":"rocket-league","pageType":"productHome"}]}}}});
        assert_eq!(parse_slug(&v).as_deref(), Some("rocket-league"));
        assert_eq!(parse_slug(&json!({"data":null})), None);
        assert_eq!(
            slug_from_title("Alba: A Wildlife Adventure"),
            "alba-a-wildlife-adventure"
        );
    }

    #[test]
    fn store_content() {
        let v = json!({"pages":[{"type":"addon"},{"type":"productHome","data":{
            "about":{"description":"Long","shortDescription":"Short"},
            "requirements":{"systems":[{"systemType":"Windows","details":[
                {"title":"OS","minimum":"Win 10","recommended":"Win 11"}]}]},
            "meta":{"releaseDate":"2020-09-23T15:00:00.000Z"},
            "gallery":{"galleryImages":[{"src":"https://cdn2.unrealengine.com/a.jpg"},{"src":""}]}}}]});
        let info = parse_store_content(&v, "alba").unwrap();
        assert_eq!(info.screenshots, ["https://cdn2.unrealengine.com/a.jpg"]);
        assert_eq!(info.description.as_deref(), Some("Long"));
        assert_eq!(info.short_description.as_deref(), Some("Short"));
        assert_eq!(info.requirements[0].recommended, "Win 11");
        assert_eq!(info.release_date.as_deref(), Some("2020-09-23T15:00:00"));
        assert_eq!(
            info.store_url.as_deref(),
            Some("https://www.epicgames.com/store/product/alba")
        );
        assert!(parse_store_content(&json!({"pages":[]}), "x").is_none());
    }

    #[test]
    fn product_config() {
        let v = json!({"data":{"Product":{"sandbox":{"configuration":[{"configs":{
            "shortDescription":"Desc","technicalRequirements":{"macos":null,
            "windows":[{"title":"CPU","minimum":"i5","recommended":"i7"}]}}}]}}}});
        let info = parse_product_config(&v, "s").unwrap();
        assert_eq!(info.description.as_deref(), Some("Desc"));
        assert_eq!(info.requirements[0].title, "CPU");
        let none = json!({"data":{"Product":{"sandbox":{"configuration":[]}}}});
        assert!(parse_product_config(&none, "s").is_none());
    }

    #[test]
    fn locales_and_status() {
        assert_eq!(store_locale("pt"), "pt-BR");
        assert_eq!(store_locale(""), "en-US");
        assert_eq!(store_locale("de"), "de");
        let down = json!({"components":[{"name":"Fortnite","status":"major_outage"},
                                        {"name":"Epic Games Store","status":"major_outage"}]});
        assert!(parse_status_offline(&down));
        let up = json!({"components":[{"name":"Epic Games Store","status":"operational"}]});
        assert!(!parse_status_offline(&up));
        assert!(!parse_status_offline(&json!({})));
    }
}
