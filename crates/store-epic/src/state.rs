//! Irori's Epic state files. All functions are blocking; call them from
//! `spawn_blocking`.
//!
//! In `Paths::store_config_dir(Runner::Legendary)`:
//! * `session.json`: the signed-in account, through `irori_secrets`.
//! * `library.json`: the account's apps ([`GameRecord`]s).
//! * `installs.json`: installed apps by app name.
//! * `builds/<app>.manifest`: manifest of each installed build, the base of
//!   update diffs, repairs and uninstalls.
//! * `third-party-installed.json`: EA app / Ubisoft titles set up by Irori.
//! * `overlay.json`: the EOS overlay install.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use irori_core::util::format_bytes;
use irori_core::{InstallPlatform, InstalledInfo, json_store};
use irori_epic_api::Session;
use irori_epic_api::library::GameRecord;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Where the Epic store keeps its files.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    pub config: PathBuf,
    /// Irori's caches for this store.
    pub cache: PathBuf,
    /// Irori's tools directory (EOS overlay, EA/Ubisoft installers).
    pub tools: PathBuf,
}

impl Layout {
    pub fn session_json(&self) -> PathBuf {
        self.config.join("session.json")
    }
    pub fn library_json(&self) -> PathBuf {
        self.config.join("library.json")
    }
    pub fn installs_json(&self) -> PathBuf {
        self.config.join("installs.json")
    }
    pub fn builds_dir(&self) -> PathBuf {
        self.config.join("builds")
    }
    pub fn build_manifest(&self, app: &str) -> PathBuf {
        self.builds_dir()
            .join(format!("{}.manifest", irori_core::paths::sanitize(app)))
    }
    pub fn third_party_json(&self) -> PathBuf {
        self.config.join("third-party-installed.json")
    }
    pub fn overlay_json(&self) -> PathBuf {
        self.config.join("overlay.json")
    }
    /// Resume journal of an interrupted download.
    pub fn resume_file(&self, app: &str) -> PathBuf {
        self.cache
            .join("resume")
            .join(format!("{}.jsonl", irori_core::paths::sanitize(app)))
    }
    /// Ownership tokens handed to DRM-protected games.
    /// Play sessions waiting for upload to Epic.
    pub fn playtime_queue(&self) -> PathBuf {
        self.config.join("playtime_queue.json")
    }
    pub fn token_dir(&self) -> PathBuf {
        self.cache.join("tokens")
    }
    pub fn install_info_cache(&self) -> PathBuf {
        self.cache.join("install_info.json")
    }
    pub fn extra_info_cache(&self) -> PathBuf {
        self.cache.join("extra_info.json")
    }
    pub fn overrides_cache(&self) -> PathBuf {
        self.cache.join("game_overrides.json")
    }
    pub fn umu_cache(&self) -> PathBuf {
        self.cache.join("umu.json")
    }
    /// EA app / Ubisoft Connect installers.
    pub fn redist_dir(&self) -> PathBuf {
        self.tools.join("redist").join("legendary")
    }
    /// Default EOS overlay install location.
    pub fn overlay_dir(&self) -> PathBuf {
        self.tools.join("eos_overlay")
    }
}

/// Accepts integers, floats and numeric strings.
fn de_lenient_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().map(|f| f.max(0.0) as u64))
            .unwrap_or(0),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    })
}

fn de_lenient_opt_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Null => None,
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        _ => None,
    })
}

/// Prerequisites a build asks to install once per prefix.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Prerequisites {
    pub ids: Vec<String>,
    pub name: String,
    pub path: String,
    pub args: String,
}

/// An installed app. Field names match legendary's `installed.json`, so
/// Heroic's records import as they are.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct InstalledGame {
    pub app_name: String,
    pub title: String,
    pub version: String,
    /// `Windows`, `Win32` or `Mac`.
    pub platform: String,
    pub install_path: String,
    pub executable: String,
    pub launch_parameters: String,
    #[serde(deserialize_with = "de_lenient_u64")]
    pub install_size: u64,
    pub install_tags: Vec<String>,
    pub is_dlc: bool,
    pub can_run_offline: bool,
    pub requires_ot: bool,
    #[serde(alias = "prereq_info")]
    pub prereq: Option<Prerequisites>,
    pub save_path: Option<String>,
    #[serde(deserialize_with = "de_lenient_opt_u64")]
    pub save_timestamp: Option<u64>,
    /// Installation GUID shared with an Epic Games Launcher (EGL sync).
    pub egl_guid: String,
    /// Decryption keys of the installed build.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub manifest_secrets: BTreeMap<String, String>,
    /// The user keeps this version: no update checks. Irori's own key;
    /// legendary ignores it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pinned_version: bool,
    #[serde(skip)]
    pub third_party: bool,
}

