//! Pure conversions from API responses to Ember's domain types.

use std::path::Path;

use ember_core::{ExtraInfo, GameInfo, InstallInfo, InstallPlatform, Runner};
use once_cell::sync::Lazy;
use regex::Regex;

use crate::api::{FilesResponse, ZoomFile, ZoomGame};
use crate::records::{InstalledFile, InstalledRecord};

/// Build the library entry of a game, overlaying its install state.
pub fn to_game_info(raw: &ZoomGame, installed: Option<&InstalledRecord>) -> GameInfo {
    let has_os = |os: &str| {
        raw.operating_systems
            .iter()
            .any(|o| o.eq_ignore_ascii_case(os))
    };
    let mut platforms = Vec::new();
    if has_os("windows") {
        platforms.push(InstallPlatform::Windows);
    }
    if has_os("linux") {
        platforms.push(InstallPlatform::Linux);
    }
    if has_os("osx") || has_os("mac") {
        platforms.push(InstallPlatform::Mac);
    }
    let non_empty = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    let joined = |v: &[String]| non_empty(&v.join(", "));
    let poster = non_empty(&raw.poster_url);
    let description = non_empty(&raw.description);
    let store_url = non_empty(&raw.store_url);
    GameInfo {
        runner: Some(Runner::Zoom),
        app_name: raw.id.clone(),
        title: if raw.name.is_empty() {
            raw.id.clone()
        } else {
            raw.name.clone()
        },
        developer: joined(&raw.developers),
        publisher: joined(&raw.publishers),
        description: description.clone(),
        store_url: store_url.clone(),
        art_cover: poster.clone(),
        art_square: poster.clone(),
        art_background: poster,
        is_installed: installed.is_some(),
        install: installed.map(InstalledRecord::to_installed_info),
        installable: true,
        can_run_offline: true,
        cloud_save_enabled: false,
        folder_name: non_empty(&raw.slug),
        is_linux_native: has_os("linux"),
        is_mac_native: has_os("osx") || has_os("mac"),
        platforms,
        extra: Some(ExtraInfo {
            description,
            store_url,
            ..Default::default()
        }),
        ..Default::default()
    }
    .finalize()
}

/// Entry for an installed game that is missing from the listing (cache not
/// loaded yet, or the game left the account): enough to launch and uninstall.
pub fn orphan_game_info(record: &InstalledRecord) -> GameInfo {
    GameInfo {
        runner: Some(Runner::Zoom),
        app_name: record.app_name.clone(),
        title: Path::new(&record.install_path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| record.app_name.clone()),
        is_installed: true,
        install: Some(record.to_installed_info()),
        installable: true,
        can_run_offline: true,
        is_linux_native: record.platform == InstallPlatform::Linux,
        ..Default::default()
    }
    .finalize()
}

/// Parse ZOOM's human readable sizes ("1.2 GB", "512 MB", "700 bytes").
/// Units are binary multiples, as ZOOM's numbers are; a bare number is bytes.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase().replace(',', "");
    const UNITS: [(&str, u32); 11] = [
        ("bytes", 0),
        ("kib", 1),
        ("mib", 2),
        ("gib", 3),
        ("tib", 4),
        ("kb", 1),
        ("mb", 2),
        ("gb", 3),
        ("tb", 4),
        ("pb", 5),
        ("b", 0),
    ];
    let (num, exp) = UNITS
        .iter()
        .find_map(|(suffix, exp)| s.strip_suffix(suffix).map(|n| (n, *exp)))
        .unwrap_or((s.as_str(), 0));
    let value: f64 = num.trim().parse().ok()?;
    (value.is_finite() && value >= 0.0).then(|| (value * 1024f64.powi(exp as i32)).round() as u64)
}

/// Best known size of a file in bytes (0 when unknown).
pub fn file_size(f: &ZoomFile) -> u64 {
    f.total_size
        .filter(|&n| n > 0)
        .or_else(|| parse_size(&f.size))
        .unwrap_or(0)
}

