//! Ubisoft Connect: products from the client's configuration cache, installs
//! from the registry.
//!
//! The cache file `configurations` is a protobuf-encoded list of records, each
//! holding an install id, a launch id and a YAML document describing the
//! product. Products that can be started have a `root.start_game` section;
//! their name is `root.name`, which is sometimes a key (`l1`) into
//! `localizations.default`. The launcher records an install as
//! `HKEY_LOCAL_MACHINE\SOFTWARE\Ubisoft\Launcher\Installs\<install id>` with
//! `InstallDir`; some products name another registry value in their YAML
//! (`working_directory.register`).
//!
//! Games launch through the client with `uplay://launch/<launch id>/0`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context;
use prost::Message;

use crate::yaml::{self, Node};
use crate::{
    InstalledGame, LaunchSpec, Launcher, Registry, ScanReport, drive_c, registry, retain_present,
};

/// Where the client keeps its configuration cache, relative to `drive_c` (or
/// `C:\`): the install folder, then, for newer clients, each user's local
/// app data (`users/<name>/AppData/Local/…`).
pub const CONFIGURATIONS: &str =
    "Program Files (x86)/Ubisoft/Ubisoft Game Launcher/cache/configuration/configurations";
/// The per-user cache location, relative to a user's folder under `users/`.
pub const USER_CONFIGURATIONS: &str =
    "AppData/Local/Ubisoft Game Launcher/cache/configuration/configurations";

/// The registry key holding one subkey per installed product.
pub const INSTALLS_KEY: &str = r"SOFTWARE\Ubisoft\Launcher\Installs";

/// Names in the cache that are placeholders rather than titles.
const PLACEHOLDER_NAMES: &[&str] = &[
    "",
    "name",
    "gamename",
    "l1",
    "l2",
    "thumbimage",
    "ubisoft game",
];

/// One record of the configuration cache.
#[derive(Clone, PartialEq, prost::Message)]
struct Record {
    #[prost(uint32, tag = "1")]
    install_id: u32,
    #[prost(uint32, tag = "2")]
    launch_id: u32,
    #[prost(bytes = "vec", tag = "3")]
    configuration: Vec<u8>,
}

/// A product the client can start, from the configuration cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    /// The id the registry files the install under.
    pub install_id: u32,
    /// The id `uplay://launch/` takes; the install id when the record has none.
    pub launch_id: u32,
    pub name: String,
    /// The first executable's registry value path for its working folder.
    pub install_dir_register: Option<String>,
    /// The first executable's path relative to the install folder.
    pub executable: Option<String>,
    /// The first executable's registry value path for its full path.
    pub executable_register: Option<String>,
}

/// Products the client can start, from the bytes of a `configurations` file.
/// Records that don't decode become warnings.
pub fn parse_configurations(bytes: &[u8]) -> (Vec<Product>, Vec<String>) {
    let mut products: Vec<Product> = Vec::new();
    let mut warnings = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let Some(tag) = read_varint(bytes, &mut pos) else {
            warnings.push(truncated());
            break;
        };
        let (field, wire_type) = (tag >> 3, tag & 7);
        let body = match wire_type {
            0 => read_varint(bytes, &mut pos).map(|_| None),
            1 => skip(bytes, &mut pos, 8).map(|_| None),
            2 => read_varint(bytes, &mut pos)
                .and_then(|len| usize::try_from(len).ok())
                .and_then(|len| skip(bytes, &mut pos, len))
                .map(Some),
            5 => skip(bytes, &mut pos, 4).map(|_| None),
            _ => {
                warnings.push(
                    "Ubisoft Connect's configuration cache has data Irori doesn't understand; \
                     some games may be missing."
                        .to_string(),
                );
                break;
            }
        };
        let Some(body) = body else {
            warnings.push(truncated());
            break;
        };
        let Some(body) = body.filter(|_| field == 1) else {
            continue;
        };
        match Record::decode(body) {
            Ok(record) => {
                if let Some(product) = product_from_record(&record)
                    && !products.iter().any(|p| p.install_id == product.install_id)
                {
                    products.push(product);
                }
            }
            Err(e) => warnings.push(format!(
                "Skipped an unreadable entry in Ubisoft Connect's configuration cache: {e}"
            )),
        }
    }
    (products, warnings)
}

