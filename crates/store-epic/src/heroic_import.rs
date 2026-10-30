//! Converting legendary's config directory (Heroic's
//! `legendaryConfig/legendary`, or the one earlier Irori builds kept for the
//! legendary helper) into Irori's Epic files.
//!
//! legendary keeps `user.json` (tokens), `installed.json`, `metadata/<app>.json`,
//! `manifests/<app>_<platform>_<version>.manifest`,
//! `third-party-installed.json` and `overlay_install.json`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context as _, Result};
use irori_core::json_store;
use irori_epic_api::library::GameRecord;
use irori_epic_api::{Asset, Session, Sidecar};
use serde_json::Value;

use crate::state::{self, InstalledGame, Layout, OverlayInstall};

/// What was carried over.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Imported {
    pub login: bool,
    pub installs: usize,
    pub records: usize,
}

fn parse_time(v: Option<&Value>) -> u64 {
    let Some(s) = v.and_then(Value::as_str) else {
        return 0;
    };
    // legendary writes `2024-01-01T00:00:00.000Z`.
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .map(|t| t.unix_timestamp().max(0) as u64)
        .unwrap_or(0)
}

fn session_of(v: &Value) -> Option<Session> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Session {
        access_token: s("access_token").unwrap_or_default(),
        refresh_token: s("refresh_token").filter(|t| !t.is_empty())?,
        expires_at: parse_time(v.get("expires_at")),
        refresh_expires_at: parse_time(v.get("refresh_expires_at")),
        account_id: s("account_id")?,
        display_name: s("displayName").unwrap_or_default(),
    })
}

/// A legendary `GameAsset` (snake_case) as an [`Asset`].
fn asset_of(v: &Value) -> Asset {
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Asset {
        app_name: s("app_name"),
        asset_id: s("asset_id"),
        build_version: s("build_version"),
        catalog_item_id: s("catalog_item_id"),
        label_name: s("label_name"),
        namespace: s("namespace"),
        metadata: v.get("metadata").cloned().unwrap_or(Value::Null),
        sidecar_rvn: v.get("sidecar_rev").and_then(Value::as_u64).unwrap_or(0),
    }
}

/// A legendary metadata file as a [`GameRecord`].
pub(crate) fn record_of(v: &Value) -> Option<GameRecord> {
    let app_name = v.get("app_name")?.as_str()?.to_string();
    let asset_infos: BTreeMap<String, Asset> = v
        .get("asset_infos")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, a)| (k.clone(), asset_of(a))).collect())
        .unwrap_or_default();
    Some(GameRecord {
        app_title: v
            .get("app_title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        app_name,
        asset_infos,
        metadata: v.get("metadata").cloned().unwrap_or(Value::Null),
        sidecar: v.get("sidecar").filter(|s| !s.is_null()).map(|s| Sidecar {
            config: s.get("config").cloned().unwrap_or(Value::Null),
            rev: s.get("rev").and_then(Value::as_u64).unwrap_or(0),
        }),
    })
}

fn clean_filename(s: &str) -> String {
    s.chars().filter(|c| !"<>:\"/\\|?*".contains(*c)).collect()
}

/// The stored manifest of an installed build.
fn find_manifest(src: &Path, game: &InstalledGame) -> Option<Vec<u8>> {
    let dir = src.join("manifests");
    [
        format!("{}_{}_{}", game.app_name, game.platform_arg(), game.version),
        format!("{}_{}", game.app_name, game.version),
        game.app_name.clone(),
    ]
    .iter()
    .find_map(|name| std::fs::read(dir.join(format!("{}.manifest", clean_filename(name)))).ok())
}

/// Convert legendary's state in `src` into `layout`. An existing Irori login
/// is kept; library records and installs are merged (Irori's win).
pub(crate) fn import_legendary_dir(src: &Path, layout: &Layout) -> Result<Imported> {
    let mut out = Imported::default();
    if !src.is_dir() {
        return Ok(out);
    }
    std::fs::create_dir_all(&layout.config)
        .with_context(|| format!("creating {}", layout.config.display()))?;

    if state::read_session(layout).is_none()
        && let Some(session) = json_store::read::<Value>(&src.join("user.json"))
            .ok()
            .flatten()
            .as_ref()
            .and_then(session_of)
    {
        state::write_session(layout, &session)?;
        out.login = true;
    }

    let mut records: HashMap<String, GameRecord> = state::read_library(layout)
        .into_iter()
        .map(|r| (r.app_name.clone(), r))
        .collect();
    if let Ok(dir) = std::fs::read_dir(src.join("metadata")) {
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(rec) = json_store::read::<Value>(&path)
                .ok()
                .flatten()
                .as_ref()
                .and_then(record_of)
            else {
                tracing::warn!("skipping unreadable Epic metadata {}", path.display());
                continue;
            };
            if !records.contains_key(&rec.app_name) {
                records.insert(rec.app_name.clone(), rec);
                out.records += 1;
            }
        }
    }
    if out.records > 0 {
        let mut list: Vec<GameRecord> = records.into_values().collect();
        list.sort_by(|a, b| a.app_name.cmp(&b.app_name));
        state::write_library(layout, &list)?;
    }

    let theirs: BTreeMap<String, InstalledGame> = json_store::read(&src.join("installed.json"))
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut installed = state::read_installed(layout);
    for (app, mut game) in theirs {
        if installed.contains_key(&app) || game.install_path.is_empty() {
            continue;
        }
        if game.app_name.is_empty() {
            game.app_name = app.clone();
        }
        if let Some(bytes) = find_manifest(src, &game) {
            json_store::write_bytes(&layout.build_manifest(&app), &bytes)?;
        }
        installed.insert(app, game);
        out.installs += 1;
    }
    if out.installs > 0 {
        state::write_installed(layout, &installed)?;
    }

    for (app, platform) in json_store::read_or_default::<Vec<(String, String)>>(
        &src.join("third-party-installed.json"),
    ) {
        state::add_third_party(layout, &app, &platform)?;
    }

    if !layout.overlay_json().exists()
        && let Some(v) = json_store::read::<Value>(&src.join("overlay_install.json"))
            .ok()
            .flatten()
    {
        let overlay = OverlayInstall {
            version: v
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            install_path: v
                .get("install_path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
        };
        if Path::new(&overlay.install_path).is_dir() {
            json_store::write(&layout.overlay_json(), &overlay)?;
        }
    }
    Ok(out)
}