impl InstalledGame {
    pub fn install_platform(&self) -> InstallPlatform {
        InstallPlatform::parse(&self.platform).unwrap_or_default()
    }

    /// Epic's platform name, defaulting to Windows.
    pub fn platform_arg(&self) -> &str {
        if self.platform.is_empty() {
            "Windows"
        } else {
            &self.platform
        }
    }

    pub fn to_installed_info(&self) -> InstalledInfo {
        InstalledInfo {
            install_path: self.install_path.clone(),
            executable: self.executable.clone(),
            install_size: if self.third_party {
                String::new()
            } else {
                format_bytes(self.install_size)
            },
            install_size_bytes: self.install_size,
            version: self.version.clone(),
            platform: self.install_platform(),
            is_dlc: self.is_dlc,
            sdl: self.install_tags.clone(),
            pinned_version: self.pinned_version,
            ..Default::default()
        }
    }

    pub fn secrets(&self) -> irori_epic_api::Secrets {
        self.manifest_secrets
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

pub(crate) type InstalledMap = HashMap<String, InstalledGame>;

/// Installed apps: `installs.json` plus registered third-party titles.
pub(crate) fn read_installed(layout: &Layout) -> InstalledMap {
    let mut map: InstalledMap =
        json_store::read::<BTreeMap<String, InstalledGame>>(&layout.installs_json())
            .unwrap_or_else(|e| {
                tracing::error!("{e:#}");
                None
            })
            .unwrap_or_default()
            .into_iter()
            .map(|(k, mut v)| {
                if v.app_name.is_empty() {
                    v.app_name = k.clone();
                }
                (k, v)
            })
            .collect();
    for (app, platform) in read_third_party(layout) {
        map.entry(app.clone()).or_insert(InstalledGame {
            app_name: app,
            platform,
            third_party: true,
            ..Default::default()
        });
    }
    map
}

/// Write the install records (third-party entries live in their own file).
pub(crate) fn write_installed(layout: &Layout, map: &InstalledMap) -> Result<()> {
    let own: BTreeMap<&String, &InstalledGame> =
        map.iter().filter(|(_, g)| !g.third_party).collect();
    json_store::write(&layout.installs_json(), &own)
        .context("could not save the list of installed Epic games")
}

/// `third-party-installed.json`: `[[appName, platform], ...]`.
pub(crate) fn read_third_party(layout: &Layout) -> Vec<(String, String)> {
    json_store::read_or_default::<Vec<(String, String)>>(&layout.third_party_json())
}

pub(crate) fn add_third_party(layout: &Layout, app: &str, platform: &str) -> Result<()> {
    let mut list = read_third_party(layout);
    if !list.iter().any(|(a, _)| a == app) {
        list.push((app.to_string(), platform.to_string()));
    }
    json_store::write(&layout.third_party_json(), &list)
}

pub(crate) fn remove_third_party(layout: &Layout, app: &str) -> Result<()> {
    let mut list = read_third_party(layout);
    let before = list.len();
    list.retain(|(a, _)| a != app);
    if list.len() != before {
        json_store::write(&layout.third_party_json(), &list)?;
    }
    Ok(())
}

pub(crate) fn read_library(layout: &Layout) -> Vec<GameRecord> {
    json_store::read::<Vec<GameRecord>>(&layout.library_json())
        .unwrap_or_else(|e| {
            tracing::error!("{e:#}");
            None
        })
        .unwrap_or_default()
}

pub(crate) fn write_library(layout: &Layout, list: &[GameRecord]) -> Result<()> {
    json_store::write_bytes(&layout.library_json(), &serde_json::to_vec(list)?)
}

pub(crate) fn read_session(layout: &Layout) -> Option<Session> {
    irori_secrets::load::<Session>(&layout.session_json()).filter(|s| !s.refresh_token.is_empty())
}

pub(crate) fn write_session(layout: &Layout, session: &Session) -> Result<()> {
    irori_secrets::write_json(&layout.session_json(), session)
}

/// The EOS overlay install.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct OverlayInstall {
    pub version: String,
    pub install_path: String,
}

/// Delete a file, ignoring a missing one.
pub(crate) fn remove_quietly(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("removing {}: {e}", path.display());
    }
}

