//! New versions of Irori itself. Finds out how Irori was installed, asks
//! GitHub at most once a day whether a newer release is out, and remembers
//! the version the user dismissed and the version that ran last.
//!
//! Irori never installs an update itself: it points at the release page, or
//! at the package manager's command. Installs that update with the system
//! (Flatpak, distribution packages, Nix) are never checked. Packagers who
//! build Irori themselves turn the check off by setting
//! `IRORI_UPDATE_EXPLANATION` when building (or in the environment); its text
//! is shown instead of checking.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result};
use irori_core::{json_store, util};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// GitHub's newest published release: never a draft or a pre-release.
pub const RELEASES_API: &str = "https://api.github.com/repos/irori-app/irori/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/irori-app/irori/releases/tag/v";
/// Seconds between two automatic checks.
const CHECK_EVERY: u64 = 24 * 60 * 60;

/// How this copy of Irori was installed, which decides who updates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    Flatpak,
    /// A distribution package (apt, dnf, pacman, AUR) under `/usr`.
    SystemPackage,
    Nix,
    /// Run from a Cargo `target` folder.
    Source,
    /// A packager set `IRORI_UPDATE_EXPLANATION`; their text, untranslated.
    Packaged(String),
    Homebrew,
    Scoop,
    Winget,
    /// The tarball, a disk image, a zip or the Windows setup: the user
    /// updates by hand.
    Manual,
}

impl InstallKind {
    /// Look at this process.
    pub fn detect() -> Self {
        let exe = std::env::current_exe()
            .ok()
            .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
            .unwrap_or_default();
        let probe = Probe {
            explanation: option_env!("IRORI_UPDATE_EXPLANATION")
                .map(str::to_string)
                .or_else(|| std::env::var("IRORI_UPDATE_EXPLANATION").ok())
                .filter(|s| !s.trim().is_empty()),
            flatpak: std::env::var_os("FLATPAK_ID").is_some()
                || Path::new("/.flatpak-info").exists(),
            source_build: exe.ancestors().any(|a| {
                a.file_name().is_some_and(|n| n == "target")
                    && a.parent().is_some_and(|p| p.join("Cargo.toml").is_file())
            }),
            homebrew_cask: cfg!(target_os = "macos")
                && ["/opt/homebrew/Caskroom/irori", "/usr/local/Caskroom/irori"]
                    .iter()
                    .any(|p| Path::new(p).is_dir()),
            install_ini: exe
                .parent()
                .and_then(|dir| std::fs::read_to_string(dir.join("install.ini")).ok()),
        };
        classify(&exe, &probe)
    }

    /// Whether Irori looks for new versions itself.
    pub fn checks(&self) -> bool {
        matches!(
            self,
            Self::Homebrew | Self::Scoop | Self::Winget | Self::Manual
        )
    }

    /// The command that updates Irori, for package managers the user runs.
    pub fn update_command(&self) -> Option<&'static str> {
        match self {
            Self::Homebrew => Some("brew upgrade --cask irori"),
            Self::Scoop => Some("scoop update irori"),
            Self::Winget => Some("winget upgrade Irori.Irori"),
            _ => None,
        }
    }
}

/// What [`classify`] needs to know besides the executable's path.
#[derive(Debug, Default)]
struct Probe {
    explanation: Option<String>,
    flatpak: bool,
    source_build: bool,
    homebrew_cask: bool,
    /// `install.ini` beside the executable, written by the Windows setup.
    install_ini: Option<String>,
}

fn classify(exe: &Path, probe: &Probe) -> InstallKind {
    if let Some(text) = &probe.explanation {
        return InstallKind::Packaged(text.clone());
    }
    if probe.flatpak {
        return InstallKind::Flatpak;
    }
    if probe.source_build {
        return InstallKind::Source;
    }
    let path = exe.to_string_lossy().replace('\\', "/").to_lowercase();
    if path.starts_with("/nix/store/") {
        return InstallKind::Nix;
    }
    if path.starts_with("/usr/") && !path.starts_with("/usr/local/") {
        return InstallKind::SystemPackage;
    }
    if probe.homebrew_cask {
        return InstallKind::Homebrew;
    }
    if path.contains("/scoop/apps/irori/") {
        return InstallKind::Scoop;
    }
    if probe
        .install_ini
        .as_deref()
        .and_then(ini_channel)
        .is_some_and(|c| c.eq_ignore_ascii_case("winget"))
    {
        return InstallKind::Winget;
    }
    InstallKind::Manual
}

/// `Channel=` of the Windows setup's `install.ini`.
fn ini_channel(ini: &str) -> Option<&str> {
    ini.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("channel")
            .then(|| value.trim())
    })
}

