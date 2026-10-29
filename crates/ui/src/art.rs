//! Game art pipeline.
//!
//! Store art is often several megabytes per image. Decoding originals for a
//! grid of hundreds of tiles would cost gigabytes of RAM and seconds of CPU, so:
//!
//! 1. originals are downloaded once into the disk cache;
//! 2. a thumbnail is produced per width bucket (decode + resize once, on the
//!    blocking pool) and stored as JPEG/PNG next to it;
//! 3. views render the thumbnail path through an [`LruImageCache`], which
//!    bounds how many decoded images stay in GPU/CPU memory.
//!
//! Backdrop art also gets an [`Ambient`]: a small pre-blurred copy (a GPU blur
//! of a full-size image every frame would cost far more) and the colour the
//! art is mostly made of, which tints the page around it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Global, ImageCache, ImageCacheError, ImageCacheItem,
    RenderImage, Resource, SharedString, Window, hash,
};
use irori_core::Paths;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::state::backend;

/// Thumbnail widths in physical pixels. Requests round up to the next bucket so
/// small window resizes reuse files.
const BUCKETS: [u32; 7] = [160, 240, 320, 480, 720, 1280, 1920];
/// Concurrent art downloads.
const MAX_DOWNLOADS: usize = 6;
/// Width of blurred backdrops in physical pixels. Blurred art has no detail
/// to lose, so a small image scaled up looks the same as a large one.
const AMBIENT_WIDTH: u32 = 640;
/// Blur radius (Gaussian sigma) of backdrops at [`AMBIENT_WIDTH`].
const AMBIENT_BLUR: f32 = 18.0;
/// How long art counts as just arrived, for views that fade it in.
const FRESH_FOR: Duration = Duration::from_millis(500);
/// Concurrent decodes. A full-size store image decodes to tens of MiB, so this
/// bounds peak memory (results are cached on disk; this only paces first loads).
const MAX_DECODES: usize = 2;

static DECODES: once_cell::sync::Lazy<Arc<Semaphore>> =
    once_cell::sync::Lazy::new(|| Arc::new(Semaphore::new(MAX_DECODES)));

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ArtKey {
    url: SharedString,
    width: u32,
    /// PNG with transparency (logos) vs JPEG; both may exist for one URL.
    alpha: bool,
}

/// Availability of a piece of art.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Art {
    Ready(Arc<Path>),
    Loading,
    Missing,
}

/// The blurred backdrop and dominant colour of one image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ambient {
    pub backdrop: Arc<Path>,
    /// sRGB colour the art is mostly made of: its most common vivid hue, or
    /// its average for art without one.
    pub rgb: [u8; 3],
}

pub struct ArtStore {
    ready: HashMap<ArtKey, Arc<Path>>,
    pending: HashSet<ArtKey>,
    failed: HashSet<ArtKey>,
    /// Pending thumbnails shown meanwhile at another width (see
    /// [`ArtStore::stand_in`]); they replace it without a fade.
    stood_in: HashSet<ArtKey>,
    /// When thumbnails produced in this session became ready (art already on
    /// disk is not listed: it shows at once and needs no fade).
    arrived: HashMap<ArtKey, Instant>,
    /// Ambient art by URL; `None` when it could not be produced.
    ambient: HashMap<SharedString, Option<Ambient>>,
    ambient_pending: HashSet<SharedString>,
    downloads: Arc<Semaphore>,
}

#[derive(Clone)]
struct ArtStoreHandle(Entity<ArtStore>);
impl Global for ArtStoreHandle {}

/// The blurred backdrop and colour of `url`, starting their production when
/// they are not on disk yet. `None` until they are ready or if they fail.
pub fn ambient(url: &str, cx: &mut App) -> Option<Ambient> {
    art_store(cx).update(cx, |s, cx| s.ambient(url, cx))
}

/// The art store entity; observe it to re-render when art arrives.
pub fn art_store(cx: &App) -> Entity<ArtStore> {
    cx.global::<ArtStoreHandle>().0.clone()
}

