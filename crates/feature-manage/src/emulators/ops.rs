//! Running emulator and core installs. Kept in a global entity so an install
//! keeps its progress and cancel handle when the Emulators page is closed
//! and reopened, and so other screens (the setup guide, "Play with…") can
//! start installs that the page shows.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ember_core::process::CancellationToken;
use ember_core::{InstallProgress, Notice, NoticeLevel, OpContext};
use ember_engines::releases::Release;
use ember_ui::backend;
use ember_ui::i18n::t_args;
use gpui_kit::{App, AppContext as _, Entity, Global};

/// Live state of one install, written from the backend runtime.
#[derive(Debug, Clone, Default)]
pub(crate) struct OpLive {
    pub progress: Option<InstallProgress>,
    pub step: Option<String>,
}

pub(crate) struct EngineOp {
    pub live: Arc<Mutex<OpLive>>,
    pub cancel: CancellationToken,
}

impl EngineOp {
    pub fn snapshot(&self) -> OpLive {
        self.live.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

/// Installs in flight by key (`engine:<id>` or `core:<id>`). `revision`
/// bumps when one ends so views reload.
#[derive(Default)]
pub(crate) struct EngineOps {
    pub running: HashMap<String, EngineOp>,
    pub revision: u64,
}

struct EngineOpsHandle(Entity<EngineOps>);
impl Global for EngineOpsHandle {}

pub(crate) fn engine_ops(cx: &mut App) -> Entity<EngineOps> {
    if let Some(h) = cx.try_global::<EngineOpsHandle>() {
        return h.0.clone();
    }
    let e = cx.new(|_| EngineOps::default());
    cx.set_global(EngineOpsHandle(e.clone()));
    e
}

pub(crate) fn engine_key(id: &str) -> String {
    format!("engine:{id}")
}

pub(crate) fn core_key(id: &str) -> String {
    format!("core:{id}")
}

/// What to install.
#[derive(Debug, Clone)]
pub enum InstallTarget {
    /// An emulator: a version (the newest when `None`); `make_default`
    /// switches games that don't pin a version to it (Update).
    Engine {
        id: String,
        release: Option<Release>,
        make_default: bool,
    },
    /// A RetroArch core.
    Core { id: String },
}

impl InstallTarget {
    fn key(&self) -> String {
        match self {
            InstallTarget::Engine { id, .. } => engine_key(id),
            InstallTarget::Core { id } => core_key(id),
        }
    }
}

/// Start an install. The outcome is reported through a backend notice, so
/// it reaches the user even when the page is closed.
pub fn start_install(target: InstallTarget, cx: &mut App) {
    let ops = engine_ops(cx);
    let key = target.key();
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
            EngineOp {
                live,
                cancel: cancel.clone(),
            },
        );
        cx.notify();
    });
    let b = backend(cx);
    let task = ember_ui::run(cx, async move {
        match target {
            InstallTarget::Engine {
                id,
                release,
                make_default,
            } => {
                let name = ember_engines::engine(&id)
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| id.clone());
                match b.install_engine(&id, release, make_default, &ctx).await {
                    Ok(Some(installed)) => b.bus.notice(Notice::new(
                        NoticeLevel::Success,
                        t_args(
                            "manage.emulators.installed_notice",
                            &[("engine", &name), ("version", &installed.version)],
                        )
                        .to_string(),
                    )),
                    Ok(None) => {}
                    Err(_) if ctx.cancel.is_cancelled() => {}
                    Err(e) => b.bus.notice(
                        Notice::new(
                            NoticeLevel::Error,
                            t_args("manage.emulators.install_failed", &[("engine", &name)])
                                .to_string(),
                        )
                        .message(format!("{e:#}")),
                    ),
                }
            }
            InstallTarget::Core { id } => {
                let name = core_name(&id);
                match b.install_core(&id, &ctx).await {
                    Ok(Some(_)) => b.bus.notice(Notice::new(
                        NoticeLevel::Success,
                        t_args("manage.emulators.core.installed_notice", &[("core", &name)])
                            .to_string(),
                    )),
                    Ok(None) => {}
                    Err(_) if ctx.cancel.is_cancelled() => {}
                    Err(e) => b.bus.notice(
                        Notice::new(
                            NoticeLevel::Error,
                            t_args("manage.emulators.core.install_failed", &[("core", &name)])
                                .to_string(),
                        )
                        .message(format!("{e:#}")),
                    ),
                }
            }
        }
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

/// The display name of a RetroArch core.
pub(crate) fn core_name(id: &str) -> String {
    ember_engines::engine("retroarch")
        .and_then(|m| m.core(id).map(|c| c.name.clone()))
        .unwrap_or_else(|| id.to_string())
}

/// Cancel a running install; partial downloads stay for a retry.
pub(crate) fn cancel_install(key: &str, cx: &mut App) {
    if let Some(op) = engine_ops(cx).read(cx).running.get(key) {
        op.cancel.cancel();
    }
}
