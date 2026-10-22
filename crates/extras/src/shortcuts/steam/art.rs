//! Library artwork for non-Steam shortcuts in `userdata/<id>/config/grid`.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use ember_core::GameInfo;
use image::{DynamicImage, GenericImageView as _, ImageFormat, RgbaImage, imageops};

use super::shortcuts_file::GridIds;
use crate::util;

/// A 1×1 transparent PNG, written as the logo when no logo art exists so
/// Steam does not draw the title text over the hero image.
pub const TRANSPARENT_PNG: [u8; 68] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x18, 0x95, 0x63, 0x60, 0x00, 0x02, 0x00,
    0x00, 0x05, 0x00, 0x01, 0xd6, 0x9f, 0x16, 0xcd, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];

const STEAM_CDN: &str = "https://cdn.cloudflare.steamstatic.com/steam/apps";
const HERO_W: u32 = 1920;
const HERO_H: u32 = 620;

/// Steam artwork slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtKind {
    /// Portrait capsule, `<id>p`.
    Cover,
    /// Wide capsule/header, `<id>`.
    Header,
    /// Library hero, `<id>_hero`.
    Hero,
    /// Logo drawn over the hero, `<id>_logo`.
    Logo,
}

impl ArtKind {
    pub const ALL: [ArtKind; 4] = [
        ArtKind::Cover,
        ArtKind::Header,
        ArtKind::Hero,
        ArtKind::Logo,
    ];

    fn stem(self, short: u32) -> String {
        match self {
            ArtKind::Cover => format!("{short}p"),
            ArtKind::Header => format!("{short}"),
            ArtKind::Hero => format!("{short}_hero"),
            ArtKind::Logo => format!("{short}_logo"),
        }
    }

    fn steam_cdn_file(self) -> &'static str {
        match self {
            ArtKind::Cover => "library_600x900_2x.jpg",
            ArtKind::Header => "header.jpg",
            ArtKind::Hero => "library_hero.jpg",
            ArtKind::Logo => "logo.png",
        }
    }
}

/// Existing file for `stem` in either supported extension.
async fn existing(grid: &Path, stem: &str) -> Option<PathBuf> {
    for ext in ["png", "jpg"] {
        let p = grid.join(format!("{stem}.{ext}"));
        if tokio::fs::try_exists(&p).await.unwrap_or(false) {
            return Some(p);
        }
    }
    None
}

/// Write any missing artwork for a shortcut into `grid`. Existing files are
/// kept (the user may have picked their own). Returns per-slot errors; a
/// failed slot does not stop the others.
pub async fn prepare(
    grid: &Path,
    ids: GridIds,
    game: &GameInfo,
    steam_app_id: Option<&str>,
) -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(e) = tokio::fs::create_dir_all(grid).await {
        errors.push(format!("creating {}: {e}", grid.display()));
        return errors;
    }
    for kind in ArtKind::ALL {
        let stem = kind.stem(ids.short);
        if existing(grid, &stem).await.is_some() {
            continue;
        }
        match art_bytes(kind, game, steam_app_id).await {
            Ok(Some((bytes, ext))) => {
                let path = grid.join(format!("{stem}.{ext}"));
                if let Err(e) = util::write_atomic(&path, &bytes).await {
                    errors.push(format!("{e:#}"));
                }
            }
            Ok(None) => {}
            Err(e) => errors.push(format!("{kind:?} art: {e:#}")),
        }
    }
    errors
}

/// Remove the artwork of a shortcut (both extensions, all slots, and the
/// legacy Big Picture grid file).
pub async fn remove(grid: &Path, ids: GridIds) {
    let mut stems: Vec<String> = ArtKind::ALL.iter().map(|k| k.stem(ids.short)).collect();
    stems.push(ids.big_picture.to_string());
    for stem in stems {
        for ext in ["png", "jpg"] {
            let p = grid.join(format!("{stem}.{ext}"));
            match tokio::fs::remove_file(&p).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("removing {}: {e}", p.display()),
            }
        }
    }
}

