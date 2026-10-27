//! Irori's motion: a few shared tempos and helpers so every animation moves
//! the same way. GPUI renders all of them in their end state while the user
//! reduces motion (the shell keeps `App::reduce_motion` in sync with the
//! setting), so callers need no checks of their own.

use std::time::Duration;

use gpui_kit::component::progress::Progress;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, ElementId, IntoElement, SpringAnimation,
    SpringConfig, Styled, px,
};

/// Feedback to a pointer or key: hover lifts, presses.
pub const QUICK: Duration = Duration::from_millis(140);
/// Something arriving: pages, art, bars sliding in.
pub const STANDARD: Duration = Duration::from_millis(240);
/// Critically damped spring (≈180 ms response) for hover and focus.
pub const HOVER_SPRING: SpringConfig = SpringConfig::new(1200.0, 69.3, 1.0);

/// Fast start, gentle stop: the curve for things entering.
fn enter_curve() -> impl Fn(f32) -> f32 {
    gpui_kit::ease_out_quint()
}

/// Fades `element` in over [`STANDARD`] the first time it renders under `id`.
/// A new `id` plays it again.
pub fn fade_in<E>(id: impl Into<ElementId>, element: E) -> AnyElement
where
    E: IntoElement + Styled + 'static,
{
    element
        .with_animation(
            id,
            Animation::new(STANDARD).with_easing(enter_curve()),
            |el, t| el.opacity(t),
        )
        .into_any_element()
}

/// Fades `element` in while it settles `rise` pixels upward into place. For
/// elements in normal flow (it offsets them with relative positioning).
pub fn rise_in<E>(id: impl Into<ElementId>, element: E, rise: f32) -> AnyElement
where
    E: IntoElement + Styled + 'static,
{
    element
        .with_animation(
            id,
            Animation::new(STANDARD).with_easing(enter_curve()),
            move |el, t| el.opacity(t).relative().top(px(rise * (1.0 - t))),
        )
        .into_any_element()
}

/// Slides a bar pinned to the top of its parent (`absolute().top_0()`) down
/// into view by `distance` pixels while it fades in.
pub fn drop_in<E>(id: impl Into<ElementId>, element: E, distance: f32) -> AnyElement
where
    E: IntoElement + Styled + 'static,
{
    element
        .with_animation(
            id,
            Animation::new(STANDARD).with_easing(enter_curve()),
            move |el, t| el.opacity(t).top(px(-distance * (1.0 - t))),
        )
        .into_any_element()
}

/// Critically damped spring (≈400 ms response) for progress bars.
const PROGRESS_SPRING: SpringConfig = SpringConfig::new(200.0, 28.3, 1.0);

/// A progress bar that glides to each new percentage instead of jumping, as
/// download progress arrives in steps. `None` shows the indeterminate bar.
pub fn gliding_progress(id: impl Into<ElementId>, percent: Option<f32>) -> AnyElement {
    let id = id.into();
    let bar = Progress::new(id.clone()).loading(percent.is_none());
    match percent {
        Some(percent) => bar
            .with_spring(
                id,
                SpringAnimation::new(PROGRESS_SPRING).to(percent),
                |bar, value: f32| bar.value(value),
            )
            .into_any_element(),
        None => bar.into_any_element(),
    }
}
