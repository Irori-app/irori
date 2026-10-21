//! System information for the settings page and log headers: CPU, memory,
//! GPUs, distribution, kernel, desktop session, Vulkan and the versions of
//! the gaming tools Ember uses.
//!
//! Everything is read locally (`/proc`, `/sys`, `pci.ids`, short tool
//! probes); unlike Heroic no DNS lookups are made and a GPU whose name is
//! unknown is still listed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ember_core::process::CommandSpec;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use tokio::sync::Mutex;

use crate::discovery::run_with_timeout;

#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuInfo {
    pub model: String,
    /// Logical CPUs.
    pub cores: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryInfo {
    pub total: u64,
    pub used: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GpuInfo {
    /// PCI address (`0000:03:00.0`).
    pub slot: String,
    /// 4-digit lower-case hex ids.
    pub vendor_id: String,
    pub device_id: String,
    pub subvendor_id: Option<String>,
    pub subdevice_id: Option<String>,
    pub vendor_name: Option<String>,
    pub device_name: Option<String>,
    /// Kernel driver (`amdgpu`, `nvidia`, `i915`, …).
    pub driver: Option<String>,
    /// Driver version when the kernel module reports one.
    pub driver_version: Option<String>,
    /// The GPU the firmware used for boot (usually the primary display).
    pub boot_vga: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OsInfo {
    pub name: String,
    pub version: Option<String>,
    pub kernel: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SteamDeckModel {
    Lcd,
    Oled,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct VulkanInfo {
    /// The Vulkan loader library is installed.
    pub loader: bool,
    /// Installable client drivers found (ICD manifest names).
    pub icds: Vec<String>,
    pub instance_version: Option<String>,
    /// `(device name, API version)` from `vulkaninfo --summary`.
    pub devices: Vec<(String, String)>,
}

impl VulkanInfo {
    pub fn available(&self) -> bool {
        self.loader && (!self.icds.is_empty() || !self.devices.is_empty())
    }
}

/// Versions of tools; `None` when not installed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SoftwareVersions {
    pub ember: String,
    pub wine: Option<String>,
    pub gamescope: Option<String>,
    pub gamemode: Option<String>,
    pub mangohud: Option<String>,
    pub umu: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SystemInfo {
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub gpus: Vec<GpuInfo>,
    pub os: OsInfo,
    pub desktop: Option<String>,
    /// `wayland`, `x11`, `tty`…
    pub session_type: Option<String>,
    pub steam_deck: Option<(SteamDeckModel, bool)>,
    pub is_flatpak: bool,
    pub is_appimage: bool,
    pub vulkan: VulkanInfo,
    pub software: SoftwareVersions,
}

static CACHE: Mutex<Option<Arc<SystemInfo>>> = Mutex::const_new(None);

/// System information, computed once per process unless `refresh`.
pub async fn get(refresh: bool) -> Arc<SystemInfo> {
    let mut guard = CACHE.lock().await;
    if !refresh && let Some(info) = guard.as_ref() {
        return info.clone();
    }
    let info = Arc::new(collect().await);
    *guard = Some(info.clone());
    info
}

async fn collect() -> SystemInfo {
    let blocking = tokio::task::spawn_blocking(|| {
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let os_release = ["/run/host/os-release", "/etc/os-release"]
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok());
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let gpus = read_gpus(Path::new("/sys/bus/pci/devices"));
        let vulkan_files = vulkan_files();
        (cpuinfo, meminfo, os_release, kernel, gpus, vulkan_files)
    });
    let (summary, versions) = tokio::join!(vulkan_summary(), software_versions());
    let (cpuinfo, meminfo, os_release, kernel, mut gpus, (loader, icds)) =
        blocking.await.unwrap_or_default();
    if gpus.iter().any(|g| g.device_name.is_none()) {
        name_gpus_with_lspci(&mut gpus).await;
    }
    let cpu = parse_cpu(&cpuinfo);
    let mut os = parse_os_release(os_release.as_deref());
    os.kernel = kernel;
    let steam_deck = steam_deck(&cpu, &gpus);
    let (instance_version, devices) = summary;
    SystemInfo {
        memory: parse_meminfo(&meminfo),
        cpu,
        gpus,
        os,
        desktop: std::env::var("XDG_CURRENT_DESKTOP")
            .ok()
            .filter(|s| !s.is_empty()),
        session_type: std::env::var("XDG_SESSION_TYPE")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var_os("WAYLAND_DISPLAY").map(|_| "wayland".into())),
        steam_deck,
        is_flatpak: std::env::var_os("FLATPAK_ID").is_some(),
        is_appimage: std::env::var_os("APPIMAGE").is_some(),
        vulkan: VulkanInfo {
            loader,
            icds,
            instance_version,
            devices,
        },
        software: versions,
    }
}

/// CPU model (first `model name`) and logical CPU count.
pub fn parse_cpu(cpuinfo: &str) -> CpuInfo {
    let mut model = String::new();
    let mut cores = 0;
    for line in cpuinfo.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        match k.trim() {
            "processor" => cores += 1,
            "model name" | "Model" if model.is_empty() => model = v.trim().to_string(),
            _ => {}
        }
    }
    CpuInfo {
        model,
        cores: cores.max(1),
    }
}

/// `MemTotal` and `MemTotal - MemAvailable`, in bytes.
pub fn parse_meminfo(meminfo: &str) -> MemoryInfo {
    let field = |name: &str| {
        meminfo
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|r| r.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    };
    let total = field("MemTotal:");
    let available = field("MemAvailable:");
    MemoryInfo {
        total,
        used: if total > 0 {
            total.saturating_sub(available)
        } else {
            0
        },
    }
}

/// Distribution from an os-release document (Heroic's rules: `NAME` else
/// `PRETTY_NAME`; `VERSION` else `VERSION_ID VERSION_CODENAME`).
pub fn parse_os_release(text: Option<&str>) -> OsInfo {
    let Some(text) = text else {
        return OsInfo {
            name: "Unknown Linux Distribution".into(),
            ..Default::default()
        };
    };
    let get = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
            .map(|v| {
                let v = v.trim();
                v.strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(v)
                    .to_string()
            })
            .filter(|v| !v.is_empty())
    };
    let name = get("NAME")
        .or_else(|| get("PRETTY_NAME"))
        .unwrap_or_else(|| "Linux".into());
    let version = get("VERSION").or_else(|| match (get("VERSION_ID"), get("VERSION_CODENAME")) {
        (Some(id), Some(code)) => Some(format!("{id} {code}")),
        _ => None,
    });
    OsInfo {
        name,
        version,
        kernel: String::new(),
    }
}

