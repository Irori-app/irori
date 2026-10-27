//! Build summaries for the install dialog, Epic's selective-download
//! definitions, and launch options.

use std::fmt;
use std::marker::PhantomData;

use irori_core::{DlcInfo, InstallInfo, InstallPlatform, LaunchOption, SdlOption};
use irori_epic_api::Manifest;
use irori_epic_api::library::GameRecord;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::state::{InstalledMap, Prerequisites};

/// What the install dialog needs from a build's manifest (cached).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct BuildSummary {
    pub version: String,
    pub download_size: u64,
    pub disk_size: u64,
    pub install_tags: Vec<String>,
    pub launch_exe: String,
    pub prerequisites: Option<Prerequisites>,
}

impl BuildSummary {
    pub fn of(m: &Manifest) -> Self {
        Self {
            version: m.meta.build_version.clone(),
            download_size: m.download_size(&[]),
            disk_size: m.install_size(&[]),
            install_tags: m.install_tags(),
            launch_exe: m.meta.launch_exe.clone(),
            prerequisites: prerequisites_of(m),
        }
    }
}

/// The prerequisites a manifest declares.
pub(crate) fn prerequisites_of(m: &Manifest) -> Option<Prerequisites> {
    (!m.meta.prereq_ids.is_empty() || !m.meta.prereq_path.is_empty()).then(|| Prerequisites {
        ids: m.meta.prereq_ids.clone(),
        name: m.meta.prereq_name.clone(),
        path: m.meta.prereq_path.clone(),
        args: m.meta.prereq_args.clone(),
    })
}

/// Map a build summary to the install dialog's model.
pub(crate) fn to_install_info(
    build: &BuildSummary,
    game: &GameRecord,
    platform: InstallPlatform,
    dlcs: Vec<DlcInfo>,
    sdl: Vec<SdlOption>,
    resumable: bool,
) -> InstallInfo {
    InstallInfo {
        app_name: game.app_name.clone(),
        title: game.app_title.clone(),
        platform,
        download_size: build.download_size,
        disk_size: build.disk_size,
        version: Some(build.version.clone()).filter(|v| !v.is_empty()),
        dlcs,
        sdl,
        dependencies: build
            .prerequisites
            .as_ref()
            .filter(|p| !p.name.is_empty())
            .map(|p| vec![p.name.clone()])
            .unwrap_or_default(),
        is_native: platform == InstallPlatform::Mac && cfg!(target_os = "macos"),
        resumable,
        ..Default::default()
    }
}

/// DLCs of `game` the account owns with their own build on `platform`.
pub(crate) fn owned_dlcs<'a>(
    game: &GameRecord,
    records: impl IntoIterator<Item = &'a GameRecord>,
    platform: &str,
    installed: &InstalledMap,
) -> Vec<DlcInfo> {
    let mut out: Vec<DlcInfo> = records
        .into_iter()
        .filter(|r| {
            r.main_game_id() == Some(game.catalog_item_id()) && r.asset_infos.contains_key(platform)
        })
        .map(|r| DlcInfo {
            app_name: r.app_name.clone(),
            title: r.app_title.clone(),
            is_installed: installed.contains_key(&r.app_name),
            owned: true,
        })
        .collect();
    out.sort_by_key(|d| d.title.to_lowercase());
    out
}

/// A JSON object deserialized with its key order preserved.
pub(crate) struct Ordered<T>(pub Vec<(String, T)>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Ordered<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Ordered<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, T>()? {
                    out.push((k, v));
                }
                Ok(Ordered(out))
            }
        }
        d.deserialize_map(V(PhantomData))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SdlEntry {
    name: String,
    tags: Vec<String>,
}

/// Parse an SDL definition (`{key: {name, description, tags}}`). The
/// `__required` entry comes first and is mandatory; others are optional and
/// unselected, as in Heroic.
pub(crate) fn parse_sdl(text: &str) -> Vec<SdlOption> {
    let Ok(Ordered(entries)) = serde_json::from_str::<Ordered<SdlEntry>>(text) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(entries.len());
    for (key, e) in entries {
        let required = key == "__required";
        let opt = SdlOption {
            id: key,
            name: e.name,
            tags: e.tags,
            mandatory: required,
            default_selected: required,
        };
        if required {
            out.insert(0, opt);
        } else {
            out.push(opt);
        }
    }
    out
}

