//! The "Connected controllers" panel of General → Controller.

use ember_bigpicture::gamepad::{self, Battery, ControllerInfo, ControllerKind};
use ember_ui::i18n::{t, t_args};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, div,
};

use crate::widgets::{muted, panel};

/// Display name of a controller family.
pub(crate) fn kind_label(kind: ControllerKind) -> &'static str {
    match kind {
        ControllerKind::Xbox => "Xbox",
        ControllerKind::PlayStation4 => "PlayStation 4",
        ControllerKind::PlayStation5 => "PlayStation 5",
        ControllerKind::Nintendo => "Nintendo",
        ControllerKind::SteamDeck => "Steam Deck",
        ControllerKind::Generic => "Generic",
    }
}

/// Display name of a controller family in the interface language.
fn kind_text(kind: ControllerKind) -> SharedString {
    match kind {
        ControllerKind::Generic => t("settings.controllers.kind_generic"),
        other => kind_label(other).into(),
    }
}

/// Text of the battery column; unknown shows nothing.
pub(crate) fn battery_text(battery: Battery) -> Option<SharedString> {
    match battery {
        Battery::Wired => Some(t("settings.controllers.battery.wired")),
        Battery::Discharging(level) => Some(format!("{level}%").into()),
        Battery::Charging(level) => Some(t_args(
            "settings.controllers.battery.charging",
            &[("level", &level.to_string())],
        )),
        Battery::Full => Some(t("settings.controllers.battery.full")),
        Battery::Unknown => None,
    }
}

/// Lists the controllers the gamepad reader sees. Follows connections live
/// when controller support is running; the refresh button re-reads the list
/// otherwise.
pub(crate) struct ControllersPanel {
    controllers: Vec<ControllerInfo>,
    _subscriptions: Vec<Subscription>,
}

impl ControllersPanel {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        if let Some(model) = gamepad::model(cx) {
            subscriptions.push(cx.observe(&model, |this, _, cx| this.refresh(cx)));
        }
        Self {
            controllers: gamepad::controllers(),
            _subscriptions: subscriptions,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.controllers = gamepad::controllers();
        cx.notify();
    }
}

impl Render for ControllersPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted_fg, danger) = (theme.border, theme.muted_foreground, theme.danger);
        let refresh = Button::new("controllers-refresh")
            .ghost()
            .xsmall()
            .icon(IconName::RefreshCw)
            .label(t("settings.common.refresh"))
            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)));
        let list = if self.controllers.is_empty() {
            panel(cx).child(
                h_flex()
                    .gap_2()
                    .p_3()
                    .child(Icon::new(IconName::Gamepad2).small().text_color(muted_fg))
                    .child(muted(t("settings.controllers.empty"), cx)),
            )
        } else {
            self.controllers
                .iter()
                .enumerate()
                .fold(panel(cx), |list, (ix, c)| {
                    let battery = battery_text(c.battery);
                    let low = c.battery.is_low();
                    list.child(
                        h_flex()
                            .id(ElementId::Name(format!("controller-{ix}").into()))
                            .gap_3()
                            .px_3()
                            .py_2()
                            .when(ix > 0, |r| r.border_t_1().border_color(border))
                            .child(Icon::new(IconName::Gamepad2).small())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .child(c.name.clone()),
                            )
                            .child(Tag::secondary().xsmall().outline().child(kind_text(c.kind)))
                            .when_some(battery, |r, b| {
                                r.child(
                                    div()
                                        .min_w_24()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_right()
                                        .text_color(if low { danger } else { muted_fg })
                                        .child(b),
                                )
                            }),
                    )
                })
        };
        v_flex()
            .w_full()
            .gap_2()
            .child(list)
            .child(h_flex().justify_end().child(refresh))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_labels() {
        crate::i18n::register();
        assert_eq!(kind_label(ControllerKind::PlayStation5), "PlayStation 5");
        assert_eq!(kind_label(ControllerKind::SteamDeck), "Steam Deck");
        assert_eq!(battery_text(Battery::Wired).as_deref(), Some("Wired"));
        assert_eq!(battery_text(Battery::Unknown), None);
        assert_eq!(
            battery_text(Battery::Discharging(15)).as_deref(),
            Some("15%")
        );
    }
}
