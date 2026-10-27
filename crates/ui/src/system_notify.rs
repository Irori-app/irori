//! Desktop notifications for what happens while Irori is not in front: a
//! download finished or failed, a game failed to start, a store needs logging
//! in again, updates came out. While Irori's window is active its own toasts
//! are enough.

use std::collections::VecDeque;
use std::rc::Rc;

use gpui_kit::{App, BorrowAppContext as _, Global, SharedString, SystemNotification};
use irori_core::{GameId, Notice};

use crate::i18n::{t, t_args};
use crate::nav::Route;

/// Identity Irori's notifications are shown under. Matches the macOS bundle
/// id in `packaging/package.py`; Windows toasts need it set to show at all.
const APP_ID: &str = "io.github.irori-app.Irori";
/// Notifications whose click target is remembered.
const REMEMBERED: usize = 32;

type OpenFn = Rc<dyn Fn(Option<Route>, &mut App)>;

struct SystemNotices {
    next: u64,
    /// Tag and the page a click opens, newest last.
    targets: VecDeque<(SharedString, Option<Route>)>,
}

impl Global for SystemNotices {}

struct OnOpen(OpenFn);

impl Global for OnOpen {}

/// Register Irori with the desktop's notification service. `open` brings the
/// window forward (reopening it from the tray) and shows the page, if any,
/// when a notification is clicked.
pub fn install(open: impl Fn(Option<Route>, &mut App) + 'static, cx: &mut App) {
    cx.set_app_identity(APP_ID, "Irori");
    cx.set_global(SystemNotices {
        next: 0,
        targets: VecDeque::new(),
    });
    cx.set_global(OnOpen(Rc::new(open)));
    cx.on_system_notification_response(|response, cx| {
        let target = cx
            .try_global::<SystemNotices>()
            .and_then(|n| n.targets.iter().find(|(tag, _)| *tag == response.tag))
            .map(|(_, route)| route.clone());
        let open = cx.try_global::<OnOpen>().map(|o| o.0.clone());
        if let (Some(route), Some(open)) = (target, open) {
            open(route, cx);
        }
    });
}

/// The title and message of a notice in the interface language.
pub fn notice_text(n: &Notice) -> (SharedString, Option<SharedString>) {
    let args: Vec<(&str, &str)> = n.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let title = match n.title_key {
        Some(key) => t_args(key, &args),
        None => SharedString::from(n.title.clone()),
    };
    let message = match (n.message_key, &n.message) {
        (Some(key), _) => Some(t_args(key, &args)),
        (None, Some(m)) => Some(SharedString::from(m.clone())),
        (None, None) => None,
    };
    (title, message)
}

/// Whether Irori is in the background: no window of it is active.
fn in_background(cx: &App) -> bool {
    cx.active_window().is_none()
}

/// The page a click on `notice` opens: its game, or Accounts for a notice
/// about a store account.
pub fn notice_route(notice: &Notice) -> Option<Route> {
    match (&notice.game, notice.account) {
        (Some(id), _) => Some(Route::Game(id.clone())),
        (None, Some(_)) => Some(Route::Accounts),
        (None, None) => None,
    }
}

fn post(title: SharedString, body: SharedString, route: Option<Route>, cx: &mut App) {
    let Some(state) = cx.try_global::<SystemNotices>() else {
        return;
    };
    let tag = SharedString::from(format!("irori-{}", state.next));
    cx.update_global::<SystemNotices, _>(|n, _| {
        n.next += 1;
        n.targets.push_back((tag.clone(), route));
        while n.targets.len() > REMEMBERED {
            n.targets.pop_front();
        }
    });
    cx.show_system_notification(SystemNotification {
        tag,
        title,
        body,
        actions: Vec::new(),
    });
}

/// Show `notice` on the desktop when Irori is in the background.
pub(crate) fn notice(notice: &Notice, cx: &mut App) {
    if !in_background(cx) {
        return;
    }
    let (title, message) = notice_text(notice);
    post(title, message.unwrap_or_default(), notice_route(notice), cx);
}

/// Tell the user a game failed to start, when Irori is in the background.
pub(crate) fn failed_start(id: &GameId, title: &str, cx: &mut App) {
    if !in_background(cx) {
        return;
    }
    post(
        t_args("ui.notify.failed_start", &[("title", title)]),
        t("ui.notify.failed_start_body"),
        Some(Route::Game(id.clone())),
        cx,
    );
}