fn read_hex(path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim().trim_start_matches("0x");
    Some(format!("{:0>4}", s.to_ascii_lowercase()))
}

/// Display-class PCI devices from sysfs, boot GPU first, named from the local
/// `pci.ids` database.
pub fn read_gpus(root: &Path) -> Vec<GpuInfo> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut gpus: Vec<GpuInfo> = rd
        .flatten()
        .filter_map(|e| {
            let dir = e.path();
            let class = std::fs::read_to_string(dir.join("class")).ok()?;
            if !class.trim().starts_with("0x03") {
                return None;
            }
            let driver = std::fs::read_link(dir.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            let driver_version = driver.as_deref().and_then(driver_version);
            Some(GpuInfo {
                slot: e.file_name().to_string_lossy().into_owned(),
                vendor_id: read_hex(&dir.join("vendor"))?,
                device_id: read_hex(&dir.join("device"))?,
                subvendor_id: read_hex(&dir.join("subsystem_vendor")),
                subdevice_id: read_hex(&dir.join("subsystem_device")),
                boot_vga: std::fs::read_to_string(dir.join("boot_vga"))
                    .is_ok_and(|s| s.trim() == "1"),
                driver,
                driver_version,
                ..Default::default()
            })
        })
        .collect();
    gpus.sort_by(|a, b| b.boot_vga.cmp(&a.boot_vga).then(a.slot.cmp(&b.slot)));
    if let Some(ids) = pci_ids_file()
        && let Ok(text) = std::fs::read_to_string(ids)
    {
        name_gpus(&text, &mut gpus);
    }
    gpus
}

