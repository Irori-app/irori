//! Files inside a GOG install (`goggame-<id>.info`) and build metas.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use irori_core::InstallPlatform;

use crate::api::id_string;

/// A launchable task from `goggame-<id>.info`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlayTask {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: Option<String>,
    pub path: Option<String>,
    pub arguments: Option<Value>,
    pub working_dir: Option<String>,
    pub is_primary: bool,
    pub is_hidden: bool,
    pub category: Option<String>,
    pub link: Option<String>,
}

/// `goggame-<id>.info`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GameInfoFile {
    #[serde(deserialize_with = "de_id")]
    pub game_id: String,
    #[serde(deserialize_with = "de_id")]
    pub build_id: String,
    #[serde(deserialize_with = "de_id")]
    pub client_id: String,
    pub name: String,
    pub language: Option<String>,
    pub play_tasks: Vec<PlayTask>,
}

fn de_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(id_string(&Value::deserialize(d)?))
}

/// Directory holding the `goggame-*` files for a platform.
pub fn info_dir(install_path: &Path, platform: InstallPlatform) -> PathBuf {
    if platform == InstallPlatform::Mac {
        install_path.join("Contents").join("Resources")
    } else {
        install_path.to_path_buf()
    }
}

/// Parse `goggame-<app>.info`, falling back to `goggame-<app>.id` for the build id.
pub fn read_info_file(
    install_path: &Path,
    app_name: &str,
    platform: InstallPlatform,
) -> Option<GameInfoFile> {
    let dir = info_dir(install_path, platform);
    let raw = std::fs::read(dir.join(format!("goggame-{app_name}.info"))).ok()?;
    let mut info: GameInfoFile = serde_json::from_slice(&raw).ok()?;
    if info.build_id.is_empty()
        && let Ok(id_raw) = std::fs::read(dir.join(format!("goggame-{app_name}.id")))
        && let Ok(v) = serde_json::from_slice::<Value>(&id_raw)
    {
        info.build_id = v.get("buildId").map(id_string).unwrap_or_default();
    }
    Some(info)
}

/// DLC ids present in an install (every `goggame-<id>.info` except the base game), sorted.
pub fn installed_dlc_ids(
    install_path: &Path,
    app_name: &str,
    platform: InstallPlatform,
) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(info_dir(install_path, platform)) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name
                .strip_prefix("goggame-")?
                .strip_suffix(".info")?
                .to_string();
            (id.chars().all(|c| c.is_ascii_digit()) && !id.is_empty() && id != app_name)
                .then_some(id)
        })
        .collect();
    ids.sort();
    ids
}

/// Redistributables a build meta depends on (v1 depot `redist`, v2 `dependencies`).
pub fn manifest_dependencies(m: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let version = m.get("version").and_then(Value::as_u64).unwrap_or(2);
    let iter: Box<dyn Iterator<Item = &Value>> = if version == 1 {
        Box::new(
            m.pointer("/product/depots")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|d| d.get("redist")),
        )
    } else {
        Box::new(
            m.get("dependencies")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
    };
    for v in iter {
        if let Some(s) = v.as_str()
            && !out.iter().any(|o| o == s)
        {
            out.push(s.to_string());
        }
    }
    out
}

/// Galaxy client id from a v2 build meta.
pub fn manifest_client_id(m: &Value) -> Option<String> {
    m.get("clientId").map(id_string).filter(|s| !s.is_empty())
}

/// English name of a language code (`en-US` → `English`), as setup scripts expect.
pub fn language_name(code: &str) -> Option<&'static str> {
    let base = code.split(['-', '_']).next()?.to_ascii_lowercase();
    Some(match base.as_str() {
        "en" => "English",
        "de" => "German",
        "fr" => "French",
        "es" => "Spanish",
        "it" => "Italian",
        "pl" => "Polish",
        "ru" => "Russian",
        "zh" => "Chinese",
        "ja" => "Japanese",
        "ko" => "Korean",
        "pt" => "Portuguese",
        "cs" => "Czech",
        "hu" => "Hungarian",
        "nl" => "Dutch",
        "tr" => "Turkish",
        "sv" => "Swedish",
        "da" => "Danish",
        "fi" => "Finnish",
        "no" => "Norwegian",
        "nb" => "Norwegian Bokmål",
        "uk" => "Ukrainian",
        "ar" => "Arabic",
        "ro" => "Romanian",
        "el" => "Greek",
        "bg" => "Bulgarian",
        "th" => "Thai",
        "vi" => "Vietnamese",
        "he" => "Hebrew",
        "sk" => "Slovak",
        "sr" => "Serbian",
        "hr" => "Croatian",
        "id" => "Indonesian",
        "fa" => "Persian",
        "et" => "Estonian",
        "lv" => "Latvian",
        "lt" => "Lithuanian",
        "sl" => "Slovenian",
        "ca" => "Catalan",
        "is" => "Icelandic",
        "be" => "Belarusian",
        "hi" => "Hindi",
        "ms" => "Malay",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_info_file_and_dlcs() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("goggame-1.info"),
            r#"{"gameId":"1","rootGameId":"1","clientId":"55","name":"Game","language":"English",
               "playTasks":[{"type":"FileTask","path":"game.exe","isPrimary":true,"name":"Game"},
                            {"type":"URLTask","link":"https://x","category":"document"}]}"#,
        )
        .unwrap();
        std::fs::write(d.path().join("goggame-1.id"), r#"{"buildId": 77}"#).unwrap();
        std::fs::write(d.path().join("goggame-3.info"), "{}").unwrap();
        std::fs::write(d.path().join("goggame-2.info"), "{}").unwrap();
        let info = read_info_file(d.path(), "1", InstallPlatform::Windows).unwrap();
        assert_eq!(info.client_id, "55");
        assert_eq!(info.build_id, "77");
        assert_eq!(info.play_tasks.len(), 2);
        assert!(info.play_tasks[0].is_primary);
        assert_eq!(
            installed_dlc_ids(d.path(), "1", InstallPlatform::Windows),
            vec!["2", "3"]
        );
    }

    #[test]
    fn manifest_dependencies_v1_v2() {
        let v2 = serde_json::json!({"version": 2, "clientId": "53652320436400298",
            "dependencies": ["MSVC2012_x64", "MSVC2015", "MSVC2015"]});
        assert_eq!(manifest_dependencies(&v2), vec!["MSVC2012_x64", "MSVC2015"]);
        assert_eq!(
            manifest_client_id(&v2).as_deref(),
            Some("53652320436400298")
        );
        let v1 = serde_json::json!({"version": 1, "product": {"depots": [
            {"redist": "DOTNET4"}, {"languages": ["en"]}, {"redist": "DOTNET4"}]}});
        assert_eq!(manifest_dependencies(&v1), vec!["DOTNET4"]);
    }

    #[test]
    fn language_names() {
        assert_eq!(language_name("en-US"), Some("English"));
        assert_eq!(language_name("zh-Hans"), Some("Chinese"));
        assert_eq!(language_name("xx"), None);
    }
}
