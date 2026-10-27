//! Downloadable versions of an engine for this computer, from the managed
//! sources of its manifest (GitHub releases, the libretro buildbot or a
//! download page). Lists are cached per engine, so the Emulators page opens
//! instantly and works offline; [`list`] refreshes a list older than a day.
//!
//! Download links are never pinned in a manifest: each source says where
//! versions are listed and how to recognise this computer's build, so new
//! versions appear without an Irori update.

use std::cmp::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use irori_core::json_store;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::host::Host;
use crate::manifest::{EngineManifest, Source};

/// How long a cached list is used without asking the source again.
const FRESH: Duration = Duration::from_secs(24 * 60 * 60);
/// Versions kept per list.
const KEEP: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checksum {
    /// `sha256` or `crc32`.
    pub algorithm: String,
    pub value: String,
}

/// One downloadable build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub version: String,
    /// Publication date, `YYYY-MM-DD`, when the source says.
    #[serde(default)]
    pub date: Option<String>,
    pub url: String,
    pub file_name: String,
    /// Download size in bytes, when the source says.
    #[serde(default)]
    pub size: Option<u64>,
    /// Where the expected digest is published, fetched at install time.
    #[serde(default)]
    pub checksum_url: Option<String>,
    /// Further files of the release this computer needs, unpacked beside
    /// the main one (see `extraAssets` in the manifest).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extras: Vec<ReleaseFile>,
}

/// A further file of a [`Release`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseFile {
    pub url: String,
    pub file_name: String,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedList {
    fetched_at: u64,
    host: String,
    releases: Vec<Release>,
}

