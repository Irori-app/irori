//! Amazon Games protocol client (a Rust port of the parts of nile Irori
//! needs, GPL-3.0).
//!
//! * [`auth`]: the PKCE sign-in page and device registration.
//! * [`Client`]: tokens, owned games, build locations and update checks.
//! * [`Manifest`]: Amazon's signed build manifests, turned into
//!   [`irori_depot`] download entries.
//! * [`fuel`]: `fuel.json` (launch command and post-install steps).
//! * [`sdk`]: the runtime SDK files games expect.

#![forbid(unsafe_code)]

mod api;
pub mod auth;
pub mod fuel;
pub mod library;
mod manifest;
pub mod sdk;

pub use api::{Client, Endpoints, FetchedManifest, GameDownload, SignInRejected};
pub use auth::{Credentials, LoginRequest};
pub use library::Entitlement;
#[cfg(any(test, feature = "test-support"))]
pub use manifest::testing;
pub use manifest::{Manifest, ManifestFile};
