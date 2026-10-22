//! The sideloaded-app model: what the user enters in the "Add app" dialog,
//! how it is validated, persisted and turned into a library entry.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ember_core::paths::expand_home;
use ember_core::{GameInfo, InstallPlatform, InstalledInfo, Runner};
use serde::{Deserialize, Serialize};

/// Platform of a sideloaded app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SideloadPlatform {
    /// A Windows executable run through Wine/Proton.
    #[default]
    Windows,
    /// A native Linux executable or script.
    Linux,
    /// A web app opened by URL.
    Browser,
}

impl SideloadPlatform {
    pub fn install_platform(self) -> InstallPlatform {
        match self {
            SideloadPlatform::Windows => InstallPlatform::Windows,
            SideloadPlatform::Linux => InstallPlatform::Linux,
            SideloadPlatform::Browser => InstallPlatform::Browser,
        }
    }

    /// `None` for platforms that cannot run on a Linux host (macOS).
    pub fn from_install_platform(platform: InstallPlatform) -> Option<Self> {
        match platform {
            InstallPlatform::Windows => Some(SideloadPlatform::Windows),
            InstallPlatform::Linux => Some(SideloadPlatform::Linux),
            InstallPlatform::Browser => Some(SideloadPlatform::Browser),
            InstallPlatform::Mac => None,
        }
    }
}

/// An app the user added manually, as entered in the add/edit dialog.
///
/// Art fields accept a URL (`https://…`, `file://…`) or a local path; local
/// paths are exposed to the library as `file://` URLs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SideloadApp {
    pub title: String,
    /// Absolute path of the program to run. Unused for browser apps.
    pub executable: Option<PathBuf>,
    pub platform: SideloadPlatform,
    /// Address of a browser app. A missing scheme is completed with `https://`.
    pub browser_url: Option<String>,
    /// User agent for a browser app; opens it in a dedicated app window.
    pub custom_user_agent: Option<String>,
    /// Open a browser app full screen in a dedicated app window.
    pub launch_fullscreen: bool,
    /// Vertical library tile art.
    pub cover_art: Option<String>,
    /// Wide hero/background art.
    pub background_art: Option<String>,
    /// Folder the app's files live in; defaults to the executable's folder.
    /// This is what "delete files" removes on uninstall.
    pub install_folder: Option<PathBuf>,
    /// Arguments always passed to the executable (shell-style quoting).
    pub launcher_args: String,
    pub description: Option<String>,
}

impl SideloadApp {
    /// Folder holding the app's files: `install_folder`, else the executable's folder.
    pub fn install_dir(&self) -> Option<PathBuf> {
        self.install_folder.clone().or_else(|| {
            self.executable
                .as_deref()
                .and_then(Path::parent)
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
        })
    }

    /// Trim and validate the user's input. Browser apps get a complete URL and
    /// lose executable fields; other apps need an absolute executable path and
    /// lose browser fields.
    pub fn normalized(mut self) -> Result<Self> {
        self.title = self.title.trim().to_string();
        if self.title.is_empty() {
            bail!("Enter a title for the app");
        }
        self.cover_art = non_empty(self.cover_art);
        self.background_art = non_empty(self.background_art);
        self.description = non_empty(self.description);
        self.launcher_args = self.launcher_args.trim().to_string();
        match self.platform {
            SideloadPlatform::Browser => {
                let url =
                    non_empty(self.browser_url).context("Enter the address of the web app")?;
                self.browser_url = Some(normalize_url(&url)?);
                self.custom_user_agent = non_empty(self.custom_user_agent);
                self.executable = None;
                self.install_folder = None;
            }
            SideloadPlatform::Windows | SideloadPlatform::Linux => {
                let exe = self
                    .executable
                    .filter(|p| !p.as_os_str().is_empty())
                    .context("Choose the executable to run")?;
                self.executable = Some(absolute(&exe, "executable")?);
                self.install_folder = match self.install_folder {
                    Some(p) if !p.as_os_str().is_empty() => Some(absolute(&p, "install folder")?),
                    _ => None,
                };
                self.browser_url = None;
                self.custom_user_agent = None;
                self.launch_fullscreen = false;
            }
        }
        Ok(self)
    }
}

