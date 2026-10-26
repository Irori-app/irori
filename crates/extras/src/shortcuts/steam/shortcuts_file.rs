//! `shortcuts.vdf` on top of the generic binary VDF codec: validation,
//! entry access, adding/removing entries, and Steam's non-Steam app IDs.

use anyhow::{Result, bail};

use super::vdf::{self, Map, Value};

/// Parsed `shortcuts.vdf`. The root map is kept whole, so unknown keys,
/// value types and order survive a rewrite.
#[derive(Debug, Clone, PartialEq)]
pub struct ShortcutsFile {
    root: Map,
}

impl Default for ShortcutsFile {
    fn default() -> Self {
        Self::empty()
    }
}

impl ShortcutsFile {
    /// A file with no shortcuts (`00 "shortcuts" 00 08 08`).
    pub fn empty() -> Self {
        Self {
            root: Map::new().with("shortcuts", Value::Map(Map::new())),
        }
    }

    /// Parse and validate. Validation failures carry Heroic's messages, so a
    /// corrupt file can be reported per Steam user.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let file = Self {
            root: vdf::parse(bytes)?,
        };
        let problems = file.validate();
        if !problems.is_empty() {
            bail!("{}", problems.join(" "));
        }
        Ok(file)
    }

    /// Structural problems, empty when the file is usable.
    pub fn validate(&self) -> Vec<String> {
        let Some(value) = self.root.get_ci("shortcuts") else {
            return vec![r#"Could not find entry "shortcuts"!"#.into()];
        };
        let Some(list) = value.as_map() else {
            return vec![r#"Entry "shortcuts" is not an array!"#.into()];
        };
        if list.0.iter().any(|(k, _)| k.parse::<u32>().is_err()) {
            return vec![r#"Entry "shortcuts" is not an array!"#.into()];
        }
        let mut problems = Vec::new();
        for (_, entry) in &list.0 {
            let Some(entry) = entry.as_map() else {
                return vec![r#"Entry "shortcuts" is not an array!"#.into()];
            };
            for key in ["AppName", "Exe", "LaunchOptions"] {
                if entry.get_ci(key).is_none() {
                    let msg = format!("One of the game entries is missing the {key} parameter!");
                    if !problems.contains(&msg) {
                        problems.push(msg);
                    }
                }
            }
        }
        problems
    }

    fn list(&self) -> Option<&Map> {
        self.root.get_ci("shortcuts").and_then(Value::as_map)
    }

    fn list_mut(&mut self) -> &mut Map {
        if self
            .root
            .get_ci("shortcuts")
            .and_then(Value::as_map)
            .is_none()
        {
            self.root
                .0
                .retain(|(k, _)| !k.eq_ignore_ascii_case("shortcuts"));
            self.root.push("shortcuts", Value::Map(Map::new()));
        }
        match self
            .root
            .get_ci_mut("shortcuts")
            .and_then(Value::as_map_mut)
        {
            Some(m) => m,
            None => unreachable!("shortcuts map inserted above"),
        }
    }

    /// Every shortcut entry, in file order.
    pub fn entries(&self) -> impl Iterator<Item = ShortcutEntry<'_>> {
        self.list()
            .into_iter()
            .flat_map(|l| l.0.iter())
            .filter_map(|(_, v)| v.as_map().map(ShortcutEntry))
    }

    /// Append an entry. Indices are renumbered.
    pub fn push(&mut self, entry: Map) {
        let list = self.list_mut();
        list.push(String::new(), Value::Map(entry));
        renumber(list);
    }

    /// Remove every entry matching `pred`; returns the removed entries.
    pub fn remove_where(&mut self, mut pred: impl FnMut(ShortcutEntry<'_>) -> bool) -> Vec<Map> {
        let list = self.list_mut();
        let mut removed = Vec::new();
        let mut kept = Vec::with_capacity(list.0.len());
        for (k, v) in std::mem::take(&mut list.0) {
            match v {
                Value::Map(m) if pred(ShortcutEntry(&m)) => removed.push(m),
                v => kept.push((k, v)),
            }
        }
        list.0 = kept;
        renumber(list);
        removed
    }

    /// Serialize to the on-disk format.
    pub fn to_bytes(&self) -> Vec<u8> {
        vdf::write(&self.root)
    }
}

fn renumber(list: &mut Map) {
    for (i, (k, _)) in list.0.iter_mut().enumerate() {
        *k = i.to_string();
    }
}

/// Read access to one shortcut, with case-insensitive keys.
#[derive(Debug, Clone, Copy)]
pub struct ShortcutEntry<'a>(pub &'a Map);

impl<'a> ShortcutEntry<'a> {
    /// `AppName` (older files: `appname`).
    pub fn app_name(&self) -> &'a str {
        self.0.str_ci("AppName")
    }

    pub fn exe(&self) -> &'a str {
        self.0.str_ci("Exe")
    }

    pub fn launch_options(&self) -> &'a str {
        self.0.str_ci("LaunchOptions")
    }

    pub fn shortcut_path(&self) -> &'a str {
        self.0.str_ci("ShortcutPath")
    }

    /// Stored app ID as unsigned (Steam writes it as a signed int32).
    pub fn app_id(&self) -> Option<u32> {
        self.0
            .get_ci("appid")
            .and_then(Value::as_i32)
            .map(|v| v as u32)
    }
}

