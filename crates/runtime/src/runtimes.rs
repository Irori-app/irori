//! Runtimes published by Lutris (umu-run, the EAC and BattlEye runtimes) and
//! umu resolution.
//!
//! Heroic queries the Lutris runtime list on every launch to decide whether a
//! runtime is current. Irori caches the list in memory for a few hours and
//! treats an installed runtime as usable when the list cannot be fetched.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use irori_core::{GameSettings, OpContext, WineKind};
use parking_lot::Mutex;
use serde::Deserialize;

use crate::download;
use crate::locations;

const RUNTIMES_URL: &str = "https://lutris.net/api/runtimes";
const LIST_TTL: Duration = Duration::from_secs(6 * 3600);

/// A runtime Irori can download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeName {
    /// umu-launcher (`umu_run.py`).
    Umu,
    /// Easy Anti-Cheat runtime for Proton.
    EacRuntime,
    /// BattlEye runtime for Proton.
    BattlEyeRuntime,
}

impl RuntimeName {
    /// Name in the Lutris runtime list and directory name on disk.
    pub fn id(self) -> &'static str {
        match self {
            RuntimeName::Umu => "umu",
            RuntimeName::EacRuntime => "eac_runtime",
            RuntimeName::BattlEyeRuntime => "battleye_runtime",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RuntimeName::Umu => "umu-launcher",
            RuntimeName::EacRuntime => "Easy Anti-Cheat runtime",
            RuntimeName::BattlEyeRuntime => "BattlEye runtime",
        }
    }
}

/// One entry of the Lutris runtime list.
#[derive(Debug, Clone, Deserialize)]
pub struct Runtime {
    pub name: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub url: Option<String>,
}

static LIST: Mutex<Option<(Instant, Vec<Runtime>)>> = Mutex::new(None);

/// The Lutris runtime list, cached for six hours.
pub async fn runtime_list(force: bool) -> Result<Vec<Runtime>> {
    if !force
        && let Some((at, list)) = LIST.lock().as_ref()
        && at.elapsed() < LIST_TTL
    {
        return Ok(list.clone());
    }
    let list: Vec<Runtime> = download::get_json(RUNTIMES_URL)
        .await
        .context("could not fetch the runtime list from lutris.net")?;
    *LIST.lock() = Some((Instant::now(), list.clone()));
    Ok(list)
}

/// Irori's directory of a runtime.
pub fn runtime_dir(name: RuntimeName) -> PathBuf {
    locations::runtimes_dir().join(name.id())
}

/// The runtime directory to point Proton at: Irori's when installed, else
/// Heroic's when the user already has it there, else Irori's (which a launch
/// downloads).
pub fn effective_runtime_dir(name: RuntimeName) -> PathBuf {
    let own = runtime_dir(name);
    if own.join("current_version").is_file() {
        return own;
    }
    if let Some(heroic) = locations::heroic_tools_dir() {
        let theirs = heroic.join("runtimes").join(name.id());
        if theirs.join("current_version").is_file() {
            return theirs;
        }
    }
    own
}

