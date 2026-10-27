//! Reading scripts: YAML or JSON, the bare script or lutris.net's installer
//! object, into the typed [`Script`]. Anything unknown (a command, a task, a
//! parameter, a variable) refuses the whole script before anything runs.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::error::ScriptError;
use crate::model::*;

mod commands;

/// Runners Lutris ships; a section named after one holds that runner's
/// options.
const RUNNERS: &[&str] = &[
    "atari800",
    "azahar",
    "cemu",
    "dolphin",
    "dosbox",
    "duckstation",
    "easyrpg",
    "flatpak",
    "fsuae",
    "hatari",
    "json",
    "jzintv",
    "libretro",
    "linux",
    "mame",
    "mednafen",
    "mupen64plus",
    "o2em",
    "openmsx",
    "osmose",
    "pcsx2",
    "pico8",
    "redream",
    "reicast",
    "rpcs3",
    "ryujinx",
    "scummvm",
    "shadps4",
    "snes9x",
    "steam",
    "vice",
    "vita3k",
    "web",
    "wine",
    "xemu",
    "xenia",
    "yuzu",
    "zdoom",
];

/// Keys of the script itself (besides runner sections).
const SCRIPT_KEYS: &[&str] = &[
    "files",
    "installer",
    "game",
    "system",
    "requires",
    "extends",
    "variables",
    "install_complete_text",
    "require-binaries",
];

/// Descriptive keys of an installer (lutris.net's fields and local files').
const META_KEYS: &[&str] = &[
    "name",
    "game_slug",
    "version",
    "slug",
    "installer_slug",
    "runner",
    "description",
    "notes",
    "credits",
    "created_at",
    "updated_at",
    "user",
    "author",
    "year",
    "id",
    "game_id",
    "revision",
    "steamid",
    "gogid",
    "gogslug",
    "humbleid",
    "humblestoreid",
    "humblestoreid_real",
    "itchid",
    "service",
    "service_id",
    "is_dlc",
    "dlcid",
    "discord_id",
    "draft",
    "published",
    "published_by",
    "rating",
    "is_playable",
    "os",
    "installer_file",
    "content",
    "script",
];

impl Script {
    /// Parse one script from YAML or JSON. A file holding several scripts is
    /// refused with [`ScriptError::SeveralScripts`]; use [`Script::parse_all`].
    pub fn parse(text: &str) -> Result<Script, ScriptError> {
        let mut all = Script::parse_all(text)?;
        match all.len() {
            1 => Ok(all.remove(0)),
            0 => Err(ScriptError::NotAScript("the file is empty".into())),
            n => Err(ScriptError::SeveralScripts(n)),
        }
    }

    /// Parse every script in a document: one installer, a list of them, or
    /// lutris.net's `{results: [...]}`.
    pub fn parse_all(text: &str) -> Result<Vec<Script>, ScriptError> {
        let value = load(text)?;
        let docs = match value {
            Value::Array(items) => items,
            Value::Object(mut m) if m.get("results").is_some_and(Value::is_array) => {
                match m.remove("results") {
                    Some(Value::Array(items)) => items,
                    _ => Vec::new(),
                }
            }
            other => vec![other],
        };
        docs.iter().map(Script::from_value).collect()
    }

    /// Build a script from an already decoded installer object or bare
    /// script.
    pub fn from_value(value: &Value) -> Result<Script, ScriptError> {
        let Value::Object(top) = value else {
            return Err(ScriptError::NotAScript("expected a mapping".into()));
        };
        let script = parse_object(top)?;
        crate::validate::validate(&script)?;
        Ok(script)
    }
}

/// Decode YAML or JSON into a JSON value. YAML keys that are numbers or
/// booleans become strings.
pub(crate) fn load(text: &str) -> Result<Value, ScriptError> {
    let trimmed = text.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(v) = serde_json::from_str::<Value>(text)
    {
        return Ok(v);
    }
    let mut yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(text).map_err(|e| ScriptError::Syntax(e.to_string()))?;
    yaml.apply_merge()
        .map_err(|e| ScriptError::Syntax(e.to_string()))?;
    yaml_to_json(yaml)
}

