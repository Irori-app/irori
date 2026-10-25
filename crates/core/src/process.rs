//! Running programs and games: process groups, line streaming, cancellation
//! and bounded output capture.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result};
#[cfg(unix)]
use nix::sys::signal::{Signal, killpg};
#[cfg(unix)]
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
pub use tokio_util::sync::CancellationToken;

/// Which pipe a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// A fully specified command line. Built by stores and the runtime, executed
/// by [`run`] or [`spawn`].
#[derive(Debug, Clone, Default)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Variables set on top of the inherited environment (ordered for logs).
    pub env: BTreeMap<String, String>,
    pub env_remove: Vec<String>,
    pub cwd: Option<PathBuf>,
}

impl CommandSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            ..Default::default()
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn envs<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.env
            .extend(vars.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Prefix the command with wrapper programs (`gamemoderun`, `mangohud`, …):
    /// `[w0, w0args.., w1, .., program, args..]`.
    pub fn wrapped(self, wrappers: &[Vec<String>]) -> Self {
        if wrappers.is_empty() {
            return self;
        }
        let mut parts: Vec<OsString> = wrappers.iter().flatten().map(OsString::from).collect();
        parts.push(self.program.clone().into_os_string());
        parts.extend(self.args);
        let program = PathBuf::from(parts.remove(0));
        Self {
            program,
            args: parts,
            ..self
        }
    }

    fn to_command(&self) -> Result<Command> {
        let program =
            if cfg!(target_os = "macos") && self.program.extension().is_some_and(|e| e == "app") {
                crate::platform::bundle_executable(&self.program)?
            } else {
                self.program.clone()
            };
        let mut cmd = Command::new(program);
        cmd.args(&self.args);
        for key in &self.env_remove {
            cmd.env_remove(key);
        }
        cmd.envs(&self.env);
        if let Some(dir) = &self.cwd {
            cmd.current_dir(dir);
        }
        // Own process group so cancellation reaches every descendant
        // (wine, wineserver children, game processes).
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.kill_on_drop(false);
        Ok(cmd)
    }
}

impl fmt::Display for CommandSpec {
    /// Shell-like rendering for logs. Values of variables that look like
    /// secrets are masked.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (k, v) in &self.env {
            let masked = ["TOKEN", "SECRET", "PASSWORD", "AUTH"]
                .iter()
                .any(|s| k.to_ascii_uppercase().contains(s));
            if masked {
                write!(f, "{k}=*** ")?;
            } else {
                write!(f, "{k}={} ", shell_quote(v))?;
            }
        }
        write!(f, "{}", shell_quote(&self.program.to_string_lossy()))?;
        let mut mask_next = false;
        for a in &self.args {
            let a = a.to_string_lossy();
            if mask_next {
                write!(f, " ***")?;
                mask_next = false;
                continue;
            }
            if let Some((flag, _)) = a.split_once('=').filter(|(k, _)| is_secret_flag(k)) {
                write!(f, " {flag}=***")?;
                continue;
            }
            mask_next = is_secret_flag(&a);
            write!(f, " {}", shell_quote(&a))?;
        }
        Ok(())
    }
}

/// Flags whose value is a credential and must not appear in logs.
fn is_secret_flag(arg: &str) -> bool {
    matches!(
        arg,
        "--code" | "--code-verifier" | "--token" | "--auth-code" | "--password" | "--refresh-token"
    )
}

/// Quote a string for display in a POSIX shell command line.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Split a user-entered argument string like a shell would (quotes, escapes).
pub fn split_args(input: &str) -> Vec<String> {
    if cfg!(windows) {
        split_windows_args(input)
    } else {
        split_unix_args(input)
    }
}

fn split_unix_args(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(&n) = chars.peek()
                    && (n == '"' || n == '\\')
                {
                    cur.push(n);
                    chars.next();
                    continue;
                }
                cur.push('\\');
            }
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_token = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    in_token = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if in_token {
                    out.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_token = true;
            }
        }
    }
    if in_token {
        out.push(cur);
    }
    out
}