/// Launch options: the game's extra launch options plus installed DLCs that
/// are launched on their own (separate executable) or via extra arguments.
/// A "Default" entry is prepended when any option exists.
pub(crate) fn launch_options<'a>(
    game: &GameRecord,
    dlcs: impl IntoIterator<Item = &'a GameRecord>,
    installed: &InstalledMap,
) -> Vec<LaunchOption> {
    let mut out: Vec<LaunchOption> = game
        .launch_options()
        .into_iter()
        .map(|(name, parameters)| LaunchOption::Basic { name, parameters })
        .collect();
    for dlc in dlcs {
        let Some(entry) = installed.get(&dlc.app_name) else {
            continue;
        };
        if !entry.executable.is_empty() {
            out.push(LaunchOption::Dlc {
                dlc_app_name: dlc.app_name.clone(),
                dlc_title: dlc.app_title.clone(),
            });
        } else if let Some(params) = dlc.additional_command_line() {
            out.push(LaunchOption::Basic {
                name: dlc.app_title.clone(),
                parameters: params.to_string(),
            });
        }
    }
    let has_default = out.iter().any(|o| {
        matches!(o, LaunchOption::Basic { name, parameters } if name == "Default" && parameters.is_empty())
    });
    if !out.is_empty() && !has_default {
        out.insert(
            0,
            LaunchOption::Basic {
                name: "Default".into(),
                parameters: String::new(),
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record(app: &str, metadata: serde_json::Value) -> GameRecord {
        GameRecord {
            app_name: app.into(),
            app_title: metadata["title"].as_str().unwrap_or(app).into(),
            metadata,
            ..Default::default()
        }
    }

    #[test]
    fn maps_install_info_and_dlcs() {
        let game = record("Sugar", json!({"id": "base", "title": "Sugar"}));
        let mut dlc = record(
            "DlcExe",
            json!({"id": "d1", "title": "Exe DLC", "mainGameItem": {"id": "base"}}),
        );
        dlc.asset_infos.insert("Windows".into(), Default::default());
        let other = record(
            "Other",
            json!({"id": "o", "mainGameItem": {"id": "elsewhere"}}),
        );
        let installed: InstalledMap =
            serde_json::from_str(r#"{"DlcExe":{"executable":"dlc.exe"}}"#).unwrap();
        let dlcs = owned_dlcs(&game, [&dlc, &other], "Windows", &installed);
        assert_eq!(dlcs.len(), 1);
        assert!(dlcs[0].is_installed && dlcs[0].owned);
        assert!(owned_dlcs(&game, [&dlc], "Mac", &installed).is_empty());

        let build = BuildSummary {
            version: "1.0".into(),
            download_size: 1000,
            disk_size: 2000,
            prerequisites: Some(Prerequisites {
                name: "UE Prereqs".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let ii = to_install_info(&build, &game, InstallPlatform::Windows, dlcs, vec![], true);
        assert_eq!((ii.download_size, ii.disk_size), (1000, 2000));
        assert_eq!(ii.version.as_deref(), Some("1.0"));
        assert_eq!(ii.dependencies, vec!["UE Prereqs".to_string()]);
        assert!(ii.resumable);
    }

    #[test]
    fn builds_launch_options() {
        let game = record(
            "Sugar",
            json!({"customAttributes": {
                "extraLaunchOption_001_Name": {"value": "Safe mode"},
                "extraLaunchOption_001_Args": {"value": "-safe"}}}),
        );
        let exe = record("DlcExe", json!({"title": "Exe DLC"}));
        let args = record(
            "DlcArgs",
            json!({"title": "Args DLC", "customAttributes": {"AdditionalCommandLine": {"value": "-dlc"}}}),
        );
        let installed: InstalledMap = serde_json::from_str(
            r#"{"DlcExe":{"executable":"dlc.exe"},"DlcArgs":{"executable":""}}"#,
        )
        .unwrap();
        let opts = launch_options(&game, [&exe, &args], &installed);
        assert_eq!(
            opts,
            vec![
                LaunchOption::Basic {
                    name: "Default".into(),
                    parameters: String::new()
                },
                LaunchOption::Basic {
                    name: "Safe mode".into(),
                    parameters: "-safe".into()
                },
                LaunchOption::Dlc {
                    dlc_app_name: "DlcExe".into(),
                    dlc_title: "Exe DLC".into()
                },
                LaunchOption::Basic {
                    name: "Args DLC".into(),
                    parameters: "-dlc".into()
                },
            ]
        );
        let none = launch_options(&record("X", json!({})), [], &installed);
        assert!(none.is_empty());
    }

    #[test]
    fn parses_sdl_in_order() {
        let sdl = parse_sdl(
            r#"{"zlang": {"name": "German", "description": "", "tags": ["de"]},
                "__required": {"name": "Base", "description": "", "tags": ["", "base"]},
                "alang": {"name": "French", "tags": ["fr"]}}"#,
        );
        let names: Vec<_> = sdl.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Base", "German", "French"]);
        assert!(sdl[0].mandatory && sdl[0].default_selected);
        assert!(!sdl[1].default_selected);
        assert!(parse_sdl("<html>").is_empty());
    }
}