async fn art_bytes(
    kind: ArtKind,
    game: &GameInfo,
    steam_app_id: Option<&str>,
) -> Result<Option<(Vec<u8>, &'static str)>> {
    if let Some(id) = steam_app_id.filter(|s| !s.is_empty()) {
        let url = format!("{STEAM_CDN}/{id}/{}", kind.steam_cdn_file());
        match util::load_image_source(&url).await {
            Ok(bytes) => return normalize(bytes).map(Some),
            Err(e) => tracing::debug!("Steam CDN art unavailable: {e:#}"),
        }
    }
    let pick = |opts: &[&Option<String>]| -> Option<String> {
        opts.iter()
            .filter_map(|o| o.as_deref())
            .find(|s| !s.is_empty())
            .map(str::to_string)
    };
    match kind {
        ArtKind::Cover => fetch_normalized(pick(&[&game.art_cover, &game.art_square])).await,
        ArtKind::Header => {
            fetch_normalized(pick(&[
                &game.art_square,
                &game.art_background,
                &game.art_cover,
            ]))
            .await
        }
        ArtKind::Hero => {
            if let Some(src) = pick(&[&game.art_background]) {
                return fetch_normalized(Some(src)).await;
            }
            let Some(src) = pick(&[&game.art_square, &game.art_cover]) else {
                return Ok(None);
            };
            let bytes = util::load_image_source(&src).await?;
            let jpg = tokio::task::spawn_blocking(move || generate_hero(&bytes))
                .await
                .map_err(|e| anyhow!("hero generation task failed: {e}"))??;
            Ok(Some((jpg, "jpg")))
        }
        ArtKind::Logo => match pick(&[&game.art_logo]) {
            Some(src) => fetch_normalized(Some(src)).await,
            None => Ok(Some((TRANSPARENT_PNG.to_vec(), "png"))),
        },
    }
}

async fn fetch_normalized(source: Option<String>) -> Result<Option<(Vec<u8>, &'static str)>> {
    let Some(source) = source else {
        return Ok(None);
    };
    let bytes = util::load_image_source(&source).await?;
    normalize(bytes).map(Some)
}

/// Keep PNG and JPEG as they are; convert anything else Steam may not read
/// (WebP…) to PNG.
fn normalize(bytes: Vec<u8>) -> Result<(Vec<u8>, &'static str)> {
    match image::guess_format(&bytes).context("unknown image format")? {
        ImageFormat::Png => Ok((bytes, "png")),
        ImageFormat::Jpeg => Ok((bytes, "jpg")),
        _ => {
            let img = image::load_from_memory(&bytes).context("decoding image")?;
            Ok((crate::shortcuts::icon::encode_png(&img)?, "png"))
        }
    }
}

/// A 1920×620 hero: the image blurred to fill the frame, with the unblurred
/// image scaled to full height on top, centred.
fn generate_hero(bytes: &[u8]) -> Result<Vec<u8>> {
    let img = image::load_from_memory(bytes).context("decoding image")?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(anyhow!("empty image"));
    }
    let background = img.resize_to_fill(HERO_W, HERO_H, imageops::FilterType::Triangle);
    let mut canvas: RgbaImage = imageops::fast_blur(&background.to_rgba8(), 10.0);
    let fg = img
        .resize(HERO_W, HERO_H, imageops::FilterType::Lanczos3)
        .to_rgba8();
    let x = i64::from((HERO_W - fg.width()) / 2);
    let y = i64::from((HERO_H - fg.height()) / 2);
    imageops::overlay(&mut canvas, &fg, x, y);
    let rgb = DynamicImage::ImageRgba8(canvas).to_rgb8();
    let mut out = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode_image(&rgb)
        .context("encoding hero image")?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_png_matches_spec_hex() {
        let hex = "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000b494441541895636000020000050001d69f16cd0000000049454e44ae426082";
        let expected: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(TRANSPARENT_PNG.to_vec(), expected);
        assert!(image::load_from_memory(&TRANSPARENT_PNG).is_ok());
    }

    #[test]
    fn hero_dimensions() {
        let src =
            DynamicImage::ImageRgb8(image::RgbImage::from_pixel(30, 40, image::Rgb([1, 2, 3])));
        let mut png = Cursor::new(Vec::new());
        src.write_to(&mut png, ImageFormat::Png).unwrap();
        let jpg = generate_hero(png.get_ref()).unwrap();
        let out = image::load_from_memory_with_format(&jpg, ImageFormat::Jpeg).unwrap();
        assert_eq!(out.dimensions(), (HERO_W, HERO_H));
    }

    #[tokio::test]
    async fn remove_deletes_both_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let ids = GridIds::from_short(0x8000_0001);
        for name in ["2147483649p.jpg", "2147483649_logo.png", "2147483649.png"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        std::fs::write(dir.path().join(format!("{}.jpg", ids.big_picture)), b"x").unwrap();
        std::fs::write(dir.path().join("keep.jpg"), b"x").unwrap();
        remove(dir.path(), ids).await;
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(left, ["keep.jpg"]);
    }
}
