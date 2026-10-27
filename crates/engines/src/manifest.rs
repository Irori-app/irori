//! Engine manifests: what an engine is, where it comes from on each OS, how
//! it starts a game and which options it offers. Manifests are data
//! (`assets/engines/<id>.json`, built in) plus optional user overrides in
//! `<config>/engines/<id>.json`, merged key by key over the built-in one.
//!
//! The launch template is a list of arguments. A plain string is one
//! argument after placeholder expansion (`{rom}`, `{rom_dir}`, `{opt.<id>}`);
//! `{"if": "<option>", "args": [...]}` adds its arguments only while a toggle
//! option is on (`"unless"` while it is off). Nothing else is evaluated: a
//! manifest cannot run code.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::host::{Host, Os};

/// How an engine is driven beyond its launch template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    /// One program that takes the game file on its command line.
    #[default]
    Standalone,
    /// RetroArch: a front end that loads one libretro core per system.
    Retroarch,
    /// DOSBox: mounts the game folder and runs its program.
    Dosbox,
    /// ScummVM: detects the game in its folder.
    Scummvm,
    /// MAME: runs a machine by its short name from a ROM folder.
    Mame,
    /// A converter (AnyPS5's relinker): turns the game into a program of
    /// this computer once, which then runs on its own.
    Relinker,
}

/// One way to get or find an engine, in priority order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Source {
    /// Releases on GitHub; `assets` maps a host key (`linux-x86_64`,
    /// `macos-aarch64`, `windows-x86_64`) to a regex matching the asset name.
    #[serde(rename_all = "camelCase")]
    Github {
        repo: String,
        assets: BTreeMap<String, String>,
        /// Regex of an asset holding SHA-256 sums, when the project
        /// publishes one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checksums: Option<String>,
        /// More assets of the same release a host needs (a program's
        /// runtime libraries), by host key. They unpack into the same
        /// folder; a release missing one is skipped.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extra_assets: BTreeMap<String, Vec<String>>,
    },
    /// The libretro buildbot: versions are the folders of `index` matching
    /// `version_pattern`; `assets` maps a host key to a path template under
    /// `index` with `{version}`.
    #[serde(rename_all = "camelCase")]
    Buildbot {
        index: String,
        version_pattern: String,
        assets: BTreeMap<String, String>,
    },
    /// A download page: `assets` maps a host key to a regex finding the
    /// download link in the page, with a `version` group.
    Page {
        url: String,
        assets: BTreeMap<String, String>,
        /// Prefix for links that are relative to the site.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
    /// An installed Flatpak (Linux). Detected, never installed.
    Flatpak { id: String },
    /// Program names searched on `PATH`.
    Path { names: Vec<String> },
    /// A macOS app bundle in `/Applications` or `~/Applications`.
    MacApp { bundle: String },
    /// Known Windows install locations (`%VAR%` expanded).
    Windows { paths: Vec<String> },
}

impl Source {
    /// Whether Irori downloads and manages this source.
    pub fn is_managed(&self) -> bool {
        matches!(
            self,
            Source::Github { .. } | Source::Buildbot { .. } | Source::Page { .. }
        )
    }

    /// The host-specific asset pattern of a managed source.
    pub fn asset_for(&self, host: Host) -> Option<&str> {
        let assets = match self {
            Source::Github { assets, .. }
            | Source::Buildbot { assets, .. }
            | Source::Page { assets, .. } => assets,
            _ => return None,
        };
        assets.get(&host.key()).map(String::as_str)
    }

    /// Patterns of the further assets `host` needs beside the main one.
    pub fn extras_for(&self, host: Host) -> &[String] {
        match self {
            Source::Github { extra_assets, .. } => extra_assets
                .get(&host.key())
                .map(Vec::as_slice)
                .unwrap_or_default(),
            _ => &[],
        }
    }
}

/// One element of a launch template.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Arg {
    Plain(String),
    When {
        #[serde(rename = "if")]
        when: String,
        args: Vec<Arg>,
    },
    Unless {
        unless: String,
        args: Vec<Arg>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LaunchTemplate {
    pub args: Vec<Arg>,
    /// Extra environment variables (placeholders expanded).
    pub env: BTreeMap<String, String>,
}

/// The kind of an engine option, mapped onto the settings form's kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptionKind {
    Toggle,
    Choice,
    Text,
}

/// When an option is shown. The only visibility rule an option has.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Visibility {
    /// Hosts (`linux`, `macos`, `windows`) the option applies on; empty for all.
    pub hosts: Vec<Os>,
    /// Shown only while this other option has this value.
    pub option: Option<String>,
    pub is: Option<Value>,
}