fn yaml_to_json(v: serde_yaml_ng::Value) -> Result<Value, ScriptError> {
    use serde_yaml_ng::Value as Y;
    Ok(match v {
        Y::Null => Value::Null,
        Y::Bool(b) => Value::Bool(b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(u) = n.as_u64() {
                Value::from(u)
            } else {
                serde_json::Number::from_f64(n.as_f64().unwrap_or(0.0))
                    .map_or_else(|| Value::String(n.to_string()), Value::Number)
            }
        }
        Y::String(s) => Value::String(s),
        Y::Sequence(items) => Value::Array(
            items
                .into_iter()
                .map(yaml_to_json)
                .collect::<Result<_, _>>()?,
        ),
        Y::Mapping(m) => {
            let mut out = Map::new();
            for (k, v) in m {
                let key = match k {
                    Y::String(s) => s,
                    Y::Number(n) => n.to_string(),
                    Y::Bool(b) => b.to_string(),
                    Y::Null => "null".into(),
                    _ => return Err(ScriptError::Syntax("a mapping key is not text".into())),
                };
                out.insert(key, yaml_to_json(v)?);
            }
            Value::Object(out)
        }
        Y::Tagged(t) => yaml_to_json(t.value)?,
    })
}

/// A scalar as text (`version: 1.0` is still the string "1.0").
pub(crate) fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn meta(top: &Map<String, Value>, key: &str) -> Option<String> {
    top.get(key)
        .and_then(scalar)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_object(top: &Map<String, Value>) -> Result<Script, ScriptError> {
    // lutris.net returns the script as a mapping under `script` (and its
    // YAML source under `content`); local files may be either shape.
    let owned_script;
    let (body, envelope) = match top.get("script") {
        Some(Value::Object(m)) => (m, true),
        Some(Value::String(text)) => {
            owned_script = load(text)?;
            match &owned_script {
                Value::Object(m) => (m, true),
                _ => return Err(ScriptError::NotAScript("“script” is not a mapping".into())),
            }
        }
        Some(_) => return Err(ScriptError::NotAScript("“script” is not a mapping".into())),
        None => match top.get("content") {
            Some(Value::String(text)) => {
                owned_script = load(text)?;
                match &owned_script {
                    Value::Object(m) => (m, true),
                    _ => return Err(ScriptError::NotAScript("“content” is not a mapping".into())),
                }
            }
            _ => (top, false),
        },
    };

    let runner = meta(top, "runner")
        .or_else(|| meta(body, "runner"))
        .ok_or_else(|| missing("Script", "runner"))?;
    if !RUNNERS.contains(&runner.as_str()) {
        return Err(invalid(
            "Script",
            format!("“{runner}” is not a runner Irori knows"),
        ));
    }

    // Unknown keys in the script are refused: a misspelt `instaler:` must
    // not silently install nothing.
    for key in body.keys() {
        let known = SCRIPT_KEYS.contains(&key.as_str())
            || RUNNERS.contains(&key.as_str())
            || (!envelope && META_KEYS.contains(&key.as_str()));
        if !known {
            return Err(ScriptError::UnknownKey {
                location: "Script".into(),
                key: key.clone(),
            });
        }
    }

    let author = match top.get("user").or_else(|| top.get("author")) {
        Some(Value::Object(u)) => u.get("username").and_then(scalar),
        Some(v) => scalar(v),
        None => None,
    };
    let info = ScriptInfo {
        name: meta(top, "name")
            .or_else(|| meta(body, "name"))
            .ok_or_else(|| missing("Script", "name"))?,
        game_slug: meta(top, "game_slug")
            .or_else(|| meta(body, "game_slug"))
            .ok_or_else(|| missing("Script", "game_slug"))?,
        version: meta(top, "version").unwrap_or_default(),
        slug: meta(top, "slug"),
        installer_slug: meta(top, "installer_slug"),
        runner: runner.clone(),
        description: meta(top, "description"),
        notes: meta(top, "notes"),
        credits: meta(top, "credits"),
        created_at: meta(top, "created_at"),
        updated_at: meta(top, "updated_at"),
        author,
        year: meta(top, "year"),
        id: top.get("id").and_then(Value::as_u64),
        revision: meta(top, "revision"),
        steamid: meta(top, "steamid"),
        gogid: meta(top, "gogid"),
        service: meta(top, "service"),
        service_id: meta(top, "service_id"),
    };

    let mut variables = BTreeMap::new();
    if let Some(v) = body.get("variables") {
        let Value::Object(m) = v else {
            return Err(invalid("Variables", "expected a mapping".into()));
        };
        for (k, v) in m {
            let value =
                scalar(v).ok_or_else(|| invalid("Variables", format!("“{k}” is not text")))?;
            variables.insert(k.replace('-', "_"), value);
        }
    }

    let files = match body.get("files") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(parse_file).collect::<Result<_, _>>()?,
        Some(_) => return Err(invalid("Files", "expected a list".into())),
    };

    let steps = match body.get("installer") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, item)| commands::parse_step(i + 1, item, &info))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(invalid("Installer", "expected a list of steps".into())),
    };

    let game = match body.get("game") {
        None | Some(Value::Null) => GameSection::default(),
        Some(Value::Object(m)) => commands::parse_game(m)?,
        Some(_) => return Err(invalid("Game section", "expected a mapping".into())),
    };

    let (system, system_env) = match body.get("system") {
        None | Some(Value::Null) => (Map::new(), BTreeMap::new()),
        Some(Value::Object(m)) => {
            let env = match m.get("env") {
                None | Some(Value::Null) => BTreeMap::new(),
                Some(v) => string_map(v, "System section (env)")?,
            };
            (m.clone(), env)
        }
        Some(_) => return Err(invalid("System section", "expected a mapping".into())),
    };

    let mut runner_sections = BTreeMap::new();
    for (k, v) in body {
        if RUNNERS.contains(&k.as_str()) {
            match v {
                Value::Object(m) => {
                    runner_sections.insert(k.clone(), m.clone());
                }
                Value::Null => {}
                _ => {
                    return Err(invalid(
                        &format!("Section “{k}”"),
                        "expected a mapping".into(),
                    ));
                }
            }
        }
    }

    let requires = match body.get("requires") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => groups(v, "requires")?,
    };
    let extends = body
        .get("extends")
        .and_then(scalar)
        .filter(|s| !s.is_empty());
    if !requires.is_empty() && extends.is_some() {
        return Err(invalid(
            "Script",
            "“requires” and “extends” cannot be used together".into(),
        ));
    }
    let require_binaries = match body.get("require-binaries") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => groups(v, "require-binaries")?,
    };

    Ok(Script {
        info,
        files,
        steps,
        game,
        system_env,
        system,
        runner_sections,
        requires,
        extends,
        variables,
        require_binaries,
        install_complete_text: body.get("install_complete_text").and_then(scalar),
    })
}

