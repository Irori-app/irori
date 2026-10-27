//! Owned Finder app bundles and Windows shell links. All dynamic PowerShell
//! values travel through JSON in the environment, never through script text.
use super::LauncherCommand;
use anyhow::{Context as _, Result, ensure};
use irori_core::process::{CommandSpec, RunOptions};
use irori_core::{GameInfo, Paths};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreatedEntries {
    pub desktop: Option<PathBuf>,
    pub menu: Option<PathBuf>,
}
#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    games: BTreeMap<String, Vec<PathBuf>>,
}
static LOCK: Mutex<()> = Mutex::const_new(());
fn manifest_path() -> PathBuf {
    Paths::get().state_dir().join("native-shortcuts.json")
}
async fn manifest() -> Manifest {
    tokio::fs::read(manifest_path())
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
async fn save(m: &Manifest) -> Result<()> {
    crate::util::write_atomic(&manifest_path(), &serde_json::to_vec_pretty(m)?).await
}
fn menu_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Ok(dirs::home_dir()
            .context("No home directory")?
            .join("Applications/Irori Games"))
    }
    #[cfg(windows)]
    {
        Ok(dirs::config_dir()
            .context("No application data directory")?
            .join("Microsoft/Windows/Start Menu/Programs/Irori Games"))
    }
}
fn file_name(game: &GameInfo) -> String {
    // A digest keeps identity stable across renames and avoids collisions after
    // sanitization, Windows reserved names, and unusually long catalogue IDs.
    let title: String = crate::util::sanitize_filename(&game.title)
        .chars()
        .take(80)
        .collect();
    let hash = md5::compute(game.id().key());
    format!(
        "Irori - {title} - {hash:x}.{}",
        if cfg!(windows) { "lnk" } else { "app" }
    )
}

#[cfg(windows)]
async fn shell_link(
    path: &Path,
    game: &GameInfo,
    launcher: Option<&LauncherCommand>,
) -> Result<bool> {
    let args = launcher.map(|l| {
        l.launch_args(&game.id())
            .into_iter()
            .skip(1)
            .map(|a| irori_core::process::windows_arg(&a))
            .collect::<Vec<_>>()
            .join(" ")
    });
    let data = serde_json::json!({"path": path, "owner": format!("Irori:{}", game.id().key()),
        "program": launcher.map(|l| &l.program), "cwd": launcher.map(|l| &l.working_dir), "args": args});
    const SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
$d = $env:IRORI_SHORTCUT | ConvertFrom-Json
$shell = New-Object -ComObject WScript.Shell
$exists = Test-Path -LiteralPath $d.path
$link = $shell.CreateShortcut($d.path)
if ($exists -and $link.Description -ne $d.owner) { throw 'This shortcut belongs to another application' }
if ($null -ne $d.program) {
  $link.TargetPath = $d.program
  $link.Arguments = $d.args
  $link.WorkingDirectory = $d.cwd
  $link.Description = $d.owner
  $link.Save()
} elseif ($exists) { Write-Output 'owned' }
"#;
    let result = irori_core::process::run(
        &CommandSpec::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                SCRIPT,
            ])
            .env("IRORI_SHORTCUT", data.to_string()),
        RunOptions::default(),
    )
    .await?;
    ensure!(
        result.success(),
        "Could not access Windows shortcut: {}",
        result.error_summary()
    );
    Ok(result.stdout.trim() == "owned")
}

#[cfg(target_os = "macos")]
fn owned_bundle(path: &Path, game: &GameInfo) -> bool {
    plist::Value::from_file(path.join("Contents/Info.plist"))
        .ok()
        .and_then(|v| {
            v.as_dictionary()?
                .get("IroriGame")?
                .as_string()
                .map(str::to_owned)
        })
        .is_some_and(|id| id == game.id().key())
}

#[cfg(target_os = "macos")]
fn shell_arg(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

async fn write_shortcut(path: &Path, game: &GameInfo, launcher: &LauncherCommand) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    #[cfg(windows)]
    {
        shell_link(path, game, Some(launcher)).await?;
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt as _;
        ensure!(
            !path.exists() || owned_bundle(path, game),
            "This app bundle belongs to another application"
        );
        tokio::fs::create_dir_all(path.join("Contents/MacOS")).await?;
        let argv = launcher
            .launch_args(&game.id())
            .iter()
            .map(|a| shell_arg(a))
            .collect::<Vec<_>>()
            .join(" ");
        let script = path.join("Contents/MacOS/launch");
        crate::util::write_atomic(&script, format!("#!/bin/sh\nexec {argv}\n").as_bytes()).await?;
        tokio::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).await?;
        let mut info = plist::Dictionary::new();
        for (key, value) in [
            ("CFBundleName", game.title.clone()),
            ("CFBundleExecutable", "launch".into()),
            ("CFBundlePackageType", "APPL".into()),
            (
                "CFBundleIdentifier",
                format!("app.irori.game.{:x}", md5::compute(game.id().key())),
            ),
            ("IroriGame", game.id().key()),
        ] {
            info.insert(key.into(), plist::Value::String(value));
        }
        let mut bytes = Vec::new();
        plist::Value::Dictionary(info).to_writer_xml(&mut bytes)?;
        crate::util::write_atomic(&path.join("Contents/Info.plist"), &bytes).await?;
    }
    Ok(())
}

