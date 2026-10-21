//! What Ember records about an installed build (`builds/<app>.json`): the
//! build meta it was installed from and the selection, so updates download
//! only what changed and repairs know every file.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use ember_gog_api::content::BuildMeta;
use ember_gog_api::plan::{LinuxFiles, Selection};

use crate::manifest;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildRecord {
    /// `windows`, `osx` or `linux`.
    pub platform: String,
    pub build_id: String,
    /// ETag of the build meta when it was installed.
    pub etag: Option<String>,
    /// Canonical language code.
    pub language: String,
    pub dlcs: Vec<String>,
    /// The raw build meta (`Null` for Linux installers).
    pub meta: Value,
    /// Files unpacked from Linux installers.
    pub linux: Option<LinuxFiles>,
}

impl BuildRecord {
    pub fn parsed_meta(&self) -> Option<BuildMeta> {
        (!self.meta.is_null())
            .then(|| BuildMeta::parse(self.meta.clone()).ok())
            .flatten()
    }

    pub fn selection(&self) -> Selection {
        Selection {
            language: self.language.clone(),
            dlcs: self.dlcs.clone(),
            os: self.platform.clone(),
        }
    }

    /// Redistributables the build depends on.
    pub fn dependencies(&self) -> Vec<String> {
        manifest::manifest_dependencies(&self.meta)
    }

    /// The game's own Galaxy client id.
    pub fn client_id(&self) -> Option<String> {
        manifest::manifest_client_id(&self.meta)
    }
}

pub fn read(path: &Path) -> Option<BuildRecord> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Every build record in `dir`.
pub fn read_all(dir: &Path) -> Vec<BuildRecord> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read(&e.path()))
        .collect()
}

/// Convert a gogdl manifest (build meta plus `HGL*` keys) into a record.
pub fn from_gogdl_manifest(mut m: Value) -> Option<BuildRecord> {
    let map = m.as_object_mut()?;
    let platform = map
        .remove("HGLPlatform")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "windows".into());
    let language = map
        .remove("HGLInstallLanguage")
        .and_then(|v| v.as_str().map(str::to_string))
        .and_then(|l| ember_gog_api::languages::code(&l).map(str::to_string))
        .unwrap_or_else(|| "en-US".into());
    let dlcs = map
        .remove("HGLdlcs")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|d| ember_gog_api::content::id_string(d.get("id").unwrap_or(d)))
        .filter(|s| !s.is_empty())
        .collect();
    map.retain(|k, _| !k.starts_with("HGL"));
    let meta = BuildMeta::parse(m.clone()).ok()?;
    let build_id = if meta.generation == 1 {
        m.pointer("/product/timestamp")
            .map(ember_gog_api::content::id_string)
            .unwrap_or_default()
    } else {
        m.get("buildId")
            .map(ember_gog_api::content::id_string)
            .unwrap_or_default()
    };
    Some(BuildRecord {
        platform,
        build_id,
        etag: None,
        language,
        dlcs,
        meta: m,
        linux: None,
    })
}

/// Convert gogdl's `.gogdl-linux-manifest` (`{"files": [{file_name, crc32}]}`).
pub fn from_gogdl_linux_manifest(v: &Value) -> Option<LinuxFiles> {
    let files = v.get("files")?.as_array()?;
    let mut out = LinuxFiles::default();
    for f in files {
        let name = f.get("file_name").and_then(Value::as_str)?;
        let Some(rel) = name.strip_prefix(ember_gog_api::linux::DATA_PREFIX) else {
            continue;
        };
        let crc = f.get("crc32").and_then(Value::as_u64).unwrap_or(0) as u32;
        if let Ok(rel) = ember_depot::path::normalize(rel) {
            out.files.insert(rel, crc);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn converts_gogdl_manifests() {
        let m = json!({"version": 2, "baseProductId": "1", "buildId": "77", "clientId": "55",
            "dependencies": ["MSVC2015"], "HGLPlatform": "windows", "HGLInstallLanguage": "German",
            "HGLdlcs": [{"id": 2}, {"id": "3"}], "depots": [], "products": []});
        let r = from_gogdl_manifest(m).unwrap();
        assert_eq!(r.language, "de-DE");
        assert_eq!(r.dlcs, ["2", "3"]);
        assert_eq!(r.build_id, "77");
        assert_eq!(r.dependencies(), ["MSVC2015"]);
        assert_eq!(r.client_id().as_deref(), Some("55"));
        assert!(r.meta.get("HGLdlcs").is_none());
        assert_eq!(r.parsed_meta().unwrap().base_product, "1");

        let l = from_gogdl_linux_manifest(&json!({"files": [
            {"file_name": "data/noarch/game/run", "crc32": 5},
            {"file_name": "scripts/x", "crc32": 1}]}))
        .unwrap();
        assert_eq!(l.files.len(), 1);
        assert_eq!(l.files["game/run"], 5);
    }
}
