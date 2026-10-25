//! Install size accounting and free-space checks for the install sheet.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use ember_core::InstallInfo;
use ember_core::util::format_bytes;
use ember_ui::i18n::{t, t_args};

/// Download and on-disk bytes of an install choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InstallSize {
    pub download: u64,
    pub disk: u64,
}

impl InstallSize {
    pub fn new(download: u64, disk: u64) -> Self {
        Self { download, disk }
    }

    fn add(&mut self, other: InstallSize) {
        self.download = self.download.saturating_add(other.download);
        self.disk = self.disk.saturating_add(other.disk);
    }

    pub fn is_known(&self) -> bool {
        self.download > 0 || self.disk > 0
    }
}

/// Size of installing the base game in `language` with the `selected` DLCs.
///
/// GOG reports per-language sizes (`language_sizes`, with `"*"` for files
/// shared by every language); those replace the manifest totals. DLC sizes
/// come from `dlc_sizes` (fetched per DLC) and are always added — Heroic
/// left Epic/Amazon DLC sizes out of the total and the disk check.
pub fn install_size(
    info: &InstallInfo,
    language: Option<&str>,
    selected: &BTreeSet<String>,
    dlc_sizes: &HashMap<String, InstallSize>,
) -> InstallSize {
    let mut total = base_size(info, language);
    for dlc in selected {
        if let Some(size) = dlc_sizes.get(dlc) {
            total.add(*size);
        }
    }
    total
}

fn base_size(info: &InstallInfo, language: Option<&str>) -> InstallSize {
    if info.language_sizes.is_empty() {
        return InstallSize::new(info.download_size, info.disk_size);
    }
    let mut size = InstallSize::default();
    let mut matched = false;
    for entry in &info.language_sizes {
        let shared = entry.language == "*";
        let chosen = language.is_some_and(|l| l.eq_ignore_ascii_case(&entry.language));
        if shared || chosen {
            matched |= chosen;
            size.add(InstallSize::new(entry.download_size, entry.disk_size));
        }
    }
    if !matched && !size.is_known() {
        return InstallSize::new(info.download_size, info.disk_size);
    }
    size
}

/// Bytes that a resumed download still needs, from the progress already made.
pub fn remaining(total: u64, percent_done: Option<f32>) -> u64 {
    match percent_done {
        Some(p) if p > 0.0 => {
            let left = (100.0 - p.clamp(0.0, 100.0)) as f64 / 100.0;
            (total as f64 * left).round() as u64
        }
        _ => total,
    }
}

/// Free space on the filesystem holding a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSpace {
    pub free: u64,
    pub total: u64,
    /// The directory (or its nearest existing ancestor) is writable.
    pub writable: bool,
}

/// Outcome of comparing free space with what an install needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskVerdict {
    /// Size or free space not known yet.
    Unknown,
    Fits {
        left_after: u64,
    },
    /// Not enough space; installing anyway is allowed but flagged.
    TooSmall {
        missing: u64,
    },
    NotWritable,
}

impl DiskVerdict {
    pub fn evaluate(space: Option<DiskSpace>, required: u64) -> Self {
        let Some(space) = space else {
            return DiskVerdict::Unknown;
        };
        if !space.writable {
            return DiskVerdict::NotWritable;
        }
        if required == 0 {
            return DiskVerdict::Unknown;
        }
        if space.free >= required {
            DiskVerdict::Fits {
                left_after: space.free - required,
            }
        } else {
            DiskVerdict::TooSmall {
                missing: required - space.free,
            }
        }
    }

    /// One-line explanation shown under the disk bar, in the interface
    /// language.
    pub fn message(self, space: Option<DiskSpace>) -> String {
        match (self, space) {
            (DiskVerdict::NotWritable, _) => t("game.disk.not_writable").to_string(),
            (DiskVerdict::TooSmall { missing }, _) => {
                t_args("game.disk.too_small", &[("size", &format_bytes(missing))]).to_string()
            }
            (DiskVerdict::Fits { left_after }, Some(s)) => t_args(
                "game.disk.fits",
                &[
                    ("free", &format_bytes(s.free)),
                    ("total", &format_bytes(s.total)),
                    ("left", &format_bytes(left_after)),
                ],
            )
            .to_string(),
            (DiskVerdict::Unknown, Some(s)) => t_args(
                "game.disk.free",
                &[
                    ("free", &format_bytes(s.free)),
                    ("total", &format_bytes(s.total)),
                ],
            )
            .to_string(),
            _ => t("game.disk.checking").to_string(),
        }
    }
}

/// Fractions of the disk bar: (already used, this install) in 0..=1.
pub fn disk_bar_fractions(space: DiskSpace, required: u64) -> (f32, f32) {
    if space.total == 0 {
        return (0.0, 0.0);
    }
    let total = space.total as f64;
    let used = (space.total.saturating_sub(space.free)) as f64 / total;
    let install = (required.min(space.free)) as f64 / total;
    (used as f32, install as f32)
}

/// The nearest existing directory at or above `path` (installs usually
/// target a folder that does not exist yet).
pub fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut cur = Some(path);
    while let Some(p) = cur {
        if p.is_dir() {
            return Some(p.to_path_buf());
        }
        cur = p.parent();
    }
    None
}

