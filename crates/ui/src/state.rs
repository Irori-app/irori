//! Access to the backend from GPUI code, and the bridge from backend futures
//! (Tokio) to GPUI tasks.

use std::future::Future;

use anyhow::{Result, anyhow};
use gpui_kit::{App, Global, Task};
use irori_services::Backend;

/// The backend handle, installed once at startup.
pub struct AppState {
    backend: Backend,
}

impl Global for AppState {}

impl AppState {
    pub fn install(backend: Backend, cx: &mut App) {
        cx.set_global(AppState { backend });
    }
}

/// The backend. Panics if called before [`crate::init`].
pub fn backend(cx: &App) -> Backend {
    cx.global::<AppState>().backend.clone()
}

/// Run a backend future on the backend runtime and get a GPUI task resolving
/// to its result. Await it from `cx.spawn`/`cx.spawn_in`.
pub fn run<T, F>(cx: &App, fut: F) -> Task<Result<T>>
where
    F: Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let handle = backend(cx).spawn(fut);
    cx.background_executor()
        .spawn(async move { handle.await.map_err(|e| anyhow!("task failed: {e}"))? })
}

/// Like [`run`] for infallible futures.
pub fn run_infallible<T, F>(cx: &App, fut: F) -> Task<Option<T>>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handle = backend(cx).spawn(fut);
    cx.background_executor()
        .spawn(async move { handle.await.ok() })
}
