//! Ember's download engine, shared by the Amazon, GOG and Epic clients.
//!
//! A store client turns its manifest into a list of [`FileEntry`] values
//! (each file built from parts of downloadable [`Chunk`]s) and hands them to
//! [`download`], which fetches chunks in parallel, resumes interrupted work,
//! verifies every file against its digest and only then moves it into place.
//! [`diff`] compares an installed build with a new one so updates fetch only
//! what changed, and [`verify`] finds files that need repairing.
//!
//! Cancellation uses the same token type as `ember_core::process`.

#![forbid(unsafe_code)]

mod adapt;
mod download;
pub mod hash;
pub mod path;
mod plan;
mod progress;
#[cfg(any(test, feature = "test-server"))]
pub mod test_server;
mod verify;

pub use download::{Finished, Job, Options, SIGN_BATCH, Signer, UrlSigner, download};
pub use hash::{Algorithm, Digest};
pub use plan::{Chunk, Codec, Decoder, Diff, FileEntry, Part, Reuse, diff};
pub use progress::{Progress, ProgressFn, ignore as ignore_progress};
pub use tokio_util::sync::CancellationToken;
pub use verify::{Problem, Verified, verify};

#[cfg(test)]
mod tests;
