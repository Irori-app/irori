//! Mapping Amazon entitlements and install records to Ember's domain types.
//!
//! Everything here is pure (no I/O) so it can be unit-tested with fixtures.

use ember_amazon_api::Entitlement;
use ember_core::util::format_bytes;
use ember_core::{ExtraInfo, GameInfo, InstallPlatform, InstallProgress, InstalledInfo, Runner};

use crate::state::InstalledGame;

/// Characters stripped from titles to derive install folder names (the same
/// set as Heroic, so folders match those of imported installs).
const SPECIAL_CHARS: &[char] = &[
    ':', '|', '/', '*', '?', '<', '>', '\\', '&', '{', '}', '%', '$', '@', '`', '!', '™', '+',
    '\'', '"', '®',
];

/// Install folder name derived from a title.
pub fn folder_name(title: &str) -> String {
    title
        .chars()
        .filter(|c| !SPECIAL_CHARS.contains(c))
        .collect::<String>()
        .trim()
        .to_string()
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_ref().filter(|s| !s.trim().is_empty()).cloned()
}

/// Installed state of a game.
pub fn installed_info(game: &InstalledGame) -> InstalledInfo {
    InstalledInfo {
        install_path: game.path.to_string_lossy().into_owned(),
        install_size: format_bytes(game.size),
        install_size_bytes: game.size,
        version: game.version.clone(),
        platform: InstallPlatform::Windows,
        ..Default::default()
    }
}

/// Map an entitlement to a library entry.
///
/// Amazon offers no vertical art: the square icon is the tile art (what
/// Heroic's grid shows) and the wide backgrounds become the hero art.
pub fn game_info(ent: &Entitlement, installed: Option<&InstalledGame>) -> GameInfo {
    let product = &ent.product;
    let detail = &product.product_detail;
    let d = &detail.details;
    let icon = non_empty(&detail.icon_url);
    let wide = non_empty(&d.background_url2);
    let title = product
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "???".to_string());
    let description = non_empty(&d.short_description);
    let release_date = non_empty(&d.release_date);
    let mut info = GameInfo {
        runner: Some(Runner::Nile),
        app_name: product.id.clone(),
        folder_name: Some(folder_name(product.title.as_deref().unwrap_or(""))),
        title,
        developer: non_empty(&d.developer),
        publisher: non_empty(&d.publisher),
        description: description.clone(),
        art_cover: icon.clone().or_else(|| wide.clone()),
        art_square: icon.or_else(|| wide.clone()),
        art_logo: non_empty(&d.logo_url),
        art_background: non_empty(&d.background_url1).or(wide),
        installable: true,
        can_run_offline: true,
        platforms: vec![InstallPlatform::Windows],
        genres: d.genres.clone(),
        release_date: release_date.clone(),
        extra: Some(ExtraInfo {
            description: description.clone(),
            short_description: description,
            release_date,
            genres: d.genres.clone(),
            ..Default::default()
        }),
        ..Default::default()
    };
    set_installed(&mut info, installed);
    info.finalize()
}

/// Apply (or clear) installed state on a library entry.
pub fn set_installed(info: &mut GameInfo, installed: Option<&InstalledGame>) {
    info.is_installed = installed.is_some();
    info.install = installed.map(installed_info);
}

/// An installed game no longer in the owned list (kept so it can still be
/// played and uninstalled).
pub fn orphan_info(product_id: &str, installed: &InstalledGame) -> GameInfo {
    let title = installed
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| product_id.to_string());
    let mut info = GameInfo {
        runner: Some(Runner::Nile),
        app_name: product_id.to_string(),
        title,
        installable: true,
        can_run_offline: true,
        platforms: vec![InstallPlatform::Windows],
        ..Default::default()
    };
    set_installed(&mut info, Some(installed));
    info.finalize()
}

