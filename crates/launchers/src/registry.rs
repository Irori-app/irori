//! A reader for Wine's registry files (`system.reg`, `user.reg`).
//!
//! The format: `[Key\\Path] <time>` section headers with backslashes doubled,
//! then `"Name"="string"`, `@="default"`, `"Name"=dword:0000001f`,
//! `"Name"=str(2):"expand string"` and `"Name"=hex(…):…` values, where long hex
//! values continue on the next line after a trailing backslash. Strings use C
//! escapes, including `\xNNNN` for characters outside ASCII.
//!
//! Only string and DWORD values are kept; binary values are skipped. Key and
//! value names compare case-insensitively, as on Windows. `system.reg` holds
//! `HKEY_LOCAL_MACHINE` with key paths relative to it (`Software\\…`).

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;

/// Wine's `HKEY_LOCAL_MACHINE` file inside a prefix.
pub const SYSTEM_REG: &str = "system.reg";

/// A registry value Irori can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegValue {
    /// `REG_SZ` or `REG_EXPAND_SZ` (unexpanded).
    String(String),
    /// `REG_DWORD`.
    Dword(u32),
}

#[derive(Debug, Clone, Default)]
struct Key {
    /// The path as written, for listing subkeys.
    path: String,
    /// Lower-case name → value.
    values: HashMap<String, RegValue>,
}

/// One registry hive: keys by path, relative to the hive's root.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    /// Lower-case path → key.
    keys: HashMap<String, Key>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Reads a Wine registry file. Lines it doesn't understand are skipped, so
    /// this only fails when the file can't be read.
    pub fn load(path: &Path) -> anyhow::Result<Registry> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("Couldn't read the registry file {}", path.display()))?;
        Ok(Registry::parse(&String::from_utf8_lossy(&bytes)))
    }

    /// Parses the text of a Wine registry file.
    pub fn parse(text: &str) -> Registry {
        let mut registry = Registry::default();
        let mut current: Option<String> = None;
        let mut lines = text.lines();
        while let Some(line) = lines.next() {
            let mut line = line.trim_end_matches('\r').to_string();
            // A trailing backslash continues the value on the next line.
            while line.ends_with('\\') {
                line.pop();
                match lines.next() {
                    Some(next) => line.push_str(next.trim_end_matches('\r').trim_start()),
                    None => break,
                }
            }
            let line = line.trim_start();
            if let Some(header) = line.strip_prefix('[') {
                current = parse_key_header(header);
                if let Some(path) = &current {
                    registry.ensure_key(path);
                }
                continue;
            }
            let Some(path) = &current else { continue };
            if let Some((name, value)) = parse_value_line(line) {
                registry.insert(path, &name, value);
            }
        }
        registry
    }

    /// Sets a value, creating its key. For callers that read the registry
    /// some other way (the Windows API on Windows) and for tests.
    pub fn insert(&mut self, key: &str, name: &str, value: RegValue) {
        let key = self.ensure_key(key);
        key.values.insert(name.to_ascii_lowercase(), value);
    }

    fn ensure_key(&mut self, path: &str) -> &mut Key {
        let path = normalize(path);
        self.keys
            .entry(path.to_ascii_lowercase())
            .or_insert_with(|| Key {
                path,
                values: HashMap::new(),
            })
    }

    /// A value of a key; `name` is `""` for the key's default value.
    pub fn value(&self, key: &str, name: &str) -> Option<&RegValue> {
        self.keys
            .get(&normalize(key).to_ascii_lowercase())?
            .values
            .get(&name.to_ascii_lowercase())
    }

    /// A string value of a key.
    pub fn string(&self, key: &str, name: &str) -> Option<&str> {
        match self.value(key, name)? {
            RegValue::String(s) => Some(s),
            RegValue::Dword(_) => None,
        }
    }

    /// The names of a key's direct subkeys, as written, sorted.
    pub fn subkeys(&self, key: &str) -> Vec<String> {
        let parent = normalize(key).to_ascii_lowercase();
        let mut names: Vec<String> = self
            .keys
            .iter()
            .filter_map(|(lower, k)| {
                let rest = lower.strip_prefix(&parent)?.strip_prefix('\\')?;
                if rest.is_empty() || rest.contains('\\') {
                    return None;
                }
                Some(k.path[k.path.len() - rest.len()..].to_string())
            })
            .collect();
        names.sort_by_key(|n| n.to_ascii_lowercase());
        names
    }

    /// A string value under `HKEY_LOCAL_MACHINE`, looked up in both the
    /// 64-bit and the 32-bit (`Wow6432Node`) views, since a 32-bit launcher in
    /// a 64-bit prefix writes to the latter. `key` may start with
    /// `HKEY_LOCAL_MACHINE\` or `HKLM\`.
    pub fn machine_string(&self, key: &str, name: &str) -> Option<&str> {
        machine_views(key)
            .into_iter()
            .find_map(|view| self.string(&view, name))
    }

    /// The subkeys of a key under `HKEY_LOCAL_MACHINE`, from both views
    /// (see [`machine_string`](Self::machine_string)), without duplicates.
    pub fn machine_subkeys(&self, key: &str) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for view in machine_views(key) {
            for name in self.subkeys(&view) {
                if !names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                    names.push(name);
                }
            }
        }
        names.sort_by_key(|n| n.to_ascii_lowercase());
        names
    }

    /// Resolves a full value path such as
    /// `HKEY_LOCAL_MACHINE\SOFTWARE\Vendor\Game\InstallDir`, whose last
    /// component is the value name, under `HKEY_LOCAL_MACHINE`.
    pub fn machine_value_path(&self, value_path: &str) -> Option<&str> {
        let (key, name) = value_path.trim_end_matches('\\').rsplit_once('\\')?;
        self.machine_string(key, name)
    }

    /// Whether the registry holds no keys.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// Loads `HKEY_LOCAL_MACHINE` from a prefix's `system.reg`. A missing file
