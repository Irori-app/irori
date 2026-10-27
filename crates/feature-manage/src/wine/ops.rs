//! Running Wine/Proton installs. Kept in a global entity so an install keeps
//! its progress and cancel handle when the Wine manager page is closed and
//! reopened.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gpui_kit::{App, AppContext as _, Entity, Global};
use irori_core::process::CancellationToken;
use irori_core::{Event, InstallProgress, Notice, NoticeLevel, OpContext};
use irori_runtime::manager::{self, Release};
use irori_ui::backend;
use irori_ui::i18n::t_args;

/// Stable key of a release across list refreshes.
pub(crate) fn release_key(r: &Release) -> String {
    format!("{}:{}", r.kind, r.version)
}

/// Live state of one install, written from the backend runtime.
#[derive(Debug, Clone, Default)]
pub(crate) struct OpLive {
    pub progress: Option<InstallProgress>,
    pub step: Option<String>,
}

pub(crate) struct WineOp {
    pub live: Arc<Mutex<OpLive>>,
    pub cancel: CancellationToken,
}

impl WineOp {
    pub fn snapshot(&self) -> OpLive {
        self.live.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

/// Installs in flight, by [`release_key`]. `revision` bumps when one ends so
/// views reload their release lists.
#[derive(Default)]
pub(crate) struct WineOps {
    pub running: HashMap<String, WineOp>,
    pub revision: u64,
}

struct WineOpsHandle(Entity<WineOps>);
impl Global for WineOpsHandle {}

pub(crate) fn wine_ops(cx: &mut App) -> Entity<WineOps> {
    if let Some(h) = cx.try_global::<WineOpsHandle>() {
        return h.0.clone();
    }
    let e = cx.new(|_| WineOps::default());
    cx.set_global(WineOpsHandle(e.clone()));
    e
}

/// Start installing (or updating) `release`. The outcome is reported through
/// a backend notice, so it reaches the user even when the page is closed.
pub(crate) fn start_install(release: Release, cx: &mut App) {
    let ops = wine_ops(cx);
    let key = release_key(&release);
    if ops.read(cx).running.contains_key(&key) {
        return;
    }
    let live = Arc::new(Mutex::new(OpLive::default()));
    let cancel = CancellationToken::new();
    let (progress_live, step_live) = (live.clone(), live.clone());
    let ctx = OpContext {
        progress: Arc::new(move |p| {
            if let Ok(mut l) = progress_live.lock() {
                l.progress = Some(p);
            }
        }),
        cancel: cancel.clone(),
        step: Arc::new(move |s| {
            if let Ok(mut l) = step_live.lock() {
                l.step = Some(s.to_string());
            }
        }),
        log_file: None,
        settings: None,
    };
    ops.update(cx, |o, cx| {
        o.running.insert(
            key.clone(),
            WineOp {
                live,
                cancel: cancel.clone(),
            },
        );
        cx.notify();
    });
    let b = backend(cx);
    let app = b.settings.app();
    let version = release.version.clone();
    let bus = b.bus.clone();
    let task = irori_ui::run(cx, async move {
        let result = manager::install(&release, &app, &ctx).await;
        match &result {
            Ok(_) => bus.notice(Notice::new(
                NoticeLevel::Success,
                t_args("manage.wine.installed", &[("version", &version)]).to_string(),
            )),
            Err(_) if ctx.cancel.is_cancelled() => {}
            Err(e) => bus.notice(
                Notice::new(
                    NoticeLevel::Error,
                    t_args("manage.wine.install_failed", &[("version", &version)]).to_string(),
                )
                .message(format!("{e:#}")),
            ),
        }
        bus.emit(Event::WineVersionsChanged);
        Ok(())
    });
    cx.spawn(async move |cx| {
        let _ = task.await;
        ops.update(cx, |o, cx| {
            o.running.remove(&key);
            o.revision += 1;
            cx.notify();
        });
    })
    .detach();
}

/// Cancel a running install; partial files are cleaned up by the runtime.
pub(crate) fn cancel_install(key: &str, cx: &mut App) {
    if let Some(op) = wine_ops(cx).read(cx).running.get(key) {
        op.cancel.cancel();
    }
}