/// Fields of a new shortcut written by Ember.
#[derive(Debug, Clone)]
pub struct NewShortcut {
    pub app_name: String,
    /// Quoted executable (`"\"/usr/bin/ember\""`).
    pub exe: String,
    /// Quoted start directory.
    pub start_dir: String,
    pub icon: Option<String>,
    pub launch_options: String,
    /// Unix seconds.
    pub last_play_time: i32,
    /// Steam collections to file the shortcut under (written as `tags`).
    pub tags: Vec<String>,
}

impl NewShortcut {
    /// Entry in the field order Steam and Heroic write.
    pub fn to_entry(&self) -> Map {
        let mut m = Map::new()
            .with("AppName", Value::String(self.app_name.clone()))
            .with("Exe", Value::String(self.exe.clone()))
            .with("StartDir", Value::String(self.start_dir.clone()))
            .with(
                "appid",
                Value::Int32(shortcut_id(&self.exe, &self.app_name)),
            );
        if let Some(icon) = &self.icon {
            m.push("icon", Value::String(icon.clone()));
        }
        m.with("LaunchOptions", Value::String(self.launch_options.clone()))
            .with("IsHidden", Value::Int32(0))
            .with("AllowDesktopConfig", Value::Int32(1))
            .with("AllowOverlay", Value::Int32(1))
            .with("OpenVR", Value::Int32(0))
            .with("Devkit", Value::Int32(0))
            .with("DevkitOverrideAppID", Value::Int32(0))
            .with("LastPlayTime", Value::Int32(self.last_play_time))
            .with("tags", Value::Map(tags_map(&self.tags)))
    }
}

/// Steam's list form: keys `"0"`, `"1"`, … in order.
fn tags_map(tags: &[String]) -> Map {
    let mut m = Map::new();
    for (i, tag) in tags.iter().enumerate() {
        m.push(i.to_string(), Value::String(tag.clone()));
    }
    m
}

