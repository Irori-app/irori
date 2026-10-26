//! Host capabilities. A game's build platform is independent of its host.
use crate::{InstallPlatform, WineKind};
use std::path::{Path, PathBuf};

/// The desktop operating systems supported by Ember.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPlatform {
    Linux,
    Mac,
    Windows,
}

impl HostPlatform {
    /// Host selected at compile time; never inferred from a game's metadata.
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Linux
        }
    }
    pub const fn native(self) -> InstallPlatform {
        match self {
            Self::Linux => InstallPlatform::Linux,
            Self::Mac => InstallPlatform::Mac,
            Self::Windows => InstallPlatform::Windows,
        }
    }
    pub fn supports(self, build: InstallPlatform) -> bool {
        build == self.native()
            || matches!(build, InstallPlatform::Windows | InstallPlatform::Browser)
    }
    pub fn needs_wine(self, build: InstallPlatform) -> bool {
        build == InstallPlatform::Windows && self != Self::Windows
    }
    pub const fn supports_wine(self, kind: WineKind) -> bool {
        match self {
            Self::Linux => matches!(kind, WineKind::Wine | WineKind::Proton),
            Self::Mac => matches!(
                kind,
                WineKind::Wine | WineKind::Crossover | WineKind::Toolkit
            ),
            Self::Windows => false,
        }
    }
}

/// Steam's default installation location. User overrides take precedence.
pub fn steam_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(std::env::temp_dir);
    match HostPlatform::current() {
        HostPlatform::Mac => home.join("Library/Application Support/Steam"),
        HostPlatform::Windows => PathBuf::from(
            std::env::var_os("ProgramFiles(x86)")
                .or_else(|| std::env::var_os("ProgramFiles"))
                .unwrap_or_else(|| r"C:\Program Files (x86)".into()),
        )
        .join("Steam"),
        HostPlatform::Linux => home.join(".steam/steam"),
    }
}

/// Whether Ember runs inside Steam's controller interface: the Steam Deck's
/// Game Mode (a gamescope session) or Steam Big Picture, which marks the
/// programs it starts with `SteamGamepadUI=1`.
pub fn in_steam_gamepad_ui() -> bool {
    steam_gamepad_ui_from(|key| std::env::var(key).ok())
}

/// [`in_steam_gamepad_ui`] over an environment lookup.
pub fn steam_gamepad_ui_from(var: impl Fn(&str) -> Option<String>) -> bool {
    var("XDG_CURRENT_DESKTOP").is_some_and(|v| v.eq_ignore_ascii_case("gamescope"))
        || var("SteamGamepadUI").is_some_and(|v| v.trim() == "1")
}

pub fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

/// Resolve a macOS app bundle to its declared executable, preserving process
/// tracking, arguments and environment rather than detaching via `open -a`.
pub fn bundle_executable(bundle: &Path) -> anyhow::Result<PathBuf> {
    use anyhow::{Context as _, ensure};
    let info = plist::Value::from_file(bundle.join("Contents/Info.plist"))
        .context("Could not read the application's Info.plist")?;
    let name = info
        .as_dictionary()
        .and_then(|d| d.get("CFBundleExecutable"))
        .and_then(plist::Value::as_string)
        .context("The app has no CFBundleExecutable")?;
    ensure!(
        !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\']),
        "Invalid bundle executable name"
    );
    let exe = bundle.join("Contents/MacOS").join(name);
    ensure!(
        crate::util::is_executable(&exe),
        "The app executable was not found or is not executable: {}",
        exe.display()
    );
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_steam_gamepad_ui() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert!(steam_gamepad_ui_from(env(&[(
            "XDG_CURRENT_DESKTOP",
            "gamescope"
        )])));
        assert!(steam_gamepad_ui_from(env(&[("SteamGamepadUI", "1")])));
        assert!(!steam_gamepad_ui_from(env(&[("SteamGamepadUI", "0")])));
        assert!(!steam_gamepad_ui_from(env(&[(
            "XDG_CURRENT_DESKTOP",
            "KDE"
        )])));
        assert!(!steam_gamepad_ui_from(env(&[])));
    }
    #[test]
    fn host_build_matrix() {
        for host in [
            HostPlatform::Linux,
            HostPlatform::Mac,
            HostPlatform::Windows,
        ] {
            assert!(host.supports(host.native()));
            assert!(host.supports(InstallPlatform::Windows));
            assert!(host.supports(InstallPlatform::Browser));
            assert!(!host.needs_wine(host.native()));
        }
        assert!(HostPlatform::Mac.needs_wine(InstallPlatform::Windows));
        assert!(HostPlatform::Linux.needs_wine(InstallPlatform::Windows));
        assert!(!HostPlatform::Windows.supports(InstallPlatform::Linux));
        assert!(!HostPlatform::Linux.supports(InstallPlatform::Mac));
        assert!(!HostPlatform::Mac.supports_wine(WineKind::Proton));
        assert!(!HostPlatform::Windows.supports_wine(WineKind::Wine));
    }
    #[test]
    fn bundle_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Contents")).unwrap();
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "CFBundleExecutable".into(),
            plist::Value::String("../outside".into()),
        );
        plist::Value::Dictionary(dict)
            .to_file_binary(dir.path().join("Contents/Info.plist"))
            .unwrap();
        assert!(
            bundle_executable(dir.path())
                .unwrap_err()
                .to_string()
                .contains("Invalid")
        );
    }
}