fn missing(location: &str, key: &str) -> ScriptError {
    ScriptError::MissingKey {
        location: location.into(),
        key: key.into(),
    }
}

pub(crate) fn invalid(location: &str, message: String) -> ScriptError {
    ScriptError::Invalid {
        location: location.into(),
        message,
    }
}

/// `"a, b | c"` or a list: every comma-separated group is needed, `|`
/// separates alternatives.
fn groups(v: &Value, key: &str) -> Result<Vec<Vec<String>>, ScriptError> {
    let parts: Vec<String> = match v {
        Value::Array(items) => items
            .iter()
            .map(|i| scalar(i).ok_or_else(|| invalid(key, "expected text".into())))
            .collect::<Result<_, _>>()?,
        v => scalar(v)
            .ok_or_else(|| invalid(key, "expected text".into()))?
            .split(',')
            .map(str::to_string)
            .collect(),
    };
    Ok(parts
        .iter()
        .map(|g| {
            g.split('|')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|g| !g.is_empty())
        .collect())
}

fn string_map(v: &Value, location: &str) -> Result<BTreeMap<String, String>, ScriptError> {
    let Value::Object(m) = v else {
        return Err(invalid(location, "expected a mapping".into()));
    };
    m.iter()
        .map(|(k, v)| {
            let value = match v {
                Value::Null => String::new(),
                v => scalar(v).ok_or_else(|| invalid(location, format!("“{k}” is not text")))?,
            };
            Ok((k.clone(), value))
        })
        .collect()
}

fn parse_file(item: &Value) -> Result<FileSpec, ScriptError> {
    let Value::Object(m) = item else {
        return Err(invalid(
            "Files",
            "each file is a mapping of an id to a source".into(),
        ));
    };
    let mut entries = m.iter();
    let (Some((raw_id, spec)), None) = (entries.next(), entries.next()) else {
        return Err(invalid(
            "Files",
            "each file entry has exactly one id".into(),
        ));
    };
    let id = raw_id.trim().replace('-', "_");
    let location = format!("File “{id}”");
    let (url, filename, checksum, referer, size) = match spec {
        Value::Object(o) => {
            let mut p = Params::new(o, location.clone());
            let url = p.req_str("url")?;
            let filename = p.str("filename")?;
            let checksum = p.str("checksum")?;
            let referer = p.str("referer")?;
            let size = p.u64("size")?;
            let total = p.u64("total_size")?;
            p.ignore("alternate_filenames");
            p.finish()?;
            (url, filename, checksum, referer, size.or(total))
        }
        v => (
            scalar(v).ok_or_else(|| invalid(&location, "expected an address".into()))?,
            None,
            None,
            None,
            None,
        ),
    };
    let url = url.trim().to_string();
    let source = if let Some(rest) = url.strip_prefix("N/A") {
        let prompt = rest
            .strip_prefix(':')
            .map(str::trim)
            .filter(|s| !s.is_empty());
        FileSource::UserPick {
            prompt: prompt.map(str::to_string),
        }
    } else if let Some(rest) = url.strip_prefix("$STEAM:") {
        let (appid, path) = rest.split_once(':').unwrap_or((rest, "."));
        if appid.is_empty() || !appid.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid(
                &location,
                format!("“{url}” does not name a Steam app"),
            ));
        }
        FileSource::Steam {
            appid: appid.into(),
            path: path.into(),
        }
    } else {
        check_url(&id, &url)?;
        FileSource::Url(url.clone())
    };
    if let Some(name) = &filename {
        check_filename(&location, name)?;
    }
    let checksum = checksum.map(|c| parse_checksum(&id, &c)).transpose()?;
    Ok(FileSpec {
        id,
        source,
        filename,
        checksum,
        referer,
        size,
    })
}