/// Query free space for `path`. Blocking (a `statvfs` call); run it off the
/// UI thread.
pub fn disk_space(path: &Path) -> anyhow::Result<DiskSpace> {
    use anyhow::Context as _;
    let dir = existing_ancestor(path).with_context(|| {
        t_args(
            "game.disk.path_missing",
            &[("path", &path.display().to_string())],
        )
    })?;
    let free = fs4::available_space(&dir).with_context(|| {
        t_args(
            "game.disk.read_failed",
            &[("path", &dir.display().to_string())],
        )
    })?;
    let total = fs4::total_space(&dir)?;
    #[cfg(unix)]
    let writable = nix::unistd::access(&dir, nix::unistd::AccessFlags::W_OK).is_ok();
    // Windows directory read-only attributes do not represent ACL write access.
    // Probe creation with a unique, automatically removed file instead.
    #[cfg(windows)]
    let writable = tempfile::Builder::new()
        .prefix(".ember-write-test-")
        .tempfile_in(&dir)
        .is_ok();
    Ok(DiskSpace {
        free,
        total,
        writable,
    })
}

/// `Install · 12.3 GiB`, or `Install` while the size is unknown.
pub fn install_button_label(verb: &str, download: u64) -> String {
    if download == 0 {
        verb.to_string()
    } else {
        format!("{verb} · {}", format_bytes(download))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_core::LanguageSize;

    fn info() -> InstallInfo {
        InstallInfo {
            download_size: 100,
            disk_size: 200,
            ..Default::default()
        }
    }

    #[test]
    fn manifest_totals_plus_selected_dlcs() {
        let mut dlcs = HashMap::new();
        dlcs.insert("a".to_string(), InstallSize::new(10, 20));
        dlcs.insert("b".to_string(), InstallSize::new(1, 2));
        let selected: BTreeSet<String> = ["a".to_string()].into();
        assert_eq!(
            install_size(&info(), None, &selected, &dlcs),
            InstallSize::new(110, 220)
        );
        // A selected DLC whose size is unknown adds nothing.
        let selected: BTreeSet<String> = ["c".to_string()].into();
        assert_eq!(
            install_size(&info(), None, &selected, &dlcs),
            InstallSize::new(100, 200)
        );
    }

    #[test]
    fn per_language_sizes() {
        let mut i = info();
        i.language_sizes = vec![
            LanguageSize {
                language: "*".into(),
                download_size: 50,
                disk_size: 60,
            },
            LanguageSize {
                language: "en-US".into(),
                download_size: 5,
                disk_size: 6,
            },
            LanguageSize {
                language: "de-DE".into(),
                download_size: 7,
                disk_size: 8,
            },
        ];
        let none = BTreeSet::new();
        let sizes = HashMap::new();
        assert_eq!(
            install_size(&i, Some("de-de"), &none, &sizes),
            InstallSize::new(57, 68)
        );
        // Unknown language: shared files only.
        assert_eq!(
            install_size(&i, Some("fr-FR"), &none, &sizes),
            InstallSize::new(50, 60)
        );
        i.language_sizes.remove(0);
        // No shared entry and no match: fall back to the manifest totals.
        assert_eq!(
            install_size(&i, Some("fr-FR"), &none, &sizes),
            InstallSize::new(100, 200)
        );
    }

    #[test]
    fn verdicts() {
        crate::i18n::register();
        let space = DiskSpace {
            free: 1000,
            total: 4000,
            writable: true,
        };
        assert_eq!(DiskVerdict::evaluate(None, 10), DiskVerdict::Unknown);
        assert_eq!(DiskVerdict::evaluate(Some(space), 0), DiskVerdict::Unknown);
        assert_eq!(
            DiskVerdict::evaluate(Some(space), 400),
            DiskVerdict::Fits { left_after: 600 }
        );
        assert_eq!(
            DiskVerdict::evaluate(Some(space), 1500),
            DiskVerdict::TooSmall { missing: 500 }
        );
        let ro = DiskSpace {
            writable: false,
            ..space
        };
        assert_eq!(DiskVerdict::evaluate(Some(ro), 1), DiskVerdict::NotWritable);
        assert!(
            DiskVerdict::TooSmall { missing: 2048 }
                .message(Some(space))
                .contains("2.0 KiB")
        );
    }

    #[test]
    fn bar_fractions() {
        let space = DiskSpace {
            free: 1000,
            total: 4000,
            writable: true,
        };
        assert_eq!(disk_bar_fractions(space, 400), (0.75, 0.1));
        // The install segment never exceeds the free part.
        assert_eq!(disk_bar_fractions(space, 9000), (0.75, 0.25));
        assert_eq!(
            disk_bar_fractions(DiskSpace { total: 0, ..space }, 1),
            (0.0, 0.0)
        );
    }

    #[test]
    fn remaining_after_resume() {
        assert_eq!(remaining(1000, None), 1000);
        assert_eq!(remaining(1000, Some(25.0)), 750);
        assert_eq!(remaining(1000, Some(150.0)), 0);
    }

    #[test]
    fn labels() {
        assert_eq!(install_button_label("Install", 0), "Install");
        assert_eq!(
            install_button_label("Install", 3 * 1024 * 1024 * 1024),
            "Install · 3.0 GiB"
        );
    }

    #[cfg(windows)]
    #[test]
    fn readonly_directory_attribute_does_not_block_installation() {
        let dir = tempfile::tempdir().unwrap();
        let original = std::fs::metadata(dir.path()).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        std::fs::set_permissions(dir.path(), readonly).unwrap();
        let result = disk_space(dir.path());
        std::fs::set_permissions(dir.path(), original).unwrap();
        assert!(result.unwrap().writable);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn real_disk_space() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not/yet/created");
        assert_eq!(existing_ancestor(&missing).as_deref(), Some(dir.path()));
        let space = disk_space(&missing).unwrap();
        assert!(space.total >= space.free);
        assert!(space.writable);
    }
}