fn truncated() -> String {
    "Ubisoft Connect's configuration cache ends early; some games may be missing.".to_string()
}

fn read_varint(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*pos)?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn skip<'a>(bytes: &'a [u8], pos: &mut usize, len: usize) -> Option<&'a [u8]> {
    let end = pos.checked_add(len)?;
    let slice = bytes.get(*pos..end)?;
    *pos = end;
    Some(slice)
}

fn product_from_record(record: &Record) -> Option<Product> {
    if record.install_id == 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&record.configuration);
    if !text.contains("start_game") {
        return None;
    }
    let doc = yaml::parse(&text);
    let root = doc.get("root")?;
    root.get("start_game")?;
    let exe = root
        .path(&["start_game", "online", "executables"])
        .or_else(|| root.path(&["start_game", "offline", "executables"]))
        .and_then(Node::first);
    let text_at = |node: Option<&Node>, keys: &[&str]| {
        node.and_then(|n| n.path(keys))
            .and_then(Node::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(Product {
        install_id: record.install_id,
        launch_id: if record.launch_id == 0 {
            record.install_id
        } else {
            record.launch_id
        },
        name: product_name(&doc, root).unwrap_or_else(|| record.install_id.to_string()),
        install_dir_register: text_at(exe, &["working_directory", "register"]),
        executable: text_at(exe, &["path", "relative"]),
        executable_register: text_at(exe, &["path", "register"]),
    })
}

/// `root.name`, resolved through `localizations.default` when it is a key,
/// else the installer's game identifier.
fn product_name(doc: &Node, root: &Node) -> Option<String> {
    let name = root.get("name").and_then(Node::as_str).unwrap_or("").trim();
    let localized = |key: &str| {
        doc.path(&["localizations", "default", key])
            .and_then(Node::as_str)
            .map(str::trim)
            .filter(|s| !is_placeholder(s))
    };
    if !is_placeholder(name) {
        return Some(name.to_string());
    }
    localized(name)
        .or_else(|| {
            root.path(&["installer", "game_identifier"])
                .and_then(Node::as_str)
                .map(str::trim)
                .filter(|s| !is_placeholder(s))
        })
        .map(str::to_string)
}

fn is_placeholder(name: &str) -> bool {
    PLACEHOLDER_NAMES
        .iter()
        .any(|p| p.eq_ignore_ascii_case(name))
}

/// How Ubisoft Connect launches a product.
pub fn launch_spec(launch_id: u32) -> LaunchSpec {
    LaunchSpec::Uri(format!("uplay://launch/{launch_id}/0"))
}

/// The products the registry (`HKEY_LOCAL_MACHINE`) records as installed.
pub fn installed_games(products: &[Product], registry: &Registry) -> Vec<InstalledGame> {
    products
        .iter()
        .filter_map(|product| {
            let install_dir = install_dir(product, registry)?;
            let executable = product.executable.clone().or_else(|| {
                product
                    .executable_register
                    .as_deref()
                    .and_then(|path| registry.machine_value_path(path))
                    .map(str::to_string)
            });
            Some(InstalledGame {
                launcher: Launcher::Ubisoft,
                id: product.install_id.to_string(),
                title: product.name.clone(),
                install_dir: PathBuf::from(install_dir),
                executable,
                launch: launch_spec(product.launch_id),
            })
        })
        .collect()
}

fn install_dir<'r>(product: &Product, registry: &'r Registry) -> Option<&'r str> {
    let by_id = |id: u32| registry.machine_string(&format!(r"{INSTALLS_KEY}\{id}"), "InstallDir");
    by_id(product.install_id)
        .or_else(|| (product.launch_id != product.install_id).then(|| by_id(product.launch_id))?)
        .or_else(|| {
            product
                .install_dir_register
                .as_deref()
                .and_then(|path| registry.machine_value_path(path))
        })
        .filter(|dir| !dir.trim().is_empty())
}

