//! Shared HTTP client. One connection pool for the whole process.

use std::time::Duration;

use once_cell::sync::Lazy;

pub const USER_AGENT: &str = concat!("Irori/", env!("CARGO_PKG_VERSION"));

/// A browser-like user agent for store web APIs that reject unknown clients.
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(2)
        .build()
        .expect("http client")
});

/// The process-wide client (must be used from within the Tokio runtime).
pub fn client() -> &'static reqwest::Client {
    &CLIENT
}

/// GET a URL and decode JSON, failing on non-2xx statuses.
pub async fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> anyhow::Result<T> {
    Ok(client()
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

static DOWNLOAD_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(4)
        .build()
        .expect("download client")
});

/// Client for large transfers: no total timeout, only connect and per-read
/// idle timeouts, so multi-gigabyte downloads are not cut off.
pub fn download_client() -> &'static reqwest::Client {
    &DOWNLOAD_CLIENT
}