/// Exact size, when the API states it in bytes (used to validate downloads).
pub fn exact_size(f: &ZoomFile) -> Option<u64> {
    f.total_size.filter(|&n| n > 0)
}

/// Installer files for a platform. Platform keys are matched regardless of
/// how the caller spells the platform.
pub fn platform_files(files: &FilesResponse, platform: InstallPlatform) -> &[ZoomFile] {
    match platform {
        InstallPlatform::Windows => &files.windows,
        InstallPlatform::Linux => &files.linux,
        InstallPlatform::Mac | InstallPlatform::Browser => &[],
    }
}

/// Safe local file name for a download: the API's `name` or `filename`
/// (whichever looks like a file name), reduced to its last path component.
pub fn disk_name(f: &ZoomFile) -> String {
    let clean = |s: &str| {
        let base = s.rsplit(['/', '\\']).next().unwrap_or("").trim();
        let base = ember_core::paths::sanitize(base);
        (!base.is_empty() && base != "." && base != ".." && !base.starts_with('.')).then_some(base)
    };
    [&f.name, &f.filename]
        .into_iter()
        .filter_map(|s| clean(s))
        .find(|s| s.contains('.'))
        .or_else(|| clean(&f.name))
        .or_else(|| clean(&f.filename))
        .unwrap_or_else(|| format!("zoom-file-{}", ember_core::paths::sanitize(&f.id)))
}

