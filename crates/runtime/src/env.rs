//! Environment variable assembly for launches and Wine commands.
//!
//! Each builder is a pure function of the settings and a snapshot of the host
//! environment, so the exact output (and precedence between layers) is unit
//! tested. Layers, lowest to highest precedence, for a store launch:
//!
//! 1. [`wrapper_env`]: game identity for wrappers and umu (`GAMEID=umu-0`)
//! 2. [`generic_env`]: PRIME offload, `STEAM_COMPAT_INSTALL_PATH`, user
//!    variables, `LD_PRELOAD` reset
//! 3. known-fixes variables
//! 4. [`wine_env`]: Wine/Proton variables (Windows games only)
//! 5. `GAMEID=<umu id>` when umu runs the game and the game is in the umu
//!    database

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use ember_core::{GameId, GameSettings, Runner, WineInstallation, WineKind};
use once_cell::sync::Lazy;
use regex::Regex;

/// Ordered environment (sorted keys make logs stable).
pub type EnvMap = BTreeMap<String, String>;

/// Snapshot of the host process environment that env assembly consults
/// (`LD_PRELOAD`, `LD_LIBRARY_PATH`, `SteamAppId`, …).
#[derive(Debug, Clone, Default)]
pub struct HostEnv(HashMap<String, String>);

impl HostEnv {
    /// The current process environment.
    pub fn current() -> Self {
        Self(std::env::vars().collect())
    }

    /// A synthetic environment (tests).
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self(
            pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }
}

/// Insert every variable of `top` into `base`, overwriting existing keys.
pub fn layer(base: &mut EnvMap, top: EnvMap) {
    base.extend(top);
}

/// Heroic's `removeQuoteIfNecessary`: a value wrapped in double quotes loses
/// all leading and trailing `"` characters; anything else is kept verbatim.
pub fn remove_quote_if_necessary(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s.trim_matches('"').to_string()
    } else {
        s.to_string()
    }
}

/// `HEROIC_APP_SOURCE` value of a runner.
pub fn app_source(runner: Runner) -> &'static str {
    match runner {
        Runner::Gog => "gog",
        Runner::Legendary => "epic",
        Runner::Nile => "amazon",
        Runner::Sideload => "sideload",
        Runner::Zoom => "zoom",
    }
}

/// `STORE` value understood by umu (none for sideloaded apps).
pub fn umu_store(runner: Runner) -> Option<&'static str> {
    match runner {
        Runner::Gog => Some("gog"),
        Runner::Legendary => Some("egs"),
        Runner::Nile => Some("amazon"),
        Runner::Zoom => Some("zoomplatform"),
        Runner::Sideload => None,
    }
}

/// Identity variables for wrappers, scripts and umu.
pub fn wrapper_env(id: &GameId) -> EnvMap {
    let mut env = EnvMap::new();
    env.insert("HEROIC_APP_NAME".into(), id.app_name.to_string());
    env.insert("HEROIC_APP_RUNNER".into(), id.runner.id().into());
    env.insert("GAMEID".into(), "umu-0".into());
    env.insert("HEROIC_APP_SOURCE".into(), app_source(id.runner).into());
    if let Some(store) = umu_store(id.runner) {
        env.insert("STORE".into(), store.into());
    }
    env
}

