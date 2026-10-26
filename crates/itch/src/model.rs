//! What the itch.io API answers, as far as a launcher needs it. The API
//! writes snake_case keys; platforms come either as a `platforms` object
//! (`{"windows": "all", "linux": "amd64"}`) or, from older servers, as
//! `traits` (`["p_windows", "p_linux", "demo"]`). Both are read.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The account an API key belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: u64,
    pub username: String,
    pub display_name: String,
    pub url: String,
    pub cover_url: String,
}

impl User {
    /// The display name, else the username.
    pub fn name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}

/// Operating systems an upload or game page offers builds for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Platforms {
    pub windows: bool,
    pub linux: bool,
    pub osx: bool,
}

impl Platforms {
    pub fn any(self) -> bool {
        self.windows || self.linux || self.osx
    }

    pub fn has(self, os: Os) -> bool {
        match os {
            Os::Windows => self.windows,
            Os::Linux => self.linux,
            Os::Mac => self.osx,
        }
    }

    fn read(platforms: Option<&Value>, traits: &[String]) -> Self {
        let mut p = Platforms::default();
        if let Some(Value::Object(map)) = platforms {
            // Architectures ("all", "amd64") from the API; booleans from
            // Ember's own cache.
            let set = |key: &str| match map.get(key) {
                None | Some(Value::Null) => false,
                Some(Value::String(s)) => !s.is_empty(),
                Some(Value::Bool(b)) => *b,
                Some(_) => true,
            };
            p.windows = set("windows");
            p.linux = set("linux");
            p.osx = set("osx");
        }
        for t in traits {
            match t.as_str() {
                "p_windows" => p.windows = true,
                "p_linux" => p.linux = true,
                "p_osx" => p.osx = true,
                _ => {}
            }
        }
        p
    }
}

/// An operating system a build is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Windows,
    Linux,
    Mac,
}

/// A game page (or tool, comic…) on itch.io.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "RawGame")]
pub struct Game {
    pub id: u64,
    pub url: String,
    pub title: String,
    pub short_text: String,
    /// `default` (downloadable), `html`, `flash`, `unity`, `java`.
    #[serde(rename = "type")]
    pub kind: String,
    /// `game`, `tool`, `assets`, `game_mod`, `physical_game`, `soundtrack`,
    /// `other`, `comic`, `book`.
    pub classification: String,
    pub cover_url: String,
    pub still_cover_url: String,
    pub published_at: String,
    pub platforms: Platforms,
    pub user: Option<User>,
}

impl Game {
    /// Whether the page is something a launcher runs: a game or a tool.
    pub fn is_playable_kind(&self) -> bool {
        matches!(self.classification.as_str(), "" | "game" | "tool")
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawGame {
    id: u64,
    url: String,
    title: String,
    short_text: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    classification: String,
    cover_url: Option<String>,
    still_cover_url: Option<String>,
    published_at: Option<String>,
    platforms: Option<Value>,
    traits: Vec<String>,
    user: Option<User>,
}

impl From<RawGame> for Game {
    fn from(r: RawGame) -> Self {
        Self {
            platforms: Platforms::read(r.platforms.as_ref(), &r.traits),
            id: r.id,
            url: r.url,
            title: r.title,
            short_text: r.short_text.unwrap_or_default(),
            kind: r.kind,
            classification: r.classification,
            cover_url: r.cover_url.unwrap_or_default(),
            still_cover_url: r.still_cover_url.unwrap_or_default(),
            published_at: r.published_at.unwrap_or_default(),
            user: r.user,
        }
    }
}

/// A download key: the proof that the account owns a game.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OwnedKey {
    pub id: u64,
    pub game_id: u64,
    pub game: Option<Game>,
    pub created_at: String,
}

/// The newest build of a channel-backed upload (pushed with butler).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Build {
    pub id: u64,
    pub version: u64,
    pub user_version: String,
}

