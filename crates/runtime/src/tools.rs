//! Prefix tools: DXVK, VKD3D-Proton and DXVK-NVAPI (download and per-prefix
//! install/remove), winetricks, winecfg, regedit, running an exe and opening
//! the prefix folder.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, bail};
use irori_core::process::{self, CommandSpec, ExecResult, RunOptions};
use irori_core::{AppSettings, GameSettings, OpContext, WineKind};
use serde::Deserialize;

use crate::download;
use crate::env::{self, HostEnv};
use crate::locations;
use crate::prefix::{self, WineCommand, WineContext};
use crate::sysinfo;

/// A DLL-replacement tool installed into Wine prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DxTool {
    Dxvk,
    Vkd3d,
    DxvkNvapi,
}

impl DxTool {
    pub const ALL: [DxTool; 3] = [DxTool::Dxvk, DxTool::Vkd3d, DxTool::DxvkNvapi];

    /// Directory and marker name (`dxvk`, `vkd3d`, `dxvk-nvapi`).
    pub fn id(self) -> &'static str {
        match self {
            DxTool::Dxvk => "dxvk",
            DxTool::Vkd3d => "vkd3d",
            DxTool::DxvkNvapi => "dxvk-nvapi",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DxTool::Dxvk => "DXVK",
            DxTool::Vkd3d => "VKD3D-Proton",
            DxTool::DxvkNvapi => "DXVK-NVAPI",
        }
    }

    /// Folder holding the 32-bit DLLs.
    fn dir32(self) -> &'static str {
        match self {
            DxTool::Vkd3d => "x86",
            _ => "x32",
        }
    }

    /// Leading path components to drop when extracting.
    fn strip(self) -> usize {
        match self {
            DxTool::DxvkNvapi => 0,
            _ => 1,
        }
    }

    /// Release endpoint; old Vulkan drivers (1.1/1.2) get the last builds
    /// that support them.
    fn url(self, vulkan: Option<(u32, u32)>) -> &'static str {
        let legacy = matches!(vulkan, Some((1, minor)) if minor < 3);
        match (self, legacy) {
            (DxTool::Dxvk, false) => "https://api.github.com/repos/doitsujin/dxvk/releases/latest",
            (DxTool::Dxvk, true) => {
                "https://api.github.com/repos/doitsujin/dxvk/releases/tags/v1.10.3"
            }
            (DxTool::Vkd3d, false) => {
                "https://api.github.com/repos/Heroic-Games-Launcher/vkd3d-proton/releases/latest"
            }
            (DxTool::Vkd3d, true) => {
                "https://api.github.com/repos/Heroic-Games-Launcher/vkd3d-proton/releases/tags/v2.6"
            }
            (DxTool::DxvkNvapi, _) => {
                "https://api.github.com/repos/jp7677/dxvk-nvapi/releases/latest"
            }
        }
    }
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    #[serde(default)]
    assets: Vec<GhAsset>,
}

fn strip_archive_ext(name: &str) -> &str {
    name.strip_suffix(".tar.gz")
        .or_else(|| name.strip_suffix(".tar.xz"))
        .or_else(|| name.strip_suffix(".tar.zst"))
        .unwrap_or(name)
}

fn marker(dir: &Path, tool: DxTool) -> Option<String> {
    std::fs::read_to_string(dir.join(format!("latest_{}", tool.id())))
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
        .filter(|s| !s.is_empty())
}

/// Irori's directory for a tool.
pub fn tool_dir(tool: DxTool) -> PathBuf {
    locations::tools_dir().join(tool.id())
}

/// The downloaded build to install from: Irori's, else Heroic's.
/// Returns `(version, version_dir)`.
pub fn tool_source(tool: DxTool) -> Option<(String, PathBuf)> {
    let mut roots = vec![tool_dir(tool)];
    if let Some(h) = locations::heroic_tools_dir() {
        roots.push(h.join(tool.id()));
    }
    roots.into_iter().find_map(|root| {
        let version = marker(&root, tool)?;
        let dir = root.join(&version);
        dir.is_dir().then_some((version, dir))
    })
}