/// CRC-32 (IEEE, as zlib).
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: once_cell::sync::Lazy<[u32; 256]> = once_cell::sync::Lazy::new(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    });
    let mut crc = !0u32;
    for &b in data {
        crc = TABLE[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

/// Grid file ID ("short app ID") of a shortcut: `crc32(exe + name) | 0x80000000`.
pub fn short_app_id(exe: &str, app_name: &str) -> u32 {
    crc32(format!("{exe}{app_name}").as_bytes()) | 0x8000_0000
}

/// Value stored as `appid` in `shortcuts.vdf` (the short ID as signed int32).
pub fn shortcut_id(exe: &str, app_name: &str) -> i32 {
    short_app_id(exe, app_name) as i32
}

/// Grid IDs derived from a short app ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridIds {
    /// Used for `<id>p.jpg`, `<id>.jpg`, `<id>_hero.jpg`, `<id>_logo.jpg`.
    pub short: u32,
    /// Legacy Big Picture grid ID (`(short << 32) | 0x02000000`).
    pub big_picture: u64,
}

impl GridIds {
    pub fn from_short(short: u32) -> Self {
        Self {
            short,
            big_picture: ((short as u64) << 32) | 0x0200_0000,
        }
    }

    pub fn from_exe_name(exe: &str, app_name: &str) -> Self {
        Self::from_short(short_app_id(exe, app_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: [u8; 13] = [
        0x00, b's', b'h', b'o', b'r', b't', b'c', b'u', b't', b's', 0x00, 0x08, 0x08,
    ];

    fn sample(name: &str) -> NewShortcut {
        NewShortcut {
            app_name: name.into(),
            exe: "\"/usr/bin/ember\"".into(),
            start_dir: "\"/usr/bin\"".into(),
            icon: Some("/icons/x.png".into()),
            launch_options: "--launch \"gog/123\"".into(),
            last_play_time: 1_700_000_000,
            tags: vec!["Ember".into()],
        }
    }

    #[test]
    fn empty_file_bytes() {
        assert_eq!(ShortcutsFile::empty().to_bytes(), EMPTY);
        let parsed = ShortcutsFile::parse(&EMPTY).unwrap();
        assert_eq!(parsed.entries().count(), 0);
    }

    #[test]
    fn app_ids_match_steam() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let exe = "\"/usr/bin/ember\"";
        assert_eq!(short_app_id(exe, "Test Game"), 4_208_632_960);
        assert_eq!(shortcut_id(exe, "Test Game"), -86_334_336);
        assert_eq!(
            GridIds::from_exe_name(exe, "Test Game").big_picture,
            18_075_940_924_101_230_592
        );
    }

    #[test]
    fn add_then_remove_is_byte_identical() {
        let mut existing = ShortcutsFile::empty();
        let foreign = Map::new()
            .with("appid", Value::Int32(-123))
            .with("AppName", Value::String("Other".into()))
            .with("Exe", Value::String("\"/bin/other\"".into()))
            .with("StartDir", Value::String("\"/bin\"".into()))
            .with("LaunchOptions", Value::String(String::new()))
            .with("FlatpakAppID", Value::String(String::new()))
            .with(
                "tags",
                Value::Map(Map::new().with("0", Value::String("favorite".into()))),
            );
        existing.push(foreign);
        let before = existing.to_bytes();

        let mut file = ShortcutsFile::parse(&before).unwrap();
        file.push(sample("Test Game").to_entry());
        let added = file.to_bytes();
        let mut reparsed = ShortcutsFile::parse(&added).unwrap();
        assert_eq!(reparsed.entries().count(), 2);
        let e = reparsed.entries().nth(1).unwrap();
        assert_eq!(e.app_name(), "Test Game");
        assert_eq!(e.app_id(), Some(4_208_632_960));

        let removed = reparsed.remove_where(|e| e.launch_options().contains("gog/123"));
        assert_eq!(removed.len(), 1);
        assert_eq!(reparsed.to_bytes(), before);
    }

    #[test]
    fn entries_carry_their_collections() {
        let entry = sample("Test Game").to_entry();
        let tags = entry.get_ci("tags").and_then(Value::as_map).unwrap();
        assert_eq!(tags.0, [("0".into(), Value::String("Ember".into()))]);
        let bytes = vdf::write(&Map::new().with(
            "shortcuts",
            Value::Map(Map::new().with("0", Value::Map(entry))),
        ));
        assert!(ShortcutsFile::parse(&bytes).is_ok());
    }

    #[test]
    fn renumbers_and_preserves_unknown_fields() {
        let mut f = ShortcutsFile::empty();
        f.push(sample("A").to_entry());
        f.push(sample("B").to_entry());
        f.push(sample("C").to_entry());
        f.remove_where(|e| e.app_name() == "A");
        let bytes = f.to_bytes();
        let root = vdf::parse(&bytes).unwrap();
        let keys: Vec<_> = root
            .get_ci("shortcuts")
            .unwrap()
            .as_map()
            .unwrap()
            .0
            .iter()
            .map(|(k, _)| k.clone())
            .collect();
        assert_eq!(keys, ["0", "1"]);
    }

    #[test]
    fn validation_messages() {
        let no_root = vdf::write(&Map::new().with("x", Value::Int32(1)));
        let err = ShortcutsFile::parse(&no_root).unwrap_err().to_string();
        assert!(err.contains(r#"Could not find entry "shortcuts"!"#));

        let not_array = vdf::write(&Map::new().with(
            "shortcuts",
            Value::Map(Map::new().with("a", Value::Int32(1))),
        ));
        assert!(
            ShortcutsFile::parse(&not_array)
                .unwrap_err()
                .to_string()
                .contains("is not an array")
        );

        let missing = vdf::write(&Map::new().with(
            "shortcuts",
            Value::Map(Map::new().with(
                "0",
                Value::Map(Map::new().with("appname", Value::String("x".into()))),
            )),
        ));
        let err = ShortcutsFile::parse(&missing).unwrap_err().to_string();
        assert!(err.contains("missing the Exe parameter"));
        assert!(err.contains("missing the LaunchOptions parameter"));
        assert!(!err.contains("AppName"));
    }
}