/// A published version of Irori.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    /// `X.Y.Z`, without the tag's `v`.
    pub version: String,
    /// The release's page.
    pub url: String,
}

/// What the UI shows about Irori's own updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUpdateStatus {
    /// The running version.
    pub current: String,
    pub kind: InstallKind,
    /// A release newer than this one, from the last check.
    pub available: Option<Release>,
    /// The user closed the notice about `available`.
    pub dismissed: bool,
    /// Tell the user about `available` now: the check is on, this install
    /// checks, and they haven't dismissed it.
    pub announce: bool,
    /// Irori runs a newer version than last time.
    pub updated: bool,
    pub checking: bool,
    /// When the last check answered, in Unix seconds.
    pub checked_at: Option<u64>,
}

/// Saved in `state/app-update.json`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Saved {
    checked_at: u64,
    latest: Option<Release>,
    dismissed: Option<String>,
    last_run_version: Option<String>,
}

pub(crate) struct AppUpdater {
    endpoint: String,
    file: PathBuf,
    current: String,
    kind: InstallKind,
    saved: RwLock<Saved>,
    updated: bool,
    checking: AtomicBool,
}

impl AppUpdater {
    /// `app` is true for the app window: only it records the version that
    /// ran, so a command without a window doesn't swallow the "updated" note.
    pub(crate) fn load(app: bool) -> Self {
        Self::with(
            RELEASES_API,
            irori_core::Paths::get().state_dir().join("app-update.json"),
            env!("CARGO_PKG_VERSION"),
            InstallKind::detect(),
            app,
        )
    }

    /// Read the saved state, and with `record_run` record this version as
    /// the one that ran.
    fn with(
        endpoint: &str,
        file: PathBuf,
        current: &str,
        kind: InstallKind,
        record_run: bool,
    ) -> Self {
        let mut saved: Saved = json_store::read_or_default(&file);
        // The first start of all has nothing to compare with, and a source
        // build has no release notes.
        let updated = record_run
            && kind != InstallKind::Source
            && saved
                .last_run_version
                .as_deref()
                .is_some_and(|last| newer(current, last));
        if record_run && saved.last_run_version.as_deref() != Some(current) {
            saved.last_run_version = Some(current.to_string());
            if let Err(e) = json_store::write(&file, &saved) {
                tracing::warn!("saving the update state: {e:#}");
            }
        }
        Self {
            endpoint: endpoint.to_string(),
            file,
            current: current.to_string(),
            kind,
            saved: RwLock::new(saved),
            updated,
            checking: AtomicBool::new(false),
        }
    }

    pub(crate) fn kind(&self) -> &InstallKind {
        &self.kind
    }

    pub(crate) fn status(&self, enabled: bool) -> AppUpdateStatus {
        let saved = self.saved.read();
        let available = saved
            .latest
            .clone()
            .filter(|r| newer(&r.version, &self.current));
        let dismissed = available
            .as_ref()
            .is_some_and(|r| saved.dismissed.as_deref() == Some(r.version.as_str()));
        AppUpdateStatus {
            current: self.current.clone(),
            kind: self.kind.clone(),
            announce: available.is_some() && !dismissed && enabled && self.kind.checks(),
            available,
            dismissed,
            updated: self.updated,
            checking: self.checking.load(Ordering::Relaxed),
            checked_at: (saved.checked_at > 0).then_some(saved.checked_at),
        }
    }

    /// The last check is older than a day.
    pub(crate) fn due(&self, now: u64) -> bool {
        now.saturating_sub(self.saved.read().checked_at) >= CHECK_EVERY
    }

    pub(crate) fn set_checking(&self, checking: bool) {
        self.checking.store(checking, Ordering::Relaxed);
    }

    /// Ask GitHub for the newest release and save the answer.
    pub(crate) async fn check(&self) -> Result<()> {
        let latest = fetch(&self.endpoint).await?;
        let mut saved = self.saved.read().clone();
        saved.checked_at = util::unix_now();
        saved.latest = latest;
        if let Err(e) = json_store::write(&self.file, &saved) {
            tracing::warn!("saving the update state: {e:#}");
        }
        *self.saved.write() = saved;
        Ok(())
    }

