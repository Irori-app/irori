//! The Epic Online Services overlay, an app Epic distributes like a game.

use std::path::Path;

/// App name of the overlay.
pub const APP_NAME: &str = "98bc04bc842e4906993fd6d6644ffb8d";
pub const NAMESPACE: &str = "302e5ede476149b1bc3e4fe6ae45e50e";
pub const CATALOG_ITEM_ID: &str = "cc15684f44d849e89e9bf4cec0508b68";

/// Whether `dir` holds an overlay install.
pub fn is_install(dir: &Path) -> bool {
    dir.join("EOSOVH-Win64-Shipping.dll").is_file()
}