/// gives an empty registry; an unreadable one also adds a warning.
pub(crate) fn load_machine(prefix: &Path, warnings: &mut Vec<String>) -> Registry {
    let path = prefix.join(SYSTEM_REG);
    if !path.exists() {
        return Registry::new();
    }
    Registry::load(&path).unwrap_or_else(|e| {
        warnings.push(format!("{e:#}"));
        Registry::new()
    })
}

/// Trims a hive prefix and stray separators, and collapses doubled ones.
fn normalize(key: &str) -> String {
    let key = key.replace('/', "\\");
    let mut parts: Vec<&str> = key.split('\\').filter(|p| !p.is_empty()).collect();
    if let Some(first) = parts.first()
        && (first.eq_ignore_ascii_case("HKEY_LOCAL_MACHINE") || first.eq_ignore_ascii_case("HKLM"))
    {
        parts.remove(0);
    }
    parts.join("\\")
}

/// `key` and its twin in the other view: `Software\X` ↔ `Software\Wow6432Node\X`.
fn machine_views(key: &str) -> Vec<String> {
    let key = normalize(key);
    let mut parts: Vec<&str> = key.split('\\').collect();
    let mut views = vec![key.clone()];
    if parts.len() >= 2 && parts[0].eq_ignore_ascii_case("software") {
        if parts[1].eq_ignore_ascii_case("wow6432node") {
            parts.remove(1);
        } else {
            parts.insert(1, "Wow6432Node");
        }
        views.push(parts.join("\\"));
    }
    views
}

/// `Software\\Vendor\\Key] 1700000000` → `Software\Vendor\Key`.
fn parse_key_header(header: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = header.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            ']' => return Some(out),
            _ => out.push(c),
        }
    }
    None
}

/// `"Name"="value"` → `("Name", String("value"))`; unsupported types → `None`.
fn parse_value_line(line: &str) -> Option<(String, RegValue)> {
    let (name, rest) = if let Some(rest) = line.strip_prefix("@=") {
        (String::new(), rest)
    } else {
        let (name, rest) = parse_quoted(line.strip_prefix('"')?)?;
        (name, rest.strip_prefix('=')?)
    };
    let value = if let Some(s) = rest.strip_prefix('"') {
        RegValue::String(parse_quoted(s)?.0)
    } else if let Some(s) = rest.strip_prefix("str(2):\"") {
        RegValue::String(parse_quoted(s)?.0)
    } else {
        // Binary and other types (`hex:`, `hex(7):`, …) are skipped here.
        let hex = rest.strip_prefix("dword:")?;
        RegValue::Dword(u32::from_str_radix(hex.trim(), 16).ok()?)
    };
    Some((name, value))
}

