//! Running Windows programs (setup scripts, redistributables, uninstallers,
//! REDmod) inside a game's prefix.
//!
//! The runtime hands the Wine layer to the store only at launch time, so the
//! store remembers it per game ([`WineContext`]) to run uninstallers later.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use ember_core::process::{self, CancellationToken, CommandSpec, ExecResult, RunOptions};
use ember_core::{LaunchEnvironment, WineKind};

/// The Wine layer of a game, as prepared by the runtime for its last launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WineContext {
    /// `wine`, the `proton` script, or `umu-run`.
    pub bin: PathBuf,
    pub kind: WineKind,
    pub prefix: PathBuf,
    pub uses_umu: bool,
    /// Wine-related environment (prefix, Proton/umu variables, library paths).
    pub env: BTreeMap<String, String>,
}

fn keep_env(key: &str) -> bool {
    const PREFIXES: [&str; 8] = [
        "WINE",
        "STEAM_COMPAT_",
        "PROTON",
        "UMU_",
        "DXVK_",
        "CX_",
        "MTL_",
        "ROSETTA_",
    ];
    PREFIXES.iter().any(|p| key.starts_with(p))
        || matches!(
            key,
            "GAMEID" | "STORE" | "LD_LIBRARY_PATH" | "SteamAppId" | "SteamGameId" | "STEAM_RUNTIME"
        )
}

