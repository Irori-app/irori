//! Pages a WebView opens as new windows (`window.open`, links with `target=_blank`), such as
//! a sign-in provider's pop-up. Each becomes a WebView of its own, rendered offscreen and
//! related to its opener so the two can talk, which the opener announces with
//! [`WebViewEvent::Popup`]. A page closing itself is announced with [`WebViewEvent::Close`].

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gpui::{App, AppContext as _, WeakEntity, Window};
use gtk::glib::prelude::Cast as _;
use webkit2gtk::WebViewExt as _;
use wry::{WebViewBuilderExtUnix as _, WebViewExtUnix as _};

use crate::{WebView, WebViewEvent, offscreen};

/// The pop-up and close handling of one WebView.
pub(crate) struct Popups {
    enabled: Rc<Cell<bool>>,
    /// The WebView entity events are emitted from, known once it renders.
    owner: Rc<RefCell<Option<WeakEntity<WebView>>>>,
}

impl Popups {
    /// Handle the new windows and close requests of `webview`, a page in `window`.
    pub(crate) fn watch(webview: &wry::WebView, window: &Window, cx: &App) -> Self {
        let enabled = Rc::new(Cell::new(false));
        let owner: Rc<RefCell<Option<WeakEntity<WebView>>>> = Rc::default();
        let view = webview.webview();
        let handle = window.window_handle();

        view.connect_create({
            let enabled = enabled.clone();
            let owner = owner.clone();
            let cx = cx.to_async();
            move |opener, _| {
                if !enabled.get() {
                    return None;
                }
                // WebKit needs the new page right away; it is shown in GPUI once wrapped.
                let builder = wry::WebViewBuilder::new().with_related_view(opener.clone());
                let parts = match offscreen::prepare(builder) {
                    Ok(parts) => parts,
                    Err(error) => {
                        log::warn!("WebView pop-up failed: {error}");
                        return None;
                    }
                };
                let page = parts.webview.webview().upcast::<gtk::Widget>();
                if std::env::var_os("DBG_SKIPWRAP").is_some() {
                    Box::leak(Box::new(parts));
                    return Some(page);
                }
                let owner = owner.clone();
                cx.spawn(async move |cx| {
                    let _ = handle.update(cx, |_, window, cx| {
                        let owner = owner.borrow().as_ref().and_then(WeakEntity::upgrade);
                        // Without an owner the pop-up is dropped, which closes it.
                        let Some(owner) = owner else {
                            return;
                        };
                        let popup = cx.new(|cx| WebView::from_parts(parts, window, cx));
                        // It may close itself before it is ever shown.
                        let weak = popup.downgrade();
                        popup.update(cx, |popup, _| popup.popups.set_owner(weak));
                        owner.update(cx, |_, cx| cx.emit(WebViewEvent::Popup(popup)));
                    });
                })
                .detach();
                Some(page)
            }
        });

        view.connect_close({
            let owner = owner.clone();
            let cx = cx.to_async();
            move |_| {
                let owner = owner.clone();
                cx.spawn(async move |cx| {
                    let owner = owner.borrow().as_ref().and_then(WeakEntity::upgrade);
                    if let Some(owner) = owner {
                        owner.update(cx, |_, cx| cx.emit(WebViewEvent::Close));
                    }
                })
                .detach();
            }
        });

        Self { enabled, owner }
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.set(enabled);
    }

    /// Remember the entity events come from.
    pub(crate) fn set_owner(&self, owner: WeakEntity<WebView>) {
        let mut current = self.owner.borrow_mut();
        if current.is_none() {
            *current = Some(owner);
        }
    }
}