/// Download the newest build of `tool` unless it is already current
/// (Heroic's `installOrUpdateTool`). Returns the installed version.
pub async fn update_tool(tool: DxTool, ctx: &OpContext) -> Result<String> {
    let vulkan = sysinfo::vulkan_api_version().await;
    if vulkan.is_none() {
        tracing::debug!("Vulkan version unknown; using the latest {}", tool.label());
    }
    let release: GhRelease = download::get_json(tool.url(vulkan))
        .await
        .with_context(|| format!("could not check for {} updates", tool.label()))?;
    let asset = release
        .assets
        .iter()
        .find(|a| {
            a.name.ends_with(".tar.gz")
                || a.name.ends_with(".tar.xz")
                || a.name.ends_with(".tar.zst")
        })
        .with_context(|| format!("the {} release has no archive", tool.label()))?;
    let version = strip_archive_ext(&asset.name).to_string();
    let root = tool_dir(tool);
    let dest = root.join(&version);
    if marker(&root, tool).as_deref() == Some(version.as_str()) && dest.is_dir() {
        return Ok(version);
    }
    tokio::fs::create_dir_all(&root)
        .await
        .with_context(|| format!("creating {}", root.display()))?;
    (ctx.step)(&format!("Downloading {}", tool.label()));
    let archive = root.join(&asset.name);
    download::download(
        &asset.browser_download_url,
        &archive,
        &ctx.progress,
        &ctx.cancel,
    )
    .await
    .with_context(|| format!("Error installing {}! Check your connection!", tool.label()))?;
    let staging = root.join(format!(".{version}.extracting"));
    let _ = tokio::fs::remove_dir_all(&staging).await;
    let (a, s, strip, cancel, progress) = (
        archive.clone(),
        staging.clone(),
        tool.strip(),
        ctx.cancel.clone(),
        ctx.progress.clone(),
    );
    let res = tokio::task::spawn_blocking(move || {
        download::extract_tar(&a, &s, strip, progress, &cancel)
    })
    .await
    .context("extraction task failed")
    .and_then(|r| r);
    let _ = tokio::fs::remove_file(&archive).await;
    if let Err(e) = res {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(e).with_context(|| format!("extracting {}", tool.label()));
    }
    let _ = tokio::fs::remove_dir_all(&dest).await;
    tokio::fs::rename(&staging, &dest).await?;
    tokio::fs::write(root.join(format!("latest_{}", tool.id())), &version).await?;
    Ok(version)
}

/// Update DXVK, VKD3D-Proton and DXVK-NVAPI concurrently (startup task).
/// Errors of single tools are logged.
pub async fn update_all_tools(ctx: &OpContext) {
    let results = futures::future::join_all(DxTool::ALL.iter().map(|t| update_tool(*t, ctx))).await;
    for (tool, res) in DxTool::ALL.iter().zip(results) {
        if let Err(e) = res {
            tracing::warn!("{}: {e:#}", tool.label());
        }
    }
}

async fn dll_names(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return out;
    };
    while let Ok(Some(e)) = rd.next_entry().await {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.to_ascii_lowercase().ends_with(".dll") {
            out.push(name);
        }
    }
    out.sort();
    out
}

fn dll_stem(name: &str) -> &str {
    let n = name.len();
    if n > 4 && name[n - 4..].eq_ignore_ascii_case(".dll") {
        &name[..n - 4]
    } else {
        name
    }
}

/// Escape a value for a REGEDIT4 string.
fn reg_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A `.reg` document setting (`Some`) or deleting (`None`) DLL overrides,
/// plus extra `(key, name, value)` entries.
pub fn dll_override_reg(
    overrides: &[(String, Option<String>)],
    extra: &[(String, String, String)],
) -> String {
    let mut out = String::from("REGEDIT4\n\n[HKEY_CURRENT_USER\\Software\\Wine\\DllOverrides]\n");
    for (dll, mode) in overrides {
        match mode {
            Some(mode) => out.push_str(&format!(
                "\"{}\"=\"{}\"\n",
                reg_escape(dll),
                reg_escape(mode)
            )),
            None => out.push_str(&format!("\"{}\"=-\n", reg_escape(dll))),
        }
    }
    for (key, name, value) in extra {
        out.push_str(&format!(
            "\n[{key}]\n\"{}\"=\"{}\"\n",
            reg_escape(name),
            reg_escape(value)
        ));
    }
    out
}