fn driver_version(driver: &str) -> Option<String> {
    if driver == "nvidia"
        && let Ok(text) = std::fs::read_to_string("/proc/driver/nvidia/version")
    {
        static NV: Lazy<Regex> = Lazy::new(|| {
            Regex::new(r"Kernel Module\s+(?:for \S+\s+)?([\d.]+)").expect("valid regex")
        });
        if let Some(c) = NV.captures(&text) {
            return Some(c[1].to_string());
        }
    }
    std::fs::read_to_string(format!("/sys/module/{driver}/version"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn pci_ids_file() -> Option<PathBuf> {
    [
        "/usr/share/hwdata/pci.ids",
        "/usr/share/misc/pci.ids",
        "/usr/share/pci.ids",
        "/run/host/usr/share/hwdata/pci.ids",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// Fill vendor/device names from a `pci.ids` document. The subsystem name is
/// preferred for the device when listed (it carries the board name).
pub fn name_gpus(pci_ids: &str, gpus: &mut [GpuInfo]) {
    let wanted: HashMap<&str, ()> = gpus.iter().map(|g| (g.vendor_id.as_str(), ())).collect();
    let mut vendor_names: HashMap<String, String> = HashMap::new();
    let mut device_names: HashMap<(String, String), String> = HashMap::new();
    let mut sub_names: HashMap<(String, String, String, String), String> = HashMap::new();
    let mut vendor: Option<String> = None;
    let mut device: Option<String> = None;
    for line in pci_ids.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if line.starts_with('C') && line.as_bytes().get(1) == Some(&b' ') {
            break;
        }
        if let Some(rest) = line.strip_prefix("\t\t") {
            let (Some(v), Some(d)) = (&vendor, &device) else {
                continue;
            };
            let mut parts = rest.splitn(3, char::is_whitespace);
            let (Some(sv), Some(sd), Some(name)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            sub_names.insert(
                (v.clone(), d.clone(), sv.to_string(), sd.to_string()),
                name.trim().to_string(),
            );
        } else if let Some(rest) = line.strip_prefix('\t') {
            let Some(v) = &vendor else { continue };
            let (id, name) = rest.split_at(rest.len().min(4));
            device = Some(id.to_string());
            device_names.insert((v.clone(), id.to_string()), name.trim().to_string());
        } else {
            let (id, name) = line.split_at(line.len().min(4));
            device = None;
            if wanted.contains_key(id) {
                vendor = Some(id.to_string());
                vendor_names.insert(id.to_string(), name.trim().to_string());
            } else {
                vendor = None;
            }
        }
    }
    for g in gpus.iter_mut() {
        g.vendor_name = vendor_names.get(&g.vendor_id).cloned();
        let base = device_names
            .get(&(g.vendor_id.clone(), g.device_id.clone()))
            .cloned();
        let sub = match (&g.subvendor_id, &g.subdevice_id) {
            (Some(sv), Some(sd)) => sub_names
                .get(&(
                    g.vendor_id.clone(),
                    g.device_id.clone(),
                    sv.clone(),
                    sd.clone(),
                ))
                .cloned(),
            _ => None,
        };
        g.device_name = sub.or(base);
    }
}

async fn name_gpus_with_lspci(gpus: &mut [GpuInfo]) {
    let Ok(lspci) = which::which("lspci") else {
        return;
    };
    for g in gpus.iter_mut().filter(|g| g.device_name.is_none()) {
        let spec = CommandSpec::new(&lspci).args(["-mm", "-s", g.slot.as_str()]);
        let Some(res) = run_with_timeout(&spec, Duration::from_secs(5)).await else {
            continue;
        };
        let fields = ember_core::process::split_args(res.stdout.lines().next().unwrap_or(""));
        if fields.len() >= 4 {
            g.vendor_name.get_or_insert_with(|| fields[2].clone());
            g.device_name = Some(fields[3].clone());
        }
    }
}

/// Heroic's Steam Deck detection.
fn steam_deck(cpu: &CpuInfo, gpus: &[GpuInfo]) -> Option<(SteamDeckModel, bool)> {
    if !matches!(
        cpu.model.as_str(),
        "AMD Custom APU 0405" | "AMD Custom APU 0932"
    ) {
        return None;
    }
    let gpu = gpus.first().filter(|g| g.vendor_id == "1002")?;
    let model = match gpu.device_id.as_str() {
        "163f" => SteamDeckModel::Lcd,
        "1435" => SteamDeckModel::Oled,
        _ => return None,
    };
    Some((model, crate::locations::is_steam_deck_game_mode()))
}

fn vulkan_files() -> (bool, Vec<String>) {
    let loader = [
        "/usr/lib64/libvulkan.so.1",
        "/usr/lib/libvulkan.so.1",
        "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
        "/usr/lib/aarch64-linux-gnu/libvulkan.so.1",
        "/app/lib/libvulkan.so.1",
        "/usr/lib/x86_64-linux-gnu/GL/default/lib/libvulkan.so.1",
    ]
    .iter()
    .any(|p| Path::new(p).exists());
    let mut icds: Vec<String> = [
        "/usr/share/vulkan/icd.d",
        "/etc/vulkan/icd.d",
        "/usr/local/share/vulkan/icd.d",
        "/run/host/usr/share/vulkan/icd.d",
    ]
    .iter()
    .filter_map(|d| std::fs::read_dir(d).ok())
    .flat_map(|rd| rd.flatten())
    .map(|e| e.file_name().to_string_lossy().into_owned())
    .filter(|n| n.ends_with(".json"))
    .collect();
    icds.sort();
    icds.dedup();
    (loader, icds)
}

/// Parse `vulkaninfo --summary`: instance version and `(deviceName,
/// apiVersion)` pairs.
pub fn parse_vulkan_summary(text: &str) -> (Option<String>, Vec<(String, String)>) {
    static INSTANCE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"Vulkan Instance Version:\s*([\d.]+)").expect("valid regex"));
    let instance = INSTANCE.captures(text).map(|c| c[1].to_string());
    let mut devices = Vec::new();
    let mut api: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("apiVersion") {
            api = Some(
                v.trim_start_matches([' ', '='])
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string(),
            );
        } else if let Some(n) = line.strip_prefix("deviceName") {
            let name = n.trim_start_matches([' ', '=']).trim().to_string();
            devices.push((name, api.take().unwrap_or_default()));
        }
    }
    (instance, devices)
}

async fn vulkan_summary() -> (Option<String>, Vec<(String, String)>) {
    let Ok(bin) = which::which("vulkaninfo") else {
        return (None, Vec::new());
    };
    let spec = CommandSpec::new(bin).arg("--summary");
    match run_with_timeout(&spec, Duration::from_secs(10)).await {
        Some(res) => parse_vulkan_summary(&res.stdout),
        None => (None, Vec::new()),
    }
}

/// Highest Vulkan API version `(major, minor)` of any physical GPU, when it
/// can be determined (used to pick DXVK/VKD3D builds).
pub async fn vulkan_api_version() -> Option<(u32, u32)> {
    let info = get(false).await;
    info.vulkan
        .devices
        .iter()
        .filter(|(name, _)| !name.to_ascii_lowercase().contains("llvmpipe"))
        .filter_map(|(_, v)| {
            let mut parts = v.split('.');
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        })
        .max()
}

fn first_version(text: &str) -> Option<String> {
    static VERSION: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"v?(\d+\.\d+(?:\.\d+)*(?:[-+.][\w.]+)?)").expect("valid regex"));
    VERSION.captures(text).map(|c| c[1].to_string())
}

