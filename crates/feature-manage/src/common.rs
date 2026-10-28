//! Small helpers shared by the management screens.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{
    AnyElement, App, Div, ParentElement as _, Pixels, SharedString, Styled as _, Window, canvas,
    div, px,
};
use irori_services::Backend;
use irori_ui::backend;

/// Gap between cards in a [`card_grid`] (`gap_4`), in rem.
const CARD_GAP_REM: f32 = 1.;
/// Columns a [`card_grid`] uses before its width is known.
const DEFAULT_COLUMNS: u16 = 3;

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

/// Columns of cards at least `min_card` rem wide that fit `available` rem,
/// at least one.
fn card_columns(available: f32, min_card: f32) -> u16 {
    let fit = ((available + CARD_GAP_REM) / (min_card + CARD_GAP_REM)).floor();
    fit.clamp(1., f32::from(u16::MAX)) as u16
}

/// Cards in equal columns that fill the width, with every row as tall as the
/// tallest card, so cards line up whatever their text. `width` keeps the
/// grid's last laid-out width; the grid renders again when it changes.
pub(crate) fn card_grid(
    width: &Rc<Cell<Pixels>>,
    min_card: f32,
    cards: Vec<AnyElement>,
    window: &Window,
) -> Div {
    let measured = width.get();
    let columns = if measured > px(0.) {
        card_columns(measured / window.rem_size(), min_card)
    } else {
        DEFAULT_COLUMNS
    };
    let rows = cards.len().div_ceil(usize::from(columns)).max(1);
    let width = width.clone();
    div()
        .relative()
        .w_full()
        .child(
            canvas(
                move |bounds, window, _| {
                    // Columns are picked while rendering, so render again once
                    // the width changed.
                    if width.replace(bounds.size.width) != bounds.size.width {
                        window.refresh();
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .child(
            div()
                .w_full()
                .grid()
                .grid_cols(columns)
                // Fractional rows share the tallest card's height.
                .grid_rows_min_content(u16::try_from(rows).unwrap_or(u16::MAX))
                .gap_4()
                .children(cards),
        )
}

#[cfg(test)]
mod tests {
    use super::card_columns;

    #[test]
    fn card_columns_fill_the_width() {
        assert_eq!(card_columns(0., 17.), 1);
        assert_eq!(card_columns(17., 17.), 1);
        assert_eq!(card_columns(35., 17.), 2);
        assert_eq!(card_columns(89., 17.), 5);
        assert_eq!(card_columns(61.9, 20.), 2);
        assert_eq!(card_columns(62., 20.), 3);
    }
}