/// One persisted app with its library id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredApp {
    pub app_name: String,
    #[serde(flatten)]
    pub app: SideloadApp,
}

/// Shape of `library.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LibraryFile {
    pub version: u32,
    pub apps: Vec<StoredApp>,
}

pub(crate) const LIBRARY_VERSION: u32 = 1;

impl StoredApp {
    /// The library entry shown for this app.
    pub fn to_game_info(&self) -> GameInfo {
        let app = &self.app;
        let platform = app.platform.install_platform();
        let install_dir = app.install_dir();
        let cover = app.cover_art.as_deref().map(art_url);
        let background = app.background_art.as_deref().map(art_url);
        GameInfo {
            runner: Some(Runner::Sideload),
            app_name: self.app_name.clone(),
            title: app.title.clone(),
            description: app.description.clone(),
            art_square: background.clone().filter(|_| cover.is_none()),
            art_cover: cover,
            art_background: background,
            is_installed: true,
            install: Some(InstalledInfo {
                install_path: install_dir
                    .as_deref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                executable: app
                    .executable
                    .as_deref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                platform,
                ..Default::default()
            }),
            installable: false,
            can_run_offline: app.platform != SideloadPlatform::Browser,
            folder_name: install_dir
                .as_deref()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned()),
            is_linux_native: app.platform == SideloadPlatform::Linux,
            platforms: vec![platform],
            browser_url: app.browser_url.clone(),
            custom_user_agent: app.custom_user_agent.clone(),
            launch_full_screen: app.launch_fullscreen,
            ..Default::default()
        }
        .finalize()
    }
}

/// Art reference as a URL: local paths become `file://` URLs, everything
/// else is kept.
pub(crate) fn art_url(value: &str) -> String {
    let path = expand_home(value);
    if path.is_absolute()
        && let Ok(url) = url::Url::from_file_path(&path)
    {
        return url.to_string();
    }
    value.to_string()
}

/// Complete and validate a web app address (`example.com` → `https://example.com/`).
pub(crate) fn normalize_url(input: &str) -> Result<String> {
    let input = input.trim();
    let candidate = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    };
    let url = url::Url::parse(&candidate)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
        .with_context(|| format!("\"{input}\" is not a valid web address"))?;
    Ok(url.to_string())
}