async fn probe(bin: &str, args: &[&str]) -> Option<String> {
    let path = which::which(bin).ok()?;
    let spec = CommandSpec::new(path).args(args.iter().copied());
    let res = run_with_timeout(&spec, Duration::from_secs(5)).await;
    let text = res
        .map(|r| format!("{}\n{}", r.stdout, r.stderr))
        .unwrap_or_default();
    Some(match bin {
        "wine" => text.lines().next().unwrap_or("").trim().to_string(),
        _ => {
            let relevant = text
                .lines()
                .find(|l| l.to_ascii_lowercase().contains("version"))
                .unwrap_or(&text);
            first_version(relevant).unwrap_or_else(|| "installed".into())
        }
    })
    .filter(|s| !s.is_empty())
}

async fn software_versions() -> SoftwareVersions {
    let (wine, gamescope, gamemode, mangohud, umu) = tokio::join!(
        probe("wine", &["--version"]),
        probe("gamescope", &["--version"]),
        async {
            match probe("gamemoded", &["--version"]).await {
                Some(v) => Some(v),
                None => probe("gamemoderun", &["--version"]).await,
            }
        },
        probe("mangohud", &["--version"]),
        probe("umu-run", &["--version"]),
    );
    SoftwareVersions {
        ember: env!("CARGO_PKG_VERSION").to_string(),
        wine,
        gamescope,
        gamemode,
        mangohud,
        umu,
    }
}