    /// Stop announcing `version`.
    pub(crate) fn dismiss(&self, version: &str) {
        let mut saved = self.saved.read().clone();
        if saved.dismissed.as_deref() == Some(version) {
            return;
        }
        saved.dismissed = Some(version.to_string());
        if let Err(e) = json_store::write(&self.file, &saved) {
            tracing::warn!("saving the update state: {e:#}");
        }
        *self.saved.write() = saved;
    }
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// The newest stable release, `None` while there is none.
async fn fetch(endpoint: &str) -> Result<Option<Release>> {
    let resp = irori_core::http::client()
        .get(endpoint)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("Could not reach GitHub")?;
    // GitHub answers 404 while a repository has no published release.
    if resp.status().as_u16() == 404 {
        return Ok(None);
    }
    let body = resp
        .error_for_status()
        .context("GitHub refused the request")?
        .text()
        .await
        .context("Unexpected answer from GitHub")?;
    let release: GhRelease =
        serde_json::from_str(&body).context("Unexpected answer from GitHub")?;
    Ok(parse(release))
}

fn parse(release: GhRelease) -> Option<Release> {
    if release.draft || release.prerelease {
        return None;
    }
    let version = release.tag_name.strip_prefix('v')?;
    parse_version(version)?;
    Some(Release {
        version: version.to_string(),
        url: release
            .html_url
            .filter(|u| u.starts_with("https://github.com/"))
            .unwrap_or_else(|| release_page(version)),
    })
}

/// The release page of `version`, with its notes.
pub fn release_page(version: &str) -> String {
    format!("{RELEASES_PAGE}{version}")
}

/// `X.Y.Z` only: tags with anything else (`-beta.1`, `nightly`) are never
/// offered.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.split('.').map(|p| {
        (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse().ok())
            .flatten()
    });
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// `a` is a later version than `b`. Unreadable versions never are.
fn newer(a: &str, b: &str) -> bool {
    match (parse_version(a), parse_version(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_depot::test_server::{Resp, TestServer};
    use serde_json::json;

    fn exe(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    #[test]
    fn install_kinds() {
        let none = Probe::default();
        let kind = |path: &str, probe: &Probe| classify(&exe(path), probe);
        assert_eq!(kind("/usr/bin/irori", &none), InstallKind::SystemPackage);
        assert_eq!(kind("/usr/local/bin/irori", &none), InstallKind::Manual);
        assert_eq!(kind("/home/a/.local/bin/irori", &none), InstallKind::Manual);
        assert_eq!(
            kind("/nix/store/abc-irori-0.2.0/bin/irori", &none),
            InstallKind::Nix
        );
        assert_eq!(
            kind(
                "/Applications/Irori.app/Contents/MacOS/irori",
                &Probe {
                    homebrew_cask: true,
                    ..Probe::default()
                }
            ),
            InstallKind::Homebrew
        );
        assert_eq!(
            kind(r"C:\Users\a\scoop\apps\irori\current\irori.exe", &none),
            InstallKind::Scoop
        );
        let setup = |channel: &str| Probe {
            install_ini: Some(format!("[Install]\r\nChannel={channel}\r\n")),
            ..Probe::default()
        };
        let programs = r"C:\Users\a\AppData\Local\Programs\Irori\irori.exe";
        assert_eq!(kind(programs, &setup("winget")), InstallKind::Winget);
        assert_eq!(kind(programs, &setup("setup")), InstallKind::Manual);
        assert_eq!(
            kind(
                "/usr/bin/irori",
                &Probe {
                    flatpak: true,
                    ..Probe::default()
                }
            ),
            InstallKind::Flatpak
        );
        assert_eq!(
            kind(
                "/home/a/irori/target/release/irori",
                &Probe {
                    source_build: true,
                    ..Probe::default()
                }
            ),
            InstallKind::Source
        );
        // A packager's word wins over every guess.
        assert_eq!(
            kind(
                "/usr/bin/irori",
                &Probe {
                    explanation: Some("Update with your distribution.".into()),
                    flatpak: true,
                    ..Probe::default()
                }
            ),
            InstallKind::Packaged("Update with your distribution.".into())
        );
        assert!(InstallKind::Manual.checks() && InstallKind::Winget.checks());
        assert!(!InstallKind::Flatpak.checks() && !InstallKind::Source.checks());
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.0"));
        assert!(!newer("0.2.0", "0.2.0"));
        assert!(!newer("0.3.0-beta.1", "0.2.0"));
        assert!(!newer("0.3", "0.2.0"));
        assert!(!newer("0.3.0.1", "0.2.0"));
    }

    #[test]
    fn only_stable_tags_are_releases() {
        let gh = |tag: &str, prerelease: bool, draft: bool| GhRelease {
            tag_name: tag.into(),
            html_url: Some(format!(
                "https://github.com/irori-app/irori/releases/tag/{tag}"
            )),
            draft,
            prerelease,
        };
        assert_eq!(
            parse(gh("v0.3.0", false, false)),
            Some(Release {
                version: "0.3.0".into(),
                url: "https://github.com/irori-app/irori/releases/tag/v0.3.0".into(),
            })
        );
        assert_eq!(parse(gh("v0.3.0", true, false)), None);
        assert_eq!(parse(gh("v0.3.0", false, true)), None);
        assert_eq!(parse(gh("v0.3.0-rc.1", false, false)), None);
        assert_eq!(parse(gh("nightly", false, false)), None);
        // A page outside GitHub is never opened.
        let mut odd = gh("v0.3.0", false, false);
        odd.html_url = Some("https://example.com/".into());
        assert_eq!(parse(odd).unwrap().url, release_page("0.3.0"));
    }

    fn release_server(tag: &'static str) -> TestServer {
        TestServer::start(move |_| {
            Resp::json(json!({
                "tag_name": tag,
                "html_url": format!("https://github.com/irori-app/irori/releases/tag/{tag}"),
                "draft": false,
                "prerelease": false,
            }))
        })
    }

    #[tokio::test]
    async fn announces_a_newer_release_until_dismissed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("app-update.json");
        let server = release_server("v0.3.0");
        let endpoint = server.url("/releases/latest");
        let updater = AppUpdater::with(&endpoint, file.clone(), "0.2.0", InstallKind::Manual, true);
        assert!(updater.due(util::unix_now()));
        updater.check().await.unwrap();
        assert!(!updater.due(util::unix_now()));
        let status = updater.status(true);
        assert_eq!(status.available.as_ref().unwrap().version, "0.3.0");
        assert!(status.announce && !status.dismissed);
        assert!(!updater.status(false).announce, "the setting is off");

        updater.dismiss("0.3.0");
        let status = updater.status(true);
        assert!(status.dismissed && !status.announce);
        assert!(status.available.is_some(), "Settings still shows it");

        // The answer and the dismissal survive a restart, without asking
        // GitHub again within the day.
        let again = AppUpdater::with(&endpoint, file, "0.2.0", InstallKind::Manual, true);
        assert!(!again.due(util::unix_now()));
        assert!(again.status(true).dismissed);
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn the_same_or_an_older_release_is_not_an_update() {
        let dir = tempfile::tempdir().unwrap();
        let server = release_server("v0.2.0");
        let updater = AppUpdater::with(
            &server.url("/releases/latest"),
            dir.path().join("app-update.json"),
            "0.2.0",
            InstallKind::Manual,
            true,
        );
        updater.check().await.unwrap();
        let status = updater.status(true);
        assert!(status.available.is_none() && !status.announce);
        assert!(status.checked_at.is_some());
    }

    #[tokio::test]
    async fn no_release_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = TestServer::start(|_| Resp::status(404));
        let updater = AppUpdater::with(
            &missing.url("/releases/latest"),
            dir.path().join("a.json"),
            "0.2.0",
            InstallKind::Manual,
            true,
        );
        updater.check().await.unwrap();
        assert!(updater.status(true).available.is_none());

        let broken = TestServer::start(|_| Resp::status(503));
        let updater = AppUpdater::with(
            &broken.url("/releases/latest"),
            dir.path().join("b.json"),
            "0.2.0",
            InstallKind::Manual,
            true,
        );
        assert!(updater.check().await.is_err());
        assert!(
            updater.due(util::unix_now()),
            "a failed check doesn't count"
        );
    }

    #[test]
    fn notices_a_new_version_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("app-update.json");
        let first = AppUpdater::with(
            RELEASES_API,
            file.clone(),
            "0.2.0",
            InstallKind::Manual,
            true,
        );
        assert!(!first.status(true).updated, "nothing to compare with");
        let same = AppUpdater::with(
            RELEASES_API,
            file.clone(),
            "0.2.0",
            InstallKind::Manual,
            true,
        );
        assert!(!same.status(true).updated);
        let newer = AppUpdater::with(
            RELEASES_API,
            file.clone(),
            "0.3.0",
            InstallKind::Manual,
            true,
        );
        assert!(newer.status(true).updated);
        let after = AppUpdater::with(
            RELEASES_API,
            file.clone(),
            "0.3.0",
            InstallKind::Manual,
            true,
        );
        assert!(!after.status(true).updated);
        // A command without a window neither notes nor records a version.
        let cli = AppUpdater::with(
            RELEASES_API,
            file.clone(),
            "0.4.0",
            InstallKind::Manual,
            false,
        );
        assert!(!cli.status(true).updated);
        let app = AppUpdater::with(RELEASES_API, file, "0.4.0", InstallKind::Manual, true);
        assert!(app.status(true).updated);
    }
}
