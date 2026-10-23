//! Presentational pieces shared by the settings screens.

use ember_ui::i18n::t;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, ParentElement as _, SharedString, Styled as _,
    Window, div,
};

/// Decorations of one field row besides its control.
#[derive(Default)]
pub(crate) struct RowState {
    pub label: SharedString,
    pub help: SharedString,
    pub tag: Option<SharedString>,
    /// Shown as "Overridden" with this reset button next to the control.
    pub reset: Option<AnyElement>,
    pub disabled_reason: Option<SharedString>,
    pub error: Option<SharedString>,
    /// Lay the control out under the label (lists, wide editors).
    pub stacked: bool,
}

/// A labelled settings row: label, qualifiers and help on the leading side,
/// the control on the trailing side (or below for stacked editors).
pub(crate) fn field_row(state: RowState, control: AnyElement, cx: &App) -> Div {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let danger = theme.danger;
    let label = h_flex()
        .gap_2()
        .flex_wrap()
        .child(div().text_sm().font_medium().child(state.label))
        .when_some(state.tag, |this, tag| {
            this.child(Tag::secondary().xsmall().outline().child(tag))
        })
        .when(state.reset.is_some(), |this| {
            this.child(Tag::info().xsmall().child(t("settings.field.overridden")))
        });
    let text = v_flex()
        .gap_1()
        .min_w_0()
        .flex_1()
        .child(label)
        .when(!state.help.is_empty(), |this| {
            this.child(div().text_xs().text_color(muted).child(state.help))
        })
        .when_some(state.disabled_reason, |this, reason| {
            this.child(
                h_flex()
                    .gap_1()
                    .text_xs()
                    .text_color(muted)
                    .child(Icon::new(IconName::Info).xsmall())
                    .child(reason),
            )
        });
    let error = state.error.map(|e| {
        h_flex()
            .gap_1()
            .text_xs()
            .text_color(danger)
            .child(Icon::new(IconName::CircleAlert).xsmall())
            .child(e)
    });
    let trailing = h_flex()
        .gap_2()
        .flex_shrink_0()
        .when_some(state.reset, |this, reset| this.child(reset));
    if state.stacked {
        v_flex()
            .w_full()
            .gap_2()
            .px_4()
            .py_3()
            .child(h_flex().gap_4().items_start().child(text).child(trailing))
            .child(control)
            .children(error)
    } else {
        v_flex()
            .w_full()
            .gap_1()
            .px_4()
            .py_3()
            .child(
                h_flex()
                    .w_full()
                    .gap_4()
                    .justify_between()
                    .child(text)
                    .child(trailing.child(div().flex_shrink_0().child(control))),
            )
            .children(error)
    }
}

/// Sub-heading inside a section.
pub(crate) fn group_heading(title: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .pt_8()
        .pb_2()
        .child(ember_ui::style::caption(title, cx))
}

/// The surface holding one group's rows; rows after the first get a divider.
pub(crate) fn group_panel(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .w_full()
        .rounded(theme.radius_lg)
        .bg(theme.secondary)
        .border_1()
        .border_color(theme.border)
}

/// Heading of a section when several sections are listed (search results,
/// game defaults).
pub(crate) fn section_heading(title: impl Into<SharedString>) -> Div {
    div().pt_8().text_lg().font_semibold().child(title.into())
}

/// A row in a section navigation column.
pub(crate) fn nav_button(
    id: impl Into<ElementId>,
    icon: IconName,
    label: impl Into<SharedString>,
    selected: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    let label = label.into();
    // Button centres its content row, so the icon and label go in a child
    // that fills it and lines them up at the start.
    Button::new(id)
        .ghost()
        .w_full()
        .accessibility_label(label.clone())
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(Icon::new(icon))
                .child(div().min_w_0().text_ellipsis().child(label)),
        )
        .selected(selected)
        .on_click(on_click)
}

/// Bordered list surface for row editors and panels.
pub(crate) fn panel(cx: &App) -> Div {
    v_flex()
        .w_full()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(cx.theme().border)
}

/// Muted explanatory line.
pub(crate) fn muted(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}
