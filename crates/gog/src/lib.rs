//! GOG protocol client (a Rust port of the parts of gogdl Ember needs,
//! GPL-3.0).
//!
//! * [`auth`]: sign-in, token refresh and game-scoped tokens.
//! * [`content`]: builds, build metadata, depot manifests, signed download
//!   links and the redistributables repository.
//! * [`plan`]: download plans for [`ember_depot`] (v1 and v2 depots,
//!   updates that reuse unchanged chunks, Linux installers via [`linux`]).
//! * [`saves`]: cloud saves.

#![forbid(unsafe_code)]

pub mod auth;
pub mod content;
pub mod languages;
pub mod linux;
pub mod plan;
pub mod saves;

/// User agent of GOG Galaxy's communication service, which GOG's APIs expect.
pub(crate) const USER_AGENT: &str =
    "GOGGalaxyCommunicationService/2.0.13.27 (Windows_32bit) installation_source/gog";
