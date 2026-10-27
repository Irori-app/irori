//! protontricks: winetricks for the Proton prefixes Steam creates for its
//! own games (`steamapps/compatdata/<app id>/pfx`). protontricks finds the
//! prefix and the Proton build from a Steam app ID, so it serves Steam games
//! only; games Irori runs itself use winetricks.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use irori_core::OpContext;
use irori_core::process::{self, CommandSpec, ExecResult, RunOptions};

/// Flathub ID of protontricks.
pub const FLATPAK_ID: &str = "com.github.Matoking.protontricks";

/// How to start protontricks: the program on `PATH`, else the Flathub app.
/// `None` when neither is installed.
pub fn command() -> Option<Vec<String>> {
    let home = dirs::home_dir().unwrap_or_default();
    command_from(
        &|name| which::which(name).ok(),
        &[
            home.join(".local/share/flatpak/app"),
            PathBuf::from("/var/lib/flatpak/app"),
        ],
    )
}

fn command_from(
    lookup: &dyn Fn(&str) -> Option<PathBuf>,
    flatpak_app_dirs: &[PathBuf],
) -> Option<Vec<String>> {
    if let Some(bin) = lookup("protontricks") {
        return Some(vec![bin.to_string_lossy().into_owned()]);
    }
    let flatpak = lookup("flatpak")?;
    flatpak_app_dirs
        .iter()
        .any(|d| d.join(FLATPAK_ID).is_dir())
        .then(|| {
            vec![
                flatpak.to_string_lossy().into_owned(),
                "run".into(),
                FLATPAK_ID.into(),
            ]
        })
}

/// Arguments after the program: `--gui <id>` opens winetricks' window on the
/// game's prefix; with verbs, `<id> -q <verbs>` installs them unattended.
pub fn args(app_id: &str, verbs: &[String]) -> Vec<String> {
    if verbs.is_empty() {
        vec!["--gui".into(), app_id.into()]
    } else {
        let mut a = vec![app_id.to_string(), "-q".into()];
        a.extend(verbs.iter().cloned());
        a
    }
}

/// Proton prefix of a Steam game installed at `install_path`
/// (`<library>/steamapps/common/<dir>`): `<library>/steamapps/compatdata/<id>/pfx`.
pub fn prefix_of(install_path: &Path, app_id: &str) -> Option<PathBuf> {
    let steamapps = install_path.parent()?.parent()?;
    Some(steamapps.join("compatdata").join(app_id).join("pfx"))
}

/// Run protontricks for a Steam game. `steam_dir` tells protontricks which
/// Steam installation to use (the one Irori reads).
pub async fn run(
    steam_dir: &Path,
    app_id: &str,
    verbs: &[String],
    ctx: &OpContext,
) -> Result<ExecResult> {
    let mut argv = command().context(
        "Protontricks isn't installed. Install it with your package manager or from Flathub (com.github.Matoking.protontricks).",
    )?;
    argv.extend(args(app_id, verbs));
    let spec = CommandSpec::new(argv.remove(0))
        .args(argv)
        .env("STEAM_DIR", steam_dir.to_string_lossy().into_owned());
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
    process::run(&spec, opts)
        .await
        .context("Protontricks error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_program_on_path_then_flathub() {
        let dir = tempfile::tempdir().unwrap();
        let apps = vec![dir.path().to_path_buf()];
        let native =
            |n: &str| (n == "protontricks").then(|| PathBuf::from("/usr/bin/protontricks"));
        assert_eq!(
            command_from(&native, &apps),
            Some(vec!["/usr/bin/protontricks".to_string()])
        );
        let flatpak_only = |n: &str| (n == "flatpak").then(|| PathBuf::from("/usr/bin/flatpak"));
        assert_eq!(command_from(&flatpak_only, &apps), None);
        std::fs::create_dir_all(dir.path().join(FLATPAK_ID)).unwrap();
        assert_eq!(
            command_from(&flatpak_only, &apps).unwrap(),
            ["/usr/bin/flatpak", "run", FLATPAK_ID]
        );
        assert_eq!(command_from(&|_| None, &apps), None);
    }

    #[test]
    fn builds_arguments_and_finds_the_prefix() {
        assert_eq!(args("620", &[]), ["--gui", "620"]);
        assert_eq!(
            args("620", &["vcrun2019".into(), "d3dx9".into()]),
            ["620", "-q", "vcrun2019", "d3dx9"]
        );
        assert_eq!(
            prefix_of(Path::new("/lib/steamapps/common/Portal 2"), "620").unwrap(),
            Path::new("/lib/steamapps/compatdata/620/pfx")
        );
        assert_eq!(prefix_of(Path::new("/"), "620"), None);
    }
}