/// The Ubisoft Connect games installed in the Wine prefix at `prefix`.
pub fn scan_prefix(prefix: &Path) -> ScanReport {
    let mut warnings = Vec::new();
    let registry = registry::load_machine(prefix, &mut warnings);
    let mut report = scan_drive(&drive_c(prefix), &registry);
    report.warnings.splice(0..0, warnings);
    report
}

/// The Ubisoft Connect games installed on `drive` (`drive_c`, or `C:\`), with
/// `registry` holding `HKEY_LOCAL_MACHINE`.
pub fn scan_drive(drive: &Path, registry: &Registry) -> ScanReport {
    let mut report = ScanReport::default();
    let Some(path) = newest_configurations(drive) else {
        return report;
    };
    let bytes =
        match std::fs::read(&path).with_context(|| format!("Couldn't read {}", path.display())) {
            Ok(bytes) => bytes,
            Err(e) => {
                report.warnings.push(format!("{e:#}"));
                return report;
            }
        };
    let (products, warnings) = parse_configurations(&bytes);
    report.warnings.extend(warnings);
    report.games = installed_games(&products, registry);
    retain_present(drive, &mut report);
    report
}

/// The most recently written configuration cache, since an updated client
/// moves it to the user's folder and leaves the old one behind.
fn newest_configurations(drive: &Path) -> Option<PathBuf> {
    let mut candidates = vec![drive.join(CONFIGURATIONS)];
    if let Ok(users) = std::fs::read_dir(drive.join("users")) {
        candidates.extend(users.flatten().map(|u| u.path().join(USER_CONFIGURATIONS)));
    }
    candidates
        .into_iter()
        .filter_map(|p| {
            let meta = std::fs::metadata(&p).ok().filter(|m| m.is_file())?;
            Some((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), p))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, p)| p)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::RegValue;

    pub(crate) const FAR_CRY_YAML: &str = "version: 2.0\n\
root:\n\
\tname: l1\n\
\tspace_id: 0a1b2c3d\n\
\tinstaller:\n\
\t\tgame_identifier: Far Cry 5\n\
\tstart_game:\n\
\t\tonline:\n\
\t\t\texecutables:\n\
\t\t\t- path:\n\
\t\t\t\t\t\trelative: bin\\FarCry5.exe\n\
\t\t\t\t\tworking_directory:\n\
\t\t\t\t\t\tregister: HKEY_LOCAL_MACHINE\\SOFTWARE\\Ubisoft\\Launcher\\Installs\\635\\InstallDir\n\
localizations:\n\
\tdefault:\n\
\t\tl1: \"Far Cry® 5\"\n";

    const ORIGINS_YAML: &str = "root:\n  name: Assassin's Creed® Origins\n  start_game:\n    online:\n      executables:\n        - path:\n            register: HKEY_LOCAL_MACHINE\\SOFTWARE\\Ubisoft\\Origins\\ExePath\n          working_directory:\n            register: HKEY_LOCAL_MACHINE\\SOFTWARE\\Ubisoft\\Origins\\GameDir\n";

    const DLC_YAML: &str = "root:\n  name: Season Pass\n  is_dlc: yes\n";

    fn record(install_id: u32, launch_id: u32, yaml: &str) -> Vec<u8> {
        Record {
            install_id,
            launch_id,
            configuration: yaml.as_bytes().to_vec(),
        }
        .encode_to_vec()
    }

    /// A `configurations` file: each record as a length-delimited field 1.
    pub(crate) fn configurations(records: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in records {
            prost::encoding::encode_key(1, prost::encoding::WireType::LengthDelimited, &mut out);
            prost::encoding::encode_varint(r.len() as u64, &mut out);
            out.extend_from_slice(r);
        }
        out
    }

    pub(crate) fn sample_configurations() -> Vec<u8> {
        configurations(&[
            record(635, 0, FAR_CRY_YAML),
            record(3539, 4923, ORIGINS_YAML),
            record(700, 0, DLC_YAML),
            record(
                800,
                0,
                "root:\n  name: Not Installed\n  start_game:\n    online: {}\n",
            ),
        ])
    }

    #[test]
    fn parses_records_with_start_game() {
        let (products, warnings) = parse_configurations(&sample_configurations());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(products.len(), 3);

        let far_cry = &products[0];
        assert_eq!(far_cry.install_id, 635);
        assert_eq!(far_cry.launch_id, 635);
        assert_eq!(far_cry.name, "Far Cry® 5");
        assert_eq!(far_cry.executable.as_deref(), Some(r"bin\FarCry5.exe"));
        assert_eq!(
            far_cry.install_dir_register.as_deref(),
            Some(r"HKEY_LOCAL_MACHINE\SOFTWARE\Ubisoft\Launcher\Installs\635\InstallDir")
        );

        let origins = &products[1];
        assert_eq!(origins.launch_id, 4923);
        assert_eq!(origins.name, "Assassin's Creed® Origins");
        assert_eq!(origins.executable, None);

        assert_eq!(products[2].name, "Not Installed");
    }

    #[test]
    fn falls_back_to_the_game_identifier_and_the_id() {
        let yaml = "root:\n  name: GAMENAME\n  installer:\n    game_identifier: Rayman Legends\n  start_game:\n    online: {}\n";
        let (products, _) = parse_configurations(&configurations(&[
            record(1, 0, yaml),
            record(2, 0, "root:\n  start_game: {}\n"),
        ]));
        assert_eq!(products[0].name, "Rayman Legends");
        assert_eq!(products[1].name, "2");
    }

    #[test]
    fn keeps_good_records_when_the_file_is_damaged() {
        let mut bytes = sample_configurations();
        // A record whose body isn't a valid message, then a truncated one.
        bytes.extend_from_slice(&[0x0a, 0x02, 0x08, 0xff]);
        bytes.extend_from_slice(&[0x0a, 0x40, 0x08]);
        let (products, warnings) = parse_configurations(&bytes);
        assert_eq!(products.len(), 3);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    #[test]
    fn installed_games_come_from_the_registry() {
        let (products, _) = parse_configurations(&sample_configurations());
        let mut registry = Registry::parse(
            "WINE REGISTRY Version 2\n\
             [Software\\\\Wow6432Node\\\\Ubisoft\\\\Launcher\\\\Installs\\\\635] 1\n\
             \"InstallDir\"=\"C:/Program Files (x86)/Ubisoft/Ubisoft Game Launcher/games/Far Cry 5/\"\n",
        );
        registry.insert(
            r"Software\Ubisoft\Origins",
            "GameDir",
            RegValue::String(r"D:\Games\AC Origins".into()),
        );
        registry.insert(
            r"Software\Ubisoft\Origins",
            "ExePath",
            RegValue::String(r"D:\Games\AC Origins\ACOrigins.exe".into()),
        );
        let games = installed_games(&products, &registry);
        assert_eq!(games.len(), 2);

        assert_eq!(games[0].app_name(), "ubisoft:635");
        assert_eq!(
            games[0].install_dir,
            PathBuf::from("C:/Program Files (x86)/Ubisoft/Ubisoft Game Launcher/games/Far Cry 5/")
        );
        assert_eq!(games[0].executable.as_deref(), Some(r"bin\FarCry5.exe"));
        assert_eq!(
            games[0].launch,
            LaunchSpec::Uri("uplay://launch/635/0".into())
        );

        assert_eq!(games[1].id, "3539");
        assert_eq!(games[1].install_dir, PathBuf::from(r"D:\Games\AC Origins"));
        assert_eq!(
            games[1].executable.as_deref(),
            Some(r"D:\Games\AC Origins\ACOrigins.exe")
        );
        assert_eq!(games[1].launch.launcher_args(), ["uplay://launch/4923/0"]);
    }
}