/// Import a `.reg` document into the prefix with `regedit /S`, waiting for
/// the registry to be written.
async fn import_reg(
    app: &AppSettings,
    settings: &GameSettings,
    wc: &WineContext,
    content: &str,
    ctx: &OpContext,
) -> Result<()> {
    let temp = wc.wine_prefix_dir().join("drive_c/windows/temp");
    tokio::fs::create_dir_all(&temp).await?;
    let name = format!("irori-{}.reg", std::process::id());
    let file = temp.join(&name);
    tokio::fs::write(&file, content).await?;
    let mut cmd = WineCommand::new(["regedit", "/S", &format!("C:\\windows\\temp\\{name}")]);
    cmd.wait = true;
    cmd.skip_prefix_check = true;
    let res = prefix::run_wine(app, settings, wc, cmd, ctx).await;
    let _ = tokio::fs::remove_file(&file).await;
    let res = res?;
    if res.aborted {
        return Err(download::Cancelled.into());
    }
    if !res.success() {
        bail!("regedit failed: {}", res.error_summary());
    }
    Ok(())
}

/// Locations of NVIDIA's `nvngx.dll` (DLSS) shipped with the driver.
fn nvngx_dir() -> Option<PathBuf> {
    [
        "/usr/lib/nvidia/wine",
        "/usr/lib64/nvidia/wine",
        "/usr/lib/x86_64-linux-gnu/nvidia/wine",
        "/usr/lib/x86_64-linux-gnu/nvidia/current/wine",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|d| d.join("nvngx.dll").is_file())
}

/// Whether DLL tools can be installed into this prefix: a plain Wine prefix
/// (Proton manages its own DXVK/VKD3D).
fn supports_dll_tools(wc: &WineContext) -> bool {
    if wc.wine.bin.contains("toolkit") || wc.wine.kind == WineKind::Proton {
        return false;
    }
    if !wc.prefix.join(".update-timestamp").exists() {
        tracing::warn!("DXVK cannot be installed on a Proton or a invalid prefix!");
        return false;
    }
    true
}

/// Install `tools` into a Wine prefix: copy their DLLs and register
/// `native,builtin` overrides, all awaited (Heroic fires these off without
/// waiting). Tools already at the current version are skipped.
pub async fn install_tools(
    app: &AppSettings,
    settings: &GameSettings,
    wc: &WineContext,
    tools: &[DxTool],
    ctx: &OpContext,
) -> Result<()> {
    if tools.is_empty() || !supports_dll_tools(wc) {
        return Ok(());
    }
    let prefix = &wc.prefix;
    let system32 = prefix.join("drive_c/windows/system32");
    let syswow64 = prefix.join("drive_c/windows/syswow64");
    let is64 = syswow64.is_dir();
    if !is64 {
        tracing::warn!("32-bit prefix detected!");
    }
    let mut overrides: Vec<(String, Option<String>)> = Vec::new();
    let mut extra = Vec::new();
    let mut done: Vec<(DxTool, String)> = Vec::new();
    for &tool in tools {
        let source = match tool_source(tool) {
            Some(s) => s,
            None => {
                update_tool(tool, ctx).await?;
                tool_source(tool).with_context(|| format!("{} is not available", tool.label()))?
            }
        };
        let (version, dir) = source;
        let current = tokio::fs::read_to_string(prefix.join(format!("current_{}", tool.id())))
            .await
            .ok()
            .and_then(|s| s.lines().next().map(|l| l.trim().to_string()));
        if current.as_deref() == Some(version.as_str()) {
            tracing::debug!("{} already installed!", tool.id());
            continue;
        }
        (ctx.step)(&format!("Installing {}", tool.label()));
        let dlls32 = dll_names(&dir.join(tool.dir32())).await;
        let dlls64 = dll_names(&dir.join("x64")).await;
        if dlls32.is_empty() && dlls64.is_empty() {
            bail!("{} {version} contains no DLLs", tool.label());
        }
        tokio::fs::create_dir_all(&system32).await?;
        let copies: Vec<(PathBuf, PathBuf)> = if is64 {
            dlls32
                .iter()
                .map(|d| (dir.join(tool.dir32()).join(d), syswow64.join(d)))
                .chain(
                    dlls64
                        .iter()
                        .map(|d| (dir.join("x64").join(d), system32.join(d))),
                )
                .collect()
        } else {
            dlls32
                .iter()
                .map(|d| (dir.join(tool.dir32()).join(d), system32.join(d)))
                .collect()
        };
        for (from, to) in copies {
            let _ = tokio::fs::remove_file(&to).await;
            tokio::fs::copy(&from, &to)
                .await
                .with_context(|| format!("copying {} into the prefix", from.display()))?;
        }
        let names = if is64 {
            dlls64.iter().chain(dlls32.iter()).collect::<Vec<_>>()
        } else {
            dlls32.iter().collect()
        };
        for name in names {
            let stem = dll_stem(name).to_string();
            if !overrides.iter().any(|(d, _)| *d == stem) {
                overrides.push((stem, Some("native,builtin".into())));
            }
        }
        if tool == DxTool::DxvkNvapi {
            match nvngx_dir() {
                Some(nv) => {
                    for f in ["nvngx.dll", "_nvngx.dll"] {
                        let from = nv.join(f);
                        if from.is_file() {
                            let _ = tokio::fs::copy(&from, system32.join(f)).await;
                        }
                    }
                    extra.push((
                        "HKEY_LOCAL_MACHINE\\SOFTWARE\\NVIDIA Corporation\\Global\\NGXCore"
                            .to_string(),
                        "FullPath".to_string(),
                        "C:\\windows\\system32".to_string(),
                    ));
                }
                None => tracing::debug!("Could not find nvngx.dll for DLSS!"),
            }
        }
        done.push((tool, version));
    }
    if done.is_empty() {
        return Ok(());
    }
    import_reg(
        app,
        settings,
        wc,
        &dll_override_reg(&overrides, &extra),
        ctx,
    )
    .await?;
    for (tool, version) in done {
        tokio::fs::write(prefix.join(format!("current_{}", tool.id())), version).await?;
    }
    Ok(())
}

