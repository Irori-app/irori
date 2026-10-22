use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, v_flex};
use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
};

/// Centered placeholder for empty or failed views: what happened and what to do.
#[derive(IntoElement)]
pub struct EmptyState {
    icon: IconName,
    title: SharedString,
    description: Option<SharedString>,
    action: Option<AnyElement>,
}

impl EmptyState {
    pub fn new(icon: IconName, title: impl Into<SharedString>) -> Self {
        Self {
            icon,
            title: title.into(),
            description: None,
            action: None,
        }
    }

    pub fn description(mut self, text: impl Into<SharedString>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_8()
            .child(
                div().p_4().rounded_full().bg(cx.theme().muted).child(
                    Icon::new(self.icon)
                        .large()
                        .text_color(cx.theme().muted_foreground),
                ),
            )
            .child(div().text_lg().font_semibold().child(self.title))
            .when_some(self.description, |this, d| {
                this.child(
                    div()
                        .max_w_96()
                        .text_sm()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child(d),
                )
            })
            .when_some(self.action, |this, a| this.child(div().pt_2().child(a)))
    }
}

use gpui_kit::prelude::FluentBuilder as _;