/// Variables shared by native and Windows launches: PRIME offload,
/// `STEAM_COMPAT_INSTALL_PATH`, the Steam runtime for native games
/// (`PROTONPATH`), the user's variables, and the `LD_PRELOAD` reset.
///
/// `steam_runtime` is the umu runtime name and must only be passed for native
/// games that run through umu.
pub fn generic_env(
    settings: &GameSettings,
    install_path: Option<&Path>,
    steam_runtime: Option<&str>,
    host: &HostEnv,
) -> EnvMap {
    let mut env = EnvMap::new();
    if settings.nvidia_prime {
        env.insert("DRI_PRIME".into(), "1".into());
        env.insert("__NV_PRIME_RENDER_OFFLOAD".into(), "1".into());
        env.insert("__GLX_VENDOR_LIBRARY_NAME".into(), "nvidia".into());
    }
    if let Some(path) = install_path.filter(|p| !p.as_os_str().is_empty()) {
        env.insert(
            "STEAM_COMPAT_INSTALL_PATH".into(),
            path.to_string_lossy().into_owned(),
        );
    }
    if let Some(runtime) = steam_runtime.filter(|r| !r.is_empty()) {
        env.insert("PROTONPATH".into(), runtime.into());
    }
    for var in &settings.environment_options {
        let key = var.key.trim();
        if key.is_empty() {
            continue;
        }
        env.insert(key.to_string(), remove_quote_if_necessary(&var.value));
    }
    let user_sets_preload = settings
        .environment_options
        .iter()
        .any(|v| v.key.trim() == "LD_PRELOAD");
    if !host.contains("LD_PRELOAD") && !user_sets_preload {
        env.insert("LD_PRELOAD".into(), String::new());
    }
    env
}

/// Inputs of [`wine_env`].
#[derive(Debug, Clone)]
pub struct WineEnvInput<'a> {
    pub settings: &'a GameSettings,
    /// The Wine/Proton build actually used (quotes already removed from `bin`).
    pub wine: &'a WineInstallation,
    pub prefix: &'a Path,
    /// Identifier used in `SteamGameId` (`heroic-<id>`); the install folder
    /// name, or `"0"`.
    pub game_id: &'a str,
    /// `STEAM_COMPAT_CLIENT_INSTALL_PATH`.
    pub steam_client_path: &'a Path,
    /// `PROTON_LOG_DIR`.
    pub proton_log_dir: &'a Path,
    pub eac_runtime: &'a Path,
    pub battleye_runtime: &'a Path,
    pub host: &'a HostEnv,
}

/// Old GE builds that misbehave with Steam's virtual gamepad variable.
static OLD_GE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(GE|Wine|Proton)-(Proton[7-8]|[4-7].*-GE|GE-4.[0-9]*)").expect("valid regex")
});

