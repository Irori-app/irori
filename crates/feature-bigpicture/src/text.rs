//! Localized text that is computed rather than looked up: status labels,
//! store names, play time, percentages and the clock.

use ember_core::{Runner, StatusKind};
use ember_ui::i18n::{t, t_args};
use gpui_kit::SharedString;

/// Present-progressive label of a game status ("Installing").
pub fn status_label(kind: StatusKind) -> SharedString {
    t(match kind {
        StatusKind::Installing => "bigpicture.status.installing",
        StatusKind::Importing => "bigpicture.status.importing",
        StatusKind::Updating => "bigpicture.status.updating",
        StatusKind::Launching => "bigpicture.status.launching",
        StatusKind::Playing => "bigpicture.status.playing",
        StatusKind::Uninstalling => "bigpicture.status.uninstalling",
        StatusKind::Repairing => "bigpicture.status.verifying",
        StatusKind::Moving => "bigpicture.status.moving",
        StatusKind::Queued => "bigpicture.status.queued",
        StatusKind::SyncingSaves => "bigpicture.status.syncing_saves",
        StatusKind::Redist => "bigpicture.status.installing_dependencies",
        StatusKind::Extracting => "bigpicture.status.extracting",
        StatusKind::Winetricks => "bigpicture.status.winetricks",
        StatusKind::Done => "bigpicture.status.done",
        StatusKind::Canceled => "bigpicture.status.canceled",
        StatusKind::Error => "bigpicture.status.error",
    })
}

/// A status label with its progress ("Installing · 42%").
pub fn status_with_percent(kind: StatusKind, percent: Option<f32>) -> SharedString {
    let label = status_label(kind);
    match percent {
        Some(p) => t_args(
            "bigpicture.status.with_percent",
            &[
                ("status", label.as_str()),
                ("percent", format!("{p:.0}").as_str()),
            ],
        ),
        None => label,
    }
}

/// Display name of a store. Store names are brands and stay as they are;
/// manually added apps get a translated name.
pub fn store_name(runner: Runner) -> SharedString {
    match runner {
        Runner::Sideload => t("bigpicture.store.sideload"),
        other => SharedString::new_static(other.store_name()),
    }
}

/// Play time in hours and minutes ("3 h 12 min"), "< 1 min" for less.
pub fn playtime(minutes: u64) -> SharedString {
    let (h, m) = ((minutes / 60).to_string(), (minutes % 60).to_string());
    match (minutes / 60, minutes % 60) {
        (0, 0) => t("bigpicture.playtime.under_minute"),
        (0, _) => t_args("bigpicture.playtime.minutes", &[("minutes", m.as_str())]),
        (_, 0) => t_args("bigpicture.playtime.hours", &[("hours", h.as_str())]),
        _ => t_args(
            "bigpicture.playtime.hours_minutes",
            &[("hours", h.as_str()), ("minutes", m.as_str())],
        ),
    }
}

/// How a language usually writes the time of day, following the CLDR short
/// time format of each interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HourCycle {
    /// 1–12 with a day period ("3:05 PM", "오후 3:05", "下午3:05").
    H12,
    /// 0–23; `pad` writes a leading zero ("09:05" rather than "9:05").
    H23 { pad: bool },
}

/// The usual clock of an interface language code.
///
/// English (Ember's English follows US/Canadian/Australian usage), Korean and
/// Traditional Chinese (Taiwan, Hong Kong) commonly use a 12-hour clock with a
/// day period. Every other shipped language uses 24 hours; Czech, Spanish and
/// Japanese write the hour without a leading zero.
pub fn hour_cycle(language: &str) -> HourCycle {
    match language {
        "en" | "ko" | "zh_Hant" => HourCycle::H12,
        "cs" | "es" | "ja" => HourCycle::H23 { pad: false },
        _ => HourCycle::H23 { pad: true },
    }
}

/// The digits of a clock reading and, on a 12-hour clock, whether it is
/// afternoon (`Some(true)` for PM).
pub fn clock_parts(hour: u32, minute: u32, cycle: HourCycle) -> (String, Option<bool>) {
    match cycle {
        HourCycle::H12 => {
            let h12 = match hour % 12 {
                0 => 12,
                h => h,
            };
            (format!("{h12}:{minute:02}"), Some(hour >= 12))
        }
        HourCycle::H23 { pad: true } => (format!("{hour:02}:{minute:02}"), None),
        HourCycle::H23 { pad: false } => (format!("{hour}:{minute:02}"), None),
    }
}

/// Clock text for `hour:minute` in the active interface language.
pub fn clock(hour: u32, minute: u32) -> SharedString {
    let (time, pm) = clock_parts(hour, minute, hour_cycle(&ember_ui::i18n::language()));
    match pm {
        None => time.into(),
        Some(pm) => {
            let period = t(if pm {
                "bigpicture.clock.pm"
            } else {
                "bigpicture.clock.am"
            });
            t_args(
                "bigpicture.clock.time_12h",
                &[("time", time.as_str()), ("period", period.as_str())],
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_follows_the_language() {
        assert_eq!(hour_cycle("en"), HourCycle::H12);
        assert_eq!(hour_cycle("ko"), HourCycle::H12);
        assert_eq!(hour_cycle("de"), HourCycle::H23 { pad: true });
        assert_eq!(hour_cycle("ja"), HourCycle::H23 { pad: false });

        assert_eq!(
            clock_parts(0, 5, HourCycle::H12),
            ("12:05".into(), Some(false))
        );
        assert_eq!(
            clock_parts(12, 0, HourCycle::H12),
            ("12:00".into(), Some(true))
        );
        assert_eq!(
            clock_parts(15, 7, HourCycle::H12),
            ("3:07".into(), Some(true))
        );
        assert_eq!(
            clock_parts(9, 5, HourCycle::H23 { pad: true }),
            ("09:05".into(), None)
        );
        assert_eq!(
            clock_parts(9, 5, HourCycle::H23 { pad: false }),
            ("9:05".into(), None)
        );
        assert_eq!(
            clock_parts(23, 59, HourCycle::H23 { pad: true }),
            ("23:59".into(), None)
        );
    }
}
