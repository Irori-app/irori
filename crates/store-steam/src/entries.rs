//! Library entries and game details built from what Steam knows: installed
//! apps, the account's games, cached and store art, and store pages.

use std::path::{Path, PathBuf};

use irori_core::util::format_bytes;
use irori_core::{
    ExtraInfo, GameInfo, HostPlatform, InstallPlatform, InstalledInfo, Requirement, Runner,
};
use irori_steam_api::{AppAssets, AppDetails};

use crate::account::{LibraryGame, RemoteArt};
use crate::scan::{InstalledApp, LocalArt};

/// Fixed art addresses. Only older apps have their art there: newer ones
/// keep it under hashed addresses that Steam's cache or store name.
const STEAM_CDN: &str = "https://cdn.cloudflare.steamstatic.com/steam/apps";

/// A local file as art URL.
fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// The four images of an app: Steam's cache first, then the store's
/// addresses, then the fixed ones.
struct Art {
    cover: String,
    hero: String,
    logo: String,
    header: String,
}

impl Art {
    fn of(id: &str, local: Option<&LocalArt>, remote: Option<&RemoteArt>) -> Self {
        let pick = |cached: Option<&PathBuf>, stored: Option<&String>, fixed: &str| {
            cached
                .map(|p| file_url(p))
                .or_else(|| stored.cloned())
                .unwrap_or_else(|| format!("{STEAM_CDN}/{id}/{fixed}"))
        };
        Self {
            cover: pick(
                local.and_then(|a| a.cover.as_ref()),
                remote.and_then(|a| a.cover.as_ref()),
                "library_600x900_2x.jpg",
            ),
            hero: pick(
                local.and_then(|a| a.hero.as_ref()),
                remote.and_then(|a| a.hero.as_ref()),
                "library_hero.jpg",
            ),
            logo: pick(
                local.and_then(|a| a.logo.as_ref()),
                remote.and_then(|a| a.logo.as_ref()),
                "logo.png",
            ),
            header: pick(
                local.and_then(|a| a.header.as_ref()),
                remote.and_then(|a| a.header.as_ref()),
                "header.jpg",
            ),
        }
    }
}

/// Whether Steam's cache lacks the cover of an app, so the store's art is
/// worth asking for.
pub(crate) fn needs_remote_art(local: Option<&LocalArt>) -> bool {
    local.is_none_or(|a| a.cover.is_none())
}

/// The store art worth keeping from Steam's answer.
pub(crate) fn remote_art(assets: &AppAssets) -> RemoteArt {
    RemoteArt {
        cover: assets.cover_2x.clone().or_else(|| assets.cover.clone()),
        hero: assets.hero_2x.clone().or_else(|| assets.hero.clone()),
        logo: assets.logo_2x.clone().or_else(|| assets.logo.clone()),
        header: assets.header.clone(),
    }
}

fn base(id: &str, title: &str, art: Art) -> GameInfo {
    let native = HostPlatform::current().native();
    GameInfo {
        runner: Some(Runner::Steam),
        app_name: id.to_string(),
        title: title.to_string(),
        store_url: Some(format!("https://store.steampowered.com/app/{id}/")),
        art_cover: Some(art.cover),
        art_square: Some(art.header),
        art_background: Some(art.hero),
        art_logo: Some(art.logo),
        can_run_offline: true,
        // Steam runs Windows games through Proton itself, so for Irori every
        // Steam game is native.
        is_linux_native: native == InstallPlatform::Linux,
        is_mac_native: native == InstallPlatform::Mac,
        platforms: vec![native],
        ..Default::default()
    }
}

/// Library entry of an installed Steam app.
pub(crate) fn installed(
    app: &InstalledApp,
    local: Option<&LocalArt>,
    remote: Option<&RemoteArt>,
) -> GameInfo {
    let native = HostPlatform::current().native();
    GameInfo {
        is_installed: true,
        install: Some(InstalledInfo {
            install_path: app.install_path.to_string_lossy().into_owned(),
            install_size: format_bytes(app.size_on_disk),
            install_size_bytes: app.size_on_disk,
            version: app.build_id.clone(),
            platform: native,
            ..Default::default()
        }),
        installable: false,
        ..base(&app.app_id, &app.name, Art::of(&app.app_id, local, remote))
    }
    .finalize()
}

/// Library entry of a game the account owns, or its family shares, that
/// isn't installed. "Install" hands it to Steam.
pub(crate) fn owned(
    game: &LibraryGame,
    local: Option<&LocalArt>,
    remote: Option<&RemoteArt>,
) -> GameInfo {
    GameInfo {
        installable: true,
        ..base(
            &game.app_id,
            &game.name,
            Art::of(&game.app_id, local, remote),
        )
    }
    .finalize()
}

/// Whether two entries show the same thing.
pub(crate) fn same(a: &GameInfo, b: &GameInfo) -> bool {
    a.app_name == b.app_name
        && a.title == b.title
        && a.is_installed == b.is_installed
        && (&a.art_cover, &a.art_background, &a.art_logo, &a.art_square)
            == (&b.art_cover, &b.art_background, &b.art_logo, &b.art_square)
        && a.install
            .as_ref()
            .map(|i| (&i.install_path, &i.version, i.install_size_bytes))
            == b.install
                .as_ref()
                .map(|i| (&i.install_path, &i.version, i.install_size_bytes))
}

