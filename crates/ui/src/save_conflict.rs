//! Choosing which saves to keep when they changed both on this device and in
//! the cloud since the last sync. Every place that starts a game sends that
//! error here instead of showing it as a failure.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{App, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div};
use irori_core::{GameId, SaveConflict, SyncDirection, util};
use irori_services::LaunchParams;

use crate::i18n::{t, t_args};
use crate::state::{backend, run};

/// When `error` is a save conflict, ask which saves of `id` to keep, then
/// sync that way and start the game with `params`. A missing emulator, core
/// or BIOS gets its fix offered instead. Returns whether it took care of the
/// error; otherwise the caller reports it as usual.
pub fn resolve_launch_error(
    id: &GameId,
    params: LaunchParams,
    error: &anyhow::Error,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    if crate::engine_problem::resolve(id, &params, error, window, cx) {
        return true;
    }
    let Some(conflict) = SaveConflict::of(error).cloned() else {
        return false;
    };
    open_dialog(id.clone(), params, conflict, window, cx);
    true
}

/// "Changed 5 minutes ago" for a save that changed at `at` (unix seconds).
pub fn changed_ago(at: Option<u64>, now: u64) -> SharedString {
    let Some(at) = at else {
        return t("ui.saves.no_saves");
    };
    let secs = now.saturating_sub(at);
    let plural = |n: u64, one: &str, other: &str| {
        t_args(
            if n == 1 { one } else { other },
            &[("count", &n.to_string())],
        )
    };
    match secs {
        s if s < 60 => t("ui.saves.changed_just_now"),
        s if s < 3600 => plural(
            s / 60,
            "ui.saves.changed_minutes_one",
            "ui.saves.changed_minutes_other",
        ),
        s if s < 86_400 => plural(
            s / 3600,
            "ui.saves.changed_hours_one",
            "ui.saves.changed_hours_other",
        ),
        s => plural(
            s / 86_400,
            "ui.saves.changed_days_one",
            "ui.saves.changed_days_other",
        ),
    }
}

fn open_dialog(
    id: GameId,
    params: LaunchParams,
    conflict: SaveConflict,
    window: &mut Window,
    cx: &mut App,
) {
    let title = backend(cx)
        .game(&id)
        .map(|g| g.title.clone())
        .unwrap_or_else(|| id.app_name.to_string());
    let now = util::unix_now();
    let (local, cloud) = (
        changed_ago(conflict.local, now),
        changed_ago(conflict.cloud, now),
    );
    // Dialog widths are pixels; follow the rem size like the rest of the UI.
    let width = window.rem_size() * 34.;
    window.open_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let side = |label: SharedString, changed: SharedString| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().font_semibold().truncate().child(label))
                .child(div().text_sm().text_color(muted).child(changed))
        };
        let choose = |direction: SyncDirection| {
            let (id, params, title) = (id.clone(), params.clone(), title.clone());
            move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                window.close_dialog(cx);
                sync_and_play(id.clone(), params.clone(), direction, &title, window, cx);
            }
        };
        dialog
            .title(t_args("ui.saves.conflict_title", &[("title", &title)]))
            .w(width)
            .child(
                v_flex()
                    .gap_4()
                    .child(div().text_sm().child(t("ui.saves.conflict_description")))
                    .child(
                        h_flex()
                            .gap_4()
                            .child(side(t("ui.saves.this_device"), local.clone()))
                            .child(side(t("ui.saves.cloud"), cloud.clone())),
                    )
                    .child(
                        DialogFooter::new()
                            .child(
                                Button::new("saves-conflict-later")
                                    .ghost()
                                    .label(t("ui.saves.not_now"))
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("saves-keep-local")
                                    .outline()
                                    .label(t("ui.saves.keep_local"))
                                    .on_click(choose(SyncDirection::ForceUpload)),
                            )
                            .child(
                                Button::new("saves-keep-cloud")
                                    .outline()
                                    .label(t("ui.saves.keep_cloud"))
                                    .on_click(choose(SyncDirection::ForceDownload)),
                            ),
                    )
                    .into_any_element(),
            )
    });
}

/// Sync in the chosen direction, then start the game.
fn sync_and_play(
    id: GameId,
    params: LaunchParams,
    direction: SyncDirection,
    title: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let b = backend(cx);
    let failed = t_args("ui.saves.start_failed", &[("title", title)]);
    let task = run(cx, async move {
        b.sync_saves(&id, direction).await?;
        b.launch_with(&id, params).await
    });
    window
        .spawn(cx, async move |cx| {
            if let Err(e) = task.await {
                let _ = cx.update(|window, cx| {
                    window
                        .push_notification(Notification::error(format!("{e:#}")).title(failed), cx);
                });
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_how_long_ago_saves_changed() {
        // Other tests switch the language; compare with the keys, not English.
        crate::i18n::register(crate::i18n::CATALOGS);
        let n = |key: &str, count: &str| t_args(key, &[("count", count)]);
        assert_eq!(changed_ago(None, 100), t("ui.saves.no_saves"));
        assert_eq!(changed_ago(Some(100), 130), t("ui.saves.changed_just_now"));
        assert_eq!(
            changed_ago(Some(0), 60),
            n("ui.saves.changed_minutes_one", "1")
        );
        assert_eq!(
            changed_ago(Some(0), 2 * 3600),
            n("ui.saves.changed_hours_other", "2")
        );
        assert_eq!(
            changed_ago(Some(0), 3 * 86_400),
            n("ui.saves.changed_days_other", "3")
        );
        assert_eq!(changed_ago(Some(500), 100), t("ui.saves.changed_just_now"));
    }
}