/// Parse an argument tail using Microsoft's CRT quoting rules. Backslashes
/// remain path separators unless immediately followed by a double quote.
/// This parses arguments, not argv[0], which has different Windows rules.
pub fn split_windows_args(input: &str) -> Vec<String> {
    let mut chars = input.chars().peekable();
    let mut result = Vec::new();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| matches!(c, ' ' | '\t')) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut arg = String::new();
        let mut quoted = false;
        loop {
            let mut slashes = 0;
            while chars.peek() == Some(&'\\') {
                slashes += 1;
                chars.next();
            }
            if chars.peek() == Some(&'"') {
                arg.extend(std::iter::repeat_n('\\', slashes / 2));
                chars.next();
                if slashes % 2 == 1 {
                    arg.push('"');
                } else if quoted && chars.peek() == Some(&'"') {
                    arg.push('"');
                    chars.next();
                } else {
                    quoted = !quoted;
                }
            } else {
                arg.extend(std::iter::repeat_n('\\', slashes));
                match chars.peek().copied() {
                    None => break,
                    Some(' ' | '\t') if !quoted => break,
                    Some(c) => {
                        arg.push(c);
                        chars.next();
                    }
                }
            }
        }
        result.push(arg);
    }
    result
}

/// Encode one Windows CRT argument for shortcuts and URL-handler registration.
/// This is not quoting for cmd.exe or PowerShell; never embed it in a script.
pub fn windows_arg(arg: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(
            '\\',
            if c == '"' { slashes * 2 + 1 } else { slashes },
        ));
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

/// Line callback. Receives each line (without terminator); `\r`-separated
/// progress updates are delivered as separate lines.
pub type LineHandler = Box<dyn FnMut(Stream, &str) + Send>;

#[derive(Default)]
pub struct RunOptions {
    pub on_line: Option<LineHandler>,
    pub cancel: Option<CancellationToken>,
    /// Append all output to this file.
    pub log_file: Option<PathBuf>,
    /// Bytes written to stdin, then closed.
    pub stdin: Option<Vec<u8>>,
    /// Max bytes retained per stream in [`ExecResult`] (tail kept). Default 256 KiB.
    pub capture_limit: Option<usize>,
}

impl RunOptions {
    pub fn on_line(mut self, f: impl FnMut(Stream, &str) + Send + 'static) -> Self {
        self.on_line = Some(Box::new(f));
        self
    }

    pub fn cancel(mut self, token: CancellationToken) -> Self {
        self.cancel = Some(token);
        self
    }

    pub fn log_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.log_file = Some(path.into());
        self
    }

    pub fn stdin(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(data.into());
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExecResult {
    /// Exit code; `None` when killed by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub aborted: bool,
}

impl ExecResult {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.aborted
    }

    /// Last non-empty stderr line, for error messages.
    pub fn error_summary(&self) -> String {
        self.stderr
            .lines()
            .rev()
            .chain(self.stdout.lines().rev())
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("process failed")
            .to_string()
    }

    /// Turn a failed run into an error carrying the summary.
    pub fn check(self) -> Result<Self> {
        if self.success() {
            Ok(self)
        } else if self.aborted {
            anyhow::bail!("aborted")
        } else {
            anyhow::bail!("{}", self.error_summary())
        }
    }
}

/// Run a command to completion, streaming lines and honouring cancellation.
pub async fn run(spec: &CommandSpec, mut opts: RunOptions) -> Result<ExecResult> {
    tracing::debug!(command = %spec, "run");
    let mut cmd = spec.to_command()?;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if opts.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to start {}", spec.program.display()))?;

    if let (Some(data), Some(mut stdin)) = (opts.stdin.take(), child.stdin.take()) {
        tokio::spawn(async move {
            let _ = stdin.write_all(&data).await;
        });
    }

    let (tx, mut rx) = mpsc::channel::<(Stream, String)>(256);
    let out = child
        .stdout
        .take()
        .map(|s| pump(s, Stream::Stdout, tx.clone()));
    let err = child
        .stderr
        .take()
        .map(|s| pump(s, Stream::Stderr, tx.clone()));
    drop(tx);

    let mut log = match &opts.log_file {
        Some(path) => crate::log_cap::CappedLog::open(path, crate::log_cap::OPERATION_LOG_CAP)
            .await
            .ok(),
        None => None,
    };

    let limit = opts.capture_limit.unwrap_or(256 * 1024);
    let mut stdout = Tail::new(limit);
    let mut stderr = Tail::new(limit);
    let cancel = opts.cancel.clone().unwrap_or_default();
    let mut aborted = false;

    loop {
        tokio::select! {
            line = rx.recv() => match line {
                Some((stream, line)) => {
                    if let Some(f) = log.as_mut() {
                        let _ = f.write_line(&line).await;
                    }
                    if let Some(h) = opts.on_line.as_mut() {
                        h(stream, &line);
                    }
                    match stream {
                        Stream::Stdout => stdout.push(&line),
                        Stream::Stderr => stderr.push(&line),
                    }
                }
                None => break,
            },
            _ = cancel.cancelled(), if !aborted => {
                aborted = true;
                terminate(&mut child).await;
            }
        }
    }
    for task in [out, err].into_iter().flatten() {
        let _ = task.await;
    }
    let status = child.wait().await?;
    if let Some(f) = log.as_mut() {
        let _ = f.flush().await;
    }
    Ok(ExecResult {
        code: status.code(),
        stdout: stdout.into_string(),
        stderr: stderr.into_string(),
        aborted: aborted || cancel.is_cancelled(),
    })
}