static VERSION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(?:^|[_\-\s(v])v?(\d+(?:\.\d+)+[a-z]?)").expect("version regex"));

/// Version label guessed from installer names (`setup_game_1.2.3.exe` → `1.2.3`).
pub fn guess_version(files: &[ZoomFile]) -> Option<String> {
    const EXTS: [&str; 12] = [
        ".tar.gz",
        ".tar.xz",
        ".tar.zst",
        ".tar.bz2",
        ".tgz",
        ".exe",
        ".bin",
        ".zip",
        ".sh",
        ".run",
        ".appimage",
        ".tar",
    ];
    files.iter().find_map(|f| {
        let name = disk_name(f);
        let lower = name.to_ascii_lowercase();
        let stem = EXTS
            .iter()
            .find(|e| lower.ends_with(*e))
            .map(|e| &name[..name.len() - e.len()])
            .unwrap_or(&name);
        VERSION_RE.captures(stem).map(|c| c[1].to_string())
    })
}

/// Identity of the installer set, compared to detect new builds.
pub fn fingerprint(files: &[ZoomFile]) -> Vec<InstalledFile> {
    let mut v: Vec<InstalledFile> = files
        .iter()
        .map(|f| InstalledFile {
            id: f.id.clone(),
            name: disk_name(f),
            size: f.size.clone(),
        })
        .collect();
    v.sort();
    v
}

/// Install dialog data for a platform's installer set.
pub fn install_info(
    game: &GameInfo,
    files: &[ZoomFile],
    platform: InstallPlatform,
    resumable: bool,
) -> InstallInfo {
    let size: u64 = files.iter().map(file_size).sum();
    InstallInfo {
        app_name: game.app_name.clone(),
        title: game.title.clone(),
        platform,
        download_size: size,
        disk_size: size,
        version: guess_version(files),
        is_native: !platform.needs_wine(),
        resumable,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: &str, name: &str, size: &str) -> ZoomFile {
        ZoomFile {
            id: id.into(),
            name: name.into(),
            size: size.into(),
            ..Default::default()
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1.5 GB"), Some(1_610_612_736));
        assert_eq!(parse_size("512 MB"), Some(512 * 1024 * 1024));
        assert_eq!(parse_size("700 bytes"), Some(700));
        assert_eq!(parse_size("500 B"), Some(500));
        assert_eq!(parse_size("1,024 KB"), Some(1024 * 1024));
        assert_eq!(parse_size("1234"), Some(1234));
        assert_eq!(parse_size("??"), None);
        assert_eq!(parse_size(""), None);
    }

    #[test]
    fn maps_game_info() {
        let raw = ZoomGame {
            id: "42".into(),
            slug: "some-game".into(),
            name: "The Some Game".into(),
            poster_url: "https://x/p.jpg".into(),
            description: "Fun".into(),
            store_url: "https://zoom/some".into(),
            operating_systems: vec!["windows".into(), "Linux".into()],
            developers: vec!["A".into(), "B".into()],
            publishers: vec![],
        };
        let g = to_game_info(&raw, None);
        assert_eq!(g.runner, Some(Runner::Zoom));
        assert_eq!(g.app_name, "42");
        assert_eq!(g.sort_title, "some game");
        assert_eq!(g.developer.as_deref(), Some("A, B"));
        assert_eq!(g.publisher, None);
        assert_eq!(g.folder_name.as_deref(), Some("some-game"));
        assert_eq!(g.art_cover.as_deref(), Some("https://x/p.jpg"));
        assert!(g.is_linux_native && !g.is_mac_native && !g.is_installed);
        assert!(g.can_run_offline && !g.cloud_save_enabled);
        assert_eq!(
            g.platforms,
            vec![InstallPlatform::Windows, InstallPlatform::Linux]
        );
        assert_eq!(g.extra.unwrap().description.as_deref(), Some("Fun"));

        let rec = InstalledRecord {
            app_name: "42".into(),
            install_path: "/g/some-game".into(),
            executable: "game.exe".into(),
            install_size_bytes: 2048,
            ..Default::default()
        };
        let g = to_game_info(&raw, Some(&rec));
        assert!(g.is_installed);
        let i = g.install.unwrap();
        assert_eq!(Path::new(&i.executable), Path::new("/g/some-game/game.exe"));
        assert_eq!(i.install_size, "2.0 KiB");
    }

    #[test]
    fn install_info_sums_platform_files() {
        let files = FilesResponse {
            windows: vec![
                file("1", "setup_game_1.2.3.exe", "1 GB"),
                file("2", "setup_game_1.2.3-1.bin", "512 MB"),
            ],
            linux: vec![file("3", "game_linux.tar.xz", "1 GB")],
            ..Default::default()
        };
        let game = GameInfo {
            app_name: "42".into(),
            title: "Game".into(),
            ..Default::default()
        };
        let win = platform_files(&files, InstallPlatform::Windows);
        let info = install_info(&game, win, InstallPlatform::Windows, false);
        assert_eq!(info.download_size, 1536 * 1024 * 1024);
        assert_eq!(info.version.as_deref(), Some("1.2.3"));
        assert_eq!(info.is_native, cfg!(windows));
        let lin = platform_files(&files, InstallPlatform::Linux);
        assert!(install_info(&game, lin, InstallPlatform::Linux, true).is_native);
        assert!(platform_files(&files, InstallPlatform::Mac).is_empty());
    }

    #[test]
    fn disk_names_are_safe() {
        assert_eq!(disk_name(&file("1", "setup.exe", "")), "setup.exe");
        let mut f = file("2", "Game Installer", "");
        f.filename = "dir/../game.tar.gz".into();
        assert_eq!(disk_name(&f), "game.tar.gz");
        assert_eq!(disk_name(&file("3", "..", "")), "zoom-file-3");
        assert_eq!(disk_name(&file("4", r"C:\x\setup.exe", "")), "setup.exe");
    }

    #[test]
    fn fingerprint_is_order_independent() {
        let a = [file("1", "a.exe", "1 MB"), file("2", "b.bin", "2 MB")];
        let b = [file("2", "b.bin", "2 MB"), file("1", "a.exe", "1 MB")];
        assert_eq!(fingerprint(&a), fingerprint(&b));
        let c = [file("1", "a.exe", "1 MB"), file("3", "b.bin", "2 MB")];
        assert_ne!(fingerprint(&a), fingerprint(&c));
    }
}
