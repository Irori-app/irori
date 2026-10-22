//! Import of Heroic's sideloaded apps (`<heroic>/sideload_apps/library.json`).

use std::path::Path;

use anyhow::{Context as _, Result};
use ember_core::json_store;
use serde::Deserialize;

use crate::app::{LIBRARY_VERSION, LibraryFile, SideloadApp, SideloadPlatform, StoredApp};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct HeroicLibrary {
    games: Vec<HeroicApp>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct HeroicApp {
    app_name: String,
    title: String,
    install: HeroicInstall,
    art_cover: Option<String>,
    art_square: Option<String>,
    #[serde(rename = "browserUrl")]
    browser_url: Option<String>,
    description: Option<String>,
    #[serde(rename = "customUserAgent")]
    custom_user_agent: Option<String>,
    #[serde(rename = "launchFullScreen")]
    launch_full_screen: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct HeroicInstall {
    executable: Option<String>,
    platform: Option<String>,
}

/// Merge Heroic's sideloaded apps into Ember's library file at `target`.
/// Apps already in Ember (same id) are kept as they are; macOS apps and
/// invalid entries are skipped. Returns how many apps were added.
pub(crate) fn import_into(heroic_config_dir: &Path, target: &Path) -> Result<usize> {
    let source = heroic_config_dir.join("sideload_apps").join("library.json");
    let Some(heroic) = json_store::read::<HeroicLibrary>(&source)
        .with_context(|| format!("reading Heroic's sideloaded apps from {}", source.display()))?
    else {
        return Ok(0);
    };
    let mut library: LibraryFile = json_store::read(target)
        .with_context(|| format!("reading {}", target.display()))?
        .unwrap_or_default();
    let mut added = 0;
    for entry in heroic.games {
        if entry.app_name.is_empty() || library.apps.iter().any(|a| a.app_name == entry.app_name) {
            continue;
        }
        let app_name = entry.app_name.clone();
        match convert(entry).and_then(SideloadApp::normalized) {
            Ok(app) => {
                library.apps.push(StoredApp { app_name, app });
                added += 1;
            }
            Err(e) => tracing::warn!("skipping Heroic sideloaded app {app_name}: {e:#}"),
        }
    }
    if added > 0 {
        library.version = LIBRARY_VERSION;
        json_store::write(target, &library)?;
    }
    Ok(added)
}

fn convert(entry: HeroicApp) -> Result<SideloadApp> {
    let executable = entry
        .install
        .executable
        .filter(|e| !e.is_empty())
        .map(std::path::PathBuf::from);
    let browser_url = entry.browser_url.filter(|u| !u.trim().is_empty());
    let platform = match entry
        .install
        .platform
        .as_deref()
        .map(str::to_ascii_lowercase)
    {
        _ if browser_url.is_some() => SideloadPlatform::Browser,
        Some(p) if p == "browser" => SideloadPlatform::Browser,
        Some(p) if p == "linux" => SideloadPlatform::Linux,
        Some(p) if p == "windows" => SideloadPlatform::Windows,
        Some(p) => anyhow::bail!("platform {p} cannot run on Linux"),
        None if executable
            .as_deref()
            .and_then(|e| e.extension())
            .is_some_and(|x| x.eq_ignore_ascii_case("exe")) =>
        {
            SideloadPlatform::Windows
        }
        None => SideloadPlatform::Linux,
    };
    // Heroic's `art_square` is the tile image and `art_cover` the wide hero;
    // both fall back to a bundled placeholder, which is not a usable URL.
    let usable = |v: Option<String>| {
        v.filter(|s| {
            ["http://", "https://", "file://"]
                .iter()
                .any(|p| s.starts_with(p))
        })
    };
    let tile = usable(entry.art_square);
    let hero = usable(entry.art_cover).filter(|h| Some(h) != tile.as_ref());
    Ok(SideloadApp {
        title: entry.title,
        executable,
        platform,
        browser_url,
        custom_user_agent: entry.custom_user_agent,
        launch_fullscreen: entry.launch_full_screen.unwrap_or(false),
        cover_art: tile.or_else(|| hero.clone()),
        background_art: hero,
        install_folder: None,
        launcher_args: String::new(),
        description: entry.description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
        "games": [
            {
                "runner": "sideload",
                "app_name": "winApp22CharsLongId123",
                "title": "Windows Game",
                "install": {"executable": "/games/win/game.exe", "platform": "Windows", "is_dlc": false},
                "folder_name": "/games/win",
                "art_cover": "file:///art/hero.jpg",
                "art_square": "file:///art/tile.jpg",
                "is_installed": true,
                "canRunOffline": true
            },
            {
                "app_name": "linuxApp",
                "title": "Linux Game",
                "install": {"executable": "/games/lin/start.sh", "platform": "linux"},
                "art_cover": "./assets/heroic_card.jpg",
                "art_square": "./assets/heroic_card.jpg"
            },
            {
                "app_name": "webApp",
                "title": "Web Game",
                "install": {"platform": "Browser"},
                "art_cover": "https://x.org/a.png",
                "art_square": "https://x.org/a.png",
                "browserUrl": "https://play.example.com",
                "customUserAgent": "UA",
                "launchFullScreen": true,
                "canRunOffline": false
            },
            {
                "app_name": "macApp",
                "title": "Mac Game",
                "install": {"executable": "/Applications/G.app", "platform": "Mac"}
            },
            {
                "app_name": "existing",
                "title": "From Heroic",
                "install": {"executable": "/x/y", "platform": "linux"}
            }
        ]
    }"#;

    #[test]
    fn imports_and_merges() {
        let heroic = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(heroic.path().join("sideload_apps")).unwrap();
        std::fs::write(heroic.path().join("sideload_apps/library.json"), FIXTURE).unwrap();
        let ember = tempfile::tempdir().unwrap();
        let target = ember.path().join("library.json");
        let existing = StoredApp {
            app_name: "existing".into(),
            app: SideloadApp {
                title: "Mine".into(),
                executable: Some("/a/b".into()),
                platform: SideloadPlatform::Linux,
                ..Default::default()
            },
        };
        json_store::write(
            &target,
            &LibraryFile {
                version: LIBRARY_VERSION,
                apps: vec![existing.clone()],
            },
        )
        .unwrap();

        assert_eq!(import_into(heroic.path(), &target).unwrap(), 3);
        let lib: LibraryFile = json_store::read(&target).unwrap().unwrap();
        assert_eq!(lib.apps.len(), 4);
        assert_eq!(lib.apps[0], existing);

        let win = &lib.apps[1];
        assert_eq!(win.app_name, "winApp22CharsLongId123");
        assert_eq!(win.app.platform, SideloadPlatform::Windows);
        assert_eq!(win.app.cover_art.as_deref(), Some("file:///art/tile.jpg"));
        assert_eq!(
            win.app.background_art.as_deref(),
            Some("file:///art/hero.jpg")
        );

        let lin = &lib.apps[2];
        assert_eq!(lin.app.platform, SideloadPlatform::Linux);
        assert!(lin.app.cover_art.is_none());

        let web = &lib.apps[3];
        assert_eq!(web.app.platform, SideloadPlatform::Browser);
        assert_eq!(
            web.app.browser_url.as_deref(),
            Some("https://play.example.com/")
        );
        assert_eq!(web.app.custom_user_agent.as_deref(), Some("UA"));
        assert!(web.app.launch_fullscreen);
        assert_eq!(web.app.cover_art.as_deref(), Some("https://x.org/a.png"));
        assert!(web.app.background_art.is_none());

        // Re-import adds nothing.
        assert_eq!(import_into(heroic.path(), &target).unwrap(), 0);
    }

    #[test]
    fn missing_heroic_library_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            import_into(dir.path(), &dir.path().join("lib.json")).unwrap(),
            0
        );
    }
}
