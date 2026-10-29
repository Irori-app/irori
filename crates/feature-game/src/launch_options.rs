//! Launch options of a game that its own client starts (Steam): the
//! arguments Irori adds to the client's own launch options for this game.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement, Render,
    Styled, Subscription, Window, div,
};
use irori_core::GameId;
use irori_ui::backend;
use irori_ui::i18n::{t, t_args};
use serde_json::Value;

/// Settings key the arguments are kept under (Heroic's name for a game's
/// own arguments).
const ARGS_KEY: &str = "launcherArgs";

/// Edit the extra arguments Irori passes when it starts `id`.
pub fn open_launch_options_dialog(id: GameId, window: &mut Window, cx: &mut App) {
    let title = backend(cx)
        .game(&id)
        .map(|g| g.title.clone())
        .unwrap_or_else(|| id.app_name.to_string());
    let heading = t_args("game.launch_options.title", &[("title", &title)]);
    let form = cx.new(|cx| LaunchOptionsForm::new(id, window, cx));
    let focus = form.read(cx).input.focus_handle(cx);
    focus.focus(window, cx);
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.title(heading.clone()).child(form.clone())
    });
}

struct LaunchOptionsForm {
    id: GameId,
    input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl LaunchOptionsForm {
    fn new(id: GameId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let current = backend(cx).settings.game(&id).launcher_args;
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t("game.launch_options.placeholder"))
                .default_value(current)
        });
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.save(window, cx);
                }
            }),
        ];
        Self {
            id,
            input,
            _subscriptions: subscriptions,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.input.read(cx).value().trim().to_string();
        let value = (!value.is_empty()).then(|| Value::String(value));
        backend(cx)
            .settings
            .set_game_value(&self.id, ARGS_KEY, value);
        window.close_dialog(cx);
    }
}

impl Render for LaunchOptionsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.id.runner.store_name();
        v_flex()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t_args(
                        "game.launch_options.description",
                        &[("store", store)],
                    )),
            )
            .child(Input::new(&self.input))
            .child(
                DialogFooter::new()
                    .child(
                        Button::new("launch-options-cancel")
                            .ghost()
                            .label(t("game.common.cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("launch-options-save")
                            .primary()
                            .label(t("game.launch_options.save"))
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}