/// A downloadable file of a game page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "RawUpload")]
pub struct Upload {
    pub id: u64,
    /// `hosted`, `build` (pushed with butler) or `external`.
    pub storage: String,
    pub filename: String,
    pub display_name: String,
    /// Bytes (for builds, the size of the archive).
    pub size: u64,
    pub channel_name: String,
    pub build: Option<Build>,
    pub build_id: u64,
    /// `default` for game files; `soundtrack`, `book`, `video`,
    /// `documentation`, `mod`, `audio_assets`, `graphical_assets`,
    /// `sourcecode` and `other` for the rest.
    #[serde(rename = "type")]
    pub kind: String,
    pub preorder: bool,
    pub demo: bool,
    pub platforms: Platforms,
    pub md5_hash: String,
    pub updated_at: String,
}

impl Upload {
    /// A human name: the developer's label, else the file name.
    pub fn label(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.filename
        } else {
            &self.display_name
        }
    }

    /// What identifies the content: the build for butler channels, else
    /// the upload's last change. A different value means an update.
    pub fn revision(&self) -> String {
        match self.build.as_ref().map(|b| b.id).filter(|id| *id > 0) {
            Some(id) => format!("build:{id}"),
            None if self.build_id > 0 => format!("build:{}", self.build_id),
            None => format!("upload:{}:{}", self.id, self.updated_at),
        }
    }

    /// A version to show: the developer's version string, the build number
    /// or the upload date.
    pub fn version_label(&self) -> String {
        if let Some(b) = &self.build {
            if !b.user_version.trim().is_empty() {
                return b.user_version.trim().to_string();
            }
            if b.version > 0 {
                return format!("build {}", b.version);
            }
        }
        self.updated_at
            .split('T')
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUpload {
    id: u64,
    storage: String,
    filename: String,
    display_name: Option<String>,
    size: Option<u64>,
    channel_name: Option<String>,
    build: Option<Build>,
    build_id: Option<u64>,
    #[serde(rename = "type")]
    kind: String,
    preorder: bool,
    demo: bool,
    platforms: Option<Value>,
    traits: Vec<String>,
    md5_hash: Option<String>,
    updated_at: Option<String>,
}

impl From<RawUpload> for Upload {
    fn from(r: RawUpload) -> Self {
        Self {
            platforms: Platforms::read(r.platforms.as_ref(), &r.traits),
            demo: r.demo || r.traits.iter().any(|t| t == "demo"),
            preorder: r.preorder || r.traits.iter().any(|t| t == "preorder"),
            id: r.id,
            storage: r.storage,
            filename: r.filename,
            display_name: r.display_name.unwrap_or_default(),
            size: r.size.unwrap_or(0),
            channel_name: r.channel_name.unwrap_or_default(),
            build: r.build,
            build_id: r.build_id.unwrap_or(0),
            kind: r.kind,
            md5_hash: r.md5_hash.unwrap_or_default(),
            updated_at: r.updated_at.unwrap_or_default(),
        }
    }
}

/// The upload to install for `os`, if the game has one. Game files that
/// can be downloaded through the API and run on `os` qualify; among them a
/// full version beats a demo, a butler channel (which reports updates
/// precisely) beats a plain file, an archive beats a lone file, and an
/// upload for `os` alone beats one listed for several systems. Ties go to
/// the most recently changed upload.
pub fn best_upload(uploads: &[Upload], os: Os) -> Option<&Upload> {
    uploads
        .iter()
        .filter(|u| is_installable(u) && u.platforms.has(os))
        .max_by_key(|u| {
            let only_this = [Os::Windows, Os::Linux, Os::Mac]
                .iter()
                .filter(|o| u.platforms.has(**o))
                .count()
                == 1;
            (
                !u.demo,
                u.storage == "build",
                is_archive(&u.filename),
                only_this,
                u.updated_at.clone(),
            )
        })
}

/// Game files the API can serve (not soundtracks, pre-order placeholders
/// or links to other sites).
pub fn is_installable(u: &Upload) -> bool {
    matches!(u.kind.as_str(), "" | "default") && !u.preorder && u.storage != "external"
}

fn is_archive(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [".zip", ".tar.gz", ".tgz", ".tar.xz", ".tar.bz2", ".tar"]
        .iter()
        .any(|e| name.ends_with(e))
}

/// Uploads keyed by id, for lookups by an installed upload's id.
pub fn by_id(uploads: &[Upload]) -> BTreeMap<u64, &Upload> {
    uploads.iter().map(|u| (u.id, u)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(json: Value) -> Upload {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn reads_both_platform_spellings() {
        let new = upload(serde_json::json!({
            "id": 1, "storage": "hosted", "filename": "game.zip", "type": "default",
            "platforms": {"windows": "all", "linux": "amd64"}, "size": 10
        }));
        assert!(new.platforms.windows && new.platforms.linux && !new.platforms.osx);
        let old = upload(serde_json::json!({
            "id": 2, "storage": "build", "filename": "mac.zip", "type": "default",
            "traits": ["p_osx", "demo"], "build": {"id": 7, "version": 3, "user_version": "1.2"}
        }));
        assert!(old.platforms.osx && old.demo);
        assert_eq!(old.revision(), "build:7");
        assert_eq!(old.version_label(), "1.2");
        let game: Game = serde_json::from_value(serde_json::json!({
            "id": 5, "title": "Celeste Classic", "classification": "game", "type": "html",
            "traits": ["p_windows"], "short_text": null, "user": {"id": 1, "username": "maddy"}
        }))
        .unwrap();
        assert!(game.platforms.windows && game.is_playable_kind());
        assert_eq!(game.short_text, "");
        // What Ember caches reads back the same.
        let again: Game = serde_json::from_value(serde_json::to_value(&game).unwrap()).unwrap();
        assert_eq!(again, game);
        let cached: Upload = serde_json::from_value(serde_json::to_value(&old).unwrap()).unwrap();
        assert_eq!(cached, old);
        assert_eq!(game.user.unwrap().name(), "maddy");
    }

    #[test]
    fn picks_the_best_upload_per_system() {
        let uploads: Vec<Upload> = serde_json::from_value(serde_json::json!([
            {"id": 1, "storage": "hosted", "filename": "demo.zip", "type": "default",
             "demo": true, "platforms": {"windows": "all"}},
            {"id": 2, "storage": "hosted", "filename": "game-win.exe", "type": "default",
             "platforms": {"windows": "all"}, "updated_at": "2024-01-02T00:00:00Z"},
            {"id": 3, "storage": "hosted", "filename": "game-win.zip", "type": "default",
             "platforms": {"windows": "all"}, "updated_at": "2024-01-01T00:00:00Z"},
            {"id": 4, "storage": "hosted", "filename": "ost.zip", "type": "soundtrack",
             "platforms": {"linux": "all"}},
            {"id": 5, "storage": "external", "filename": "", "type": "default",
             "platforms": {"linux": "all"}},
            {"id": 6, "storage": "build", "filename": "linux.zip", "type": "default",
             "platforms": {"linux": "all", "windows": "all"}},
            {"id": 7, "storage": "hosted", "filename": "pre.zip", "type": "default",
             "preorder": true, "platforms": {"osx": "all"}}
        ]))
        .unwrap();
        assert_eq!(best_upload(&uploads, Os::Windows).unwrap().id, 6);
        let without_build: Vec<Upload> = uploads.iter().filter(|u| u.id != 6).cloned().collect();
        assert_eq!(best_upload(&without_build, Os::Windows).unwrap().id, 3);
        assert_eq!(best_upload(&uploads, Os::Linux).unwrap().id, 6);
        assert!(best_upload(&uploads, Os::Mac).is_none());
        assert_eq!(uploads[2].revision(), "upload:3:2024-01-01T00:00:00Z");
        assert_eq!(uploads[2].version_label(), "2024-01-01");
    }
}
