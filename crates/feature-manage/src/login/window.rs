//! The sign-in window: the store's page in a process of its own, which reports
//! what the store's login returned. Irori's window never embeds a web engine.
//! The process is the `irori-sign-in` helper or, on macOS and Windows, Irori
//! started in its sign-in mode; on Linux without the helper the dialog uses the
//! browser.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context as _, Result, bail};
use irori_core::process::CancellationToken;
use tokio::io::AsyncReadExt as _;

/// The helper's file name.
const HELPER: &str = if cfg!(windows) {
    "irori-sign-in.exe"
} else {
    "irori-sign-in"
};
/// The helper's exit status when its window was closed without signing in.
const CLOSED: i32 = 2;

/// How the sign-in window ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum WindowOutcome {
    /// The page returned what the login needs: an address or the page's text.
    SignedIn(String),
    /// The user closed the window.
    Closed,
}

/// Irori's first argument for its sign-in mode (`irori_sign_in::SIGN_IN_FLAG`).
const SIGN_IN_FLAG: &str = "--sign-in-window";

/// The program that shows the sign-in window.
#[derive(Debug, Clone)]
pub(super) struct Helper {
    program: PathBuf,
    /// Arguments before the window's own.
    args: &'static [&'static str],
}

/// The helper installed next to Irori or on `PATH`; otherwise, on macOS and
/// Windows, Irori itself (which a plain `cargo run -p irori` leaves without the
/// helper).
pub(super) fn helper() -> Option<Helper> {
    let exe = std::env::current_exe().ok();
    let beside = exe
        .as_ref()
        .map(|exe| exe.with_file_name(HELPER))
        .filter(|p| p.is_file());
    if let Some(program) = beside.or_else(|| which::which(HELPER).ok()) {
        return Some(Helper { program, args: &[] });
    }
    exe.filter(|_| cfg!(any(target_os = "macos", target_os = "windows")))
        .map(|program| Helper {
            program,
            args: &[SIGN_IN_FLAG],
        })
}

/// Show `url` in the sign-in window until the page yields a match of `capture`,
/// the window closes, or `cancel` fires (which closes the window).
pub(super) async fn sign_in(
    helper: Helper,
    url: String,
    capture: String,
    title: String,
    cancel: CancellationToken,
) -> Result<WindowOutcome> {
    let mut child = tokio::process::Command::new(&helper.program)
        .args(helper.args)
        .args(["--url", &url, "--capture", &capture, "--title", &title])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {}", helper.program.display()))?;
    let mut stdout = child
        .stdout
        .take()
        .context("no output from the sign-in window")?;
    let mut stderr = child
        .stderr
        .take()
        .context("no output from the sign-in window")?;
    let finished = async {
        let (mut out, mut err) = (String::new(), String::new());
        let _ = tokio::join!(
            stdout.read_to_string(&mut out),
            stderr.read_to_string(&mut err)
        );
        let status = child.wait().await?;
        anyhow::Ok((status, out, err))
    };
    // Dropping `finished` drops the child, which kills it.
    let (status, out, err) = tokio::select! {
        result = finished => result?,
        _ = cancel.cancelled() => return Ok(WindowOutcome::Closed),
    };
    match status.code() {
        Some(0) => parse_output(&out).map(WindowOutcome::SignedIn),
        Some(CLOSED) => Ok(WindowOutcome::Closed),
        _ => {
            let reason = err.lines().last().unwrap_or("it stopped unexpectedly");
            bail!("the sign-in window failed: {reason}")
        }
    }
}

/// The text from the helper's `{"text": "..."}` line.
fn parse_output(out: &str) -> Result<String> {
    let line = out
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .context("the sign-in window reported nothing")?;
    let value: serde_json::Value =
        serde_json::from_str(line).context("unexpected output from the sign-in window")?;
    value
        .get("text")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .context("unexpected output from the sign-in window")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_output() {
        assert_eq!(
            parse_output("log line\n{\"text\":\"code=abc\"}\n").unwrap(),
            "code=abc"
        );
        assert!(parse_output("").is_err());
        assert!(parse_output("{\"other\":1}").is_err());
    }
}
