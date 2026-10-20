//! What the Epic Games Launcher passes to games: the executable, the build's
//! own arguments and the `-AUTH_*`/`-epic*` parameters.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// Inputs of [`launch_params`].
pub struct LaunchInput<'a> {
    pub install_dir: &'a Path,
    /// Executable relative to the install folder (manifest or override).
    pub executable: &'a str,
    /// The build's launch command (`LaunchCommand` of the manifest).
    pub launch_parameters: &'a str,
    /// `AdditionalCommandLine` of the game's (or add-on's) metadata.
    pub additional_command_line: Option<&'a str>,
    /// App name passed as `-epicapp`.
    pub app_name: &'a str,
    /// One-time exchange code; `None` when offline.
    pub exchange_code: Option<&'a str>,
    /// Ownership token file for DRM-protected games.
    pub ownership_token: Option<&'a Path>,
    pub account_id: &'a str,
    pub display_name: &'a str,
    pub language: &'a str,
    pub namespace: &'a str,
    pub deployment_id: Option<&'a str>,
}

/// The resolved launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchParams {
    pub executable: PathBuf,
    pub working_dir: PathBuf,
    /// The game's own arguments (manifest and metadata).
    pub game_args: Vec<String>,
    /// The launcher arguments (authentication, account, locale).
    pub egl_args: Vec<String>,
}

/// Split like Python's `shlex.split(posix=False)`: whitespace outside
/// double quotes separates words, and quotes are kept.
pub fn split_keep_quotes(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Resolve the executable and build the argument lists.
pub fn launch_params(i: &LaunchInput<'_>) -> Result<LaunchParams> {
    let rel = i.executable.replace('\\', "/");
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() {
        bail!("The game's executable is unknown; choose one in the game settings");
    }
    let executable = ember_depot::path::join(i.install_dir, &ember_depot::path::normalize(rel)?);
    let working_dir = executable
        .parent()
        .map_or_else(|| i.install_dir.to_path_buf(), Path::to_path_buf);
    let mut game_args = split_keep_quotes(i.launch_parameters);
    if let Some(extra) = i.additional_command_line {
        game_args.extend(split_keep_quotes(extra.trim()));
    }
    let mut egl_args = vec![
        "-AUTH_LOGIN=unused".to_string(),
        format!("-AUTH_PASSWORD={}", i.exchange_code.unwrap_or_default()),
        "-AUTH_TYPE=exchangecode".to_string(),
        format!("-epicapp={}", i.app_name),
        "-epicenv=Prod".to_string(),
    ];
    if let Some(ovt) = i.ownership_token {
        egl_args.push(format!("-epicovt={}", ovt.display()));
    }
    egl_args.extend([
        "-EpicPortal".to_string(),
        format!("-epicusername={}", i.display_name),
        format!("-epicuserid={}", i.account_id),
        format!("-epiclocale={}", i.language),
        format!("-epicsandboxid={}", i.namespace),
    ]);
    if let Some(dep) = i.deployment_id.filter(|d| !d.is_empty()) {
        egl_args.push(format!("-epicdeploymentid={dep}"));
    }
    Ok(LaunchParams {
        executable,
        working_dir,
        game_args,
        egl_args,
    })
}

/// `link2ea://` URI that starts an EA app game for the account.
pub fn origin_uri(
    app_name: &str,
    exchange_code: Option<&str>,
    display_name: &str,
    account_id: &str,
    language: &str,
    additional_command_line: Option<&str>,
) -> String {
    let mut q = url::form_urlencoded::Serializer::new(String::new());
    q.append_pair("AUTH_PASSWORD", exchange_code.unwrap_or("0"))
        .append_pair("AUTH_TYPE", "exchangecode")
        .append_pair("epicusername", display_name)
        .append_pair("epicuserid", account_id)
        .append_pair("epiclocale", language);
    if let Some(extra) = additional_command_line {
        for (k, v) in url::form_urlencoded::parse(extra.as_bytes()) {
            q.append_pair(&k, &v);
        }
    }
    format!("link2ea://launchgame/{app_name}?{}", q.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>() -> LaunchInput<'a> {
        LaunchInput {
            install_dir: Path::new("/games/Sugar"),
            executable: "Bin\\Win64\\Sugar.exe",
            launch_parameters: "-nosplash \"-ini:Engine:[Core]:x=1\"",
            additional_command_line: Some(" -dx12 "),
            app_name: "Sugar",
            exchange_code: Some("XCHG"),
            ownership_token: None,
            account_id: "acc",
            display_name: "Player",
            language: "en",
            namespace: "ns",
            deployment_id: Some("dep"),
        }
    }

    #[test]
    fn builds_epic_parameters() {
        let p = launch_params(&input()).unwrap();
        assert_eq!(p.executable, Path::new("/games/Sugar/Bin/Win64/Sugar.exe"));
        assert_eq!(p.working_dir, Path::new("/games/Sugar/Bin/Win64"));
        assert_eq!(
            p.game_args,
            ["-nosplash", "\"-ini:Engine:[Core]:x=1\"", "-dx12"]
        );
        assert_eq!(
            p.egl_args,
            [
                "-AUTH_LOGIN=unused",
                "-AUTH_PASSWORD=XCHG",
                "-AUTH_TYPE=exchangecode",
                "-epicapp=Sugar",
                "-epicenv=Prod",
                "-EpicPortal",
                "-epicusername=Player",
                "-epicuserid=acc",
                "-epiclocale=en",
                "-epicsandboxid=ns",
                "-epicdeploymentid=dep"
            ]
        );
        let offline = LaunchInput {
            exchange_code: None,
            ownership_token: Some(Path::new("/tmp/x.ovt")),
            ..input()
        };
        let p = launch_params(&offline).unwrap();
        assert_eq!(p.egl_args[1], "-AUTH_PASSWORD=");
        assert_eq!(p.egl_args[5], "-epicovt=/tmp/x.ovt");
        let escape = LaunchInput {
            executable: "../../etc/x",
            ..input()
        };
        assert!(launch_params(&escape).is_err());
    }

    #[test]
    fn builds_origin_uri() {
        assert_eq!(
            origin_uri("Ea", None, "Player One", "acc", "en", Some("a=1&b=2")),
            "link2ea://launchgame/Ea?AUTH_PASSWORD=0&AUTH_TYPE=exchangecode&epicusername=Player+One&epicuserid=acc&epiclocale=en&a=1&b=2"
        );
    }
}