/// legendary's own files in a config directory (removed after converting
/// the directory Irori used for the legendary helper).
pub(crate) const LEGENDARY_FILES: &[&str] = &[
    "user.json",
    "installed.json",
    "assets.json",
    "aliases.json",
    "config.ini",
    "version.json",
    "overlay_install.json",
    "overlay_version.json",
    "metadata",
    "manifests",
    "tmp",
];

impl crate::EpicStore {
    /// Convert the legendary files an earlier Irori build kept in the config
    /// directory, then move them into `legendary-backup/`.
    pub(crate) async fn migrate_legendary_dir(&self) {
        let dir = self.layout.config.clone();
        // Backups of earlier builds hold the old login in the clear.
        let _ = tokio::fs::remove_file(dir.join("legendary-backup/user.json")).await;
        if !dir.join("user.json").is_file() && !dir.join("installed.json").is_file() {
            return;
        }
        let layout = self.layout.clone();
        let res = tokio::task::spawn_blocking(move || -> Result<Imported> {
            let imported = import_legendary_dir(&layout.config, &layout)?;
            let backup = layout.config.join("legendary-backup");
            std::fs::create_dir_all(&backup)?;
            for name in LEGENDARY_FILES {
                let p = layout.config.join(name);
                if p.exists() {
                    let to = backup.join(name);
                    if to.is_dir() {
                        std::fs::remove_dir_all(&to)?;
                    }
                    std::fs::rename(&p, &to)?;
                }
            }
            state::remove_quietly(&backup.join("user.json"));
            state::remove_quietly(&layout.cache.join("catalog.json"));
            Ok(imported)
        })
        .await;
        match res {
            Ok(Ok(i)) => tracing::info!(
                "moved the Epic login ({}), {} installs and {} library entries to Irori's format",
                i.login,
                i.installs,
                i.records
            ),
            Ok(Err(e)) => tracing::warn!("converting legendary's Epic data: {e:#}"),
            Err(e) => tracing::warn!("converting legendary's Epic data: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_legendary_state() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("legendary");
        std::fs::create_dir_all(src.join("metadata")).unwrap();
        std::fs::create_dir_all(src.join("manifests")).unwrap();
        std::fs::write(
            src.join("user.json"),
            r#"{"access_token":"a","refresh_token":"r","expires_at":"2030-01-01T00:00:00.000Z",
                "refresh_expires_at":"2030-02-01T00:00:00.000Z","account_id":"acc","displayName":"Player"}"#,
        )
        .unwrap();
        std::fs::write(
            src.join("metadata/Sugar.json"),
            r#"{"app_name":"Sugar","app_title":"Sugar","base_urls":[],
                "asset_infos":{"Windows":{"app_name":"Sugar","build_version":"2","catalog_item_id":"c",
                               "label_name":"Live-Windows","namespace":"ns","sidecar_rev":3}},
                "metadata":{"id":"c","namespace":"ns","title":"Sugar"},
                "sidecar":{"config":{"deploymentId":"d"},"rev":3}}"#,
        )
        .unwrap();
        std::fs::write(src.join("metadata/broken.json"), b"{").unwrap();
        std::fs::write(
            src.join("installed.json"),
            r#"{"Sugar":{"app_name":"Sugar","install_path":"/g/Sugar","platform":"Windows",
                         "version":"2","install_size":10,"executable":"Game.exe"}}"#,
        )
        .unwrap();
        std::fs::write(src.join("manifests/Sugar_Windows_2.manifest"), b"m").unwrap();
        std::fs::write(
            src.join("third-party-installed.json"),
            r#"[["Ea","Windows"]]"#,
        )
        .unwrap();

        let layout = Layout {
            config: tmp.path().join("irori"),
            cache: tmp.path().join("cache"),
            tools: tmp.path().join("tools"),
        };
        let imported = import_legendary_dir(&src, &layout).unwrap();
        assert_eq!(
            imported,
            Imported {
                login: true,
                installs: 1,
                records: 1
            }
        );
        let s = state::read_session(&layout).unwrap();
        assert_eq!(
            (s.account_id.as_str(), s.expires_at),
            ("acc", 1_893_456_000)
        );
        let lib = state::read_library(&layout);
        assert_eq!(lib[0].version("Windows"), Some("2"));
        assert_eq!(lib[0].asset_infos["Windows"].sidecar_rvn, 3);
        assert_eq!(lib[0].deployment_id(), Some("d"));
        let installed = state::read_installed(&layout);
        assert_eq!(installed["Sugar"].executable, "Game.exe");
        assert!(installed["Ea"].third_party);
        assert_eq!(std::fs::read(layout.build_manifest("Sugar")).unwrap(), b"m");

        // Running again changes nothing.
        assert_eq!(
            import_legendary_dir(&src, &layout).unwrap(),
            Imported::default()
        );
        assert_eq!(
            import_legendary_dir(&tmp.path().join("none"), &layout).unwrap(),
            Imported::default()
        );
    }
}
