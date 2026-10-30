pub static GALAXY_CLIENT_ID: &str = "46899977096215655";
pub static NOTIFICATIONS_PUSHER_SOCKET: &str = "wss://notifications-pusher.gog.com/";

use crate::api::structs::Token;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::Mutex;

pub type TokenStorage = Arc<Mutex<HashMap<String, Token>>>;
/// Client ids of the games the host started. The port is open to every local
/// program, so only these may exchange the account's login for a token.
pub type AllowedClients = Arc<Mutex<HashSet<String>>>;

/// Whether `id` has the shape of a Galaxy client id. Ids are joined into file
/// paths and addresses, and arrive from whoever connected to the port.
pub fn is_client_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit())
}
