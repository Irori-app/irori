//! Small helpers shared by the management screens.

use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{App, Div, SharedString, Styled as _, Window};
use irori_services::Backend;
use irori_ui::backend;

/// Run a synchronous queue/settings mutation of the backend on its blocking
/// pool (they persist state files), never on the UI thread.
pub(crate) fn backend_op(cx: &App, f: impl FnOnce(&Backend) + Send + 'static) {
    let b = backend(cx);
    let worker = b.clone();
    drop(b.spawn_blocking(move || f(&worker)));
}

/// Show an operation failure: what failed, and the error the backend gave.
pub(crate) fn notify_error(
    title: impl Into<SharedString>,
    error: &anyhow::Error,
    window: &mut Window,
    cx: &mut App,
) {
    window.push_notification(Notification::error(format!("{error:#}")).title(title), cx);
}

/// Show a success message whose result is not otherwise visible.
pub(crate) fn notify_success(message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    window.push_notification(Notification::success(message), cx);
}

/// A bordered surface for one object (a store, the current download).
pub(crate) fn card(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .p_4()
        .gap_3()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.group_box)
}
