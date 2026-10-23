//! Per-game "Sync saves now" panel of the Cloud saves section.

use ember_core::{GameId, StatusKind, SyncDirection};
use ember_ui::i18n::{t, t_args};
use ember_ui::{backend, models};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IndexPath, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div,
};

use crate::form::ChoiceItem;
use crate::i18n::LanguageStamp;

/// Directions offered, in menu order.
pub(crate) const DIRECTIONS: [SyncDirection; 5] = [
    SyncDirection::Download,
    SyncDirection::Upload,
    SyncDirection::Auto,
    SyncDirection::ForceDownload,
    SyncDirection::ForceUpload,
];

/// Stable select key of a direction.
pub(crate) fn direction_key(d: SyncDirection) -> &'static str {
    match d {
        SyncDirection::Auto => "auto",
        SyncDirection::Download => "download",
        SyncDirection::Upload => "upload",
        SyncDirection::ForceDownload => "force-download",
        SyncDirection::ForceUpload => "force-upload",
    }
}

pub(crate) fn direction_from_key(key: &str) -> Option<SyncDirection> {
    DIRECTIONS.into_iter().find(|d| direction_key(*d) == key)
}

/// Translation key of a direction's menu label.
pub(crate) fn direction_label(d: SyncDirection) -> &'static str {
    match d {
        SyncDirection::Auto => "settings.saves.direction.auto",
        SyncDirection::Download => "settings.saves.direction.download",
        SyncDirection::Upload => "settings.saves.direction.upload",
        SyncDirection::ForceDownload => "settings.saves.direction.force_download",
        SyncDirection::ForceUpload => "settings.saves.direction.force_upload",
    }
}

fn direction_items() -> Vec<ChoiceItem> {
    DIRECTIONS
        .iter()
        .map(|d| ChoiceItem::new(direction_key(*d), t(direction_label(*d))))
        .collect()
}

/// Translation key of why a manual sync can't run, or `None` when it can.
pub(crate) fn sync_unavailable_reason(
    installed: bool,
    store_supports: bool,
    game_has_cloud_saves: bool,
    online: bool,
) -> Option<&'static str> {
    if !store_supports {
        Some("settings.saves.unavailable.store")
    } else if !game_has_cloud_saves {
        Some("settings.saves.unavailable.game")
    } else if !installed {
        Some("settings.saves.unavailable.not_installed")
    } else if !online {
        Some("settings.saves.unavailable.offline")
    } else {
        None
    }
}

/// The line of a sync report worth showing after a sync.
pub(crate) fn summarize_output(output: &str) -> Option<String> {
    let line = output.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    let mut s: String = line.chars().take(200).collect();
    if line.chars().count() > 200 {
        s.push('…');
    }
    Some(s)
}

/// Manual cloud-save sync of one game with a direction choice.
pub(crate) struct SaveSyncPanel {
    id: GameId,
    select: Entity<SelectState<Vec<ChoiceItem>>>,
    direction: SyncDirection,
    syncing: bool,
    /// Outcome of the last sync: success flag and message.
    result: Option<(bool, SharedString)>,
    language: LanguageStamp,
    _subscriptions: Vec<Subscription>,
}

impl SaveSyncPanel {
    pub(crate) fn new(id: GameId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let items = direction_items();
        let select =
            cx.new(|cx| SelectState::new(items, Some(IndexPath::default().row(0)), window, cx));
        let m = models(cx);
        let (statuses, accounts, library) =
            (m.statuses.clone(), m.accounts.clone(), m.library.clone());
        let subscriptions = vec![
            cx.subscribe_in(
                &select,
                window,
                |this, _, event: &SelectEvent<Vec<ChoiceItem>>, _, cx| {
                    if let SelectEvent::Confirm(Some(key)) = event
                        && let Some(d) = direction_from_key(key)
                    {
                        this.direction = d;
                        cx.notify();
                    }
                },
            ),
            cx.observe(&statuses, |_, _, cx| cx.notify()),
            cx.observe(&accounts, |_, _, cx| cx.notify()),
            cx.observe(&library, |_, _, cx| cx.notify()),
        ];
        Self {
            id,
            select,
            direction: DIRECTIONS[0],
            syncing: false,
            result: None,
            language: LanguageStamp::new(),
            _subscriptions: subscriptions,
        }
    }

