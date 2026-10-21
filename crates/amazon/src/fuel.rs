//! `fuel.json`, shipped in every install: the main executable and the
//! post-install steps (redistributables).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Fuel {
    #[serde(rename = "SchemaVersion", deserialize_with = "string_or_number")]
    pub schema_version: Option<String>,
    #[serde(rename = "Main")]
    pub main: Option<FuelMain>,
    #[serde(rename = "PostInstall")]
    pub post_install: Vec<FuelAction>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct FuelMain {
    #[serde(rename = "Command")]
    pub command: String,
    #[serde(rename = "Args", deserialize_with = "strings_or_null")]
    pub args: Vec<String>,
    #[serde(rename = "WorkingSubdirOverride")]
    pub working_subdir_override: Option<String>,
}

/// One post-install step. `ValidReturns`, `AlwaysRun` and `HideWindow` are
/// ignored, as in Heroic.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct FuelAction {
    #[serde(rename = "Command")]
    pub command: String,
    #[serde(rename = "Args", deserialize_with = "strings_or_null")]
    pub args: Vec<String>,
}

fn strings_or_null<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(items) => items
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    })
}

fn string_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => Some(s),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

/// Parse `fuel.json`, which is JSON5 in the wild (comments, trailing commas).
pub fn parse(text: &str) -> serde_json::Result<Fuel> {
    serde_json::from_str(&json5_to_json(text))
}

/// Read `<install_dir>/fuel.json`.
pub fn read(install_dir: &Path) -> Result<Fuel> {
    let path = install_dir.join("fuel.json");
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "The game files are incomplete: {} is missing",
            path.display()
        )
    })?;
    parse(&text).with_context(|| format!("{} is invalid", path.display()))
}

/// Resolve a Windows-style path from fuel.json against the install folder.
pub fn resolve(install_dir: &Path, rel: &str) -> PathBuf {
    let mut out = install_dir.to_path_buf();
    out.extend(
        rel.split(['\\', '/'])
            .filter(|p| !p.is_empty() && *p != "."),
    );
    out
}

/// What to start for a game.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchInfo {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
}

impl Fuel {
    /// The main executable, its arguments and working folder.
    pub fn launch_info(&self, install_dir: &Path) -> Result<LaunchInfo> {
        let main = self
            .main
            .as_ref()
            .filter(|m| !m.command.trim().is_empty())
            .context("fuel.json does not name the game's executable")?;
        let working_dir = match main.working_subdir_override.as_deref() {
            Some(sub) if !sub.trim().is_empty() => resolve(install_dir, sub),
            _ => install_dir.to_path_buf(),
        };
        Ok(LaunchInfo {
            executable: resolve(install_dir, &main.command),
            args: main.args.clone(),
            working_dir,
        })
    }
}

/// Strip `//` and `/* */` comments and trailing commas outside strings.
pub fn json5_to_json(text: &str) -> String {
    let text = text.trim_start_matches('\u{feff}');
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = in_string {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == q {
                in_string = None;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = Some(c);
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            ',' => {
                let rest: String = chars.clone().collect();
                let next = strip_leading_comments(&rest);
                if !(next.starts_with('}') || next.starts_with(']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn strip_leading_comments(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("//") {
            s = rest.find('\n').map(|i| &rest[i..]).unwrap_or("");
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.find("*/").map(|i| &rest[i + 2..]).unwrap_or("");
        } else {
            return s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json5_fuel() {
        let text = "\u{feff}{\n // comment\n \"SchemaVersion\": \"2\", /* block */\n \"Main\": {\"Command\": \"Bin\\\\Game.exe\", \"Args\": [\"-x\"], \"WorkingSubdirOverride\": \"Bin\"},\n \"PostInstall\": [\n  {\"Command\": \"__Installer\\\\vcredist.exe\", \"Args\": [\"/q\", \"//not-a-comment\",],},\n ],\n}";
        let fuel = parse(text).unwrap();
        assert_eq!(fuel.schema_version.as_deref(), Some("2"));
        assert_eq!(
            fuel.post_install,
            vec![FuelAction {
                command: "__Installer\\vcredist.exe".into(),
                args: vec!["/q".into(), "//not-a-comment".into()],
            }]
        );
        let info = fuel.launch_info(Path::new("/g")).unwrap();
        assert_eq!(info.executable, Path::new("/g/Bin/Game.exe"));
        assert_eq!(info.args, ["-x"]);
        assert_eq!(info.working_dir, Path::new("/g/Bin"));

        let other = parse(r#"{"SchemaVersion": 1, "Main": {"Command": "a.exe", "Args": null}, "PostInstall": [{"Command": "x.exe", "Args": null}]}"#).unwrap();
        assert_eq!(other.schema_version.as_deref(), Some("1"));
        assert!(other.post_install[0].args.is_empty());
        let info = other.launch_info(Path::new("/g")).unwrap();
        assert_eq!(info.working_dir, Path::new("/g"));
        assert!(Fuel::default().launch_info(Path::new("/g")).is_err());
    }
}
