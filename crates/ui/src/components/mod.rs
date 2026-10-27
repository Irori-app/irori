//! Domain-aware components shared by feature crates.

mod brand;
mod cover;
mod empty;
mod page;
mod status;
mod store_badge;

pub use brand::BrandMark;
pub use cover::{
    ArtImage, GameCover, art_arrival, art_image, art_img, art_is_fresh, art_loading, art_missing,
    missing_art_fill, physical_width,
};
pub use empty::EmptyState;
pub use page::{PageHeader, SectionTitle};
pub use status::{ProgressSummary, progress_text};
pub use store_badge::StoreBadge;