/// Decimal size like Heroic's log header (`16.62 GB`).
fn decimal_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{:.2} {}", v, UNITS[unit])
    }
}

/// Human-readable report for logs and "copy system info" (Heroic's
/// `formatSystemInfo` layout, extended with Vulkan and tool versions).
pub fn format(info: &SystemInfo) -> String {
    let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "unknown".into());
    let mut out = format!(
        "CPU: {}x {}\nMemory: {} (used: {})\nGPUs:\n",
        info.cpu.cores,
        info.cpu.model,
        decimal_size(info.memory.total),
        decimal_size(info.memory.used)
    );
    let gpus: Vec<String> = info
        .gpus
        .iter()
        .enumerate()
        .map(|(i, g)| {
            format!(
                "  GPU {i}:\n    Name: {} {}\n    IDs: D={} V={} SD={} SV={}\n    Driver: {}{}",
                opt(&g.vendor_name),
                opt(&g.device_name),
                g.device_id,
                g.vendor_id,
                opt(&g.subdevice_id),
                opt(&g.subvendor_id),
                opt(&g.driver),
                g.driver_version
                    .as_deref()
                    .map(|v| format!(" {v}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    out.push_str(&gpus.join("\n"));
    out.push_str(&format!(
        "\nOS: {} {} (linux)\nKernel: {}\nDesktop: {} ({})\n\n",
        info.os.name,
        info.os.version.as_deref().unwrap_or(&info.os.kernel),
        info.os.kernel,
        opt(&info.desktop),
        opt(&info.session_type)
    ));
    match info.steam_deck {
        Some((model, game_mode)) => out.push_str(&format!(
            "The current system is a Steam Deck (model: {}) in {} mode\n",
            match model {
                SteamDeckModel::Lcd => "LCD",
                SteamDeckModel::Oled => "OLED",
            },
            if game_mode { "game" } else { "desktop" }
        )),
        None => out.push_str("The current system is not a Steam Deck\n"),
    }
    out.push_str(&format!(
        "We are{} running inside a Flatpak container\nWe are{} running from an AppImage\n\n",
        if info.is_flatpak { "" } else { " not" },
        if info.is_appimage { "" } else { " not" }
    ));
    out.push_str(&format!(
        "Vulkan: {}{}\n",
        if info.vulkan.available() {
            "available"
        } else {
            "not available"
        },
        info.vulkan
            .instance_version
            .as_deref()
            .map(|v| format!(" (instance {v})"))
            .unwrap_or_default()
    ));
    for (name, api) in &info.vulkan.devices {
        out.push_str(&format!("  {name}: {api}\n"));
    }
    let s = &info.software;
    let ni = |v: &Option<String>| v.clone().unwrap_or_else(|| "not installed".into());
    out.push_str(&format!(
        "\nSoftware Versions:\n  Ember: {}\n  Wine: {}\n  gamescope: {}\n  GameMode: {}\n  MangoHud: {}\n  umu-launcher: {}\n",
        s.ember,
        ni(&s.wine),
        ni(&s.gamescope),
        ni(&s.gamemode),
        ni(&s.mangohud),
        ni(&s.umu)
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_and_memory() {
        let cpu = parse_cpu(
            "processor\t: 0\nmodel name\t: AMD Ryzen 9 7950X\nprocessor\t: 1\nmodel name\t: AMD Ryzen 9 7950X\n",
        );
        assert_eq!(cpu.cores, 2);
        assert_eq!(cpu.model, "AMD Ryzen 9 7950X");
        let mem =
            parse_meminfo("MemTotal:       1000 kB\nMemFree: 1 kB\nMemAvailable:    400 kB\n");
        assert_eq!(mem.total, 1_024_000);
        assert_eq!(mem.used, 614_400);
        assert_eq!(parse_meminfo("").used, 0);
    }

    #[test]
    fn os_release() {
        let os = parse_os_release(Some(
            "NAME=\"Nobara Linux\"\nVERSION=\"44 (KDE Plasma)\"\nVERSION_ID=44\n",
        ));
        assert_eq!(os.name, "Nobara Linux");
        assert_eq!(os.version.as_deref(), Some("44 (KDE Plasma)"));
        let os = parse_os_release(Some(
            "PRETTY_NAME=\"Arch\"\nVERSION_ID=1\nVERSION_CODENAME=x\n",
        ));
        assert_eq!(os.name, "Arch");
        assert_eq!(os.version.as_deref(), Some("1 x"));
        assert_eq!(parse_os_release(None).name, "Unknown Linux Distribution");
    }

    #[test]
    fn gpus_from_sysfs_and_pci_ids() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |slot: &str, class: &str, vendor: &str, device: &str, boot: &str| {
            let d = dir.path().join(slot);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("class"), class).unwrap();
            std::fs::write(d.join("vendor"), vendor).unwrap();
            std::fs::write(d.join("device"), device).unwrap();
            std::fs::write(d.join("subsystem_vendor"), "0x1458\n").unwrap();
            std::fs::write(d.join("subsystem_device"), "0x240e\n").unwrap();
            std::fs::write(d.join("boot_vga"), boot).unwrap();
        };
        mk("0000:18:00.0", "0x030000\n", "0x1002\n", "0x164e\n", "0\n");
        mk("0000:03:00.0", "0x030000\n", "0x1002\n", "0x744c\n", "1\n");
        mk("0000:00:01.0", "0x060400\n", "0x1022\n", "0x14da\n", "0\n");
        let mut gpus = read_gpus(dir.path());
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].slot, "0000:03:00.0");
        assert_eq!(gpus[0].device_id, "744c");
        assert_eq!(gpus[0].subvendor_id.as_deref(), Some("1458"));
        let ids = "# comment\n1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\t164e  Raphael\n\t744c  Navi 31 [Radeon RX 7900 XT/7900 XTX]\n\t\t1458 240e  Radeon RX 7900 XTX Gaming OC\n10de  NVIDIA Corporation\n\t1b80  GP104\nC 00  Unclassified device\n";
        name_gpus(ids, &mut gpus);
        assert_eq!(
            gpus[0].vendor_name.as_deref(),
            Some("Advanced Micro Devices, Inc. [AMD/ATI]")
        );
        assert_eq!(
            gpus[0].device_name.as_deref(),
            Some("Radeon RX 7900 XTX Gaming OC")
        );
        assert_eq!(gpus[1].device_name.as_deref(), Some("Raphael"));
    }

    #[test]
    fn vulkan_summary_parsing() {
        let text = "Vulkan Instance Version: 1.4.341\n\nDevices:\nGPU0:\n\tapiVersion         = 1.4.354\n\tdriverVersion      = 25.2.0\n\tdeviceName         = AMD Radeon RX 7900 XTX (RADV NAVI31)\nGPU1:\n\tapiVersion         = 1.3.255\n\tdeviceName         = llvmpipe (LLVM 22.1.8, 256 bits)\n";
        let (inst, devs) = parse_vulkan_summary(text);
        assert_eq!(inst.as_deref(), Some("1.4.341"));
        assert_eq!(
            devs[0],
            (
                "AMD Radeon RX 7900 XTX (RADV NAVI31)".to_string(),
                "1.4.354".to_string()
            )
        );
        assert_eq!(devs[1].1, "1.3.255");
    }

    #[test]
    fn versions_and_format() {
        assert_eq!(
            first_version("gamemode version: v1.8.2").as_deref(),
            Some("1.8.2")
        );
        assert_eq!(
            first_version("umu-launcher version 1.4.4 (3.14.7 (main").as_deref(),
            Some("1.4.4")
        );
        assert_eq!(decimal_size(16_620_000_000), "16.62 GB");
        let info = SystemInfo {
            cpu: CpuInfo {
                model: "CPU".into(),
                cores: 4,
            },
            ..Default::default()
        };
        let text = format(&info);
        assert!(text.starts_with("CPU: 4x CPU\nMemory: 0 B (used: 0 B)\nGPUs:\n"));
        assert!(text.contains("The current system is not a Steam Deck"));
        assert!(text.contains("We are not running inside a Flatpak container"));
    }

    #[test]
    fn deck_detection() {
        let cpu = CpuInfo {
            model: "AMD Custom APU 0405".into(),
            cores: 8,
        };
        let gpu = GpuInfo {
            vendor_id: "1002".into(),
            device_id: "163f".into(),
            ..Default::default()
        };
        assert_eq!(
            steam_deck(&cpu, &[gpu]).map(|d| d.0),
            Some(SteamDeckModel::Lcd)
        );
        assert!(steam_deck(&CpuInfo::default(), &[]).is_none());
    }
}
