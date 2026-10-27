//! Irori's mark, as the dock, taskbar and tray show it.

use std::sync::Arc;

use gpui_kit::{App, Image, ImageFormat, IntoElement, Rems, RenderOnce, Styled as _, Window, img};
use once_cell::sync::Lazy;

/// `assets/brand/irori-mark.svg`, given a small intrinsic size: GPUI rasterizes
/// an SVG image at that size, and the 1024-unit original would be scaled down
/// on the GPU with visible aliasing.
static MARK: Lazy<Arc<Image>> = Lazy::new(|| {
    let svg = include_str!("../../../../assets/brand/irori-mark.svg").replacen(
        "<svg ",
        r#"<svg width="192" height="192" "#,
        1,
    );
    Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()))
});

/// The app icon: the hearth frame and its flame on their own rounded tile.
#[derive(IntoElement)]
pub struct BrandMark {
    size: Rems,
}

impl BrandMark {
    pub fn new(size: Rems) -> Self {
        Self { size }
    }
}

impl RenderOnce for BrandMark {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        img(MARK.clone()).size(self.size).flex_shrink_0()
    }
}