/// Run and return stdout when the command succeeds.
pub async fn output(spec: &CommandSpec) -> Result<String> {
    Ok(run(spec, RunOptions::default()).await?.check()?.stdout)
}

/// Start a long-lived process (a game) without piping its output through
/// memory: stdout/stderr go straight to `log_file` (or are discarded).
pub fn spawn(spec: &CommandSpec, log_file: Option<&Path>) -> Result<Child> {
    tracing::info!(command = %spec, "spawn");
    let mut cmd = spec.to_command()?;
    cmd.stdin(Stdio::null());
    match log_file {
        Some(path) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            cmd.stdout(file.try_clone()?).stderr(file);
        }
        None => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    cmd.spawn()
        .with_context(|| format!("failed to start {}", spec.program.display()))
}

/// SIGTERM the child's process group, escalating to SIGKILL after 5 s.
#[cfg(unix)]
pub async fn terminate(child: &mut Child) {
    let Some(pid) = child.id() else { return };
    let pgid = Pid::from_raw(pid as i32);
    let _ = killpg(pgid, Signal::SIGTERM);
    if tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_err()
    {
        let _ = killpg(pgid, Signal::SIGKILL);
        let _ = child.wait().await;
    }
}

/// Signal a process group by its leader pid (used for games tracked by pid).
#[cfg(unix)]
pub fn kill_group(pid: u32, force: bool) {
    let sig = if force {
        Signal::SIGKILL
    } else {
        Signal::SIGTERM
    };
    let _ = killpg(Pid::from_raw(pid as i32), sig);
}

#[cfg(windows)]
pub async fn terminate(child: &mut Child) {
    if let Some(pid) = child.id() {
        let _ = Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(windows)]
pub fn kill_group(pid: u32, force: bool) {
    // Spawn off the UI thread; `/T` includes descendants of the game process.
    std::thread::spawn(move || {
        let mut command = std::process::Command::new("taskkill.exe");
        command.args(["/PID", &pid.to_string(), "/T"]);
        if force {
            command.arg("/F");
        }
        let _ = command.stdout(Stdio::null()).stderr(Stdio::null()).status();
    });
}

fn pump<R: AsyncRead + Unpin + Send + 'static>(
    reader: R,
    stream: Stream,
    tx: mpsc::Sender<(Stream, String)>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::with_capacity(16 * 1024, reader);
        let mut buf = Vec::with_capacity(512);
        loop {
            buf.clear();
            // Split on '\n' and treat embedded '\r' as separators so progress
            // bars that redraw in place yield one line per update.
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&buf);
                    for part in text.split(['\r', '\n']).filter(|p| !p.is_empty()) {
                        if tx.send((stream, part.to_string())).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    })
}

/// Bounded capture that keeps the tail of the output.
struct Tail {
    buf: String,
    limit: usize,
}

impl Tail {
    fn new(limit: usize) -> Self {
        Self {
            buf: String::new(),
            limit,
        }
    }

    fn push(&mut self, line: &str) {
        self.buf.push_str(line);
        self.buf.push('\n');
        if self.buf.len() > self.limit * 2 {
            let cut = self.buf.len() - self.limit;
            let cut = (cut..self.buf.len())
                .find(|&i| self.buf.is_char_boundary(i))
                .unwrap_or(self.buf.len());
            self.buf.drain(..cut);
        }
    }