fn cache_file(id: &str) -> std::path::PathBuf {
    crate::dirs::cache()
        .join("releases")
        .join(format!("{id}.json"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The cached list of `id` for this host, without touching the network.
pub fn cached(id: &str) -> Option<Vec<Release>> {
    let list: CachedList = json_store::read(&cache_file(id)).ok().flatten()?;
    (list.host == Host::current().key()).then_some(list.releases)
}

/// Versions of `manifest` for this computer, newest first. `refresh` asks
/// the source even when the cached list is fresh. Offline, a cached list is
/// returned as it is.
pub async fn list(manifest: &EngineManifest, refresh: bool) -> Result<Vec<Release>> {
    let host = Host::current();
    let file = cache_file(&manifest.id);
    let cached: Option<CachedList> = json_store::read(&file).ok().flatten();
    let cached = cached.filter(|c| c.host == host.key());
    if !refresh
        && let Some(c) = &cached
        && now().saturating_sub(c.fetched_at) < FRESH.as_secs()
    {
        return Ok(c.releases.clone());
    }
    match fetch(manifest, host).await {
        Ok(releases) => {
            let list = CachedList {
                fetched_at: now(),
                host: host.key(),
                releases: releases.clone(),
            };
            if let Err(e) = json_store::write(&file, &list) {
                tracing::warn!("caching {} releases: {e:#}", manifest.id);
            }
            Ok(releases)
        }
        Err(e) => match cached {
            Some(c) => {
                tracing::warn!(
                    "listing {} releases: {e:#}; using the saved list",
                    manifest.id
                );
                Ok(c.releases)
            }
            None => Err(e),
        },
    }
}

/// Ask the first managed source with a build for `host`.
pub async fn fetch(manifest: &EngineManifest, host: Host) -> Result<Vec<Release>> {
    let Some(source) = manifest
        .sources
        .iter()
        .find(|s| s.asset_for(host).is_some())
    else {
        bail!("{} has no download for {host}", manifest.name);
    };
    let pattern = source.asset_for(host).unwrap_or_default();
    let extras = source.extras_for(host);
    let mut releases = match source {
        Source::Github {
            repo, checksums, ..
        } => {
            let url = format!("https://api.github.com/repos/{repo}/releases?per_page=100");
            let body = get_text(&url).await?;
            let mut found = from_github(&body, pattern, extras, checksums.as_deref())?;
            // Projects publishing nightly pre-releases (PCSX2) can push every
            // stable release off the list; GitHub's "latest" is the newest
            // stable one.
            if found.is_empty() {
                let latest = format!("https://api.github.com/repos/{repo}/releases/latest");
                let body = get_text(&latest).await?;
                found = from_github(&format!("[{body}]"), pattern, extras, checksums.as_deref())?;
            }
            found
        }
        Source::Buildbot {
            index,
            version_pattern,
            ..
        } => {
            let body = get_text(index).await?;
            from_buildbot(&body, index, version_pattern, pattern)?
        }
        Source::Page { url, base, .. } => {
            let body = get_text(url).await?;
            from_page(&body, pattern, base.as_deref().unwrap_or(""))?
        }
        _ => unreachable!("only managed sources have assets"),
    };
    releases.sort_by(|a, b| compare_versions(&b.version, &a.version));
    releases.dedup_by(|a, b| a.version == b.version);
    releases.truncate(KEEP);
    if releases.is_empty() {
        bail!("{} lists no download for {host} right now", manifest.name);
    }
    Ok(releases)
}

async fn get_text(url: &str) -> Result<String> {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    irori_core::http::client()
        .get(url)
        .send()
        .await
        .with_context(|| format!("could not reach {host}"))?
        .error_for_status()
        .with_context(|| format!("{host} refused the request"))?
        .text()
        .await
        .with_context(|| format!("unexpected answer from {host}"))
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    updated_at: Option<String>,
}

fn date_of(stamp: Option<&str>) -> Option<String> {
    stamp.and_then(|s| s.get(..10)).map(str::to_string)
}

fn from_github(
    body: &str,
    pattern: &str,
    extras: &[String],
    checksums: Option<&str>,
) -> Result<Vec<Release>> {
    let list: Vec<GhRelease> =
        serde_json::from_str(body).context("unexpected answer from GitHub")?;
    let asset_re = Regex::new(pattern)?;
    let extra_res = extras
        .iter()
        .map(|p| Regex::new(p))
        .collect::<Result<Vec<_>, _>>()?;
    let sums_re = checksums.map(Regex::new).transpose()?;
    let mut out = Vec::new();
    for r in list.into_iter().filter(|r| !r.draft && !r.prerelease) {
        let Some(asset) = r.assets.iter().find(|a| asset_re.is_match(&a.name)) else {
            continue;
        };
        let Some(extras) = extra_res
            .iter()
            .map(|re| {
                r.assets
                    .iter()
                    .find(|a| re.is_match(&a.name))
                    .map(|a| ReleaseFile {
                        url: a.browser_download_url.clone(),
                        file_name: a.name.clone(),
                        size: (a.size > 0).then_some(a.size),
                    })
            })
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        // Rolling releases ("latest") are told apart by their build date.
        let version = if r.tag_name.chars().any(|c| c.is_ascii_digit()) {
            r.tag_name.trim_start_matches(['v', 'V', '.']).to_string()
        } else {
            date_of(asset.updated_at.as_deref())
                .or_else(|| date_of(r.published_at.as_deref()))
                .unwrap_or(r.tag_name.clone())
        };
        out.push(Release {
            version,
            date: date_of(asset.updated_at.as_deref().or(r.published_at.as_deref())),
            url: asset.browser_download_url.clone(),
            file_name: asset.name.clone(),
            size: (asset.size > 0).then_some(asset.size),
            checksum_url: sums_re.as_ref().and_then(|re| {
                r.assets
                    .iter()
                    .find(|a| re.is_match(&a.name))
                    .map(|a| a.browser_download_url.clone())
            }),
            extras,
        });
    }
    Ok(out)
}

static HREF: Lazy<Regex> = Lazy::new(|| Regex::new(r#"href="([^"]+)""#).expect("valid regex"));

fn from_buildbot(
    body: &str,
    index: &str,
    version_pattern: &str,
    template: &str,
) -> Result<Vec<Release>> {
    let version_re = Regex::new(version_pattern)?;
    let base = if index.ends_with('/') {
        index.to_string()
    } else {
        format!("{index}/")
    };
    Ok(HREF
        .captures_iter(body)
        .filter_map(|c| {
            let href = c.get(1)?.as_str();
            let href = href.rsplit_once(&base).map_or(href, |(_, rest)| rest);
            let href = href.trim_start_matches('/');
            let href = href.strip_prefix("stable/").unwrap_or(href);
            let version = version_re.captures(href)?.get(1)?.as_str().to_string();
            let path = template.replace("{version}", &version);
            Some(Release {
                file_name: path.rsplit('/').next().unwrap_or(&path).to_string(),
                url: format!("{base}{path}"),
                version,
                date: None,
                size: None,
                checksum_url: None,
                extras: Vec::new(),
            })
        })
        .collect())
}

fn from_page(body: &str, pattern: &str, base: &str) -> Result<Vec<Release>> {
    let re = Regex::new(pattern)?;
    Ok(re
        .captures_iter(body)
        .filter_map(|c| {
            let link = c.get(0)?.as_str();
            let version = c.name("version")?.as_str().replace('_', ".");
            let url = if link.starts_with("http") {
                link.to_string()
            } else {
                format!("{base}{link}")
            };
            Some(Release {
                file_name: url.rsplit('/').next().unwrap_or(&url).to_string(),
                url,
                version,
                date: None,
                size: None,
                checksum_url: None,
                extras: Vec::new(),
            })
        })
        .collect())
}

/// Compare versions by their numeric parts (`1.21.0` < `1.22`,
/// `2025-01-02` < `2025-10-01`), then as text.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    fn parts(s: &str) -> Vec<u64> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .map(|p| p.parse().unwrap_or(u64::MAX))
            .collect()
    }
    parts(a).cmp(&parts(b)).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(compare_versions("1.21.0", "1.9.0"), Ordering::Greater);
        assert_eq!(compare_versions("2025-01-02", "2025-10-01"), Ordering::Less);
        assert_eq!(compare_versions("2509", "2506"), Ordering::Greater);
    }

    #[test]
    fn github_lists_matching_assets_and_dates_rolling_builds() {
        let body = serde_json::json!([
            {"tag_name": "latest", "published_at": "2025-09-30T10:00:00Z", "assets": [
                {"name": "DuckStation-x64.AppImage", "browser_download_url": "https://x/a", "size": 10, "updated_at": "2025-10-01T08:00:00Z"},
                {"name": "duckstation-windows-x64-release.zip", "browser_download_url": "https://x/b", "size": 20}
            ]},
            {"tag_name": "preview", "prerelease": true, "assets": [
                {"name": "DuckStation-x64.AppImage", "browser_download_url": "https://x/c"}
            ]},
            {"tag_name": "v0.82.2", "assets": [
                {"name": "dosbox-staging-linux-x86_64-v0.82.2.tar.xz", "browser_download_url": "https://x/d"}
            ]}
        ])
        .to_string();
        let r = from_github(&body, "^DuckStation-x64\\.AppImage$", &[], None).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].version, "2025-10-01");
        assert_eq!(r[0].size, Some(10));
        let r = from_github(
            &body,
            "^dosbox-staging-linux-x86_64-v[\\d.]+\\.tar\\.xz$",
            &[],
            None,
        )
        .unwrap();
        assert_eq!(r[0].version, "0.82.2");
    }

    #[test]
    fn github_releases_carry_extra_assets_and_skip_incomplete_ones() {
        let body = serde_json::json!([
            {"tag_name": "v0.1.3", "assets": [
                {"name": "relinker-v0.1.3", "browser_download_url": "https://x/r3", "size": 4},
                {"name": "prx-linux-v0.1.3.tar.gz", "browser_download_url": "https://x/p3", "size": 100}
            ]},
            {"tag_name": "v0.1.2", "assets": [
                {"name": "relinker-v0.1.2", "browser_download_url": "https://x/r2"}
            ]}
        ])
        .to_string();
        let extras = ["^prx-linux-v[\\d.]+\\.tar\\.gz$".to_string()];
        let r = from_github(&body, "^relinker-v[\\d.]+$", &extras, None).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].version, "0.1.3");
        assert_eq!(
            r[0].extras,
            [ReleaseFile {
                url: "https://x/p3".into(),
                file_name: "prx-linux-v0.1.3.tar.gz".into(),
                size: Some(100),
            }]
        );
    }

    #[test]
    fn buildbot_reads_the_directory_listing() {
        let body = r#"<a href="/stable/1.20.0/">1.20.0/</a> <a href="/stable/1.21.0/">1.21.0/</a> <a href="/stable/nightly-x/">x</a>"#;
        let r = from_buildbot(
            body,
            "https://buildbot.libretro.com/stable/",
            "^(\\d+\\.\\d+\\.\\d+)/$",
            "{version}/linux/x86_64/RetroArch.7z",
        )
        .unwrap();
        let versions: Vec<&str> = r.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(versions, ["1.20.0", "1.21.0"]);
        assert_eq!(
            r[1].url,
            "https://buildbot.libretro.com/stable/1.21.0/linux/x86_64/RetroArch.7z"
        );
        assert_eq!(r[1].file_name, "RetroArch.7z");
    }

    #[test]
    fn pages_yield_links_with_versions() {
        let body = r#"<a href="/files/1_19_3/ppsspp_win.zip">Win</a><a href="/files/1_18_1/ppsspp_win.zip">old</a>"#;
        let r = from_page(
            body,
            "/files/(?P<version>\\d+_\\d+(?:_\\d+)?)/ppsspp_win\\.zip",
            "https://www.ppsspp.org",
        )
        .unwrap();
        assert_eq!(r[0].version, "1.19.3");
        assert_eq!(
            r[0].url,
            "https://www.ppsspp.org/files/1_19_3/ppsspp_win.zip"
        );
    }
}
