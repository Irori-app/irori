use gpui_kit::component::tag::Tag;
use gpui_kit::component::{Sizable as _, StyledExt as _};
use gpui_kit::{App, IntoElement, ParentElement as _, RenderOnce, Window};
use irori_core::Runner;

/// Small neutral label naming a game's store.
#[derive(IntoElement)]
pub struct StoreBadge {
    runner: Runner,
}

impl StoreBadge {
    pub fn new(runner: Runner) -> Self {
        Self { runner }
    }
}

impl RenderOnce for StoreBadge {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        Tag::secondary()
            .xsmall()
            .outline()
            .font_medium()
            .child(self.runner.short_name())
    }
}
