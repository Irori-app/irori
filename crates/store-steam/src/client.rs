//! Talking to the Steam client: `steam://` links for what Steam does itself
//! (install, uninstall, verify, show a game), and its program for launches
//! with arguments.

use std::path::{Path, PathBuf};

use irori_core::process::CommandSpec;

/// Flatpak Steam's application ID.
const FLATPAK_ID: &str = "com.valvesoftware.Steam";

/// The command that hands `url` to the program registered for it.
pub(crate) fn open_url(url: &str) -> CommandSpec {
    if cfg!(target_os = "macos") {
        CommandSpec::new("/usr/bin/open").arg(url)
    } else if cfg!(windows) {
        CommandSpec::new("rundll32.exe")
            .arg("url.dll,FileProtocolHandler")
            .arg(url)
    } else {
        CommandSpec::new("xdg-open").arg(url)
    }
}

/// The Steam program for `steam_dir`, as a command to add arguments to.
/// `None` when it can't be found; `steam://` links still work then.
pub(crate) fn steam_program(steam_dir: &Path) -> Option<CommandSpec> {
    if cfg!(target_os = "linux") {
        if steam_dir.components().any(|c| c.as_os_str() == FLATPAK_ID) {
            return which::which("flatpak")
                .ok()
                .map(|flatpak| CommandSpec::new(flatpak).arg("run").arg(FLATPAK_ID));
        }
        return which::which("steam").ok().map(CommandSpec::new);
    }
    let candidates: Vec<PathBuf> = if cfg!(windows) {
        vec![steam_dir.join("steam.exe")]
    } else {
        vec![
            PathBuf::from("/Applications/Steam.app/Contents/MacOS/steam_osx"),
            irori_core::paths::expand_home("~/Applications/Steam.app/Contents/MacOS/steam_osx"),
        ]
    };
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .map(CommandSpec::new)
}

/// The command starting `app_id` with `args`. Without arguments Steam's
/// `rungameid` link is enough. With them, `-applaunch` passes them silently;
/// the `steam://run` link is the fallback, for which Steam asks the user to
/// confirm the arguments.
pub(crate) fn launch_command(steam_dir: &Path, app_id: &str, args: &[String]) -> CommandSpec {
    if args.is_empty() {
        return open_url(&format!("steam://rungameid/{app_id}"));
    }
    if let Some(steam) = steam_program(steam_dir) {
        return steam.arg("-applaunch").arg(app_id).args(args.iter());
    }
    let joined = args.join(" ");
    let encoded: String = url::form_urlencoded::byte_serialize(joined.as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    open_url(&format!("steam://run/{app_id}//{encoded}/"))
}

/// A request to the Steam client about one game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientLink {
    Install,
    Uninstall,
    Verify,
    Details,
}

impl ClientLink {
    pub(crate) fn url(self, app_id: &str) -> String {
        match self {
            ClientLink::Install => format!("steam://install/{app_id}"),
            ClientLink::Uninstall => format!("steam://uninstall/{app_id}"),
            ClientLink::Verify => format!("steam://validate/{app_id}"),
            ClientLink::Details => format!("steam://nav/games/details/{app_id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(spec: &CommandSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn launches_through_links_or_the_program() {
        let dir = tempfile::tempdir().unwrap();
        let plain = launch_command(dir.path(), "620", &[]);
        assert_eq!(argv(&plain).last().unwrap(), "steam://rungameid/620");

        let args = vec!["-novid".to_string(), "+map test".to_string()];
        let with_args = launch_command(dir.path(), "620", &args);
        let v = argv(&with_args);
        if steam_program(dir.path()).is_some() {
            assert_eq!(
                &v[v.len() - 4..],
                ["-applaunch", "620", "-novid", "+map test"]
            );
        } else {
            assert_eq!(
                v.last().unwrap(),
                "steam://run/620//-novid%20%2Bmap%20test/"
            );
        }
    }

    #[test]
    fn links_name_the_app() {
        assert_eq!(ClientLink::Install.url("70"), "steam://install/70");
        assert_eq!(ClientLink::Uninstall.url("70"), "steam://uninstall/70");
        assert_eq!(ClientLink::Verify.url("70"), "steam://validate/70");
        assert_eq!(
            ClientLink::Details.url("70"),
            "steam://nav/games/details/70"
        );
    }
}