/// Steam's name for the store language matching Irori's `uiLanguage`.
pub(crate) fn store_language(ui_language: &str) -> &'static str {
    match ui_language {
        "cs" => "czech",
        "de" => "german",
        "es" => "spanish",
        "fr" => "french",
        "it" => "italian",
        "nl" => "dutch",
        "pl" => "polish",
        "pt" => "portuguese",
        "pt_BR" => "brazilian",
        "ro" => "romanian",
        "sv" => "swedish",
        "tr" => "turkish",
        "ru" => "russian",
        "uk" => "ukrainian",
        "ja" => "japanese",
        "ko" => "koreana",
        "zh_Hans" => "schinese",
        "zh_Hant" => "tchinese",
        _ => "english",
    }
}

/// The game page's details from the store page.
pub(crate) fn extra_info(id: &str, details: &AppDetails) -> ExtraInfo {
    let host = HostPlatform::current().native();
    let mut systems = vec![
        (
            InstallPlatform::Windows,
            "Windows",
            &details.pc_requirements,
        ),
        (InstallPlatform::Linux, "Linux", &details.linux_requirements),
        (InstallPlatform::Mac, "macOS", &details.mac_requirements),
    ];
    // This computer's system first.
    systems.sort_by_key(|(platform, _, _)| *platform != host);
    let requirements = systems
        .into_iter()
        .filter(|(_, _, r)| r.minimum.is_some() || r.recommended.is_some())
        .map(|(_, title, r)| Requirement {
            title: title.to_string(),
            minimum: r.minimum.as_deref().map(plain_text).unwrap_or_default(),
            recommended: r.recommended.as_deref().map(plain_text).unwrap_or_default(),
        })
        .collect();
    let nonempty = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    ExtraInfo {
        description: nonempty(details.description()),
        short_description: nonempty(&details.short_description),
        requirements,
        release_date: nonempty(&details.release_date.date),
        store_url: Some(format!("https://store.steampowered.com/app/{id}/")),
        changelog: None,
        genres: details
            .genres
            .iter()
            .map(|g| g.description.clone())
            .filter(|g| !g.is_empty())
            .collect(),
        screenshots: details
            .screenshots
            .iter()
            .map(|s| s.path_full.clone())
            .filter(|p| !p.is_empty())
            .collect(),
    }
}

/// Text of Steam's requirement HTML, one item per line, with the
/// "Minimum:" heading Steam repeats in every block left out.
fn plain_text(html: &str) -> String {
    let mut out = String::new();
    let mut tag = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match (in_tag, c) {
            (false, '<') => {
                in_tag = true;
                tag.clear();
            }
            (true, '>') => {
                in_tag = false;
                let name = tag
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(name.as_str(), "br" | "li" | "p" | "ul") && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            (true, c) => tag.push(c),
            (false, c) => out.push(c),
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !matches!(*l, "Minimum:" | "Recommended:"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_prefers_the_cache_then_the_store() {
        let local = LocalArt {
            cover: Some(PathBuf::from(
                "/steam/appcache/librarycache/7/a/library_capsule.jpg",
            )),
            ..Default::default()
        };
        let remote = RemoteArt {
            cover: Some("https://store/cover.jpg".into()),
            hero: Some("https://store/hero.jpg".into()),
            ..Default::default()
        };
        let art = Art::of("7", Some(&local), Some(&remote));
        assert_eq!(
            art.cover,
            "file:///steam/appcache/librarycache/7/a/library_capsule.jpg"
        );
        assert_eq!(art.hero, "https://store/hero.jpg");
        assert!(art.logo.ends_with("/7/logo.png"));
        assert!(needs_remote_art(None));
        assert!(!needs_remote_art(Some(&local)));
    }

    #[test]
    fn owned_games_are_installable_through_steam() {
        let game = LibraryGame {
            app_id: "70".into(),
            name: "Half-Life".into(),
            ..Default::default()
        };
        let info = owned(&game, None, None);
        assert!(!info.is_installed && info.installable);
        assert_eq!(info.runner(), Runner::Steam);
    }

    #[test]
    fn requirements_become_plain_lines() {
        let html = "<strong>Minimum:</strong><br><ul class=\"bb_ul\"><li><strong>OS:</strong> Windows 10<br></li><li><strong>Memory:</strong> 8 GB RAM</li></ul>";
        assert_eq!(plain_text(html), "OS: Windows 10\nMemory: 8 GB RAM");
        assert_eq!(plain_text("A &amp; B"), "A & B");
    }

    #[test]
    fn maps_irori_languages_to_steam_names() {
        assert_eq!(store_language("pt_BR"), "brazilian");
        assert_eq!(store_language("zh_Hans"), "schinese");
        assert_eq!(store_language("en"), "english");
    }
}