    fn into_string(self) -> String {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn mac_bundle_launch_tracks_exit_and_environment() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Game With Spaces.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        let mut info = plist::Dictionary::new();
        info.insert(
            "CFBundleExecutable".into(),
            plist::Value::String("game".into()),
        );
        plist::Value::Dictionary(info)
            .to_file_xml(bundle.join("Contents/Info.plist"))
            .unwrap();
        let exe = bundle.join("Contents/MacOS/game");
        std::fs::write(
            &exe,
            b"#!/bin/sh\nprintf '%s|%s' \"$EMBER_TEST\" \"$1\"\nexit 7\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let result = run(
            &CommandSpec::new(bundle)
                .arg("space value")
                .env("EMBER_TEST", "present")
                .cwd(dir.path()),
            RunOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(result.stdout.trim_end(), "present|space value");
        assert_eq!(result.code, Some(7));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_native_run_and_cancel() {
        let result = run(
            &CommandSpec::new("cmd.exe").args(["/D", "/C", "exit 7"]),
            RunOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(result.code, Some(7));
        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            cancel.cancel();
        });
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            run(
                &CommandSpec::new("powershell.exe").args([
                    "-NoProfile",
                    "-Command",
                    "Start-Sleep -Seconds 30",
                ]),
                RunOptions::default().cancel(token),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.aborted);
    }

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(
            split_unix_args(r#"-a "b c" 'd e' f\ g "h\"i""#),
            vec!["-a", "b c", "d e", "f g", "h\"i"]
        );
        assert!(split_args("   ").is_empty());
        assert_eq!(split_unix_args("''"), vec![""]);
    }

    #[test]
    fn windows_paths_and_crt_examples() {
        assert_eq!(
            split_windows_args(r#"C:\Games\Game.exe "C:\My Games\"folder" \\server\share """#),
            [
                r"C:\Games\Game.exe",
                r#"C:\My Games"folder"#,
                r"\\server\share",
                ""
            ]
        );
        assert_eq!(
            split_windows_args(r#"a\\\b d"e f"g h"#),
            [r"a\\\b", "de fg", "h"]
        );
        assert_eq!(split_windows_args(r#"a"b"" c d"#), ["ab\" c d"]);
    }

    #[test]
    fn windows_argument_roundtrip_exhaustive() {
        // Exhaust the short strings over characters with special meaning, plus
        // Unicode and metacharacters. No test-only dependency is needed.
        let alphabet = ['a', ' ', '\t', '\\', '"', '\'', 'é', '$', '%'];
        let mut values = vec![String::new()];
        for _ in 0..4 {
            let previous = values.clone();
            values = previous
                .iter()
                .flat_map(|s| alphabet.iter().map(move |c| format!("{s}{c}")))
                .collect();
            for value in &values {
                let command = format!(
                    "{} {} {}",
                    windows_arg("before"),
                    windows_arg(value),
                    windows_arg("after")
                );
                assert_eq!(split_windows_args(&command), ["before", value, "after"]);
            }
        }
        assert_eq!(split_windows_args(&windows_arg("")), [""]);
    }

    #[test]
    fn masks_secret_flags() {
        let spec = CommandSpec::new("legendary").args(["auth", "--code", "abc", "--token=xyz"]);
        let shown = spec.to_string();
        assert!(!shown.contains("abc") && !shown.contains("xyz"), "{shown}");
    }

    #[test]
    fn wraps_commands() {
        let spec = CommandSpec::new("/bin/game").arg("--x").wrapped(&[
            vec!["gamemoderun".into()],
            vec!["mangohud".into(), "--dlsym".into()],
        ]);
        assert_eq!(spec.program, PathBuf::from("gamemoderun"));
        assert_eq!(
            spec.args,
            vec!["mangohud", "--dlsym", "/bin/game", "--x"]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn streams_and_captures() {
        let spec = CommandSpec::new("sh").args(["-c", "printf 'a\\rb\\n'; echo err >&2; exit 3"]);
        let lines = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let l2 = lines.clone();
        let res = run(
            &spec,
            RunOptions::default().on_line(move |s, l| l2.lock().push((s, l.to_string()))),
        )
        .await
        .unwrap();
        assert_eq!(res.code, Some(3));
        assert_eq!(res.stderr.trim(), "err");
        let got = lines.lock().clone();
        assert!(got.contains(&(Stream::Stdout, "a".into())));
        assert!(got.contains(&(Stream::Stdout, "b".into())));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_kills_group() {
        let token = CancellationToken::new();
        let t2 = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            t2.cancel();
        });
        let spec = CommandSpec::new("sh").args(["-c", "sleep 30 & sleep 30"]);
        let res = run(&spec, RunOptions::default().cancel(token))
            .await
            .unwrap();
        assert!(res.aborted);
    }
}