/// Only http(s) downloads. Addresses with variables are checked again once
/// substituted.
pub(crate) fn check_url(id: &str, url: &str) -> Result<(), ScriptError> {
    let refuse = || ScriptError::UnsupportedUrl {
        id: id.into(),
        url: url.into(),
    };
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(refuse());
    }
    if !url.contains('$') {
        let parsed = url::Url::parse(url).map_err(|_| refuse())?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(refuse());
        }
    }
    Ok(())
}

pub(crate) fn check_filename(location: &str, name: &str) -> Result<(), ScriptError> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err(invalid(location, format!("“{name}” is not a file name")));
    }
    Ok(())
}

fn parse_checksum(id: &str, text: &str) -> Result<Checksum, ScriptError> {
    let bad = || {
        invalid(
            &format!("File “{id}”"),
            format!("“{text}” is not a checksum (use md5:, sha1:, sha256: or sha512:)"),
        )
    };
    let (alg, hex) = text.trim().split_once(':').ok_or_else(bad)?;
    let algorithm = match alg.trim().to_ascii_lowercase().as_str() {
        "md5" => HashAlgorithm::Md5,
        "sha1" => HashAlgorithm::Sha1,
        "sha256" => HashAlgorithm::Sha256,
        "sha512" => HashAlgorithm::Sha512,
        _ => return Err(bad()),
    };
    let hex = hex.trim().to_ascii_lowercase();
    if hex.len() != algorithm.hex_len() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    Ok(Checksum { algorithm, hex })
}

/// A command's parameters, tracking which keys were read so the rest can be
/// refused.
struct Params<'a> {
    map: Option<&'a Map<String, Value>>,
    location: String,
    used: BTreeSet<&'static str>,
}

