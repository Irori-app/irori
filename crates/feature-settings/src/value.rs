//! Pure value logic behind the settings form: JSON path access, text
//! conversion and validation, override planning and list-editor rules.

use anyhow::{Context as _, Result};
use irori_core::settings::{GameSettings, GameSettingsOverrides};
use irori_ui::i18n::{t, t_args};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::registry::TextKind;

/// First segment of a dotted path: the key stored in a game's override map.
pub fn override_key(path: &str) -> &str {
    path.split('.').next().unwrap_or(path)
}

/// The value at a dotted path.
pub fn get_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(root, |v, seg| v.get(seg))
}

/// Set the value at a dotted path, creating intermediate objects. Returns
/// false when an intermediate value is not an object.
pub fn set_path(root: &mut Value, path: &str, value: Value) -> bool {
    let mut cur = root;
    let mut segs = path.split('.').peekable();
    while let Some(seg) = segs.next() {
        if !cur.is_object() {
            if cur.is_null() {
                *cur = Value::Object(Map::new());
            } else {
                return false;
            }
        }
        let Value::Object(map) = cur else {
            return false;
        };
        if segs.peek().is_none() {
            map.insert(seg.to_string(), value);
            return true;
        }
        cur = map.entry(seg.to_string()).or_insert(Value::Null);
    }
    false
}

/// `item` with the value at `path` replaced, through a JSON round-trip.
pub fn with_path<T: Serialize + DeserializeOwned>(item: &T, path: &str, value: Value) -> Result<T> {
    let mut json = serde_json::to_value(item).context("serializing settings")?;
    if !set_path(&mut json, path, value) {
        anyhow::bail!("{path} is not a settings key");
    }
    serde_json::from_value(json).with_context(|| format!("invalid value for {path}"))
}

/// The override to store for the key owning `path` after the user sets
/// `path` to `value` in a game whose effective settings are `effective`:
/// `None` when the result equals the global default (the game keeps
/// inheriting it), the new value of the whole key otherwise. Nested paths
/// (`gamescope.fpsLimiter`) override the whole parent object.
pub fn plan_game_write(
    defaults: &GameSettings,
    effective: &GameSettings,
    path: &str,
    value: Value,
) -> Result<Option<Value>> {
    let key = override_key(path);
    let next = with_path(effective, path, value)?;
    let next_json = serde_json::to_value(&next)?;
    let default_json = serde_json::to_value(defaults)?;
    let new = next_json.get(key).cloned().unwrap_or(Value::Null);
    Ok((default_json.get(key) != Some(&new)).then_some(new))
}

/// Keys of `overrides` that actually differ from `defaults`. Stale entries
/// (written before a default changed to the same value) are not reported.
pub fn effective_overrides(
    defaults: &GameSettings,
    overrides: &GameSettingsOverrides,
) -> Vec<String> {
    let Ok(Value::Object(d)) = serde_json::to_value(defaults) else {
        return Vec::new();
    };
    let effective = serde_json::to_value(overrides.apply(defaults)).unwrap_or_default();
    overrides
        .0
        .keys()
        .filter(|k| d.get(k.as_str()) != effective.get(k.as_str()))
        .cloned()
        .collect()
}

/// Text shown in an input for a stored value.
pub fn value_to_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Convert input text into the stored value, or a user-facing reason why it
/// is invalid.
pub fn text_to_value(text: &str, kind: TextKind) -> Result<Value, String> {
    let text = text.trim();
    match kind {
        TextKind::Plain | TextKind::Directory | TextKind::File | TextKind::Secret => {
            Ok(Value::String(text.to_string()))
        }
        TextKind::Digits { max_len } => {
            if !text.chars().all(|c| c.is_ascii_digit()) {
                return Err(t("settings.validate.whole_number").into());
            }
            if text.len() > max_len {
                return Err(t_args(
                    "settings.validate.max_digits",
                    &[("count", &max_len.to_string())],
                )
                .into());
            }
            if !text.is_empty() && text.trim_start_matches('0').is_empty() {
                return Err(t("settings.validate.above_zero").into());
            }
            Ok(Value::String(text.to_string()))
        }
        TextKind::Number { min, max } => {
            let n: u32 = text
                .parse()
                .map_err(|_| t("settings.validate.whole_number").to_string())?;
            if n < min || n > max {
                return Err(t_args(
                    "settings.validate.number_range",
                    &[("min", &min.to_string()), ("max", &max.to_string())],
                )
                .into());
            }
            Ok(Value::from(n))
        }
    }
}