/// A download report in Ember's terms.
pub fn progress(p: ember_depot::Progress) -> InstallProgress {
    InstallProgress {
        percent: Some(p.percent.clamp(0.0, 100.0)),
        bytes_done: Some(p.bytes_done),
        bytes_total: Some(p.bytes_total),
        eta_secs: p.eta_secs,
        download_speed: Some(p.download_speed),
        disk_speed: Some(p.disk_speed),
        bottleneck: p.bottleneck,
        preparing: p.preparing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARY: &str = r#"[
      {"id": "ent-1", "product": {"id": "amzn1.adg.product.abc", "title": "Foo: The Game™", "sku": "S",
        "productDetail": {"iconUrl": "https://img/icon.png", "details": {
          "backgroundUrl1": "https://img/bg1.jpg", "backgroundUrl2": "https://img/bg2.jpg",
          "developer": "Foo Studio", "publisher": "Foo Pub", "genres": ["Action", "RPG"],
          "logoUrl": "https://img/logo.png", "releaseDate": "2020-01-02T00:00:00Z",
          "shortDescription": "A game.", "esrbRating": "TEEN", "screenshots": [],
          "websites": {"official": null}}}}},
      {"id": "ent-2", "product": {"id": "amzn1.adg.product.def",
        "productDetail": {"iconUrl": "", "details": {"backgroundUrl2": "https://img/w.jpg",
          "genres": null}}}}
    ]"#;

    #[test]
    fn maps_library_entries() {
        let games: Vec<Entitlement> = serde_json::from_str(LIBRARY).unwrap();
        let installed = InstalledGame {
            version: "v1".into(),
            path: "/g/Foo The Game".into(),
            size: 2147483648,
        };

        let a = game_info(&games[0], Some(&installed));
        assert_eq!(a.app_name, "amzn1.adg.product.abc");
        assert_eq!(a.runner, Some(Runner::Nile));
        assert_eq!(a.title, "Foo: The Game™");
        assert_eq!(a.folder_name.as_deref(), Some("Foo The Game"));
        assert_eq!(a.art_cover.as_deref(), Some("https://img/icon.png"));
        assert_eq!(a.art_square.as_deref(), Some("https://img/icon.png"));
        assert_eq!(a.art_background.as_deref(), Some("https://img/bg1.jpg"));
        assert_eq!(a.art_logo.as_deref(), Some("https://img/logo.png"));
        assert_eq!(a.developer.as_deref(), Some("Foo Studio"));
        assert_eq!(a.publisher.as_deref(), Some("Foo Pub"));
        assert_eq!(a.genres, vec!["Action", "RPG"]);
        assert_eq!(a.platforms, vec![InstallPlatform::Windows]);
        assert!(a.can_run_offline && a.is_installed && !a.is_linux_native);
        let inst = a.install.as_ref().unwrap();
        assert_eq!(inst.install_path, "/g/Foo The Game");
        assert_eq!(inst.version, "v1");
        assert_eq!(inst.install_size, "2.0 GiB");
        assert_eq!(inst.platform, InstallPlatform::Windows);
        let extra = a.extra.as_ref().unwrap();
        assert_eq!(extra.description.as_deref(), Some("A game."));
        assert_eq!(extra.release_date.as_deref(), Some("2020-01-02T00:00:00Z"));
        assert_eq!(a.sort_title, "foo: the game™");

        let b = game_info(&games[1], None);
        assert_eq!(b.title, "???");
        assert_eq!(b.folder_name.as_deref(), Some(""));
        assert_eq!(b.art_cover.as_deref(), Some("https://img/w.jpg"));
        assert_eq!(b.art_background.as_deref(), Some("https://img/w.jpg"));
        assert!(b.genres.is_empty());
        assert!(!b.is_installed && b.install.is_none());

        let o = orphan_info("amzn1.adg.product.zzz", &installed);
        assert_eq!(o.title, "Foo The Game");
        assert!(o.is_installed);
    }

    #[test]
    fn folder_names_strip_special_characters() {
        assert_eq!(
            folder_name("A/B: C? <D> \"E\" 'F' ®™ & {G}%$@`!+|*\\"),
            "AB C D E F   G"
        );
    }

    #[test]
    fn maps_progress() {
        let p = progress(ember_depot::Progress {
            percent: 50.0,
            bytes_done: 5,
            bytes_total: 10,
            eta_secs: Some(3),
            download_speed: 7,
            disk_speed: 8,
            bottleneck: Some(ember_core::Bottleneck::Disk),
            preparing: None,
        });
        assert_eq!(p.percent, Some(50.0));
        assert_eq!((p.bytes_done, p.bytes_total), (Some(5), Some(10)));
        assert_eq!(
            (p.download_speed, p.disk_speed, p.eta_secs),
            (Some(7), Some(8), Some(3))
        );
        assert_eq!(p.bottleneck, Some(ember_core::Bottleneck::Disk));
    }
}