pub(crate) fn install(cx: &mut App) {
    let store = cx.new(|_| ArtStore {
        ready: HashMap::new(),
        pending: HashSet::new(),
        failed: HashSet::new(),
        stood_in: HashSet::new(),
        arrived: HashMap::new(),
        ambient: HashMap::new(),
        ambient_pending: HashSet::new(),
        downloads: Arc::new(Semaphore::new(MAX_DOWNLOADS)),
    });
    cx.set_global(ArtStoreHandle(store));
}

fn bucket(width: u32) -> u32 {
    BUCKETS
        .iter()
        .copied()
        .find(|b| *b >= width)
        .unwrap_or(*BUCKETS.last().unwrap_or(&1920))
}

fn digest(url: &str) -> String {
    let mut h = Sha256::new();
    h.update(url.as_bytes());
    hex::encode(&h.finalize()[..12])
}

fn thumb_path(url: &str, width: u32, alpha: bool) -> PathBuf {
    let ext = if alpha { "png" } else { "jpg" };
    Paths::get()
        .images_dir()
        .join("thumbs")
        .join(format!("{}-{width}.{ext}", digest(url)))
}

fn ambient_paths(url: &str) -> (PathBuf, PathBuf) {
    let dir = Paths::get().images_dir().join("thumbs");
    let name = format!("{}-ambient", digest(url));
    (
        dir.join(format!("{name}.jpg")),
        dir.join(format!("{name}.rgb")),
    )
}

fn original_path(url: &str) -> PathBuf {
    Paths::get().images_dir().join("orig").join(digest(url))
}

/// A local file referenced by a URL-ish string, if it is one.
fn local_path(url: &str) -> Option<PathBuf> {
    if let Some(p) = url.strip_prefix("file://") {
        return Some(PathBuf::from(p));
    }
    url.starts_with('/').then(|| PathBuf::from(url))
}

