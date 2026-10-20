//! Formatting and parsing helpers shared by stores and UI.

use std::time::{SystemTime, UNIX_EPOCH};

/// `1536` → `"1.5 KiB"`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[unit])
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// Parse sizes such as `"12.3 GiB"`, `"512 MB"`, `"1,024.5 MiB"`.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().replace(',', "");
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let num: f64 = num.trim().parse().ok()?;
    let mult: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" | "bytes" => 1.0,
        "kb" => 1e3,
        "kib" | "k" => 1024.0,
        "mb" => 1e6,
        "mib" | "m" => 1024.0 * 1024.0,
        "gb" => 1e9,
        "gib" | "g" => 1024.0 * 1024.0 * 1024.0,
        "tb" => 1e12,
        "tib" | "t" => 1024f64.powi(4),
        _ => return None,
    };
    Some((num * mult) as u64)
}

/// `3725` → `"1h 2m"`, `95` → `"1m 35s"`.
pub fn format_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    match (h, m) {
        (0, 0) => format!("{s}s"),
        (0, _) => format!("{m}m {s}s"),
        _ => format!("{h}h {m}m"),
    }
}

/// Minutes of playtime as `"12 h 5 min"`.
pub fn format_playtime(minutes: u64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// Whether `path` is a file the user may execute (any file off Unix).
pub fn is_executable(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Current time as RFC 3339 (UTC).
pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Unix seconds as RFC 3339 (UTC).
pub fn rfc3339_from_unix(secs: i64) -> Option<String> {
    time::OffsetDateTime::from_unix_timestamp(secs)
        .ok()?
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

/// Parse RFC 3339 to unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|t| t.unix_timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(parse_size("1.5 KiB"), Some(1536));
        assert_eq!(parse_size("2 GB"), Some(2_000_000_000));
        assert_eq!(parse_size("1,024 MiB"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size("x"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(3725), "1h 2m");
        assert_eq!(format_playtime(125), "2 h 5 min");
    }
}
