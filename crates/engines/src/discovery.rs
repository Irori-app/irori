//! Finding the installs of an engine on this computer: versions Ember
//! manages, Flatpaks, programs on `PATH`, macOS app bundles, known Windows
//! locations and the program the user picked. Discovery only looks at the
//! filesystem (no `flatpak` or registry calls) and is blocking; run it on
//! the blocking pool.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::host::Os;
use crate::manifest::{EngineManifest, Source};
use crate::state::EngineState;

/// Where an installation came from. Ember updates and removes only
/// [`Origin::Managed`] ones; the others are labelled and used as they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Managed,
    Flatpak,
    /// On `PATH` (a distribution package or a manual install).
    System,
    /// A macOS app bundle in an Applications folder.
    App,
    /// A known Windows install location.
    Windows,
    /// A program the user chose.
    Custom,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Managed => "managed",
            Origin::Flatpak => "flatpak",
            Origin::System => "system",
            Origin::App => "app",
            Origin::Windows => "windows",
            Origin::Custom => "custom",
        }
    }
}

/// One usable install of an engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installation {
    pub engine: String,
    pub origin: Origin,
    /// Known for managed installs.
    pub version: Option<String>,
    /// The program to run: an executable, or an `.app` bundle on macOS.
    pub program: PathBuf,
    /// The Flatpak application id, for [`Origin::Flatpak`].
    pub flatpak: Option<String>,
    /// The managed install folder.
    pub dir: Option<PathBuf>,
    /// Size on disk of a managed install.
    pub size: u64,
}

impl Installation {
    /// Stable id stored in settings: `managed:<version>` or the origin.
    pub fn id(&self) -> String {
        match (&self.origin, &self.version) {
            (Origin::Managed, Some(v)) => format!("managed:{v}"),
            (origin, _) => origin.as_str().to_string(),
        }
    }
}

/// Every installation of `manifest`, best first: managed versions newest
/// first, then the other sources in manifest order, then the custom path.
pub fn find(manifest: &EngineManifest, state: &EngineState) -> Vec<Installation> {
    find_with(manifest, state, &Probe::system())
}

/// The places discovery looks, so tests can point it elsewhere.
pub(crate) struct Probe {
    pub flatpak_roots: Vec<PathBuf>,
    pub applications: Vec<PathBuf>,
    pub path: Option<std::ffi::OsString>,
    pub os: Os,
}

impl Probe {
    fn system() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            flatpak_roots: vec![
                home.join(".local/share/flatpak/app"),
                PathBuf::from("/var/lib/flatpak/app"),
            ],
            applications: vec![PathBuf::from("/Applications"), home.join("Applications")],
            path: std::env::var_os("PATH"),
            os: Os::current(),
        }
    }
}

pub(crate) fn find_with(
    manifest: &EngineManifest,
    state: &EngineState,
    probe: &Probe,
) -> Vec<Installation> {
    let mut out = Vec::new();
    let base = |origin, program: PathBuf| Installation {
        engine: manifest.id.clone(),
        origin,
        version: None,
        program,
        flatpak: None,
        dir: None,
        size: 0,
    };
    let mut managed: Vec<_> = state
        .managed_versions(&manifest.id)
        .iter()
        .filter(|m| m.program.exists())
        .collect();
    managed.sort_by(|a, b| crate::releases::compare_versions(&b.version, &a.version));
    for m in managed {
        out.push(Installation {
            version: Some(m.version.clone()),
            dir: Some(m.dir.clone()),
            size: m.size,
            ..base(Origin::Managed, m.program.clone())
        });
    }
    for source in &manifest.sources {
        match source {
            Source::Flatpak { id } if probe.os == Os::Linux => {
                if probe
                    .flatpak_roots
                    .iter()
                    .any(|root| root.join(id).join("current").exists())
                {
                    out.push(Installation {
                        flatpak: Some(id.clone()),
                        ..base(Origin::Flatpak, PathBuf::from("flatpak"))
                    });
                }
            }
            Source::Path { names } => {
                let cwd = std::env::current_dir().unwrap_or_default();
                if let Some(program) = names.iter().find_map(|n| {
                    which::which_in(n, probe.path.as_ref(), &cwd)
                        .ok()
                        .filter(|p| !is_flatpak_export(p))
                }) {
                    out.push(base(Origin::System, program));
                }
            }
            Source::MacApp { bundle } if probe.os == Os::Macos => {
                if let Some(app) = probe
                    .applications
                    .iter()
                    .map(|dir| dir.join(bundle))
                    .find(|p| p.join("Contents/Info.plist").exists())
                {
                    out.push(base(Origin::App, app));
                }
            }
            Source::Windows { paths } if probe.os == Os::Windows => {
                if let Some(program) = paths.iter().map(|p| expand_env(p)).find(|p| p.is_file()) {
                    out.push(base(Origin::Windows, program));
                }
            }
            _ => {}
        }
    }
    if let Some(custom) = state.custom.get(&manifest.id)
        && custom.exists()
    {
        out.push(base(Origin::Custom, custom.clone()));
    }
    out
}

