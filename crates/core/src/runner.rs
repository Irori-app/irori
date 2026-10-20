use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The store backend a game belongs to. Serialized with Heroic's runner names so
/// Heroic config files import unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runner {
    /// Epic Games Store (`legendary` is its id in Heroic-compatible data).
    Legendary,
    /// GOG.
    Gog,
    /// Amazon Games (`nile` is its id in Heroic-compatible data).
    Nile,
    /// ZOOM Platform, driven through its HTTP API.
    Zoom,
    /// Apps the user added manually.
    Sideload,
}

impl Runner {
    pub const ALL: [Runner; 5] = [
        Runner::Legendary,
        Runner::Gog,
        Runner::Nile,
        Runner::Zoom,
        Runner::Sideload,
    ];

    /// Stable identifier used in paths, IDs and config keys.
    pub fn id(self) -> &'static str {
        match self {
            Runner::Legendary => "legendary",
            Runner::Gog => "gog",
            Runner::Nile => "nile",
            Runner::Zoom => "zoom",
            Runner::Sideload => "sideload",
        }
    }

    /// Store name as users know it.
    pub fn store_name(self) -> &'static str {
        match self {
            Runner::Legendary => "Epic Games",
            Runner::Gog => "GOG",
            Runner::Nile => "Amazon Games",
            Runner::Zoom => "ZOOM Platform",
            Runner::Sideload => "Added manually",
        }
    }

    /// Short store label for badges.
    pub fn short_name(self) -> &'static str {
        match self {
            Runner::Legendary => "Epic",
            Runner::Gog => "GOG",
            Runner::Nile => "Amazon",
            Runner::Zoom => "ZOOM",
            Runner::Sideload => "Other",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "legendary" | "epic" => Runner::Legendary,
            "gog" => Runner::Gog,
            "nile" | "amazon" => Runner::Nile,
            "zoom" => Runner::Zoom,
            "sideload" => Runner::Sideload,
            _ => return None,
        })
    }

    /// Whether this store has an account to log in to.
    pub fn has_account(self) -> bool {
        !matches!(self, Runner::Sideload)
    }
}

impl fmt::Display for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.store_name())
    }
}

/// Platform of a game build. Stores spell these differently ("Windows",
/// "windows", "Win32", "osx", "Mac", "linux"); deserialization accepts all of
/// them and serialization writes Heroic's canonical spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InstallPlatform {
    #[default]
    Windows,
    Linux,
    Mac,
    /// A web game opened in the browser (sideloaded browser apps).
    Browser,
}

impl InstallPlatform {
    pub fn as_str(self) -> &'static str {
        match self {
            InstallPlatform::Windows => "windows",
            InstallPlatform::Linux => "linux",
            InstallPlatform::Mac => "osx",
            InstallPlatform::Browser => "Browser",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            InstallPlatform::Windows => "Windows",
            InstallPlatform::Linux => "Linux",
            InstallPlatform::Mac => "macOS",
            InstallPlatform::Browser => "Browser",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "windows" | "win32" | "win64" | "win" => InstallPlatform::Windows,
            "linux" => InstallPlatform::Linux,
            "mac" | "osx" | "macos" => InstallPlatform::Mac,
            "browser" => InstallPlatform::Browser,
            _ => return None,
        })
    }

    /// Whether running this platform on the Linux host needs a Wine layer.
    pub fn needs_wine(self) -> bool {
        matches!(self, InstallPlatform::Windows)
    }
}

impl Serialize for InstallPlatform {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InstallPlatform {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(InstallPlatform::parse(&s).unwrap_or_default())
    }
}

impl fmt::Display for InstallPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Stable identity of a game across stores: the runner plus the store's app name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GameId {
    pub runner: Runner,
    pub app_name: Arc<str>,
}

impl GameId {
    pub fn new(runner: Runner, app_name: impl Into<Arc<str>>) -> Self {
        Self {
            runner,
            app_name: app_name.into(),
        }
    }

    /// `runner:app_name`, used as element IDs and log/file keys.
    pub fn key(&self) -> String {
        format!("{}:{}", self.runner.id(), self.app_name)
    }
}

impl fmt::Display for GameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.runner.id(), self.app_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_accepts_store_spellings() {
        for (s, p) in [
            ("Windows", InstallPlatform::Windows),
            ("Win32", InstallPlatform::Windows),
            ("linux", InstallPlatform::Linux),
            ("osx", InstallPlatform::Mac),
            ("Mac", InstallPlatform::Mac),
            ("Browser", InstallPlatform::Browser),
        ] {
            let json = format!("\"{s}\"");
            assert_eq!(serde_json::from_str::<InstallPlatform>(&json).unwrap(), p);
        }
    }

    #[test]
    fn runner_roundtrip() {
        for r in Runner::ALL {
            let json = serde_json::to_string(&r).unwrap();
            assert_eq!(json, format!("\"{}\"", r.id()));
            assert_eq!(serde_json::from_str::<Runner>(&json).unwrap(), r);
        }
    }
}
