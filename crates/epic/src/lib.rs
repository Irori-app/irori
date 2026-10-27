//! Epic Games Store protocol client (a Rust port of the parts of legendary
//! Irori needs, GPL-3.0).
//!
//! * [`Manifest`]: binary and JSON build manifests, including encrypted
//!   builds, and [`chunk`]s.
//! * [`install`]: turning manifests into [`irori_depot`] download plans
//!   (selective downloads, updates that reuse unchanged data, signed chunk
//!   addresses).
//! * [`Client`] and [`library`]: accounts, owned games and store metadata.
//! * [`launch`], [`saves`], [`egl`], [`overlay`], [`registry`]: launch
//!   parameters, cloud saves, Epic Games Launcher sync, the EOS overlay and
//!   Wine registry access.

#![forbid(unsafe_code)]

pub mod api;
pub mod auth;
mod binary;
pub mod chunk;
mod crypto;
pub mod egl;
pub mod install;
mod json_manifest;
pub mod launch;
pub mod library;
pub mod manifest;
pub mod overlay;
pub mod registry;
pub mod saves;

pub use api::{
    Asset, Client, Endpoints, Identity, LibraryItem, ManifestInfo, PlaytimeItem, PlaytimeUpload,
    SaveFile, Session, Sidecar, SignInRejected, Ticket,
};
pub use manifest::{ChunkInfo, ChunkPart, Comparison, FileManifest, Guid, Manifest, Meta, Secrets};
