use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
};

/// Title row of a page with optional description and trailing actions.
#[derive(IntoElement)]
pub struct PageHeader {
    title: SharedString,
    description: Option<SharedString>,
    actions: Vec<AnyElement>,
}

impl PageHeader {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            description: None,
            actions: Vec::new(),
        }
    }

    pub fn description(mut self, text: impl Into<SharedString>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }
}

impl RenderOnce for PageHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_4()
            .justify_between()
            .child(
                v_flex()
                    .flex_1()
                    .gap_1()
                    .min_w_0()
                    .child(div().text_3xl().font_extrabold().child(self.title))
                    .when_some(self.description, |this, d| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(d),
                        )
                    }),
            )
            .when(!self.actions.is_empty(), |this| {
                this.child(h_flex().gap_2().flex_shrink_0().children(self.actions))
            })
    }
}

/// Heading of a section within a page.
#[derive(IntoElement)]
pub struct SectionTitle {
    title: SharedString,
    trailing: Option<AnyElement>,
}

impl SectionTitle {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            trailing: None,
        }
    }

    pub fn trailing(mut self, el: impl IntoElement) -> Self {
        self.trailing = Some(el.into_any_element());
        self
    }
}

impl RenderOnce for SectionTitle {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .w_full()
            .justify_between()
            .child(crate::style::caption(self.title, cx))
            .when_some(self.trailing, |this, t| this.child(t))
    }
}