/// Extra checks on game arguments: environment variables and Steam's
/// `%command%` syntax don't belong there.
pub fn validate_launcher_args(args: &str) -> Result<(), String> {
    if args.contains("%command%") {
        return Err(t("settings.validate.args_command_syntax").into());
    }
    let looks_like_env = args.split_whitespace().any(|w| {
        w.split_once('=').is_some_and(|(k, v)| {
            !k.is_empty()
                && !v.is_empty()
                && k.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && !k.starts_with('-')
        })
    });
    if looks_like_env {
        return Err(t("settings.validate.args_env_vars").into());
    }
    Ok(())
}

/// Check a Discord application ID: empty (presence off) or a 17–20 digit
/// snowflake as shown on discord.com/developers.
pub fn validate_discord_client_id(id: &str) -> Result<(), String> {
    let id = id.trim();
    if id.is_empty() {
        return Ok(());
    }
    if !id.chars().all(|c| c.is_ascii_digit()) || !(17..=20).contains(&id.len()) {
        return Err(t("settings.validate.discord_id").into());
    }
    Ok(())
}

/// Validate an environment variable name against the other rows
/// (`skip` is the row being edited).
pub fn validate_env_key(
    key: &str,
    existing: &[String],
    skip: Option<usize>,
) -> Result<String, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err(t("settings.validate.env_name_empty").into());
    }
    if key.contains('=') {
        return Err(t("settings.validate.env_name_equals").into());
    }
    if key.chars().any(char::is_whitespace) {
        return Err(t("settings.validate.env_name_spaces").into());
    }
    if existing
        .iter()
        .enumerate()
        .any(|(i, k)| Some(i) != skip && k == key)
    {
        return Err(t_args("settings.validate.env_name_taken", &[("name", key)]).into());
    }
    Ok(key.to_string())
}

/// Split `KEY=VALUE` typed into the name field while the value is empty.
pub fn split_assignment(name: &str, value: &str) -> (String, String) {
    if value.trim().is_empty()
        && let Some((k, v)) = name.split_once('=')
    {
        return (k.trim().to_string(), v.trim().to_string());
    }
    (name.trim().to_string(), value.trim().to_string())
}

/// Replace (`Some(index)`) or append a row in a JSON array of objects.
pub fn upsert_row(rows: &Value, index: Option<usize>, row: Value) -> Value {
    let mut list = rows.as_array().cloned().unwrap_or_default();
    match index {
        Some(i) if i < list.len() => list[i] = row,
        _ => list.push(row),
    }
    Value::Array(list)
}