/// Remove a tool from a Wine prefix: delete its overrides and DLLs (from
/// both `system32` and `syswow64`, which Heroic forgets) and restore Wine's
/// builtin DLLs with `wineboot -u`.
pub async fn remove_tool(
    app: &AppSettings,
    settings: &GameSettings,
    wc: &WineContext,
    tool: DxTool,
    ctx: &OpContext,
) -> Result<()> {
    if !supports_dll_tools(wc) {
        return Ok(());
    }
    let prefix = &wc.prefix;
    let _ = tokio::fs::remove_file(prefix.join(format!("current_{}", tool.id()))).await;
    let Some((_, dir)) = tool_source(tool) else {
        return Ok(());
    };
    (ctx.step)(&format!("Removing {}", tool.label()));
    let dlls32 = dll_names(&dir.join(tool.dir32())).await;
    let dlls64 = dll_names(&dir.join("x64")).await;
    let system32 = prefix.join("drive_c/windows/system32");
    let syswow64 = prefix.join("drive_c/windows/syswow64");
    let is64 = syswow64.is_dir();
    let overrides: Vec<(String, Option<String>)> = dlls64
        .iter()
        .filter(|_| is64)
        .chain(dlls32.iter())
        .map(|d| (dll_stem(d).to_string(), None))
        .collect();
    import_reg(app, settings, wc, &dll_override_reg(&overrides, &[]), ctx).await?;
    if is64 {
        for d in &dlls64 {
            let _ = tokio::fs::remove_file(system32.join(d)).await;
        }
        for d in &dlls32 {
            let _ = tokio::fs::remove_file(syswow64.join(d)).await;
        }
    } else {
        for d in &dlls32 {
            let _ = tokio::fs::remove_file(system32.join(d)).await;
        }
    }
    let mut cmd = WineCommand::new(["wineboot", "-u"]);
    cmd.wait = true;
    cmd.skip_prefix_check = true;
    prefix::run_wine(app, settings, wc, cmd, ctx).await?;
    Ok(())
}

// ---- winetricks -----------------------------------------------------------

const WINETRICKS_URL: &str =
    "https://raw.githubusercontent.com/Winetricks/winetricks/master/src/winetricks";