pub async fn create(
    game: &GameInfo,
    launcher: &LauncherCommand,
    _icon: Option<&Path>,
    desktop: bool,
    menu: bool,
) -> Result<CreatedEntries> {
    let _guard = LOCK.lock().await;
    let mut state = manifest().await;
    let mut created = CreatedEntries::default();
    for (enabled, dir, dest) in [
        (desktop, dirs::desktop_dir(), &mut created.desktop),
        (menu, Some(menu_dir()?), &mut created.menu),
    ] {
        if !enabled {
            continue;
        }
        let path = dir
            .context("No desktop folder is configured")?
            .join(file_name(game));
        write_shortcut(&path, game, launcher).await?;
        let files = state.games.entry(game.id().key()).or_default();
        if !files.contains(&path) {
            files.push(path.clone());
        }
        // Persist each successful write so partial failures remain removable.
        save(&state).await?;
        *dest = Some(path);
    }
    Ok(created)
}

pub async fn remove(game: &GameInfo) -> Result<Vec<PathBuf>> {
    let _guard = LOCK.lock().await;
    let mut state = manifest().await;
    let candidates = state.games.remove(&game.id().key()).unwrap_or_default();
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for path in candidates {
        if !path.exists() {
            continue;
        }
        #[cfg(windows)]
        let owned = shell_link(&path, game, None).await.unwrap_or(false);
        #[cfg(target_os = "macos")]
        let owned = owned_bundle(&path, game);
        if !owned {
            continue;
        }
        #[cfg(windows)]
        let result = tokio::fs::remove_file(&path).await;
        #[cfg(target_os = "macos")]
        let result = tokio::fs::remove_dir_all(&path).await;
        if result.is_ok() {
            removed.push(path);
        } else {
            failed.push(path);
        }
    }
    if !failed.is_empty() {
        state.games.insert(game.id().key(), failed);
    }
    save(&state).await?;
    Ok(removed)
}

pub async fn exists(game: &GameInfo) -> bool {
    manifest()
        .await
        .games
        .get(&game.id().key())
        .is_some_and(|files| files.iter().any(|p| p.exists()))
}

pub async fn register_url_scheme(launcher: &LauncherCommand) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        // LaunchServices reads the URL declaration in the packaged Info.plist.
        // A standalone development binary has no bundle to register.
        let bin = Path::new(&launcher.program);
        if let Some(bundle) = bin
            .ancestors()
            .find(|p| p.extension().is_some_and(|e| e == "app"))
        {
            let result = irori_core::process::run(&CommandSpec::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
                .arg("-f").arg(bundle), RunOptions::default()).await?;
            ensure!(
                result.success(),
                "Could not register Irori URL handler: {}",
                result.error_summary()
            );
        }
    }
    #[cfg(windows)]
    {
        let command = std::iter::once(launcher.program.as_str())
            .chain(launcher.prefix_args.iter().map(String::as_str))
            .chain(std::iter::once("%1"))
            .map(irori_core::process::windows_arg)
            .collect::<Vec<_>>()
            .join(" ");
        for (key, name, value) in [
            (r"HKCU\Software\Classes\irori", None, "URL:Irori"),
            (r"HKCU\Software\Classes\irori", Some("URL Protocol"), ""),
            (
                r"HKCU\Software\Classes\irori\shell\open\command",
                None,
                command.as_str(),
            ),
        ] {
            let mut spec =
                CommandSpec::new("reg.exe").args(["add", key, "/f", "/t", "REG_SZ", "/d", value]);
            spec = if let Some(name) = name {
                spec.args(["/v", name])
            } else {
                spec.arg("/ve")
            };
            let result = irori_core::process::run(&spec, RunOptions::default()).await?;
            ensure!(
                result.success(),
                "Could not register Irori URL handler: {}",
                result.error_summary()
            );
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bundle_preserves_arguments_and_checks_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let game = GameInfo {
            app_name: "quote' space".into(),
            title: "../Game".into(),
            ..Default::default()
        };
        let launcher = LauncherCommand {
            program: "/tmp/a'b/irori".into(),
            prefix_args: vec![],
            working_dir: "/tmp".into(),
        };
        let path = temp.path().join(file_name(&game));
        write_shortcut(&path, &game, &launcher).await.unwrap();
        assert!(owned_bundle(&path, &game));
        let exe = irori_core::platform::bundle_executable(&path).unwrap();
        let script = std::fs::read_to_string(exe).unwrap();
        assert!(script.contains("'/tmp/a'\\''b/irori'"));
        let other = GameInfo {
            app_name: "other".into(),
            ..game.clone()
        };
        assert!(write_shortcut(&path, &other, &launcher).await.is_err());
    }
}