impl<'a> Params<'a> {
    fn new(map: &'a Map<String, Value>, location: String) -> Self {
        Self {
            map: Some(map),
            location,
            used: BTreeSet::new(),
        }
    }

    fn of(value: &'a Value, location: String) -> Result<Self, ScriptError> {
        match value {
            Value::Object(m) => Ok(Self::new(m, location)),
            Value::Null => Ok(Self {
                map: None,
                location,
                used: BTreeSet::new(),
            }),
            _ => Err(invalid(
                &location,
                "expected a mapping of parameters".into(),
            )),
        }
    }

    fn raw(&mut self, key: &'static str) -> Option<&'a Value> {
        self.used.insert(key);
        self.map.and_then(|m| m.get(key)).filter(|v| !v.is_null())
    }

    fn ignore(&mut self, key: &'static str) {
        self.used.insert(key);
    }

    fn str(&mut self, key: &'static str) -> Result<Option<String>, ScriptError> {
        match self.raw(key) {
            None => Ok(None),
            Some(v) => scalar(v)
                .map(Some)
                .ok_or_else(|| invalid(&self.location, format!("“{key}” must be text"))),
        }
    }

    fn req_str(&mut self, key: &'static str) -> Result<String, ScriptError> {
        self.str(key)?.ok_or_else(|| missing(&self.location, key))
    }

    fn bool(&mut self, key: &'static str) -> Result<Option<bool>, ScriptError> {
        match self.raw(key) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(v) => match scalar(v).as_deref().map(str::to_ascii_lowercase).as_deref() {
                Some("true" | "yes" | "1") => Ok(Some(true)),
                Some("false" | "no" | "0") => Ok(Some(false)),
                _ => Err(invalid(
                    &self.location,
                    format!("“{key}” must be true or false"),
                )),
            },
        }
    }

    fn u64(&mut self, key: &'static str) -> Result<Option<u64>, ScriptError> {
        match self.raw(key) {
            None => Ok(None),
            Some(v) => scalar(v)
                .and_then(|s| s.trim().parse().ok())
                .map(Some)
                .ok_or_else(|| invalid(&self.location, format!("“{key}” must be a number"))),
        }
    }

    fn return_code(&mut self) -> Result<Option<i32>, ScriptError> {
        match self.raw("return_code") {
            None => Ok(None),
            Some(v) => scalar(v)
                .and_then(|s| s.trim().parse().ok())
                .map(Some)
                .ok_or_else(|| invalid(&self.location, "“return_code” must be a number".into())),
        }
    }

    fn map(&mut self, key: &'static str) -> Result<BTreeMap<String, String>, ScriptError> {
        match self.raw(key) {
            None => Ok(BTreeMap::new()),
            Some(v) => string_map(v, &format!("{} ({key})", self.location)),
        }
    }

    /// A list given as a list or as a space-separated string.
    fn words(&mut self, key: &'static str) -> Result<Vec<String>, ScriptError> {
        match self.raw(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|i| {
                    scalar(i)
                        .ok_or_else(|| invalid(&self.location, format!("“{key}” must be text")))
                })
                .collect(),
            Some(v) => Ok(split_words(&scalar(v).ok_or_else(|| {
                invalid(&self.location, format!("“{key}” must be text"))
            })?)),
        }
    }

    fn finish(self) -> Result<(), ScriptError> {
        if let Some(m) = self.map
            && let Some(key) = m.keys().find(|k| !self.used.contains(k.as_str()))
        {
            return Err(ScriptError::UnknownKey {
                location: self.location,
                key: key.clone(),
            });
        }
        Ok(())
    }
}

/// Split an argument string like a POSIX shell (quotes and backslashes),
/// the way Lutris's `shlex.split` does on every platform.
pub(crate) fn split_words(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => match chars.peek() {
                Some(&n) if matches!(n, '"' | '\\' | '$' | '`') => {
                    cur.push(n);
                    chars.next();
                }
                _ => cur.push('\\'),
            },
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_token = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    in_token = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if in_token {
                    out.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_token = true;
            }
        }
    }
    if in_token {
        out.push(cur);
    }
    out
}
