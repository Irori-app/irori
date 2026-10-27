//! Crash reports. Release builds abort on a panic, so a crash used to leave
//! no trace: the window vanished and the log ended mid-session. A panic hook
//! now writes a report next to the log before the abort, and the next start
//! tells the user where it is.

use std::path::{Path, PathBuf};

use irori_core::{EventBus, Notice, NoticeLevel, Paths, util};

/// Reports kept on disk; older ones are deleted.
const KEPT: usize = 5;
const PREFIX: &str = "crash-";
/// Suffix of a report the user has been told about.
const REPORTED: &str = ".reported.txt";

/// Write a crash report for every panic, then run the default hook (which
/// prints to stderr). Call once, before other threads start.
pub(crate) fn install() {
    let dir = Paths::get().logs_dir();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let text = report(
            info.payload_as_str().unwrap_or("(no message)"),
            info.location().map(|l| l.to_string()).as_deref(),
            &backtrace.to_string(),
        );
        // Plain file writes: the logger may be the thing that panicked.
        if let Some(path) = write(&dir, util::unix_now(), &text) {
            eprintln!("Irori crashed; report saved to {}", path.display());
        }
        previous(info);
    }));
}

/// The text of a crash report.
fn report(message: &str, location: Option<&str>, backtrace: &str) -> String {
    let thread = std::thread::current();
    format!(
        "Irori {version} crashed\n\
         OS: {os} {arch}\n\
         Thread: {thread}\n\
         Location: {location}\n\
         Message: {message}\n\
         \n\
         Backtrace:\n{backtrace}\n",
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        thread = thread.name().unwrap_or("unnamed"),
        location = location.unwrap_or("unknown"),
    )
}

fn write(dir: &Path, now: u64, text: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("{PREFIX}{now}.txt"));
    std::fs::write(&path, text).ok()?;
    Some(path)
}

/// Reports in `dir`, oldest first.
fn reports(dir: &Path) -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(".txt"))
        })
        .collect();
    list.sort();
    list
}

/// The newest report the user has not been told about, marked as told; older
/// reports past [`KEPT`] are deleted.
fn take_unreported(dir: &Path) -> Option<PathBuf> {
    let all = reports(dir);
    for old in all.iter().take(all.len().saturating_sub(KEPT)) {
        let _ = std::fs::remove_file(old);
    }
    let mut newest = None;
    for path in reports(dir) {
        let name = path.file_name()?.to_str()?.to_string();
        if name.ends_with(REPORTED) {
            continue;
        }
        let told = path.with_file_name(name.replace(".txt", REPORTED));
        if std::fs::rename(&path, &told).is_ok() {
            newest = Some(told);
        }
    }
    newest
}

/// Tell the user, once, that the last session crashed and where its report
/// is.
pub(crate) fn notice_last_crash(bus: &EventBus) {
    let Some(path) = take_unreported(&Paths::get().logs_dir()) else {
        return;
    };
    let shown = path.display().to_string();
    bus.notice(
        Notice::new(NoticeLevel::Warning, "Irori closed unexpectedly last time")
            .message(format!("A crash report was saved to {shown}"))
            .keys(
                "shell.notice.crashed_last_time",
                Some("shell.notice.crashed_last_time_message"),
                &[("path", &shown)],
            ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_say_what_failed_where() {
        let text = report("index out of bounds", Some("src/a.rs:3:9"), "0: main");
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
        assert!(text.contains("Location: src/a.rs:3:9"));
        assert!(text.contains("Message: index out of bounds"));
        assert!(text.contains("0: main"));
    }

    #[test]
    fn tells_about_the_newest_crash_once_and_keeps_a_few() {
        let dir = tempfile::tempdir().unwrap();
        assert!(take_unreported(dir.path()).is_none());
        for t in 1..=7 {
            write(dir.path(), 1_000 + t, "x").unwrap();
        }
        let told = take_unreported(dir.path()).unwrap();
        assert!(told.ends_with(format!("crash-1007{REPORTED}")));
        assert!(take_unreported(dir.path()).is_none(), "only once");
        assert_eq!(reports(dir.path()).len(), KEPT);
    }
}
