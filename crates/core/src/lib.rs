//! Ember core: the domain model and contracts shared by every other crate.
//! No UI dependency; everything here is usable from tests and headless tools.

pub mod events;
pub mod fs_guard;
pub mod game;
pub mod http;
pub mod install;
pub mod json_store;
pub mod launch;
pub mod log_cap;
pub mod paths;
pub mod process;
pub mod runner;
pub mod settings;
pub mod status;
pub mod store;
pub mod util;

pub use events::{Event, EventBus, Notice, NoticeLevel};
pub use game::*;
pub use install::*;
pub use launch::*;
pub use paths::Paths;
pub use runner::{GameId, InstallPlatform, Runner};
pub use settings::*;
pub use status::{GameStatus, StatusKind};
pub use store::{DynStore, InstallInfoQuery, LoginFlow, OpContext, SettingsFn, Store};

pub use anyhow;
pub use async_trait::async_trait;
