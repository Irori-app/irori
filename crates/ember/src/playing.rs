//! What is playing: the title bar pill naming the running game and how long it
//! has run, and the note after a session saying how long it lasted.

use std::time::Duration;

use ember_core::GameId;
use ember_ui::components::GameCover;
use ember_ui::i18n::{t, t_args};
use ember_ui::models::models;
use ember_ui::nav::{self, Route};
use ember_ui::style::{KeyboardFocusable as _, art_tint};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};

/// How often the pill's elapsed time is refreshed while a game runs. It shows
/// whole minutes, so this only bounds how late a minute ticks over.
pub(crate) const TICK: Duration = Duration::from_secs(20);
/// Sessions shorter than this end without a note: a launcher that closed at
/// once, a quick look at a menu.
const MIN_NOTED_SECONDS: u64 = 60;

/// Minutes as "45 min", "2 h", "2 h 5 min", or "< 1 min".
pub(crate) fn duration_text(minutes: u64) -> SharedString {
    let (h, m) = ((minutes / 60).to_string(), (minutes % 60).to_string());
    match (minutes / 60, minutes % 60) {
        (0, 0) => t("shell.duration.under_minute"),
        (0, _) => t_args("shell.duration.minutes", &[("minutes", &m)]),
        (_, 0) => t_args("shell.duration.hours", &[("hours", &h)]),
        _ => t_args(
            "shell.duration.hours_minutes",
            &[("hours", &h), ("minutes", &m)],
        ),
    }
}

/// Whether a finished session gets a note: it ran long enough to matter and
/// did not crash (crashes have their own note).
fn noted(seconds: u64, crashed: bool) -> bool {
    !crashed && seconds >= MIN_NOTED_SECONDS
}

/// The note after a session of `id` ("You played Hades for 47 min · 12 h in
/// total"), opening the game's page when clicked.
pub(crate) fn session_note(
    id: &GameId,
    seconds: u64,
    crashed: bool,
    cx: &App,
) -> Option<Notification> {
    if !noted(seconds, crashed) {
        return None;
    }
    let b = ember_ui::backend(cx);
    let game = b.game(id)?;
    let total = b
        .userdata
        .snapshot()
        .playtime
        .get(&id.key())
        .map(|r| r.total_minutes)
        .unwrap_or(0);
    let played = duration_text(seconds / 60);
    let total = duration_text(total);
    let id = id.clone();
    Some(
        Notification::info(t_args("shell.session.total", &[("total", &total)]))
            .title(t_args(
                "shell.session.played",
                &[("title", &game.title), ("duration", &played)],
            ))
            .on_click(move |_, _, cx| nav::navigate(Route::Game(id.clone()), cx)),
    )
}

/// The title bar pill for the running game, or `None` when nothing runs.
/// Clicking it opens the game's page; its button stops the game.
pub(crate) fn pill(cx: &mut App) -> Option<AnyElement> {
    let playing: Vec<GameId> = models(cx).statuses.read(cx).playing().cloned().collect();
    let id = playing.first()?.clone();
    let b = ember_ui::backend(cx);
    let game = b.game(&id)?;
    let minutes = b.playing_for(&id).map_or(0, |d| d.as_secs() / 60);
    // The pill takes the colour of the game's art, like its page does.
    let tint = game
        .hero_art()
        .map(str::to_owned)
        .and_then(|url| ember_ui::art::ambient(&url, cx))
        .map(|a| art_tint(a.rgb, cx));
    let theme = cx.theme();
    let accent = tint.unwrap_or(theme.success);
    let (muted, live) = (theme.muted_foreground, theme.success);
    let open = t_args("shell.playing.open", &[("title", &game.title)]);
    let stop_failed = t_args("shell.playing.stop_failed", &[("title", &game.title)]);
    let others = playing.len() - 1;
    let (open_id, stop_id) = (id.clone(), id);
    Some(
        h_flex()
            .id("playing-now")
            .keyboard_focusable(cx)
            .h(rems(1.75))
            .pl_1()
            .pr_0p5()
            .gap_1p5()
            .rounded_full()
            .bg(accent.opacity(0.16))
            .border_1()
            .border_color(accent.opacity(0.45))
            .text_xs()
            .cursor_pointer()
            .child(
                GameCover::new(game.clone())
                    .width(rems(0.95))
                    .radius(px(3.)),
            )
            .child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(live))
            .child(
                div()
                    .max_w(rems(12.))
                    .truncate()
                    .font_semibold()
                    .child(game.title.clone()),
            )
            .child(div().text_color(muted).child(duration_text(minutes)))
            .when(others > 0, |d| {
                d.child(div().text_color(muted).child(format!("+{others}")))
            })
            .child(
                Button::new("playing-stop")
                    .ghost()
                    .xsmall()
                    .icon(IconName::CircleStop)
                    .tooltip(t("shell.playing.stop"))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        stop(stop_id.clone(), stop_failed.clone(), window, cx);
                    }),
            )
            .tooltip(move |window, cx| Tooltip::new(open.clone()).build(window, cx))
            .on_click(move |_, _, cx| nav::navigate(Route::Game(open_id.clone()), cx))
            .into_any_element(),
    )
}

/// Stop a running game, reporting failure as a notification.
fn stop(id: GameId, failed: SharedString, window: &mut Window, cx: &mut App) {
    let b = ember_ui::backend(cx);
    let task = ember_ui::run(cx, async move { b.stop(&id).await });
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
    fn only_real_sessions_are_noted() {
        assert!(noted(60, false));
        assert!(noted(3 * 3600, false));
        assert!(!noted(59, false));
        assert!(!noted(600, true));
    }
}