    fn unavailable(&self, cx: &Context<Self>) -> Option<&'static str> {
        let b = backend(cx);
        let Some(game) = b.game(&self.id) else {
            return Some("settings.saves.unavailable.unknown_game");
        };
        let store_supports = b
            .store(self.id.runner)
            .is_some_and(|s| s.supports_cloud_saves());
        sync_unavailable_reason(
            game.is_installed,
            store_supports,
            game.cloud_save_enabled,
            models(cx).accounts.read(cx).online,
        )
    }

    fn busy(&self, cx: &Context<Self>) -> bool {
        self.syncing
            || models(cx).statuses.read(cx).kind(&self.id) == Some(StatusKind::SyncingSaves)
    }

    fn request_sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let direction = self.direction;
        let (title, description, ok) = match direction {
            SyncDirection::ForceDownload => (
                "settings.saves.force_download.title",
                "settings.saves.force_download.description",
                "settings.saves.force_download.confirm",
            ),
            SyncDirection::ForceUpload => (
                "settings.saves.force_upload.title",
                "settings.saves.force_upload.description",
                "settings.saves.force_upload.confirm",
            ),
            _ => return self.sync(direction, window, cx),
        };
        let name = backend(cx)
            .game(&self.id)
            .map(|g| g.title.clone())
            .unwrap_or_else(|| self.id.app_name.to_string());
        let title = t_args(title, &[("title", &name)]);
        let (description, ok) = (t(description), t(ok));
        let weak = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let weak = weak.clone();
            alert
                .title(title.clone())
                .description(description.clone())
                .show_cancel(true)
                .cancel_text(t("settings.common.cancel"))
                .ok_text(ok.clone())
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    weak.update(cx, |this, cx| this.sync(direction, window, cx))
                        .ok();
                    true
                })
        });
    }

    fn sync(&mut self, direction: SyncDirection, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy(cx) {
            return;
        }
        self.syncing = true;
        self.result = None;
        let b = backend(cx);
        let id = self.id.clone();
        let task = ember_ui::run(cx, async move { b.sync_saves(&id, direction).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.syncing = false;
                match result {
                    Ok(output) => {
                        let summary = summarize_output(&output)
                            .map(SharedString::from)
                            .unwrap_or_else(|| t("settings.saves.in_sync"));
                        window.push_notification(
                            Notification::success(t("settings.saves.synced")),
                            cx,
                        );
                        this.result = Some((true, summary));
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        window.push_notification(
                            Notification::error(msg.clone()).title(t("settings.saves.failed")),
                            cx,
                        );
                        this.result = Some((false, msg.into()));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for SaveSyncPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language.changed() {
            cx.defer_in(window, |this, window, cx| {
                let items = direction_items();
                this.select
                    .update(cx, |s, cx| s.set_items(items, window, cx));
            });
        }
        let theme = cx.theme();
        let (muted_fg, danger) = (theme.muted_foreground, theme.danger);
        let reason = self.unavailable(cx);
        let busy = self.busy(cx);
        let disabled = reason.is_some() || busy;
        v_flex()
            .gap_2()
            .w_full()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Select::new(&self.select)
                            .id("save-sync-direction")
                            .accessibility_label(t("settings.saves.direction.label"))
                            .disabled(disabled)
                            .w_80(),
                    )
                    .child(
                        Button::new("save-sync-now")
                            .icon(IconName::RefreshCw)
                            .label(if busy {
                                t("settings.saves.syncing")
                            } else {
                                t("settings.saves.sync_now")
                            })
                            .loading(busy)
                            .disabled(disabled)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.request_sync(window, cx)),
                            ),
                    ),
            )
            .when_some(reason, |v, r| {
                v.child(
                    h_flex()
                        .gap_1()
                        .text_xs()
                        .text_color(muted_fg)
                        .child(Icon::new(IconName::Info).xsmall())
                        .child(t(r)),
                )
            })
            .when_some(self.result.clone(), |v, (ok, msg)| {
                v.child(
                    div()
                        .text_xs()
                        .text_color(if ok { muted_fg } else { danger })
                        .child(msg),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_keys_round_trip() {
        for d in DIRECTIONS {
            assert_eq!(direction_from_key(direction_key(d)), Some(d));
        }
        assert_eq!(direction_from_key("sideways"), None);
        assert_eq!(DIRECTIONS[0], SyncDirection::Download);
    }

    #[test]
    fn unavailable_reasons_in_priority_order() {
        assert_eq!(sync_unavailable_reason(true, true, true, true), None);
        assert_eq!(
            sync_unavailable_reason(true, false, true, true),
            Some("settings.saves.unavailable.store")
        );
        assert_eq!(
            sync_unavailable_reason(false, true, false, true),
            Some("settings.saves.unavailable.game")
        );
        assert_eq!(
            sync_unavailable_reason(false, true, true, true),
            Some("settings.saves.unavailable.not_installed")
        );
        assert_eq!(
            sync_unavailable_reason(true, true, true, false),
            Some("settings.saves.unavailable.offline")
        );
    }

    #[test]
    fn output_summary_is_the_last_line() {
        assert_eq!(
            summarize_output("a\nSync complete\n\n").as_deref(),
            Some("Sync complete")
        );
        assert_eq!(summarize_output("  \n"), None);
        let long = "x".repeat(300);
        assert_eq!(summarize_output(&long).unwrap().chars().count(), 201);
    }
}