/// Reads an escaped string up to its closing quote (the opening one already
/// consumed); returns it and the text after the quote.
fn parse_quoted(s: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &s[i + 1..])),
            '\\' => {
                let (_, e) = chars.next()?;
                match e {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'a' => out.push('\x07'),
                    'b' => out.push('\x08'),
                    'e' => out.push('\x1b'),
                    'f' => out.push('\x0c'),
                    'v' => out.push('\x0b'),
                    'x' => {
                        let mut code = 0u32;
                        let mut digits = 0;
                        while digits < 4 {
                            match chars.peek().and_then(|&(_, d)| d.to_digit(16)) {
                                Some(d) => {
                                    code = code * 16 + d;
                                    digits += 1;
                                    chars.next();
                                }
                                None => break,
                            }
                        }
                        out.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
                    }
                    '0'..='7' => {
                        let mut code = e.to_digit(8).unwrap_or(0);
                        for _ in 0..2 {
                            match chars.peek().and_then(|&(_, d)| d.to_digit(8)) {
                                Some(d) => {
                                    code = code * 8 + d;
                                    chars.next();
                                }
                                None => break,
                            }
                        }
                        if code != 0 {
                            out.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
                        }
                    }
                    other => out.push(other),
                }
            }
            _ => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"WINE REGISTRY Version 2
;; All keys relative to \\Machine

#arch=win64

[Software\\Wow6432Node\\Ubisoft\\Launcher\\Installs\\635] 1694012345
#time=1d9e0c7a1b2c3d4
"InstallDir"="C:/Program Files (x86)/Ubisoft/Ubisoft Game Launcher/games/Far Cry 5/"
"Language"=dword:00000409
"Blob"=hex:01,02,03,04,05,06,07,08,09,0a,0b,0c,0d,0e,0f,10,11,12,13,14,15,16,\
  17,18,19,1a
"Multi"=hex(7):41,00,00,00,\
  00,00
"Quoted"="He said \"hi\" in C:\\Games\\Caf\x00e9"
@="default value"

[Software\\Rockstar Games\\Grand Theft Auto V] 1694012346
"InstallFolder"="C:\\Program Files\\Rockstar Games\\Grand Theft Auto V"
"Path"=str(2):"%ProgramFiles%\\Rockstar Games"

[Software\\Rockstar Games\\Launcher] 1694012347
"InstallFolder"="C:\\Program Files\\Rockstar Games\\Launcher"
"#;

    #[test]
    fn reads_strings_and_dwords_and_skips_binary_values() {
        let reg = Registry::parse(SAMPLE);
        let key = r"Software\Wow6432Node\Ubisoft\Launcher\Installs\635";
        assert_eq!(
            reg.string(key, "InstallDir"),
            Some("C:/Program Files (x86)/Ubisoft/Ubisoft Game Launcher/games/Far Cry 5/")
        );
        assert_eq!(reg.value(key, "language"), Some(&RegValue::Dword(0x409)));
        assert_eq!(reg.value(key, "Blob"), None);
        assert_eq!(reg.value(key, "Multi"), None);
        assert_eq!(
            reg.string(key, "Quoted"),
            Some("He said \"hi\" in C:\\Games\\Café")
        );
        assert_eq!(reg.string(key, ""), Some("default value"));
        assert_eq!(
            reg.string(r"Software\Rockstar Games\Grand Theft Auto V", "Path"),
            Some(r"%ProgramFiles%\Rockstar Games")
        );
    }

    #[test]
    fn looks_up_both_machine_views_case_insensitively() {
        let reg = Registry::parse(SAMPLE);
        assert_eq!(
            reg.machine_string(
                r"HKEY_LOCAL_MACHINE\SOFTWARE\UBISOFT\Launcher\Installs\635",
                "installdir"
            ),
            Some("C:/Program Files (x86)/Ubisoft/Ubisoft Game Launcher/games/Far Cry 5/")
        );
        assert_eq!(
            reg.machine_value_path(
                r"HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Rockstar Games\Grand Theft Auto V\InstallFolder"
            ),
            Some(r"C:\Program Files\Rockstar Games\Grand Theft Auto V")
        );
        assert_eq!(
            reg.machine_subkeys(r"HKLM\SOFTWARE\Rockstar Games"),
            vec!["Grand Theft Auto V".to_string(), "Launcher".to_string()]
        );
        assert!(reg.machine_string(r"SOFTWARE\Nothing", "X").is_none());
    }

    #[test]
    fn ignores_garbage() {
        let reg = Registry::parse("not a registry\n\"x\"=\"orphan\"\n[unterminated\n\"y\"=\"z\"");
        assert!(reg.is_empty());
    }
}