/// Short random library id: a v4 UUID in 22-character flickr base58 (the
/// format Heroic's sideload ids use).
pub(crate) fn generate_app_name() -> String {
    const ALPHABET: &[u8] = b"123456789abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ";
    let mut n = uuid::Uuid::new_v4().as_u128();
    let mut out = Vec::with_capacity(22);
    while n > 0 {
        out.push(ALPHABET[(n % 58) as usize]);
        n /= 58;
    }
    while out.len() < 22 {
        out.push(ALPHABET[0]);
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn absolute(path: &Path, what: &str) -> Result<PathBuf> {
    let expanded = expand_home(&path.to_string_lossy());
    if !expanded.is_absolute() {
        bail!("The {what} must be a full path, got {}", path.display());
    }
    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows_app() -> SideloadApp {
        SideloadApp {
            title: "  Game  ".into(),
            executable: Some("/games/Game/game.exe".into()),
            platform: SideloadPlatform::Windows,
            browser_url: Some("example.com".into()),
            custom_user_agent: Some("UA".into()),
            launch_fullscreen: true,
            cover_art: Some("/art/cover.png".into()),
            background_art: Some(" ".into()),
            ..Default::default()
        }
    }

    #[test]
    fn ids_are_short_and_distinct() {
        let a = generate_app_name();
        let b = generate_app_name();
        assert_eq!(a.len(), 22);
        assert_ne!(a, b);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() && !"0OIl".contains(c))
        );
    }

    #[test]
    fn normalizes_executable_apps() {
        let app = windows_app().normalized().unwrap();
        assert_eq!(app.title, "Game");
        assert!(app.browser_url.is_none());
        assert!(app.custom_user_agent.is_none());
        assert!(!app.launch_fullscreen);
        assert!(app.background_art.is_none());
        assert_eq!(app.install_dir(), Some(PathBuf::from("/games/Game")));

        let mut relative = windows_app();
        relative.executable = Some("game.exe".into());
        assert!(relative.normalized().is_err());
        let mut missing = windows_app();
        missing.executable = None;
        assert!(missing.normalized().is_err());
        let mut untitled = windows_app();
        untitled.title = " ".into();
        assert!(untitled.normalized().is_err());
    }

    #[test]
    fn normalizes_browser_apps() {
        let app = SideloadApp {
            title: "Web".into(),
            platform: SideloadPlatform::Browser,
            browser_url: Some(" play.example.com/game ".into()),
            executable: Some("/x".into()),
            custom_user_agent: Some("".into()),
            ..Default::default()
        }
        .normalized()
        .unwrap();
        assert_eq!(
            app.browser_url.as_deref(),
            Some("https://play.example.com/game")
        );
        assert!(app.executable.is_none());
        assert!(app.custom_user_agent.is_none());

        assert_eq!(normalize_url("http://a.b").unwrap(), "http://a.b/");
        assert!(normalize_url("ftp://a.b").is_err());
        assert!(normalize_url("https://").is_err());
        let no_url = SideloadApp {
            title: "Web".into(),
            platform: SideloadPlatform::Browser,
            ..Default::default()
        };
        assert!(no_url.normalized().is_err());
    }

    #[test]
    fn art_paths_become_file_urls() {
        assert_eq!(art_url("/a b/c.png"), "file:///a%20b/c.png");
        assert_eq!(art_url("https://x/y.jpg"), "https://x/y.jpg");
        assert_eq!(art_url("file:///z.png"), "file:///z.png");
    }

    #[test]
    fn maps_to_game_info() {
        let stored = StoredApp {
            app_name: "abc".into(),
            app: windows_app().normalized().unwrap(),
        };
        let g = stored.to_game_info();
        assert_eq!(g.runner, Some(Runner::Sideload));
        assert_eq!(g.app_name, "abc");
        assert_eq!(g.sort_title, "game");
        assert!(g.is_installed);
        assert!(g.can_run_offline);
        assert_eq!(g.art_cover.as_deref(), Some("file:///art/cover.png"));
        assert!(g.art_square.is_none());
        assert_eq!(g.folder_name.as_deref(), Some("Game"));
        let install = g.install.unwrap();
        assert_eq!(install.executable, "/games/Game/game.exe");
        assert_eq!(install.install_path, "/games/Game");
        assert_eq!(install.platform, InstallPlatform::Windows);

        let web = StoredApp {
            app_name: "w".into(),
            app: SideloadApp {
                title: "Web".into(),
                platform: SideloadPlatform::Browser,
                browser_url: Some("https://x.org/".into()),
                launch_fullscreen: true,
                background_art: Some("https://x.org/bg.png".into()),
                ..Default::default()
            },
        }
        .to_game_info();
        assert!(!web.can_run_offline);
        assert!(web.launch_full_screen);
        assert_eq!(web.browser_url.as_deref(), Some("https://x.org/"));
        assert_eq!(web.cover_art(), Some("https://x.org/bg.png"));
        assert_eq!(web.install.unwrap().platform, InstallPlatform::Browser);
    }

    #[test]
    fn library_file_roundtrip() {
        let file = LibraryFile {
            version: LIBRARY_VERSION,
            apps: vec![StoredApp {
                app_name: "id1".into(),
                app: windows_app().normalized().unwrap(),
            }],
        };
        let json = serde_json::to_string(&file).unwrap();
        assert!(json.contains("\"app_name\":\"id1\""));
        assert!(json.contains("\"platform\":\"windows\""));
        let back: LibraryFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.apps, file.apps);
    }
}
