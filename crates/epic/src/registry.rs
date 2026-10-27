//! Reading and editing a Wine prefix's `user.reg` (shell folders for save
//! paths, the EOS overlay location).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

const SHELL_FOLDERS: &str =
    r"[Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Folders]";
/// The overlay key as written in `user.reg` (backslashes doubled).
const EOS_KEY: &str = r"Software\\Epic Games\\EOS";
const OVERLAY_VALUE: &str = "OverlayPath";

fn user_reg(prefix: &Path) -> PathBuf {
    prefix.join("user.reg")
}

/// Values of the section whose header starts with `header` (case-insensitive).
fn section_values(text: &str, header: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line.to_lowercase().starts_with(&header.to_lowercase());
            continue;
        }
        if !inside {
            continue;
        }
        if let Some((k, v)) = line.split_once("\"=\"") {
            let key = k.trim_start_matches('"').to_string();
            let value = v.trim_end_matches('"').replace(r"\\", r"\");
            out.insert(key, value);
        }
    }
    out
}

/// Map a Windows path inside the prefix (`C:\users\…`) to the host path.
pub fn windows_to_host(prefix: &Path, win: &str) -> PathBuf {
    let unified = win.replace('\\', "/");
    let lower = unified.to_lowercase();
    if let Some(rest) = lower.strip_prefix("z:").map(|_| &unified[2..]) {
        return PathBuf::from(rest);
    }
    let rest = if lower.starts_with("c:") {
        &unified[2..]
    } else {
        unified.as_str()
    };
    let mut out = prefix.join("drive_c");
    out.extend(rest.split('/').filter(|p| !p.is_empty()));
    out
}

/// The prefix's shell folders (`AppData`, `Local AppData`, `Personal`,
/// known-folder GUIDs…) as host paths.
pub fn shell_folders(prefix: &Path) -> Result<HashMap<String, PathBuf>> {
    let text = std::fs::read_to_string(user_reg(prefix))
        .with_context(|| format!("{} is not an initialised Wine prefix", prefix.display()))?;
    Ok(section_values(&text, SHELL_FOLDERS)
        .into_iter()
        .map(|(k, v)| (k, windows_to_host(prefix, &v)))
        .collect())
}

/// Where the prefix's registry points the EOS overlay, if anywhere.
pub fn overlay_path(prefix: &Path) -> Result<Option<PathBuf>> {
    let text = std::fs::read_to_string(user_reg(prefix))
        .with_context(|| format!("{} is not an initialised Wine prefix", prefix.display()))?;
    let value = text
        .lines()
        .find(|l| l.contains(OVERLAY_VALUE))
        .and_then(|l| l.split_once('='))
        .map(|(_, v)| v.trim().trim_matches('"').to_string());
    Ok(value
        .filter(|v| !v.is_empty())
        .map(|v| windows_to_host(prefix, &v.replace(r"\\", r"\"))))
}

/// Point the prefix's EOS overlay registry value at `overlay_dir`.
pub fn set_overlay_path(prefix: &Path, overlay_dir: &Path) -> Result<()> {
    let file = user_reg(prefix);
    let text = std::fs::read_to_string(&file)
        .with_context(|| format!("{} is not an initialised Wine prefix", prefix.display()))?;
    let mut win = overlay_dir.to_string_lossy().replace('\\', "/");
    if win.starts_with('/') {
        win = format!("Z:{win}");
    }
    let line = format!("\"{OVERLAY_VALUE}\"=\"{win}\"");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if let Some(i) = lines.iter().position(|l| l.contains(OVERLAY_VALUE)) {
        lines[i] = line;
    } else if let Some(i) = lines
        .iter()
        .position(|l| l.starts_with('[') && l.to_lowercase().contains(&EOS_KEY.to_lowercase()))
    {
        lines.insert(i + 1, line);
    } else {
        lines.push(String::new());
        lines.push(format!("[{EOS_KEY}]"));
        lines.push(line);
    }
    write_lines(&file, &lines)
}

/// Remove the EOS overlay registry value from the prefix.
pub fn remove_overlay_path(prefix: &Path) -> Result<()> {
    let file = user_reg(prefix);
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Ok(());
    };
    if !text.contains(OVERLAY_VALUE) {
        return Ok(());
    }
    let lines: Vec<String> = text
        .lines()
        .filter(|l| !l.contains(OVERLAY_VALUE))
        .map(str::to_string)
        .collect();
    write_lines(&file, &lines)
}

fn write_lines(file: &Path, lines: &[String]) -> Result<()> {
    let mut text = lines.join("\n");
    text.push('\n');
    let tmp = file.with_extension("reg.irori-tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, file).with_context(|| format!("updating {}", file.display()))?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const USER_REG: &str = r#"WINE REGISTRY Version 2
;; All keys relative to \\User\\S-1-5-21-0-0-0-1000

#arch=win64

[Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Folders] 1700000000
#time=1da
"AppData"="C:\\users\\steamuser\\AppData\\Roaming"
"Local AppData"="C:\\users\\steamuser\\AppData\\Local"
"Personal"="C:\\users\\steamuser\\Documents"
"{4C5C32FF-BB9D-43B0-B5B4-2D72E54EAAA4}"="C:\\users\\steamuser\\Saved Games"
"{A520A1A4-1780-4FF6-BD18-167343C5AF16}"="C:\\users\\steamuser\\AppData\\LocalLow"

[Software\\Wine] 1700000000
"Version"="win10"
"#;

    #[test]
    fn reads_shell_folders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("user.reg"), USER_REG).unwrap();
        let f = shell_folders(dir.path()).unwrap();
        assert_eq!(
            f["Local AppData"],
            dir.path().join("drive_c/users/steamuser/AppData/Local")
        );
        assert_eq!(
            f["{4C5C32FF-BB9D-43B0-B5B4-2D72E54EAAA4}"],
            dir.path().join("drive_c/users/steamuser/Saved Games")
        );
        assert!(!f.contains_key("Version"));
        assert!(shell_folders(&dir.path().join("none")).is_err());
    }

    #[test]
    fn edits_the_overlay_value() {
        let dir = tempfile::tempdir().unwrap();
        let pfx = dir.path();
        std::fs::write(pfx.join("user.reg"), USER_REG).unwrap();
        assert_eq!(overlay_path(pfx).unwrap(), None);
        set_overlay_path(pfx, Path::new("/home/u/.local/share/irori/eos")).unwrap();
        assert_eq!(
            overlay_path(pfx).unwrap(),
            Some(PathBuf::from("/home/u/.local/share/irori/eos"))
        );
        let text = std::fs::read_to_string(pfx.join("user.reg")).unwrap();
        assert!(text.contains(r#"[Software\\Epic Games\\EOS]"#));
        assert!(text.contains(r#""OverlayPath"="Z:/home/u/.local/share/irori/eos""#));
        set_overlay_path(pfx, Path::new("/other")).unwrap();
        let text = std::fs::read_to_string(pfx.join("user.reg")).unwrap();
        assert_eq!(text.matches("OverlayPath").count(), 1);
        remove_overlay_path(pfx).unwrap();
        assert_eq!(overlay_path(pfx).unwrap(), None);
        assert!(
            std::fs::read_to_string(pfx.join("user.reg"))
                .unwrap()
                .contains("Shell Folders")
        );
        assert_eq!(
            windows_to_host(pfx, r"C:\Program Files\x"),
            pfx.join("drive_c/Program Files/x")
        );
    }
}