const WINETRICKS_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// Download the winetricks script into Irori's tools directory when it is
/// missing (or, with `refresh`, older than 7 days). Falls back to an existing
/// copy, then to a system `winetricks`.
pub async fn ensure_winetricks(refresh: bool) -> Result<PathBuf> {
    anyhow::ensure!(
        !cfg!(windows),
        "Winetricks is not used for native Windows games"
    );
    let path = locations::winetricks_path();
    let fresh = tokio::fs::metadata(&path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .map(|age| age < WINETRICKS_MAX_AGE);
    match fresh {
        Some(true) => return Ok(path),
        Some(false) if !refresh => return Ok(path),
        _ => {}
    }
    match download::get_text(WINETRICKS_URL).await {
        Ok(script) if script.starts_with("#!") => {
            let bytes = script.into_bytes();
            let p = path.clone();
            tokio::task::spawn_blocking(move || irori_core::json_store::write_bytes(&p, &bytes))
                .await
                .context("saving winetricks")??;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).await?;
            }
            Ok(path)
        }
        other => {
            if let Err(e) = &other {
                tracing::warn!("could not download winetricks: {e:#}");
            }
            if path.is_file() {
                return Ok(path);
            }
            which::which("winetricks").context("winetricks could not be downloaded")
        }
    }
}

/// Missing helper programs winetricks relies on.
pub fn winetricks_missing_dependencies() -> Vec<&'static str> {
    ["7z", "cabextract", "zenity", "unzip", "curl"]
        .into_iter()
        .filter(|d| which::which(d).is_err())
        .collect()
}

/// Run winetricks for a prefix (Heroic's `Winetricks.runWithArgs`). With no
/// `verbs` the GUI opens; otherwise the verbs are installed unattended
/// (`-q`). With umu, winetricks runs through `umu-run winetricks`.
pub async fn run_winetricks(
    app: &AppSettings,
    settings: &GameSettings,
    wc: &WineContext,
    game_id: &str,
    verbs: &[String],
    ctx: &OpContext,
) -> Result<ExecResult> {
    let gui = verbs.is_empty();
    let (program, args): (PathBuf, Vec<String>) = if wc.uses_umu {
        if gui {
            (wc.umu.clone(), vec![String::new()])
        } else {
            let mut a = vec!["winetricks".to_string()];
            a.extend(verbs.iter().cloned());
            (wc.umu.clone(), a)
        }
    } else {
        let script = ensure_winetricks(false).await?;
        let mut a = vec!["-q".to_string()];
        if gui {
            a.push("--gui".into());
        } else {
            a.extend(verbs.iter().cloned());
        }
        (script, a)
    };

    let (wine, wine_prefix) = prefix::wine_from_proton(wc);
    let inner = WineContext {
        wine: wine.clone(),
        prefix: wine_prefix.clone(),
        uses_umu: wc.uses_umu,
        umu: wc.umu.clone(),
    };
    let host = HostEnv::current();
    let mut vars = env::generic_env(settings, None, None, &host);
    let effective = GameSettings {
        wine_version: wine.clone(),
        ..settings.clone()
    };
    env::layer(
        &mut vars,
        prefix::wine_env_for(app, &effective, &inner, game_id, &host),
    );
    let wine_dir = Path::new(&wine.bin)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = std::env::var("PATH").unwrap_or_default();
    vars.insert(
        "WINEPREFIX".into(),
        wine_prefix.to_string_lossy().into_owned(),
    );
    vars.insert("PATH".into(), format!("{wine_dir}:{path}"));
    vars.insert(
        "GAMEID".into(),
        if gui { "winetricks-gui" } else { "umu-0" }.into(),
    );
    vars.insert("UMU_RUNTIME_UPDATE".into(), "0".into());
    if !wc.uses_umu && wine.kind == WineKind::Wine {
        vars.insert("WINE".into(), wine.bin.clone());
        if let Some(server) = wine.wineserver.as_deref().filter(|s| !s.is_empty()) {
            vars.insert("WINESERVER".into(), server.to_string());
        }
    }

    for dep in winetricks_missing_dependencies() {
        let msg = format!(
            "{dep} not installed! Winetricks might fail to install some packages or even open"
        );
        tracing::warn!("{msg}");
        (ctx.step)(&msg);
    }
    if let Some(v) = verbs.first() {
        (ctx.step)(&format!("Installing {v}"));
    }
    let spec = CommandSpec::new(program).args(args).envs(vars);
    let step = ctx.step.clone();
    let mut opts = RunOptions::default()
        .cancel(ctx.cancel.clone())
        .on_line(move |_, line| {
            let line = line.trim();
            if !line.is_empty() {
                step(line);
            }
        });
    if let Some(log) = &ctx.log_file {
        opts = opts.log_file(log);
    }
    process::run(&spec, opts).await.context("Winetricks error")
}

