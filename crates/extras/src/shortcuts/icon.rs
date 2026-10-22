//! Game icons for shortcuts: picks the best source art, converts it to a
//! square PNG and stores it under Ember's data directory.

use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use ember_core::{GameInfo, Paths, Runner};
use image::{DynamicImage, GenericImageView as _, ImageFormat, RgbaImage, imageops};
use serde::Deserialize;

use crate::util;

/// Edge length of generated icons, in pixels.
const ICON_SIZE: u32 = 256;

/// `<data>/icons`, owned by Ember.
pub fn icons_dir() -> PathBuf {
    Paths::get().data.join("icons")
}

/// Where the icon of a game lives.
pub fn icon_path(game: &GameInfo) -> PathBuf {
    icons_dir().join(format!(
        "{}-{}.png",
        game.runner().id(),
        ember_core::paths::sanitize(&game.app_name)
    ))
}

/// Make sure the game's icon exists and return its path. Sources, in order:
/// GOG's bundled icons in the install folder, the store's icon art, GOG's
/// product API icon, then the square and cover art. Returns `Ok(None)` when no
/// source produced an image.
pub async fn ensure_icon(game: &GameInfo) -> Result<Option<PathBuf>> {
    let target = icon_path(game);
    if tokio::fs::try_exists(&target).await.unwrap_or(false) {
        return Ok(Some(target));
    }
    for source in icon_sources(game).await {
        let bytes = match util::load_image_source(&source).await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!("icon source for {} skipped: {e:#}", game.app_name);
                continue;
            }
        };
        let png = match tokio::task::spawn_blocking(move || square_png(&bytes, ICON_SIZE)).await {
            Ok(Ok(png)) => png,
            Ok(Err(e)) => {
                tracing::debug!("icon source for {} unusable: {e:#}", game.app_name);
                continue;
            }
            Err(e) => return Err(anyhow!("icon conversion task failed: {e}")),
        };
        util::write_atomic(&target, &png)
            .await
            .context("saving the game icon")?;
        return Ok(Some(target));
    }
    Ok(None)
}

/// Delete the stored icon of a game, if any.
pub async fn remove_icon(game: &GameInfo) {
    let _ = tokio::fs::remove_file(icon_path(game)).await;
}

async fn icon_sources(game: &GameInfo) -> Vec<String> {
    let mut out = Vec::new();
    let is_gog = game.runner() == Runner::Gog;
    if is_gog && let Some(dir) = game.install_path() {
        for candidate in [
            dir.join("support/icon.png"),
            dir.join(format!("goggame-{}.ico", game.app_name)),
        ] {
            if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
                out.push(candidate.to_string_lossy().into_owned());
            }
        }
    }
    out.extend(game.art_icon.iter().filter(|s| !s.is_empty()).cloned());
    if is_gog && let Some(url) = gog_api_icon(&game.app_name).await {
        out.push(url);
    }
    out.extend(
        [&game.art_square, &game.art_cover, &game.art_logo]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .cloned(),
    );
    out
}

#[derive(Deserialize)]
struct GogProduct {
    #[serde(default)]
    images: GogImages,
}

#[derive(Deserialize, Default)]
struct GogImages {
    icon: Option<String>,
}

async fn gog_api_icon(app_name: &str) -> Option<String> {
    let url = format!(
        "https://api.gog.com/products/{}?locale=en-US",
        util::encode_component(app_name)
    );
    let product: GogProduct = util::get_json(&url, Duration::from_secs(10)).await.ok()?;
    let icon = product.images.icon.filter(|s| !s.is_empty())?;
    Some(if icon.starts_with("//") {
        format!("https:{icon}")
    } else {
        icon
    })
}

/// Decode any supported image and fit it, centred, into a transparent
/// `size`×`size` PNG.
pub(crate) fn square_png(bytes: &[u8], size: u32) -> Result<Vec<u8>> {
    let img = image::load_from_memory(bytes).context("decoding image")?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(anyhow!("empty image"));
    }
    let scaled = img.resize(size, size, imageops::FilterType::Lanczos3);
    let mut canvas = RgbaImage::new(size, size);
    let x = (size - scaled.width()) / 2;
    let y = (size - scaled.height()) / 2;
    imageops::overlay(&mut canvas, &scaled.to_rgba8(), x.into(), y.into());
    encode_png(&DynamicImage::ImageRgba8(canvas))
}

pub(crate) fn encode_png(img: &DynamicImage) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)
        .context("encoding PNG")?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_to_square_png() {
        let src =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(40, 80, image::Rgba([255, 0, 0, 255])));
        let mut jpg = Cursor::new(Vec::new());
        src.to_rgb8().write_to(&mut jpg, ImageFormat::Jpeg).unwrap();
        let png = square_png(jpg.get_ref(), 64).unwrap();
        let out = image::load_from_memory_with_format(&png, ImageFormat::Png).unwrap();
        assert_eq!(out.dimensions(), (64, 64));
        assert_eq!(out.to_rgba8().get_pixel(0, 0)[3], 0);
        assert_eq!(out.to_rgba8().get_pixel(32, 32)[3], 255);
    }
}