/// Real Wine prefix directory (the one holding `user.reg`): `prefix/pfx` for
/// Proton prefixes, else `prefix` itself. `None` when not initialised.
pub(crate) fn real_prefix(prefix: &Path) -> Option<PathBuf> {
    let pfx = prefix.join("pfx");
    if pfx.join("user.reg").is_file() {
        Some(pfx)
    } else if prefix.join("user.reg").is_file() {
        Some(prefix.to_path_buf())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(dir: &Path) -> Layout {
        Layout {
            config: dir.into(),
            cache: dir.join("c"),
            tools: dir.join("t"),
        }
    }

    #[test]
    fn reads_legendary_style_records() {
        let text = r#"{
          "Sugar": {"app_name":"Sugar","base_urls":[],"can_run_offline":true,"egl_guid":"",
                    "executable":"Game.exe","install_path":"/g/Sugar","install_size":81446830080,
                    "install_tags":[],"is_dlc":false,"launch_parameters":"","manifest_path":null,
                    "needs_verification":false,"platform":"Windows",
                    "prereq_info":{"ids":["x"],"name":"UE","path":"Redist\\s.exe","args":"/q","installed":true},
                    "requires_ot":false,"save_path":null,"save_timestamp":1700000000.5,"title":"Sugar","version":"1.2"},
          "Mac1": {"install_size":1024.0,"platform":"Mac","version":"3"}
        }"#;
        let map: BTreeMap<String, InstalledGame> = serde_json::from_str(text).unwrap();
        let sugar = &map["Sugar"];
        assert_eq!(sugar.install_size, 81_446_830_080);
        assert_eq!(sugar.prereq.as_ref().unwrap().name, "UE");
        assert_eq!(sugar.save_timestamp, Some(1_700_000_000));
        let info = sugar.to_installed_info();
        assert_eq!(info.install_size, "75.9 GiB");
        assert_eq!(info.platform, InstallPlatform::Windows);
        assert_eq!(map["Mac1"].install_platform(), InstallPlatform::Mac);
    }

    #[test]
    fn round_trips_state() {
        let dir = tempfile::tempdir().unwrap();
        let l = layout(dir.path());
        let mut map = InstalledMap::new();
        map.insert(
            "A".into(),
            InstalledGame {
                app_name: "A".into(),
                manifest_secrets: [("K".to_string(), "00".to_string())].into(),
                ..Default::default()
            },
        );
        add_third_party(&l, "Ea", "Windows").unwrap();
        add_third_party(&l, "Ea", "Windows").unwrap();
        let mut all = read_installed(&l);
        assert!(all["Ea"].third_party);
        all.extend(map);
        write_installed(&l, &all).unwrap();
        let back = read_installed(&l);
        assert_eq!(back["A"].secrets()["K"], "00");
        assert!(back["Ea"].third_party);
        let raw: Value = json_store::read(&l.installs_json()).unwrap().unwrap();
        assert!(
            raw.get("Ea").is_none(),
            "third-party titles stay in their own list"
        );
        remove_third_party(&l, "Ea").unwrap();
        assert!(!read_installed(&l).contains_key("Ea"));

        assert!(read_session(&l).is_none());
        let s = Session {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 1,
            refresh_expires_at: 2,
            account_id: "acc".into(),
            display_name: "P".into(),
        };
        write_session(&l, &s).unwrap();
        assert_eq!(read_session(&l), Some(s));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(l.session_json())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let rec = GameRecord {
            app_name: "A".into(),
            ..Default::default()
        };
        write_library(&l, std::slice::from_ref(&rec)).unwrap();
        assert_eq!(read_library(&l), [rec]);
    }
}