/// Flatpak's exported launchers (`…/flatpak/exports/bin/<id>`) are found as
/// Flatpaks already.
fn is_flatpak_export(path: &Path) -> bool {
    path.to_string_lossy().contains("/flatpak/exports/bin/")
}

/// Expand `%VAR%` references.
pub fn expand_env(path: &str) -> PathBuf {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    PathBuf::from(out)
}

/// Find the program named by `names` inside a managed install folder (two
/// levels deep). A name may contain one `*` and may be an `.app` bundle.
pub fn locate_program(dir: &Path, names: &[String]) -> Option<PathBuf> {
    fn matches(pattern: &str, name: &str) -> bool {
        match pattern.split_once('*') {
            Some((pre, post)) => {
                name.len() >= pre.len() + post.len()
                    && name.starts_with(pre)
                    && name.ends_with(post)
            }
            None => pattern.eq_ignore_ascii_case(name),
        }
    }
    fn walk(dir: &Path, pattern: &str, depth: usize) -> Option<PathBuf> {
        let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in &entries {
            let name = e.file_name();
            if matches(pattern, &name.to_string_lossy()) {
                return Some(e.path());
            }
        }
        if depth == 0 {
            return None;
        }
        entries
            .iter()
            .filter(|e| e.path().is_dir() && !e.path().extension().is_some_and(|x| x == "app"))
            .find_map(|e| walk(&e.path(), pattern, depth - 1))
    }
    names.iter().find_map(|n| walk(dir, n, 2))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::state::ManagedInstall;

    #[cfg(unix)]
    fn manifest() -> EngineManifest {
        EngineManifest::parse(
            &serde_json::json!({
                "id": "emu", "name": "Emu", "license": "MIT", "systems": ["nes"],
                "sources": [
                    {"type": "flatpak", "id": "org.example.Emu"},
                    {"type": "path", "names": ["emu"]},
                    {"type": "macApp", "bundle": "Emu.app"}
                ]
            })
            .to_string(),
        )
        .unwrap()
    }

    // Flatpaks and `PATH` lookups of extensionless programs are Unix things.
    #[cfg(unix)]
    #[test]
    fn finds_managed_flatpak_path_and_custom_installs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("flatpak/org.example.Emu/current")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let bin = root.join("bin/emu");
        std::fs::write(&bin, b"#!/bin/sh").unwrap();
        crate::archive::make_executable(&bin).unwrap();
        let v1 = root.join("managed/1.0/emu");
        let v2 = root.join("managed/1.10/emu");
        for p in [&v1, &v2] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
        let custom = root.join("custom-emu");
        std::fs::write(&custom, b"x").unwrap();
        let mut state = EngineState::default();
        state.managed.insert(
            "emu".into(),
            ["1.0", "1.10", "2.0"]
                .iter()
                .map(|v| ManagedInstall {
                    version: v.to_string(),
                    dir: root.join("managed").join(v),
                    program: root.join("managed").join(v).join("emu"),
                    size: 1,
                    installed_at: String::new(),
                    url: String::new(),
                })
                .collect(),
        );
        state.custom.insert("emu".into(), custom.clone());
        let probe = Probe {
            flatpak_roots: vec![root.join("flatpak")],
            applications: vec![],
            path: Some(root.join("bin").into_os_string()),
            os: Os::Linux,
        };
        let found = find_with(&manifest(), &state, &probe);
        let ids: Vec<String> = found.iter().map(Installation::id).collect();
        // 2.0's program is missing, so it is not offered.
        assert_eq!(
            ids,
            ["managed:1.10", "managed:1.0", "flatpak", "system", "custom"]
        );
        assert_eq!(found[2].flatpak.as_deref(), Some("org.example.Emu"));
        assert_eq!(found[3].program, bin);
    }

    #[test]
    fn locates_programs_by_name_and_wildcard() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Dolphin-x64")).unwrap();
        std::fs::write(dir.path().join("Dolphin-x64/Dolphin.exe"), b"MZ").unwrap();
        std::fs::write(
            dir.path().join("PPSSPP-v1.20-anylinux-x86_64.AppImage"),
            b"x",
        )
        .unwrap();
        assert_eq!(
            locate_program(dir.path(), &["Dolphin.exe".into()]).unwrap(),
            dir.path().join("Dolphin-x64/Dolphin.exe")
        );
        assert!(locate_program(dir.path(), &["PPSSPP-*.AppImage".into()]).is_some());
        assert!(locate_program(dir.path(), &["nothing".into()]).is_none());
    }

    #[test]
    fn expands_windows_variables() {
        // SAFETY: tests in this module don't read this variable concurrently.
        unsafe { std::env::set_var("EMBER_TEST_DIR", "C:\\Games") };
        assert_eq!(
            expand_env("%EMBER_TEST_DIR%\\Emu\\emu.exe"),
            PathBuf::from("C:\\Games\\Emu\\emu.exe")
        );
        assert_eq!(expand_env("%NOPE_X%\\a"), PathBuf::from("%NOPE_X%\\a"));
    }
}