/// Wine/Proton specific variables (Heroic's `setupWineEnvVars`).
pub fn wine_env(input: &WineEnvInput<'_>) -> EnvMap {
    let gs = input.settings;
    let kind = input.wine.kind;
    let proton = kind == WineKind::Proton;
    let is_wine = kind == WineKind::Wine;
    let prefix = input.prefix.to_string_lossy().into_owned();
    let mut env = EnvMap::new();
    let mut set = |k: &str, v: &str| {
        env.insert(k.to_string(), v.to_string());
    };

    match kind {
        WineKind::Wine => {
            set("WINEPREFIX", &prefix);
            match gs
                .environment_options
                .iter()
                .find(|v| v.key.trim().eq_ignore_ascii_case("WINEDLLOVERRIDES"))
            {
                Some(user) => {
                    let value = remove_quote_if_necessary(&user.value);
                    let value = value.trim_end_matches(';');
                    let merged = if value.is_empty() {
                        "winemenubuilder.exe=d".to_string()
                    } else {
                        format!("{value};winemenubuilder.exe=d")
                    };
                    set(user.key.trim(), &merged);
                }
                None => set("WINEDLLOVERRIDES", "winemenubuilder.exe=d"),
            }
        }
        WineKind::Proton => {
            set(
                "STEAM_COMPAT_CLIENT_INSTALL_PATH",
                &input.steam_client_path.to_string_lossy(),
            );
            set("WINEPREFIX", &prefix);
            set("STEAM_COMPAT_DATA_PATH", &prefix);
            let dir = Path::new(&input.wine.bin)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            set("PROTONPATH", &dir);
        }
        WineKind::Crossover | WineKind::Toolkit => set("WINEPREFIX", &prefix),
    }

    if gs.show_fps {
        set("DXVK_HUD", "fps");
    }
    if gs.enable_dxvk_fps_limit && !gs.dxvk_fps_cap.trim().is_empty() {
        set("DXVK_FRAME_RATE", gs.dxvk_fps_cap.trim());
    }
    if gs.enable_fsr {
        set("WINE_FULLSCREEN_FSR", "1");
        let strength = gs
            .max_sharpness
            .map(|s| s.to_string())
            .unwrap_or_else(|| "2".into());
        set("WINE_FULLSCREEN_FSR_STRENGTH", &strength);
    } else {
        set("WINE_FULLSCREEN_FSR", "0");
    }
    if gs.enable_esync && !proton {
        set("WINEESYNC", "1");
    }
    if !gs.enable_esync && proton {
        set("PROTON_NO_ESYNC", "1");
    }
    if gs.enable_fsync && !proton {
        set("WINEFSYNC", "1");
    }
    if !gs.enable_fsync && proton {
        set("PROTON_NO_FSYNC", "1");
    }
    if gs.enable_wine_wayland {
        if proton {
            set("PROTON_ENABLE_WAYLAND", "1");
            if gs.enable_hdr {
                set("PROTON_ENABLE_HDR", "1");
            }
        } else {
            set("DISPLAY", "");
            if gs.enable_hdr {
                set("DXVK_HDR", "1");
            }
        }
    }
    if gs.enable_wow64 {
        if proton {
            set("PROTON_USE_WOW64", "1");
        } else {
            set("WINEARCH", "wow64");
        }
    }
    if proton {
        if gs.auto_install_dxvk_nvapi {
            set("PROTON_ENABLE_NVAPI", "1");
            set("DXVK_NVAPI_ALLOW_OTHER_DRIVERS", "1");
        } else {
            set("PROTON_DISABLE_NVAPI", "1");
        }
    }
    if gs.auto_install_dxvk_nvapi && is_wine {
        set("DXVK_ENABLE_NVAPI", "1");
        set("DXVK_NVAPI_ALLOW_OTHER_DRIVERS", "1");
    }
    if gs.eac_runtime {
        set("PROTON_EAC_RUNTIME", &input.eac_runtime.to_string_lossy());
    }
    if gs.battl_eye_runtime {
        set(
            "PROTON_BATTLEYE_RUNTIME",
            &input.battleye_runtime.to_string_lossy(),
        );
    }
    if proton {
        let app_id = input
            .host
            .get("STEAM_COMPAT_APP_ID")
            .filter(|v| !v.is_empty())
            .unwrap_or("0")
            .to_string();
        let steam_app_id = input
            .host
            .get("SteamAppId")
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| app_id.clone());
        let steam_game_id = input
            .host
            .get("SteamGameId")
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("ember-{}", input.game_id));
        set("STEAM_COMPAT_APP_ID", &app_id);
        set("SteamAppId", &steam_app_id);
        set("SteamGameId", &steam_game_id);
        set("PROTON_LOG_DIR", &input.proton_log_dir.to_string_lossy());
        if gs.verbose_logs {
            for (k, v) in [
                ("WINEDEBUG", "+fixme"),
                ("DXVK_LOG_LEVEL", "info"),
                ("VKD3D_DEBUG", "fixme"),
            ] {
                if !gs.environment_options.iter().any(|e| e.key.trim() == k) {
                    set(k, v);
                }
            }
        }
    }
    if !gs.prefer_system_libs && is_wine {
        let host_ld = input.host.get("LD_LIBRARY_PATH").unwrap_or("");
        if !input.host.contains("ORIG_LD_LIBRARY_PATH") {
            set("ORIG_LD_LIBRARY_PATH", host_ld);
        }
        let lib = input.wine.lib.as_deref().unwrap_or("");
        let lib32 = input.wine.lib32.as_deref().unwrap_or("");
        if !lib.is_empty() && !lib32.is_empty() {
            let ld: Vec<&str> = [lib, lib32, host_ld]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect();
            set("LD_LIBRARY_PATH", &ld.join(":"));
            let (lib, lib32) = (PathBuf::from(lib), PathBuf::from(lib32));
            let (gst, gst32) = (lib.join("gstreamer-1.0"), lib32.join("gstreamer-1.0"));
            if gst.exists() && gst32.exists() {
                set(
                    "GST_PLUGIN_SYSTEM_PATH_1_0",
                    &format!("{}:{}", gst.display(), gst32.display()),
                );
            }
            let (dll, dll32) = (lib.join("wine"), lib32.join("wine"));
            if dll.exists() && dll32.exists() {
                set(
                    "WINEDLLPATH",
                    &format!("{}:{}", dll.display(), dll32.display()),
                );
            }
        } else {
            tracing::debug!(
                "{} ships no lib/lib32 folders; using system libraries",
                input.wine.name
            );
        }
    }
    if OLD_GE.is_match(&input.wine.name) {
        tracing::warn!(
            "{} is an old GE build; disabling Steam's virtual gamepad info",
            input.wine.name
        );
        set("SteamVirtualGamepadInfo", "");
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_core::EnvVar;

    fn proton() -> WineInstallation {
        WineInstallation {
            bin: "/t/GE-Proton9-20/proton".into(),
            name: "GE-Proton9-20".into(),
            kind: WineKind::Proton,
            ..Default::default()
        }
    }

    fn wine(dir: &Path) -> WineInstallation {
        WineInstallation {
            bin: dir.join("bin/wine").to_string_lossy().into_owned(),
            name: "wine-ge-8-26".into(),
            kind: WineKind::Wine,
            lib: Some(dir.join("lib64").to_string_lossy().into_owned()),
            lib32: Some(dir.join("lib").to_string_lossy().into_owned()),
            wineserver: Some(dir.join("bin/wineserver").to_string_lossy().into_owned()),
        }
    }

    fn input<'a>(
        gs: &'a GameSettings,
        w: &'a WineInstallation,
        host: &'a HostEnv,
    ) -> WineEnvInput<'a> {
        WineEnvInput {
            settings: gs,
            wine: w,
            prefix: Path::new("/pfx"),
            game_id: "Game",
            steam_client_path: Path::new("/home/u/.steam/steam"),
            proton_log_dir: Path::new("/home/u"),
            eac_runtime: Path::new("/rt/eac_runtime"),
            battleye_runtime: Path::new("/rt/battleye_runtime"),
            host,
        }
    }

    fn user(key: &str, value: &str) -> EnvVar {
        EnvVar {
            key: key.into(),
            value: value.into(),
        }
    }

    #[test]
    fn quotes() {
        assert_eq!(remove_quote_if_necessary("\"a b\""), "a b");
        assert_eq!(remove_quote_if_necessary("\"\"x\"\""), "x");
        assert_eq!(remove_quote_if_necessary("\"a"), "\"a");
        assert_eq!(remove_quote_if_necessary("\""), "\"");
    }

    #[test]
    fn identity_vars() {
        let env = wrapper_env(&GameId::new(Runner::Legendary, "Fortnite"));
        assert_eq!(env["HEROIC_APP_NAME"], "Fortnite");
        assert_eq!(env["HEROIC_APP_RUNNER"], "legendary");
        assert_eq!(env["HEROIC_APP_SOURCE"], "epic");
        assert_eq!(env["STORE"], "egs");
        assert_eq!(env["GAMEID"], "umu-0");
        let side = wrapper_env(&GameId::new(Runner::Sideload, "x"));
        assert!(!side.contains_key("STORE"));
    }

    #[test]
    fn generic_vars() {
        let mut gs = GameSettings {
            nvidia_prime: true,
            ..Default::default()
        };
        gs.environment_options = vec![user("FOO", "\"bar baz\""), user(" ", "x")];
        let host = HostEnv::default();
        let env = generic_env(&gs, Some(Path::new("/games/x")), Some("umu-sniper"), &host);
        assert_eq!(env["DRI_PRIME"], "1");
        assert_eq!(env["__NV_PRIME_RENDER_OFFLOAD"], "1");
        assert_eq!(env["__GLX_VENDOR_LIBRARY_NAME"], "nvidia");
        assert_eq!(env["STEAM_COMPAT_INSTALL_PATH"], "/games/x");
        assert_eq!(env["PROTONPATH"], "umu-sniper");
        assert_eq!(env["FOO"], "bar baz");
        assert_eq!(env["LD_PRELOAD"], "");
        assert_eq!(env.len(), 7);

        let host = HostEnv::from_pairs([("LD_PRELOAD", "/lib/x.so")]);
        assert!(!generic_env(&gs, None, None, &host).contains_key("LD_PRELOAD"));
        gs.environment_options.push(user("LD_PRELOAD", "/my.so"));
        let env = generic_env(&gs, None, None, &HostEnv::default());
        assert_eq!(env["LD_PRELOAD"], "/my.so");
    }

    #[test]
    fn proton_vars() {
        let gs = GameSettings {
            show_fps: true,
            enable_dxvk_fps_limit: true,
            dxvk_fps_cap: "60".into(),
            enable_esync: false,
            enable_fsync: false,
            enable_wine_wayland: true,
            enable_hdr: true,
            enable_wow64: true,
            verbose_logs: true,
            environment_options: vec![user("WINEDEBUG", "-all")],
            ..Default::default()
        };
        let w = proton();
        let host = HostEnv::from_pairs([("SteamAppId", "42")]);
        let env = wine_env(&input(&gs, &w, &host));
        assert_eq!(
            env["STEAM_COMPAT_CLIENT_INSTALL_PATH"],
            "/home/u/.steam/steam"
        );
        assert_eq!(env["WINEPREFIX"], "/pfx");
        assert_eq!(env["STEAM_COMPAT_DATA_PATH"], "/pfx");
        assert_eq!(env["PROTONPATH"], "/t/GE-Proton9-20");
        assert_eq!(env["DXVK_HUD"], "fps");
        assert_eq!(env["DXVK_FRAME_RATE"], "60");
        assert_eq!(env["WINE_FULLSCREEN_FSR"], "0");
        assert_eq!(env["PROTON_NO_ESYNC"], "1");
        assert_eq!(env["PROTON_NO_FSYNC"], "1");
        assert!(!env.contains_key("WINEESYNC") && !env.contains_key("WINEFSYNC"));
        assert_eq!(env["PROTON_ENABLE_WAYLAND"], "1");
        assert_eq!(env["PROTON_ENABLE_HDR"], "1");
        assert!(!env.contains_key("DISPLAY"));
        assert_eq!(env["PROTON_USE_WOW64"], "1");
        assert_eq!(env["PROTON_ENABLE_NVAPI"], "1");
        assert_eq!(env["DXVK_NVAPI_ALLOW_OTHER_DRIVERS"], "1");
        assert!(!env.contains_key("DXVK_ENABLE_NVAPI"));
        assert_eq!(env["PROTON_EAC_RUNTIME"], "/rt/eac_runtime");
        assert_eq!(env["PROTON_BATTLEYE_RUNTIME"], "/rt/battleye_runtime");
        assert_eq!(env["STEAM_COMPAT_APP_ID"], "0");
        assert_eq!(env["SteamAppId"], "42");
        assert_eq!(env["SteamGameId"], "ember-Game");
        assert_eq!(env["PROTON_LOG_DIR"], "/home/u");
        assert!(!env.contains_key("WINEDEBUG"), "user value must win");
        assert_eq!(env["DXVK_LOG_LEVEL"], "info");
        assert_eq!(env["VKD3D_DEBUG"], "fixme");
        assert!(!env.contains_key("WINEDLLOVERRIDES"));
        assert!(!env.contains_key("LD_LIBRARY_PATH"));
    }

    #[test]
    fn wine_vars() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["lib/wine", "lib64/wine", "lib64/gstreamer-1.0"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        let gs = GameSettings {
            enable_fsr: true,
            max_sharpness: Some(4),
            enable_wine_wayland: true,
            enable_hdr: true,
            enable_wow64: true,
            environment_options: vec![user("WineDllOverrides", "\"d3d11=n;\"")],
            ..Default::default()
        };
        let w = wine(dir.path());
        let host = HostEnv::from_pairs([("LD_LIBRARY_PATH", "/host")]);
        let env = wine_env(&input(&gs, &w, &host));
        assert_eq!(env["WINEPREFIX"], "/pfx");
        assert_eq!(env["WineDllOverrides"], "d3d11=n;winemenubuilder.exe=d");
        assert!(!env.contains_key("WINEDLLOVERRIDES"));
        assert_eq!(env["WINE_FULLSCREEN_FSR"], "1");
        assert_eq!(env["WINE_FULLSCREEN_FSR_STRENGTH"], "4");
        assert_eq!(env["WINEESYNC"], "1");
        assert_eq!(env["WINEFSYNC"], "1");
        assert_eq!(env["DISPLAY"], "");
        assert_eq!(env["DXVK_HDR"], "1");
        assert_eq!(env["WINEARCH"], "wow64");
        assert_eq!(env["DXVK_ENABLE_NVAPI"], "1");
        assert!(!env.contains_key("PROTON_ENABLE_NVAPI"));
        assert!(!env.contains_key("SteamGameId"));
        assert_eq!(env["ORIG_LD_LIBRARY_PATH"], "/host");
        let lib = dir.path().join("lib64");
        let lib32 = dir.path().join("lib");
        assert_eq!(
            env["LD_LIBRARY_PATH"],
            format!("{}:{}:/host", lib.display(), lib32.display())
        );
        assert_eq!(
            env["WINEDLLPATH"],
            format!("{}/wine:{}/wine", lib.display(), lib32.display())
        );
        assert!(
            !env.contains_key("GST_PLUGIN_SYSTEM_PATH_1_0"),
            "32-bit gst dir missing"
        );

        let plain = GameSettings::default();
        let env = wine_env(&input(&plain, &w, &host));
        assert_eq!(env["WINEDLLOVERRIDES"], "winemenubuilder.exe=d");
        assert_eq!(env["WINE_FULLSCREEN_FSR"], "0");
        assert!(!env.contains_key("WINE_FULLSCREEN_FSR_STRENGTH"));

        let sys = GameSettings {
            prefer_system_libs: true,
            ..Default::default()
        };
        let env = wine_env(&input(&sys, &w, &host));
        assert!(!env.contains_key("LD_LIBRARY_PATH"));
        assert!(!env.contains_key("ORIG_LD_LIBRARY_PATH"));
    }

    #[test]
    fn fsr_default_strength_and_nvapi_off() {
        let gs = GameSettings {
            enable_fsr: true,
            auto_install_dxvk_nvapi: false,
            ..Default::default()
        };
        let w = proton();
        let host = HostEnv::default();
        let env = wine_env(&input(&gs, &w, &host));
        assert_eq!(env["WINE_FULLSCREEN_FSR_STRENGTH"], "2");
        assert_eq!(env["PROTON_DISABLE_NVAPI"], "1");
        assert!(!env.contains_key("DXVK_NVAPI_ALLOW_OTHER_DRIVERS"));
    }

    #[test]
    fn old_ge_builds_hide_gamepad_info() {
        let gs = GameSettings::default();
        let host = HostEnv::default();
        for (name, hit) in [
            ("GE-Proton7-55", true),
            ("Wine-5.11-GE-1", true),
            ("Proton-6.21-GE-2", true),
            ("GE-Proton9-20", false),
        ] {
            let mut w = proton();
            w.name = name.into();
            let env = wine_env(&input(&gs, &w, &host));
            assert_eq!(env.contains_key("SteamVirtualGamepadInfo"), hit, "{name}");
        }
    }

    #[test]
    fn layers_follow_precedence() {
        let gs = GameSettings {
            environment_options: vec![user("WINEPREFIX", "/user"), user("FIX", "user")],
            ..Default::default()
        };
        let w = proton();
        let host = HostEnv::default();
        let mut env = wrapper_env(&GameId::new(Runner::Gog, "1"));
        layer(&mut env, generic_env(&gs, None, None, &host));
        let fixes: EnvMap = [("FIX".to_string(), "fix".to_string())].into();
        layer(&mut env, fixes);
        layer(&mut env, wine_env(&input(&gs, &w, &host)));
        assert_eq!(env["WINEPREFIX"], "/pfx");
        assert_eq!(env["FIX"], "fix");
        assert_eq!(env["STORE"], "gog");
    }
}