impl ArtStore {
    /// Art for `url` at least `width` physical pixels wide. Starts producing the
    /// thumbnail when missing; observers are notified when it is ready.
    /// `keep_alpha` keeps transparency (logos) by storing PNG.
    pub fn get(&mut self, url: &str, width: u32, keep_alpha: bool, cx: &mut Context<Self>) -> Art {
        if url.is_empty() {
            return Art::Missing;
        }
        let key = ArtKey {
            url: SharedString::from(url.to_string()),
            width: bucket(width),
            alpha: keep_alpha,
        };
        if let Some(p) = self.ready.get(&key) {
            return Art::Ready(p.clone());
        }
        if self.failed.contains(&key) {
            return Art::Missing;
        }
        if self.pending.contains(&key) {
            return self.stand_in_or_loading(&key);
        }
        let thumb = thumb_path(url, key.width, keep_alpha);
        if thumb.is_file() {
            let p: Arc<Path> = thumb.into();
            self.ready.insert(key, p.clone());
            return Art::Ready(p);
        }
        self.pending.insert(key.clone());
        let shown = self.stand_in_or_loading(&key);
        let b = backend(cx);
        let permits = self.downloads.clone();
        let url = key.url.to_string();
        let width = key.width;
        let job = b.spawn(async move {
            let _permit = permits.acquire_owned().await.ok();
            produce(&url, width, keep_alpha, thumb).await
        });
        cx.spawn(async move |this, cx| {
            let result = job.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |store, cx| {
                store.pending.remove(&key);
                if store.pending.is_empty() {
                    release_freed_memory(cx);
                }
                let stood_in = store.stood_in.remove(&key);
                match result {
                    Ok(path) => {
                        if !stood_in {
                            store.arrived.insert(key.clone(), Instant::now());
                        }
                        store.ready.insert(key, path.into());
                    }
                    Err(e) => {
                        tracing::debug!("art {}: {e:#}", key.url);
                        store.failed.insert(key);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        shown
    }

    /// While `key` is produced, the same art at the nearest width already in
    /// memory, so a tile that changes size (interface size, a window crossing
    /// a bucket) keeps its picture instead of flashing a placeholder: the next
    /// larger width when there is one, else the largest smaller one.
    fn stand_in_or_loading(&mut self, key: &ArtKey) -> Art {
        let at = |width: u32| {
            self.ready.get(&ArtKey {
                width,
                ..key.clone()
            })
        };
        let larger = BUCKETS
            .iter()
            .filter(|w| **w > key.width)
            .find_map(|w| at(*w));
        let stand_in = larger
            .or_else(|| {
                BUCKETS
                    .iter()
                    .rev()
                    .filter(|w| **w < key.width)
                    .find_map(|w| at(*w))
            })
            .cloned();
        match stand_in {
            Some(path) => {
                self.stood_in.insert(key.clone());
                Art::Ready(path)
            }
            None => Art::Loading,
        }
    }

    /// Whether the thumbnail for these arguments was produced moments ago, so
    /// a view can fade it in rather than have it pop.
    pub fn is_fresh(&mut self, url: &str, width: u32, keep_alpha: bool) -> bool {
        let key = ArtKey {
            url: SharedString::from(url.to_string()),
            width: bucket(width),
            alpha: keep_alpha,
        };
        let now = Instant::now();
        self.arrived
            .retain(|_, at| now.duration_since(*at) < FRESH_FOR);
        self.arrived.contains_key(&key)
    }

    /// The blurred backdrop and colour of `url`, or `None` while they are
    /// produced (observers are notified) or when they cannot be.
    pub fn ambient(&mut self, url: &str, cx: &mut Context<Self>) -> Option<Ambient> {
        if url.is_empty() {
            return None;
        }
        let key = SharedString::from(url.to_string());
        if let Some(known) = self.ambient.get(&key) {
            return known.clone();
        }
        if self.ambient_pending.contains(&key) {
            return None;
        }
        let (image, rgb) = ambient_paths(url);
        if let Some(found) = read_ambient(&image, &rgb) {
            self.ambient.insert(key, Some(found.clone()));
            return Some(found);
        }
        self.ambient_pending.insert(key.clone());
        let permits = self.downloads.clone();
        let source = key.to_string();
        let job = backend(cx).spawn(async move {
            let _permit = permits.acquire_owned().await.ok();
            produce_ambient(&source, image, rgb).await
        });
        cx.spawn(async move |this, cx| {
            let result = job.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |store, cx| {
                store.ambient_pending.remove(&key);
                let ambient = result
                    .inspect_err(|e| tracing::debug!("ambient art {key}: {e:#}"))
                    .ok();
                store.ambient.insert(key, ambient);
                cx.notify();
            });
        })
        .detach();
        None
    }

    /// Forget failures so they are retried (after connectivity returns).
    pub fn retry_failed(&mut self, cx: &mut Context<Self>) {
        let before = self.ambient.len();
        self.ambient.retain(|_, a| a.is_some());
        if !self.failed.is_empty() || self.ambient.len() != before {
            self.failed.clear();
            cx.notify();
        }
    }
}

/// Hand memory freed by a batch of decodes back to the system. glibc keeps
/// it otherwise; `malloc_trim` walks every arena, so run it off the UI thread.
fn release_freed_memory(cx: &App) {
    backend(cx).spawn_blocking(|| {
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        // SAFETY: malloc_trim only releases free pages; it is thread-safe.
        unsafe {
            libc::malloc_trim(0);
        }
    });
}

/// The original of `url` on disk, downloading it on first use.
async fn original(url: &str) -> Result<PathBuf> {
    if let Some(p) = local_path(url) {
        return Ok(p);
    }
    let orig = original_path(url);
    if !orig.is_file() {
        let bytes = irori_core::http::client()
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        irori_core::json_store::write_bytes(&orig, &bytes)?;
    }
    Ok(orig)
}

/// Download (or read) the original and write a resized thumbnail.
async fn produce(url: &str, width: u32, keep_alpha: bool, thumb: PathBuf) -> Result<PathBuf> {
    let source = original(url).await?;
    let _decode = DECODES.clone().acquire_owned().await?;
    tokio::task::spawn_blocking(move || resize(&source, width, keep_alpha, &thumb).map(|_| thumb))
        .await?
}

/// Download (or read) the original and write its blurred backdrop and colour.
async fn produce_ambient(url: &str, image: PathBuf, rgb: PathBuf) -> Result<Ambient> {
    let source = original(url).await?;
    let _decode = DECODES.clone().acquire_owned().await?;
    tokio::task::spawn_blocking(move || write_ambient(&source, &image, &rgb)).await?
}

fn write_ambient(source: &Path, image: &Path, rgb_path: &Path) -> Result<Ambient> {
    let img = image::ImageReader::open(source)?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("decoding {}", source.display()))?;
    let small = img
        .resize(
            AMBIENT_WIDTH,
            u32::MAX,
            image::imageops::FilterType::Triangle,
        )
        .to_rgb8();
    let rgb = dominant_color(&image::imageops::thumbnail(&small, 64, 64));
    let blurred = image::imageops::fast_blur(&small, AMBIENT_BLUR);
    if let Some(dir) = image.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = image.with_extension("tmp");
    {
        let file = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        image::codecs::jpeg::JpegEncoder::new_with_quality(file, 80).encode_image(&blurred)?;
    }
    std::fs::rename(&tmp, image)?;
    std::fs::write(rgb_path, hex::encode(rgb))?;
    Ok(Ambient {
        backdrop: image.into(),
        rgb,
    })
}

fn read_ambient(image: &Path, rgb: &Path) -> Option<Ambient> {
    if !image.is_file() {
        return None;
    }
    let bytes = hex::decode(std::fs::read_to_string(rgb).ok()?.trim()).ok()?;
    Some(Ambient {
        backdrop: image.into(),
        rgb: bytes.try_into().ok()?,
    })
}

/// The colour an image is mostly made of. Pixels vote for one of 24 hue
/// bins, weighted by how vivid they are (near-grey, near-black and
/// near-white pixels barely count); the winning bin's average is the answer.
/// Images with no vivid colour at all get their plain average.
pub fn dominant_color(img: &image::RgbImage) -> [u8; 3] {
    const BINS: usize = 24;
    let mut bins = [[0f64; 4]; BINS];
    let mut total = [0f64; 3];
    for p in img.pixels() {
        let [r, g, b] = p.0.map(|c| c as f64 / 255.0);
        total[0] += r;
        total[1] += g;
        total[2] += b;
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let chroma = max - min;
        if chroma < 0.08 || max < 0.15 {
            continue;
        }
        let hue = if max == r {
            ((g - b) / chroma).rem_euclid(6.0)
        } else if max == g {
            (b - r) / chroma + 2.0
        } else {
            (r - g) / chroma + 4.0
        } / 6.0;
        // Vivid, mid-bright pixels weigh most; pale highlights least.
        let weight = chroma * (1.0 - (max - 0.6).abs());
        let bin = &mut bins[((hue * BINS as f64) as usize).min(BINS - 1)];
        bin[0] += r * weight;
        bin[1] += g * weight;
        bin[2] += b * weight;
        bin[3] += weight;
    }
    let to_u8 = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let best = bins.iter().max_by(|a, b| a[3].total_cmp(&b[3]));
    match best {
        Some(bin) if bin[3] > img.pixels().len() as f64 * 0.01 => {
            [bin[0], bin[1], bin[2]].map(|c| to_u8(c / bin[3]))
        }
        _ => {
            let n = img.pixels().len().max(1) as f64;
            total.map(|c| to_u8(c / n))
        }
    }
}

fn resize(source: &Path, width: u32, keep_alpha: bool, out: &Path) -> Result<()> {
    let img = image::ImageReader::open(source)?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("decoding {}", source.display()))?;
    let img = if img.width() > width {
        img.resize(width, u32::MAX, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = out.with_extension("tmp");
    if keep_alpha {
        img.save_with_format(&tmp, image::ImageFormat::Png)?;
    } else {
        let rgb = img.to_rgb8();
        let file = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(file, 86);
        enc.encode_image(&rgb)?;
    }
    std::fs::rename(&tmp, out)?;
    Ok(())
}

/// Image cache that keeps at most `capacity` decoded images, evicting the
/// least recently used. Attach with `div().image_cache(cache.clone())`.
pub struct LruImageCache {
    items: HashMap<u64, ImageCacheItem>,
    order: VecDeque<u64>,
    capacity: usize,
}

impl LruImageCache {
    pub fn new(capacity: usize, cx: &mut App) -> Entity<Self> {
        let entity = cx.new(|_| Self {
            items: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(8),
        });
        cx.observe_release(&entity, |cache, cx| {
            for (_, item) in cache.items.drain() {
                if let Some(Ok(image)) = item.get() {
                    cx.drop_image(image, None);
                }
            }
        })
        .detach();
        entity
    }

    /// Keep at most `capacity` decoded images from now on, dropping the least
    /// recently used ones that no longer fit.
    pub fn set_capacity(&mut self, capacity: usize, cx: &mut App) {
        self.capacity = capacity.max(8);
        while self.items.len() > self.capacity {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(item) = self.items.remove(&old)
                && let Some(Ok(image)) = item.get()
            {
                cx.drop_image(image, None);
            }
        }
    }

    fn touch(&mut self, key: u64) {
        if let Some(pos) = self.order.iter().rposition(|k| *k == key) {
            if pos + 1 == self.order.len() {
                return;
            }
            self.order.remove(pos);
        }
        self.order.push_back(key);
    }
}

impl ImageCache for LruImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = hash(resource);
        if !self.items.contains_key(&key) {
            while self.items.len() >= self.capacity {
                let Some(old) = self.order.pop_front() else {
                    break;
                };
                if let Some(item) = self.items.remove(&old)
                    && let Some(Ok(image)) = item.get()
                {
                    cx.drop_image(image, Some(window));
                }
            }
            self.items.insert(key, ImageCacheItem::new(resource, cx));
        }
        self.touch(key);
        self.items.get(&key).and_then(|item| item.use_image(window))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dominant_color_prefers_the_vivid_majority() {
        // Mostly dark grey with a large orange area and a small blue one.
        let img = image::RgbImage::from_fn(64, 64, |x, y| {
            if y < 24 {
                image::Rgb([230, 110, 40])
            } else if x < 8 && y > 56 {
                image::Rgb([30, 80, 220])
            } else {
                image::Rgb([40, 40, 40])
            }
        });
        let [r, g, b] = dominant_color(&img);
        assert!(r > 200 && (90..130).contains(&g) && b < 60, "{r} {g} {b}");
    }

    #[test]
    fn dominant_color_of_grey_art_is_its_average() {
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([90, 90, 90]));
        assert_eq!(dominant_color(&img), [90, 90, 90]);
    }

    #[test]
    fn ambient_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("irori-ambient-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.png");
        image::RgbImage::from_pixel(1000, 500, image::Rgb([200, 60, 30]))
            .save(&src)
            .unwrap();
        let (image, rgb) = (dir.join("a.jpg"), dir.join("a.rgb"));
        let made = write_ambient(&src, &image, &rgb).unwrap();
        assert_eq!(made.rgb, [200, 60, 30]);
        assert_eq!(image::open(&image).unwrap().width(), AMBIENT_WIDTH);
        assert_eq!(read_ambient(&image, &rgb), Some(made));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn buckets_round_up() {
        assert_eq!(bucket(1), 160);
        assert_eq!(bucket(161), 240);
        assert_eq!(bucket(5000), 1920);
    }

    #[test]
    fn resizes_and_keeps_alpha() {
        let dir = std::env::temp_dir().join(format!("irori-art-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.png");
        image::RgbaImage::from_pixel(800, 400, image::Rgba([255, 0, 0, 128]))
            .save(&src)
            .unwrap();
        let out = dir.join("out.png");
        resize(&src, 200, true, &out).unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!((img.width(), img.height()), (200, 100));
        assert!(img.color().has_alpha());
        let jpg = dir.join("out.jpg");
        resize(&src, 200, false, &jpg).unwrap();
        assert!(!image::open(&jpg).unwrap().color().has_alpha());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