impl Visibility {
    /// Whether the option shows on `os` with the other options at `values`.
    pub fn visible(&self, os: Os, values: &BTreeMap<String, Value>) -> bool {
        if !self.hosts.is_empty() && !self.hosts.contains(&os) {
            return false;
        }
        match (&self.option, &self.is) {
            (Some(option), Some(is)) => values.get(option) == Some(is),
            _ => true,
        }
    }
}

/// One engine option. Its label and help are translation keys derived from
/// its id (`manage.engines.option.<id>.label` / `.help`), shared by every
/// engine offering the same option.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineOption {
    pub id: String,
    pub kind: OptionKind,
    pub default: Value,
    #[serde(default)]
    pub advanced: bool,
    /// Allowed values of a choice option; `""` means "the engine's default".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default)]
    pub visible_when: Visibility,
}

/// A libretro core offered through RetroArch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Core {
    pub id: String,
    pub name: String,
    pub license: String,
    pub systems: Vec<String>,
}

/// Program names to look for in a managed install, per OS. A name may end in
/// `.app` (a bundle) or contain one `*`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerOs<T> {
    pub linux: T,
    pub macos: T,
    pub windows: T,
}

impl<T> PerOs<T> {
    pub fn get(&self, os: Os) -> &T {
        match os {
            Os::Linux => &self.linux,
            Os::Macos => &self.macos,
            Os::Windows => &self.windows,
        }
    }
}

/// Everything Irori knows about one engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineManifest {
    pub id: String,
    pub name: String,
    /// SPDX id or a short description of the licence.
    pub license: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub kind: EngineKind,
    /// Systems the engine plays (ids from the systems catalog).
    pub systems: Vec<String>,
    pub sources: Vec<Source>,
    #[serde(default)]
    pub binaries: PerOs<Vec<String>>,
    #[serde(default)]
    pub launch: LaunchTemplate,
    #[serde(default)]
    pub options: Vec<EngineOption>,
    /// Where the engine looks for BIOS files, per OS (placeholders
    /// expanded). Irori links the user's files there before a launch.
    /// Engines configured by Irori (RetroArch) have none.
    #[serde(default)]
    pub bios_dir: PerOs<Option<String>>,
    /// A file whose presence beside the program makes the engine keep its
    /// settings in its own folder, per OS.
    #[serde(default)]
    pub portable_marker: PerOs<Option<String>>,
    /// The key or buttons that leave a game, shown once per engine.
    #[serde(default)]
    pub exit_hotkey: String,
    /// The engine is experimental: shown with a warning, never recommended.
    #[serde(default)]
    pub experimental: bool,
    /// x86-64 processor features the engine's games use without checking
    /// (`avx2`, `bmi2`, …). A game doesn't start on a processor lacking one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cpu_features: Vec<String>,
    /// Libretro cores (RetroArch only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cores: Vec<Core>,
}

impl EngineManifest {
    /// Parse a manifest and check the parts serde cannot.
    pub fn parse(json: &str) -> Result<Self> {
        let manifest: Self = serde_json::from_str(json).context("invalid engine manifest")?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            bail!(
                "engine id {:?} must be lower-case letters, digits and dashes",
                self.id
            );
        }
        let ids: Vec<&str> = self.options.iter().map(|o| o.id.as_str()).collect();
        for option in &self.options {
            if option.kind == OptionKind::Choice && option.choices.is_empty() {
                bail!("{}: choice option {} has no choices", self.id, option.id);
            }
            if let Some(other) = &option.visible_when.option
                && !ids.contains(&other.as_str())
            {
                bail!(
                    "{}: option {} depends on unknown option {other}",
                    self.id,
                    option.id
                );
            }
        }
        fn check(args: &[Arg], ids: &[&str], engine: &str) -> Result<()> {
            for arg in args {
                match arg {
                    Arg::Plain(_) => {}
                    Arg::When { when: id, args } | Arg::Unless { unless: id, args } => {
                        if !ids.contains(&id.as_str()) {
                            bail!("{engine}: launch template uses unknown option {id}");
                        }
                        check(args, ids, engine)?;
                    }
                }
            }
            Ok(())
        }
        check(&self.launch.args, &ids, &self.id)?;
        for source in &self.sources {
            let patterns: Vec<&String> = match source {
                Source::Github {
                    assets,
                    extra_assets,
                    ..
                } => assets
                    .values()
                    .chain(extra_assets.values().flatten())
                    .collect(),
                Source::Page { assets, .. } => assets.values().collect(),
                Source::Buildbot {
                    version_pattern, ..
                } => vec![version_pattern],
                _ => Vec::new(),
            };
            for pattern in patterns {
                regex::Regex::new(pattern)
                    .with_context(|| format!("{}: bad pattern {pattern}", self.id))?;
            }
        }
        Ok(())
    }

    pub fn option(&self, id: &str) -> Option<&EngineOption> {
        self.options.iter().find(|o| o.id == id)
    }

    /// Option values with defaults filled in for options not in `set`.
    pub fn option_values(&self, set: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
        self.options
            .iter()
            .map(|o| {
                let value = set
                    .get(&o.id)
                    .filter(|v| o.accepts(v))
                    .cloned()
                    .unwrap_or_else(|| o.default.clone());
                (o.id.clone(), value)
            })
            .collect()
    }

    /// Cores that play `system`, in manifest order (the first is the default).
    pub fn cores_for(&self, system: &str) -> Vec<&Core> {
        self.cores
            .iter()
            .filter(|c| c.systems.iter().any(|s| s == system))
            .collect()
    }

    pub fn core(&self, id: &str) -> Option<&Core> {
        self.cores.iter().find(|c| c.id == id)
    }

    /// Whether Irori can download this engine for `host`.
    pub fn downloadable_on(&self, host: Host) -> bool {
        self.sources.iter().any(|s| s.asset_for(host).is_some())
    }
}

