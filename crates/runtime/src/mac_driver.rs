//! Options of Wine's macOS driver kept in the prefix registry: Retina mode
//! (`HKCU\Software\Wine\Mac Driver\RetinaMode`), which renders games at the
//! display's full resolution instead of scaling a half-size picture up.
//! Wine, CrossOver and Game Porting Toolkit all read it.

use anyhow::Result;
use ember_core::{AppSettings, GameSettings, OpContext};

use crate::prefix::{self, WineCommand, WineContext};

const KEY: &str = r"HKCU\Software\Wine\Mac Driver";

/// `RetinaMode` in the text of a prefix's `user.reg`: `Some(true)` for "y",
/// `Some(false)` for "n", `None` when unset (Wine's default, off).
pub fn retina_mode_in(user_reg: &str) -> Option<bool> {
    let mut in_section = false;
    for line in user_reg.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line
                .split(']')
                .next()
                .is_some_and(|h| h.eq_ignore_ascii_case(r"[Software\\Wine\\Mac Driver"));
            continue;
        }
        if in_section && let Some(value) = line.strip_prefix("\"RetinaMode\"=") {
            let value = value.trim_matches('"');
            return Some(matches!(value, "y" | "Y" | "1" | "true"));
        }
    }
    None
}

/// Set Retina mode in the prefix when it differs from `enabled`. Reads
/// `user.reg` first so launches with an unchanged setting start no extra
/// Wine process.
pub async fn apply_retina_mode(
    app: &AppSettings,
    settings: &GameSettings,
    wc: &WineContext,
    enabled: bool,
    ctx: &OpContext,
) -> Result<()> {
    let current = tokio::fs::read_to_string(wc.prefix.join("user.reg"))
        .await
        .ok()
        .and_then(|text| retina_mode_in(&text))
        .unwrap_or(false);
    if current == enabled {
        return Ok(());
    }
    let value = if enabled { "y" } else { "n" };
    let mut cmd = WineCommand::new([
        "reg",
        "add",
        KEY,
        "/v",
        "RetinaMode",
        "/t",
        "REG_SZ",
        "/d",
        value,
        "/f",
    ]);
    cmd.wait = true;
    cmd.skip_prefix_check = true;
    prefix::run_wine(app, settings, wc, cmd, ctx)
        .await?
        .check()?;
    tracing::info!("Retina mode {} in {}", value, wc.prefix.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_retina_mode_from_user_reg() {
        let reg = "WINE REGISTRY Version 2\n\n[Software\\\\Wine\\\\Explorer] 1700000000\n\"RetinaMode\"=\"y\"\n\n[Software\\\\Wine\\\\Mac Driver] 1700000001\n#time=1da\n\"RetinaMode\"=\"n\"\n";
        assert_eq!(retina_mode_in(reg), Some(false));
        let on = reg.replace("\"RetinaMode\"=\"n\"", "\"RetinaMode\"=\"y\"");
        assert_eq!(retina_mode_in(&on), Some(true));
        assert_eq!(
            retina_mode_in("[Software\\\\Wine\\\\Explorer] 1\n\"RetinaMode\"=\"y\"\n"),
            None
        );
        assert_eq!(retina_mode_in(""), None);
    }
}