impl WineContext {
    /// Capture the Wine layer of a launch environment (`None` for native games).
    pub fn from_launch(env: &LaunchEnvironment) -> Option<Self> {
        let w = env.wine.as_ref()?;
        Some(Self {
            bin: w.bin.clone(),
            kind: w.kind,
            prefix: w.prefix.clone(),
            uses_umu: w.uses_umu,
            env: env
                .env
                .iter()
                .filter(|(k, _)| keep_env(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        })
    }

    pub fn prefix_exists(&self) -> bool {
        self.prefix.is_dir()
    }

    /// The Windows `C:` drive of the prefix (Proton keeps it under `pfx/`).
    pub fn drive_c(&self) -> PathBuf {
        let pfx = self.prefix.join("pfx").join("drive_c");
        if pfx.is_dir() || (self.kind == WineKind::Proton && !self.prefix.join("drive_c").is_dir())
        {
            pfx
        } else {
            self.prefix.join("drive_c")
        }
    }

    /// Command running a Windows program with the `run` verb (no wineserver wait).
    pub fn command(&self, exe: &str, args: &[String]) -> CommandSpec {
        let mut spec = CommandSpec::new(&self.bin).envs(self.env.clone());
        if self.uses_umu {
            spec = spec.env("PROTON_VERB", "run");
        } else if self.kind == WineKind::Proton {
            spec = spec.arg("run");
        }
        if !matches!(self.kind, WineKind::Proton | WineKind::Crossover) && !self.uses_umu {
            spec = spec.env("WINEPREFIX", self.prefix.to_string_lossy());
        }
        spec.arg(exe).args(args.iter().cloned())
    }

    /// Run a Windows program and wait for it.
    pub async fn run(
        &self,
        exe: &str,
        args: &[String],
        cwd: Option<&Path>,
        cancel: Option<CancellationToken>,
        log_file: Option<&Path>,
    ) -> Result<ExecResult> {
        let mut spec = self.command(exe, args);
        if let Some(dir) = cwd {
            spec = spec.cwd(dir);
        }
        let mut opts = RunOptions::default();
        if let Some(c) = cancel {
            opts = opts.cancel(c);
        }
        if let Some(l) = log_file {
            opts = opts.log_file(l);
        }
        process::run(&spec, opts).await
    }

    /// Translate a Windows path of this prefix to a host path.
    pub fn to_unix(&self, win: &str) -> PathBuf {
        let norm = win.replace('\\', "/");
        let (drive, rest) = match norm.split_once(':') {
            Some((d, r)) if d.len() == 1 => (d.to_ascii_lowercase(), r.trim_start_matches('/')),
            _ => return PathBuf::from(norm),
        };
        match drive.as_str() {
            "c" => self.drive_c().join(rest),
            "z" => Path::new("/").join(rest),
            other => {
                let dosdevice = self
                    .drive_c()
                    .parent()
                    .map(|p| p.join("dosdevices").join(format!("{other}:")))
                    .unwrap_or_default();
                dosdevice.join(rest)
            }
        }
    }
}

/// Run setup on Windows directly, or through the selected compatibility layer.
pub async fn run_windows(
    wine: Option<&WineContext>,
    exe: &str,
    args: &[String],
    cwd: Option<&Path>,
    cancel: Option<CancellationToken>,
    log_file: Option<&Path>,
) -> Result<ExecResult> {
    if let Some(wine) = wine {
        return wine.run(exe, args, cwd, cancel, log_file).await;
    }
    anyhow::ensure!(
        cfg!(windows),
        "A Wine runtime is required to run Windows programs on this host"
    );
    let mut spec = CommandSpec::new(exe).args(args.iter().cloned());
    if let Some(cwd) = cwd {
        spec = spec.cwd(cwd);
    }
    let mut opts = RunOptions::default();
    if let Some(cancel) = cancel {
        opts = opts.cancel(cancel);
    }
    if let Some(log) = log_file {
        opts = opts.log_file(log);
    }
    process::run(&spec, opts).await
}

/// Host path as seen from Wine (`/a/b` → `Z:\a\b`).
pub fn to_windows_path(unix: &Path) -> String {
    if cfg!(windows) {
        return unix.to_string_lossy().replace('/', "\\");
    }
    format!("Z:{}", unix.to_string_lossy().replace('/', "\\"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ember_core::WineLaunch;

    fn launch_env(kind: WineKind, uses_umu: bool, prefix: &Path) -> LaunchEnvironment {
        let mut env = BTreeMap::new();
        env.insert("WINEPREFIX".into(), prefix.to_string_lossy().into_owned());
        env.insert("STEAM_COMPAT_DATA_PATH".into(), "/p".into());
        env.insert("MANGOHUD".into(), "1".into());
        LaunchEnvironment {
            env,
            wrappers: vec![vec!["gamemoderun".into()]],
            wine: Some(WineLaunch {
                bin: if uses_umu {
                    "/usr/bin/umu-run".into()
                } else {
                    "/w/proton".into()
                },
                build_bin: "/w/proton".into(),
                kind,
                prefix: prefix.to_path_buf(),
                uses_umu,
                proton_verb: "waitforexitandrun",
            }),
        }
    }

    #[test]
    fn captures_wine_env_only() {
        let ctx =
            WineContext::from_launch(&launch_env(WineKind::Proton, true, Path::new("/p"))).unwrap();
        assert!(ctx.env.contains_key("WINEPREFIX"));
        assert!(ctx.env.contains_key("STEAM_COMPAT_DATA_PATH"));
        assert!(!ctx.env.contains_key("MANGOHUD"));
        assert!(WineContext::from_launch(&LaunchEnvironment::default()).is_none());
    }

    #[test]
    fn builds_run_commands() {
        let umu =
            WineContext::from_launch(&launch_env(WineKind::Proton, true, Path::new("/p"))).unwrap();
        let c = umu.command("C:\\x.exe", &["/S".into()]);
        assert_eq!(c.program, PathBuf::from("/usr/bin/umu-run"));
        assert_eq!(c.env.get("PROTON_VERB").map(String::as_str), Some("run"));
        assert_eq!(c.args, vec!["C:\\x.exe", "/S"]);

        let proton =
            WineContext::from_launch(&launch_env(WineKind::Proton, false, Path::new("/p")))
                .unwrap();
        let c = proton.command("x.exe", &[]);
        assert_eq!(c.args, vec!["run", "x.exe"]);

        let mut wine = umu.clone();
        wine.kind = WineKind::Wine;
        wine.uses_umu = false;
        wine.bin = "/usr/bin/wine".into();
        let c = wine.command("x.exe", &[]);
        assert_eq!(c.args, vec!["x.exe"]);
        assert_eq!(c.env.get("WINEPREFIX").map(String::as_str), Some("/p"));
    }

    #[test]
    fn path_translation() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("pfx/drive_c/users/steamuser")).unwrap();
        let ctx = WineContext::from_launch(&launch_env(WineKind::Proton, true, d.path())).unwrap();
        assert_eq!(ctx.drive_c(), d.path().join("pfx/drive_c"));
        assert_eq!(
            ctx.to_unix("C:\\ProgramData\\GOG.com"),
            d.path().join("pfx/drive_c/ProgramData/GOG.com")
        );
        assert_eq!(ctx.to_unix("Z:\\home\\u"), PathBuf::from("/home/u"));
        assert_eq!(to_windows_path(Path::new("/home/u/G")), "Z:\\home\\u\\G");
    }
}