impl EngineOption {
    /// Whether `value` is of the option's kind (and an allowed choice).
    pub fn accepts(&self, value: &Value) -> bool {
        match self.kind {
            OptionKind::Toggle => value.is_boolean(),
            OptionKind::Text => value.is_string(),
            OptionKind::Choice => value
                .as_str()
                .is_some_and(|v| self.choices.iter().any(|c| c == v)),
        }
    }
}

/// Merge `patch` into `base` key by key (RFC 7396): objects merge, `null`
/// removes a key, anything else replaces.
pub fn merge_patch(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            for (key, value) in patch {
                if value.is_null() {
                    base.remove(key);
                } else {
                    merge_patch(base.entry(key.clone()).or_insert(Value::Null), value);
                }
            }
        }
        (base, patch) => *base = patch.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn built_in_manifests_parse_and_validate() {
        for (id, json) in crate::BUILT_IN {
            let m = EngineManifest::parse(json).unwrap_or_else(|e| panic!("{id}: {e:#}"));
            assert_eq!(&m.id, id);
            assert!(!m.systems.is_empty(), "{id} plays no system");
            assert!(!m.exit_hotkey.is_empty(), "{id} has no exit hotkey");
        }
    }

    #[test]
    fn rejects_unknown_options_in_templates() {
        let json = json!({
            "id": "x", "name": "X", "license": "MIT", "systems": ["nes"], "sources": [],
            "launch": {"args": [{"if": "nope", "args": ["-f"]}]}
        });
        let e = EngineManifest::parse(&json.to_string()).unwrap_err();
        assert!(format!("{e:#}").contains("unknown option nope"));
    }

    #[test]
    fn option_values_fall_back_to_defaults_for_bad_values() {
        let m = EngineManifest::parse(crate::BUILT_IN[0].1).unwrap();
        let mut set = BTreeMap::new();
        set.insert("fullscreen".to_string(), json!("yes"));
        set.insert("video_driver".to_string(), json!("vulkan"));
        let values = m.option_values(&set);
        assert_eq!(values["fullscreen"], json!(true));
        assert_eq!(values["video_driver"], json!("vulkan"));
    }

    #[test]
    fn merge_patch_merges_objects_and_removes_nulls() {
        let mut base = json!({"a": 1, "b": {"c": 2, "d": 3}});
        merge_patch(&mut base, &json!({"b": {"c": null, "e": 4}, "f": [1]}));
        assert_eq!(base, json!({"a": 1, "b": {"d": 3, "e": 4}, "f": [1]}));
    }

    #[test]
    fn visibility_checks_host_and_other_option() {
        let v = Visibility {
            hosts: vec![Os::Linux],
            option: Some("fullscreen".into()),
            is: Some(json!(true)),
        };
        let mut values = BTreeMap::new();
        values.insert("fullscreen".to_string(), json!(true));
        assert!(v.visible(Os::Linux, &values));
        assert!(!v.visible(Os::Macos, &values));
        values.insert("fullscreen".to_string(), json!(false));
        assert!(!v.visible(Os::Linux, &values));
    }
}