/// Verbs recorded in the prefix's `winetricks.log`.
pub async fn winetricks_installed(wc: &WineContext) -> Vec<String> {
    let (_, prefix) = prefix::wine_from_proton(wc);
    tokio::fs::read_to_string(prefix.join("winetricks.log"))
        .await
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `winetricks dlls list` / `fonts list` output: the first token of
/// each line (Heroic splits per pipe chunk instead, losing entries).
pub fn parse_winetricks_list(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|t| !t.is_empty() && !t.starts_with("Using") && !t.starts_with("====="))
        .map(str::to_string)
        .collect()
}

// ---- convenience tools ----------------------------------------------------

/// Start a Windows program in the prefix without waiting (`winecfg`,
/// `regedit`, an exe). Exes start in their own folder.
pub async fn launch_in_prefix(
    app: &AppSettings,
    settings: &GameSettings,
    wc: &WineContext,
    argv: Vec<String>,
    ctx: &OpContext,
) -> Result<ExecResult> {
    let mut cmd = WineCommand::new(argv);
    if let Some(first) = cmd.argv.first() {
        let p = Path::new(first);
        if p.is_absolute() && p.is_file() {
            cmd.cwd = p.parent().map(Path::to_path_buf);
        }
    }
    cmd.detach = true;
    prefix::run_wine(app, settings, wc, cmd, ctx).await
}

/// Open the prefix's folder (the real Wine prefix for Proton) in the file
/// manager.
pub fn open_prefix_folder(wc: &WineContext) -> Result<()> {
    let dir = wc.wine_prefix_dir();
    let dir = if dir.is_dir() { dir } else { wc.prefix.clone() };
    if !dir.is_dir() {
        bail!("The prefix {} does not exist yet", dir.display());
    }
    open::that_detached(&dir).with_context(|| format!("opening {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reg_documents() {
        let doc = dll_override_reg(
            &[
                ("d3d11".into(), Some("native,builtin".into())),
                ("dxgi".into(), None),
            ],
            &[(
                "HKEY_LOCAL_MACHINE\\SOFTWARE\\NVIDIA Corporation\\Global\\NGXCore".into(),
                "FullPath".into(),
                "C:\\windows\\system32".into(),
            )],
        );
        assert_eq!(
            doc,
            "REGEDIT4\n\n[HKEY_CURRENT_USER\\Software\\Wine\\DllOverrides]\n\"d3d11\"=\"native,builtin\"\n\"dxgi\"=-\n\n[HKEY_LOCAL_MACHINE\\SOFTWARE\\NVIDIA Corporation\\Global\\NGXCore]\n\"FullPath\"=\"C:\\\\windows\\\\system32\"\n"
        );
    }

    #[test]
    fn names() {
        assert_eq!(dll_stem("d3d11.dll"), "d3d11");
        assert_eq!(dll_stem("NVAPI64.DLL"), "NVAPI64");
        assert_eq!(strip_archive_ext("dxvk-2.5.tar.gz"), "dxvk-2.5");
        assert_eq!(DxTool::Vkd3d.dir32(), "x86");
        assert_eq!(DxTool::DxvkNvapi.strip(), 0);
        assert!(DxTool::Dxvk.url(Some((1, 2))).ends_with("v1.10.3"));
        assert!(DxTool::Dxvk.url(Some((1, 4))).ends_with("latest"));
        assert!(DxTool::Vkd3d.url(None).ends_with("latest"));
    }

    #[tokio::test]
    async fn only_dlls_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["nvapi64.dll", "nvapi64-tests.exe", "nvofapi64.dll"] {
            std::fs::write(dir.path().join(f), b"").unwrap();
        }
        assert_eq!(
            dll_names(dir.path()).await,
            vec!["nvapi64.dll", "nvofapi64.dll"]
        );
        assert!(dll_names(&dir.path().join("missing")).await.is_empty());
    }

    #[test]
    fn winetricks_lists() {
        let out = "Using winetricks 20250102 - sha256sum: abc\n===== dlls =====\nd3dx9                    MS d3dx9_??.dll\nvcrun2019  Visual C++ 2015-2019\n\n";
        assert_eq!(parse_winetricks_list(out), vec!["d3dx9", "vcrun2019"]);
    }
}