fn installed_version(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("current_version"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Whether the runtime is installed and current. Offline (or when the list no
/// longer has the runtime) an installed runtime counts as current.
pub async fn is_installed(name: RuntimeName) -> bool {
    let dir = effective_runtime_dir(name);
    if !dir.is_dir() {
        return false;
    }
    let Some(local) = installed_version(&dir) else {
        return false;
    };
    match runtime_list(false).await {
        Ok(list) => match list.iter().find(|r| r.name == name.id()) {
            Some(remote) => remote.created_at == local,
            None => {
                tracing::warn!("runtime {} is no longer listed by lutris.net", name.id());
                true
            }
        },
        Err(e) => {
            tracing::debug!("{e:#}; assuming {} is current", name.id());
            true
        }
    }
}

/// Download and unpack a runtime into Irori's runtimes directory.
pub async fn download(name: RuntimeName, ctx: &OpContext) -> Result<()> {
    let base = locations::runtimes_dir();
    tokio::fs::create_dir_all(&base)
        .await
        .with_context(|| format!("creating {}", base.display()))?;
    let list = runtime_list(true).await?;
    let runtime = list
        .iter()
        .find(|r| r.name == name.id())
        .with_context(|| format!("lutris.net does not provide the {}", name.label()))?;
    let url = runtime
        .url
        .as_deref()
        .filter(|u| !u.is_empty())
        .with_context(|| format!("the {} has no download link", name.label()))?;
    tracing::info!("Downloading runtime {} with download link {url}", name.id());
    let file_name = url.rsplit('/').next().unwrap_or("runtime.tar.xz");
    let archive = base.join(file_name);
    let downloaded = download::download(url, &archive, &ctx.progress, &ctx.cancel).await?;
    let dir = runtime_dir(name);
    let staging = base.join(format!(".{}.extracting", name.id()));
    let _ = tokio::fs::remove_dir_all(&staging).await;
    let (a, s, cancel) = (downloaded.path.clone(), staging.clone(), ctx.cancel.clone());
    let progress = ctx.progress.clone();
    let extracted =
        tokio::task::spawn_blocking(move || download::extract_tar(&a, &s, 1, progress, &cancel))
            .await
            .context("extraction task failed")?;
    let _ = tokio::fs::remove_file(&downloaded.path).await;
    if let Err(e) = extracted {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(e).with_context(|| format!("unpacking the {}", name.label()));
    }
    tokio::fs::write(staging.join("current_version"), &runtime.created_at).await?;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::rename(&staging, &dir)
        .await
        .with_context(|| format!("installing {}", dir.display()))?;
    Ok(())
}

/// Download a runtime unless it is installed and current.
pub async fn ensure(name: RuntimeName, ctx: &OpContext) -> Result<()> {
    anyhow::ensure!(
        cfg!(target_os = "linux"),
        "This runtime is only available on Linux"
    );
    if is_installed(name).await {
        return Ok(());
    }
    (ctx.step)(&format!("Downloading the {}", name.label()));
    download(name, ctx).await
}

/// The umu-run to use: `umu-run` on `PATH`, else Irori's download, else
/// Heroic's download. Returns Irori's default path (possibly missing) when
/// none exists.
pub fn umu_path() -> PathBuf {
    if let Ok(p) = which::which("umu-run") {
        return p;
    }
    let own = locations::default_umu_path();
    if own.is_file() {
        return own;
    }
    if let Some(heroic) = locations::heroic_tools_dir() {
        let theirs = heroic.join("runtimes/umu/umu_run.py");
        if theirs.is_file() {
            return theirs;
        }
    }
    own
}

/// Heroic's `isUmuSupported`: Proton, umu not disabled, and (when
/// `check_installed`) umu-run present.
pub fn is_umu_supported(settings: &GameSettings, check_installed: bool) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    settings.wine_version.kind == WineKind::Proton
        && !settings.disable_umu
        && (!check_installed || umu_path().is_file())
}

/// Make umu-run available: a system or Heroic copy is used as is; Irori's own
/// copy is downloaded or refreshed from lutris.net. Returns the path to run.
pub async fn ensure_umu(ctx: &OpContext) -> Result<PathBuf> {
    anyhow::ensure!(cfg!(target_os = "linux"), "umu is only supported on Linux");
    let path = umu_path();
    if path != locations::default_umu_path() && path.is_file() {
        return Ok(path);
    }
    if path.is_file() && is_installed(RuntimeName::Umu).await {
        return Ok(path);
    }
    match ensure(RuntimeName::Umu, ctx).await {
        Ok(()) => Ok(locations::default_umu_path()),
        Err(e) if path.is_file() => {
            tracing::warn!("could not update umu-run: {e:#}; using the installed copy");
            Ok(path)
        }
        Err(e) => Err(e).context("umu-run is needed for Proton but could not be downloaded"),
    }
}

/// Normalise a Steam runtime setting to umu's `PROTONPATH` value
/// (`sniper` → `umu-sniper`).
pub fn steam_runtime_protonpath(name: &str) -> String {
    let name = name.trim();
    if name.starts_with("umu-") {
        name.to_string()
    } else {
        format!("umu-{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_names() {
        assert_eq!(steam_runtime_protonpath("sniper"), "umu-sniper");
        assert_eq!(steam_runtime_protonpath("umu-soldier"), "umu-soldier");
        assert_eq!(RuntimeName::EacRuntime.id(), "eac_runtime");
    }

    #[test]
    fn parses_runtime_list() {
        let json = r#"[{"id":1,"name":"umu","created_at":"2024-01-14T19:38:26.671443Z","architecture":"x86_64","url":"https://x/umu.tar.xz"},{"name":"other"}]"#;
        let list: Vec<Runtime> = serde_json::from_str(json).unwrap();
        assert_eq!(list[0].url.as_deref(), Some("https://x/umu.tar.xz"));
        assert_eq!(list[1].created_at, "");
    }

    #[test]
    fn umu_support_rules() {
        let mut gs = GameSettings::default();
        gs.wine_version.kind = WineKind::Proton;
        assert_eq!(is_umu_supported(&gs, false), cfg!(target_os = "linux"));
        gs.disable_umu = true;
        assert!(!is_umu_supported(&gs, false));
        gs.disable_umu = false;
        gs.wine_version.kind = WineKind::Wine;
        assert!(!is_umu_supported(&gs, false));
    }
}