/// Remove a row from a JSON array.
pub fn remove_row(rows: &Value, index: usize) -> Value {
    let mut list = rows.as_array().cloned().unwrap_or_default();
    if index < list.len() {
        list.remove(index);
    }
    Value::Array(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_core::AppSettings;

    #[test]
    fn dotted_paths_read_and_write() {
        let mut v = serde_json::json!({"a": {"b": 1}, "c": true});
        assert_eq!(get_path(&v, "a.b"), Some(&Value::from(1)));
        assert!(set_path(&mut v, "a.b", Value::from(2)));
        assert!(set_path(&mut v, "d.e", Value::from("x")));
        assert_eq!(v["d"]["e"], "x");
        assert!(!set_path(&mut v, "c.x", Value::from(1)));
    }

    #[test]
    fn app_settings_round_trip_through_paths() {
        let s = AppSettings::default();
        let s = with_path(&s, "exitToTray", Value::Bool(true)).unwrap();
        assert!(s.exit_to_tray);
        let s = with_path(&s, "iroriUi.fontSize", Value::from(18.0)).unwrap();
        assert_eq!(s.ui.font_size, 18.0);
        let s = with_path(&s, "gamescope.fpsLimiter", Value::from("60")).unwrap();
        assert_eq!(s.game.gamescope.fps_limiter, "60");
        let s = with_path(&s, "steamRuntime", Value::Bool(false)).unwrap();
        assert_eq!(s.game.steam_runtime, None);
        let s = with_path(&s, "steamRuntime", Value::from("umu-sniper")).unwrap();
        assert_eq!(s.game.steam_runtime.as_deref(), Some("umu-sniper"));
        assert!(with_path(&s, "exitToTray", Value::from("yes")).is_err());
    }

    #[test]
    fn writing_the_default_clears_the_override() {
        let defaults = GameSettings::default();
        let effective = GameSettings {
            show_fps: true,
            ..defaults.clone()
        };
        let plan = plan_game_write(&defaults, &effective, "showFps", Value::Bool(false)).unwrap();
        assert_eq!(plan, None);
        let plan = plan_game_write(&defaults, &defaults, "showFps", Value::Bool(true)).unwrap();
        assert_eq!(plan, Some(Value::Bool(true)));
    }

    #[test]
    fn nested_write_overrides_the_parent_object() {
        let defaults = GameSettings::default();
        let plan = plan_game_write(
            &defaults,
            &defaults,
            "gamescope.fpsLimiter",
            Value::from("60"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan["fpsLimiter"], "60");
        assert_eq!(plan["windowType"], "fullscreen");

        let mut overrides = GameSettingsOverrides::default();
        overrides.set("gamescope", plan);
        let effective = overrides.apply(&defaults);
        assert_eq!(effective.gamescope.fps_limiter, "60");
        // Clearing the nested value again returns to the inherited object.
        let back = plan_game_write(
            &defaults,
            &effective,
            "gamescope.fpsLimiter",
            Value::from(""),
        )
        .unwrap();
        assert_eq!(back, None);
    }

    #[test]
    fn stale_overrides_are_not_reported() {
        let mut defaults = GameSettings::default();
        let mut o = GameSettingsOverrides::default();
        o.set("showFps", Value::Bool(true));
        o.set("enableEsync", Value::Bool(true));
        assert_eq!(
            effective_overrides(&defaults, &o),
            vec!["showFps".to_string()]
        );
        defaults.show_fps = true;
        assert!(effective_overrides(&defaults, &o).is_empty());
    }

    #[test]
    fn diff_keeps_only_changed_keys() {
        let defaults = GameSettings::default();
        let effective =
            with_path(&defaults, "gamescope.enableUpscaling", Value::Bool(true)).unwrap();
        let o = GameSettingsOverrides::diff(&defaults, &effective);
        assert_eq!(o.0.keys().collect::<Vec<_>>(), vec!["gamescope"]);
    }

    #[test]
    fn text_conversion_validates_kinds() {
        let num = TextKind::Number { min: 5, max: 60 };
        assert_eq!(text_to_value(" 10 ", num), Ok(Value::from(10)));
        assert!(text_to_value("4", num).is_err());
        assert!(text_to_value("ten", num).is_err());
        let digits = TextKind::Digits { max_len: 3 };
        assert_eq!(text_to_value("", digits), Ok(Value::from("")));
        assert_eq!(text_to_value("144", digits), Ok(Value::from("144")));
        assert!(text_to_value("1440", digits).is_err());
        assert!(text_to_value("-1", digits).is_err());
        assert!(text_to_value("0", digits).is_err());
        assert_eq!(value_to_text(Some(&Value::from(7))), "7");
        assert_eq!(value_to_text(None), "");
    }

    #[test]
    fn launcher_args_reject_env_and_command_syntax() {
        assert!(validate_launcher_args("-dx11 -nolauncher").is_ok());
        assert!(validate_launcher_args("--width=1920").is_ok());
        assert!(validate_launcher_args("%command% -dx11").is_err());
        assert!(validate_launcher_args("DXVK_HUD=1").is_err());
    }

    #[test]
    fn discord_ids_are_snowflakes_or_empty() {
        assert!(validate_discord_client_id("").is_ok());
        assert!(validate_discord_client_id(" 1234567890123456789 ").is_ok());
        assert!(validate_discord_client_id("12345").is_err());
        assert!(validate_discord_client_id("12345678901234567x9").is_err());
        assert!(validate_discord_client_id("123456789012345678901").is_err());
    }

    #[test]
    fn env_rows_validate_and_split() {
        let existing = vec!["A".to_string(), "B".to_string()];
        assert_eq!(validate_env_key(" C ", &existing, None), Ok("C".into()));
        assert!(validate_env_key("A", &existing, None).is_err());
        assert_eq!(validate_env_key("A", &existing, Some(0)), Ok("A".into()));
        assert!(validate_env_key("", &existing, None).is_err());
        assert!(validate_env_key("A B", &existing, None).is_err());
        assert_eq!(
            split_assignment("DXVK_HUD=fps", ""),
            ("DXVK_HUD".into(), "fps".into())
        );
        assert_eq!(split_assignment("K", " v "), ("K".into(), "v".into()));
    }

    #[test]
    fn row_edits() {
        let rows = serde_json::json!([{"key": "A"}]);
        let rows = upsert_row(&rows, None, serde_json::json!({"key": "B"}));
        let rows = upsert_row(&rows, Some(0), serde_json::json!({"key": "C"}));
        assert_eq!(rows, serde_json::json!([{"key": "C"}, {"key": "B"}]));
        assert_eq!(remove_row(&rows, 1), serde_json::json!([{"key": "C"}]));
    }
}
