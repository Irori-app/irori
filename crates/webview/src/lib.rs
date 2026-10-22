extern crate gpui_kit as gpui;

#[cfg(all(feature = "gpui-fast", target_os = "macos"))]
mod composition;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod offscreen;
#[cfg(target_os = "linux")]
mod popups;
#[cfg(target_os = "linux")]
mod toplevel;

#[cfg(all(feature = "gpui-fast", target_os = "macos"))]
use composition::NativeWebViewSurface;
#[cfg(all(feature = "gpui-fast", target_os = "linux"))]
use linux::NativeWebViewSurface;

use std::{cell::Cell, ops::Deref, rc::Rc};

/// The Wry version WebViews are built with.
pub use wry;
use wry::Rect;

use gpui::{
    App, Bounds, ContentMask, DismissEvent, Element, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, Hitbox, InteractiveElement, IntoElement, LayoutId, MouseDownEvent,
    ParentElement as _, Pixels, Render, Size, Style, Styled as _, Window, canvas, div,
};

// `gpui-kit` selects the GPUI backend; this crate's `gpui-fast` feature only follows it, so
// the two must agree. A mismatch would otherwise keep the gpui-pre attachment on GPUI Fast.
const _: () = {
    assert!(
        !gpui::GPUI_FAST || cfg!(feature = "gpui-fast"),
        "gpui-kit is built with `gpui-fast`, but gpui-webview is not: enable the \
         `gpui-webview/gpui-fast` feature so the WebView uses GPUI Fast's window composition"
    );
    assert!(
        gpui::GPUI_FAST || !cfg!(feature = "gpui-fast"),
        "gpui-webview is built with `gpui-fast`, but gpui-kit is not: enable the \
         `gpui-kit/gpui-fast` feature, which selects the GPUI backend"
    );
};

/// Prepare the process for WebViews on a native Wayland window.
///
/// There, WebKitGTK renders offscreen, which its DMA-BUF renderer cannot do; this turns it
/// off through the environment WebKit reads when it starts. Elsewhere it does nothing.
///
/// # Safety
///
/// This sets an environment variable, so it must run before any other thread exists: call it
/// first thing in `main`.
pub unsafe fn prepare_process() {
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var_os(offscreen::DMABUF_ENV).is_none()
    {
        // SAFETY: The caller guarantees no other thread reads the environment.
        unsafe { std::env::set_var(offscreen::DMABUF_ENV, "1") };
    }
}

/// Offscreen state for the end-to-end check.
#[doc(hidden)]
#[derive(Debug)]
pub struct OffscreenDebug {
    pub cursor: gpui::CursorStyle,
    /// The device size of the last captured frame.
    pub frame_size: Option<Size<gpui::DevicePixels>>,
    /// The labels of the open `<select>` menu.
    pub option_menu: Option<Vec<String>>,
}

/// An owned, UI-thread-local handle to the raw wry webview.
///
/// Cloning this handle prolongs the native webview's lifetime. Dropping the owning [`WebView`]
/// entity hides the child view, but final native destruction waits until all handle and frame
/// clones are dropped. All handles must be dropped before the parent window is destroyed.
#[derive(Clone)]
pub struct WebViewHandle(Rc<wry::WebView>);

impl WebViewHandle {
    /// Get the raw wry webview.
    pub fn raw(&self) -> &wry::WebView {
        &self.0
    }
}

/// A webview based on wry WebView.
///
/// With the `gpui-fast` feature, macOS and Linux X11 host the native view in a window
/// composition surface, so deferred GPUI overlays render above the page. Without it, the
/// native view covers GPUI content in its bounds and extends past a container that clips it.
///
/// On a native Wayland window the page renders offscreen and GPUI paints it, so overlays and
/// clipping always work there; call [`prepare_process`] at startup.
///
/// [experimental]
pub struct WebView {
    focus_handle: FocusHandle,
    webview: Rc<wry::WebView>,
    visible: Cell<bool>,
    /// The visibility last applied to the native view, so prepaint does not repeat it.
    native_visible: Cell<Option<bool>>,
    bounds: Bounds<Pixels>,
    /// The page zoom the script asked for, before any display scale compensation.
    zoom: Cell<f64>,
    /// Whether the last geometry update failed, so each failure is logged once until it recovers.
    geometry_failed: Cell<bool>,
    /// The GPUI focus seen at the last prepaint, to notice when focus moves elsewhere.
    last_focus: std::cell::RefCell<Option<gpui::WeakFocusHandle>>,
    #[cfg(all(feature = "gpui-fast", any(target_os = "macos", target_os = "linux")))]
    composition: Option<NativeWebViewSurface>,
    /// The GPUI scale factor the page zoom was last matched to.
    #[cfg(target_os = "linux")]
    scale_factor: Cell<f32>,
    /// The window-relative origin that page clicks are forwarded to GPUI from.
    #[cfg(target_os = "linux")]
    origin: Rc<Cell<gpui::Point<Pixels>>>,
    /// The offscreen host on a native Wayland window.
    #[cfg(target_os = "linux")]
    offscreen: Option<Rc<offscreen::OffscreenView>>,
    #[cfg(target_os = "linux")]
    _gtk: Option<linux::GtkUse>,
    #[cfg(target_os = "linux")]
    popups: popups::Popups,
}

/// What a [`WebView`] reports to the entity that shows it.
pub enum WebViewEvent {
    /// The page opened a new window, such as a sign-in provider's pop-up or a link with
    /// `target=_blank`. Show this WebView for the user to continue there. Sent only after
    /// [`WebView::set_popups_enabled`]; otherwise new windows are refused.
    ///
    /// Linux only for now.
    Popup(Entity<WebView>),
    /// The page closed itself (`window.close()`).
    Close,
}

impl Drop for WebView {
    fn drop(&mut self) {
        self.hide();
        #[cfg(all(feature = "gpui-fast", any(target_os = "macos", target_os = "linux")))]
        if let Some(surface) = self.composition.take() {
            surface.release(self.webview.clone());
        }
    }
}

impl WebView {
    /// Create a new WebView from a wry WebView.
    pub fn new(webview: wry::WebView, window: &mut Window, cx: &mut App) -> Self {
        let _ = webview.set_bounds(Rect::default());

        #[cfg(all(feature = "gpui-fast", target_os = "macos"))]
        let composition = match NativeWebViewSurface::new(&webview, window, cx) {
            Ok(surface) => Some(surface),
            Err(error) => {
                log::warn!("WebView composition unavailable: {error:#}");
                None
            }
        };
        #[cfg(target_os = "linux")]
        let origin = Rc::new(Cell::new(gpui::Point::default()));
        #[cfg(target_os = "linux")]
        linux::forward_mouse_down(&webview, origin.clone(), window, cx);
        #[cfg(target_os = "linux")]
        let popups = popups::Popups::watch(&webview, window, cx);
        #[cfg(target_os = "linux")]
        linux::match_scale_factor(&webview, window.scale_factor(), 1.0);
        #[cfg(not(any(all(feature = "gpui-fast", target_os = "macos"), target_os = "linux")))]
        let _ = window;

        Self {
            focus_handle: cx.focus_handle(),
            visible: Cell::new(true),
            native_visible: Cell::new(None),
            bounds: Bounds::default(),
            zoom: Cell::new(1.0),
            geometry_failed: Cell::new(false),
            last_focus: Default::default(),
            webview: Rc::new(webview),
            #[cfg(all(feature = "gpui-fast", target_os = "macos"))]
            composition,
            #[cfg(all(feature = "gpui-fast", target_os = "linux"))]
            composition: None,
            #[cfg(target_os = "linux")]
            scale_factor: Cell::new(window.scale_factor()),
            #[cfg(target_os = "linux")]
            origin,
            #[cfg(target_os = "linux")]
            offscreen: None,
            #[cfg(target_os = "linux")]
            _gtk: None,
            #[cfg(target_os = "linux")]
            popups,
        }
    }

    /// Build a webview as a child of `window`, performing the platform setup Wry requires.
    ///
    /// On Linux this initializes GTK on the backend of `window` and drives its main loop from
    /// GPUI. An X11 (or XWayland) window hosts the page in a native child window; with the
    /// `gpui-fast` feature, in a window composition surface so GPUI overlays render above it.
    /// A native Wayland window hosts it offscreen, which needs [`prepare_process`] at startup.
    pub fn build(
        builder: wry::WebViewBuilder,
        window: &mut Window,
        cx: &mut App,
    ) -> wry::Result<Self> {
        #[cfg(target_os = "linux")]
        let (webview, gtk) = {
            if offscreen::is_wayland(window) {
                let gtk = linux::ensure_gtk(linux::GdkBackend::Offscreen, cx)?;
                let parts = offscreen::prepare(builder)?;
                let (webview, host) = offscreen::OffscreenView::attach(parts, window, cx);
                let mut this = Self::new_offscreen(webview, host, window, cx);
                this._gtk = Some(gtk);
                return Ok(this);
            }
            let parent = linux::X11Parent::new(window)?;
            let gtk = linux::ensure_gtk(linux::GdkBackend::X11, cx)?;

            #[cfg(feature = "gpui-fast")]
            match NativeWebViewSurface::new(window, cx) {
                Ok(surface) => {
                    let webview = builder.build_as_child(&surface.parent()?)?;
                    let mut this = Self::new(webview, window, cx);
                    this.composition = Some(surface);
                    this._gtk = Some(gtk);
                    return Ok(this);
                }
                Err(error) => log::warn!("WebView composition unavailable: {error:#}"),
            }

            (builder.build_as_child(&parent)?, gtk)
        };
        #[cfg(not(target_os = "linux"))]
        let webview = builder.build_as_child(&*window)?;

        #[allow(unused_mut)]
        let mut this = Self::new(webview, window, cx);
        #[cfg(target_os = "linux")]
        {
            this._gtk = Some(gtk);
        }
        Ok(this)
    }

    /// A WebView whose page renders offscreen and is painted by GPUI. Its clicks come from
    /// GPUI, so they are not forwarded back to it.
    #[cfg(target_os = "linux")]
    fn new_offscreen(
        webview: wry::WebView,
        host: Rc<offscreen::OffscreenView>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        linux::match_scale_factor(&webview, window.scale_factor(), 1.0);
        let popups = popups::Popups::watch(&webview, window, cx);
        Self {
            focus_handle: cx.focus_handle(),
            visible: Cell::new(true),
            native_visible: Cell::new(None),
            bounds: Bounds::default(),
            zoom: Cell::new(1.0),
            geometry_failed: Cell::new(false),
            last_focus: Default::default(),
            webview: Rc::new(webview),
            #[cfg(feature = "gpui-fast")]
            composition: None,
            scale_factor: Cell::new(window.scale_factor()),
            origin: Rc::new(Cell::new(gpui::Point::default())),
            offscreen: Some(host),
            _gtk: None,
            popups,
        }
    }

    /// A WebView for a page a WebView opened as a new window.
    #[cfg(target_os = "linux")]
    pub(crate) fn from_parts(
        parts: offscreen::OffscreenParts,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let gtk = linux::ensure_gtk(linux::GdkBackend::Offscreen, cx).ok();
        let (webview, host) = offscreen::OffscreenView::attach(parts, window, cx);
        let mut this = Self::new_offscreen(webview, host, window, cx);
        this._gtk = gtk;
        this
    }

    /// Let the page open new windows, which are announced with [`WebViewEvent::Popup`].
    /// Off by default, refusing them. Linux only for now.
    pub fn set_popups_enabled(&self, enabled: bool) {
        #[cfg(target_os = "linux")]
        self.popups.set_enabled(enabled);
        #[cfg(not(target_os = "linux"))]
        let _ = enabled;
    }

    /// Whether the page renders offscreen and is painted by GPUI (native Wayland).
    pub fn is_offscreen(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.offscreen.is_some();
        #[cfg(not(target_os = "linux"))]
        false
    }

    /// Offscreen state for the end-to-end check.
    #[doc(hidden)]
    pub fn offscreen_debug(&self) -> Option<OffscreenDebug> {
        #[cfg(target_os = "linux")]
        if let Some(host) = &self.offscreen {
            return Some(OffscreenDebug {
                cursor: host.cursor(),
                frame_size: host.frame_size(),
                option_menu: host
                    .option_menu()
                    .map(|(items, _)| items.into_iter().map(|i| i.label).collect()),
            });
        }
        None
    }

    /// Set window-relative bounds, including offscreen loading bounds.
    /// With window composition, position the managed container and its child together.
    pub fn set_bounds(&self, bounds: Rect) -> wry::Result<()> {
        #[cfg(all(feature = "gpui-fast", any(target_os = "macos", target_os = "linux")))]
        if let Some(surface) = &self.composition {
            return surface
                .set_bounds(&self.webview, bounds)
                .map_err(|error| wry::Error::Io(std::io::Error::other(error)));
        }
        #[cfg(target_os = "linux")]
        if let Some(host) = &self.offscreen {
            let scale = self.scale_factor.get();
            let position = bounds.position.to_logical::<f32>(f64::from(scale));
            let size = bounds.size.to_logical::<f32>(f64::from(scale));
            host.set_geometry(
                Bounds::new(
                    gpui::point(gpui::px(position.x), gpui::px(position.y)),
                    gpui::size(gpui::px(size.width), gpui::px(size.height)),
                ),
                scale,
            );
            return Ok(());
        }
        self.webview.set_bounds(bounds)
    }

    /// Apply full document bounds and an inherited clip without changing the DOM viewport.
    /// Empty clips hide the native surface while preserving the loaded document.
    /// Partial clipping requires native window composition or offscreen rendering; without
    /// either, the native view takes the full document bounds and extends past the clip.
    pub fn set_geometry(
        &self,
        document: Bounds<Pixels>,
        clip: Bounds<Pixels>,
    ) -> anyhow::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(host) = &self.offscreen {
            // GPUI clips the painted page itself; the page keeps rendering while scrolled away.
            host.set_geometry(document, self.scale_factor.get());
            self.set_native_visible(self.visible.get())?;
            return Ok(());
        }
        let visible = document.intersect(&clip);
        if visible.is_empty() || !self.visible.get() {
            self.set_native_visible(false)?;
            #[cfg(all(feature = "gpui-fast", any(target_os = "macos", target_os = "linux")))]
            if let Some(surface) = &self.composition {
                // Hidden preparation still gives WebKit its real layout width.
                if self.visible.get() && !document.is_empty() {
                    surface.set_geometry(&self.webview, document, document)?;
                }
            }
            return Ok(());
        }
        #[cfg(all(feature = "gpui-fast", any(target_os = "macos", target_os = "linux")))]
        if let Some(surface) = &self.composition {
            surface.set_geometry(&self.webview, document, visible)?;
            self.set_native_visible(true)?;
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        let rect = linux::device_bounds(document, self.scale_factor.get());
        #[cfg(not(target_os = "linux"))]
        let rect = Rect {
            position: wry::dpi::LogicalPosition::new(
                f64::from(document.origin.x),
                f64::from(document.origin.y),
            )
            .into(),
            size: wry::dpi::LogicalSize::new(
                f64::from(document.size.width),
                f64::from(document.size.height),
            )
            .into(),
        };
        self.set_bounds(rect)?;
        self.set_native_visible(true)?;
        Ok(())
    }

    // Geometry can temporarily hide the native view without changing the caller's
    // visibility preference, so prepaint can retry when the view becomes unclipped.
    fn set_native_visible(&self, visible: bool) -> wry::Result<()> {
        if self.native_visible.get() == Some(visible) {
            return Ok(());
        }
        self.webview.set_visible(visible)?;
        #[cfg(all(feature = "gpui-fast", any(target_os = "macos", target_os = "linux")))]
        if let Some(surface) = &self.composition {
            surface
                .set_visible(visible)
                .map_err(|error| wry::Error::Io(std::io::Error::other(error)))?;
        }
        self.native_visible.set(Some(visible));
        Ok(())
    }

    /// Set visibility and report native errors without discarding the loaded page.
    pub fn set_visible(&self, visible: bool) -> wry::Result<()> {
        // A focus error must not leave a closing native view covering the window.
        let focus_result = if !visible && self.visible.get() {
            self.blur_native()
        } else {
            Ok(())
        };
        self.set_native_visible(visible)?;
        self.visible.set(visible);
        focus_result
    }

    /// Take keyboard focus away from the page, back to the GPUI window.
    fn blur_native(&self) -> wry::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(host) = &self.offscreen {
            host.set_focused(false);
            return Ok(());
        }
        self.webview.focus_parent()
    }

    /// Show the webview.
    pub fn show(&mut self) {
        let _ = self.set_visible(true);
    }

    /// Hide the webview.
    pub fn hide(&mut self) {
        let _ = self.set_visible(false);
    }

    /// Get whether the webview is visible.
    pub fn visible(&self) -> bool {
        self.visible.get()
    }

    /// Get the current bounds of the webview.
    pub fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// Go back in the webview history.
    pub fn back(&mut self) -> anyhow::Result<()> {
        Ok(self.webview.evaluate_script("history.back();")?)
    }

    /// Go forward in the webview history.
    pub fn forward(&mut self) -> anyhow::Result<()> {
        Ok(self.webview.evaluate_script("history.forward();")?)
    }

    /// Load a URL in the webview.
    pub fn load_url(&mut self, url: &str) {
        let _ = self.webview.load_url(url);
    }

    /// Set the colour scheme the page sees in `prefers-color-scheme` and draws its
    /// native controls in. `Theme::Auto` follows the system again.
    ///
    /// On Linux this is GTK's process-wide `gtk-application-prefer-dark-theme`, so it
    /// applies to every webview in the application.
    pub fn set_theme(&self, theme: wry::Theme) -> wry::Result<()> {
        #[cfg(target_os = "linux")]
        linux::set_theme(theme);
        #[cfg(target_os = "macos")]
        macos::set_theme(&self.webview, theme);
        #[cfg(target_os = "windows")]
        wry::WebViewExtWindows::set_theme(&*self.webview, theme)?;
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        let _ = theme;
        Ok(())
    }

    /// Zoom the page, where `1.0` is its natural size. The engine scales the page
    /// itself, without the relayout and repaint a CSS `zoom` causes, and keeps the
    /// factor across navigations.
    pub fn set_zoom(&self, zoom: f64) -> wry::Result<()> {
        self.zoom.set(zoom);
        // WebKitGTK's zoom also makes up for the gap between GPUI's scale factor and
        // GTK's integer one, so the page zoom is applied on top of it.
        #[cfg(target_os = "linux")]
        linux::match_scale_factor(&self.webview, self.scale_factor.get(), zoom);
        #[cfg(not(target_os = "linux"))]
        self.webview.zoom(zoom)?;
        Ok(())
    }

    /// The page zoom last set with [`WebView::set_zoom`].
    pub fn zoom(&self) -> f64 {
        self.zoom.get()
    }

    /// Get the raw wry webview.
    pub fn raw(&self) -> &wry::WebView {
        &self.webview
    }

    /// Get an owned, UI-thread-local handle to the raw wry webview.
    pub fn handle(&self) -> WebViewHandle {
        WebViewHandle(self.webview.clone())
    }

    /// Keep native keyboard focus in step with GPUI's: when GPUI focus moves to another
    /// element, the page gives up the keyboard, so GPUI inputs receive typing again.
    fn sync_focus(&self, window: &Window, cx: &App) {
        let ours = self.focus_handle.contains_focused(window, cx);
        #[cfg(target_os = "linux")]
        if let Some(host) = &self.offscreen {
            host.set_focused(ours && window.is_window_active());
            return;
        }
        let Some(focused) = window.focused(cx) else {
            return;
        };
        let mut last = self.last_focus.borrow_mut();
        if last.as_ref().is_some_and(|last| *last == focused) {
            return;
        }
        *last = Some(focused.downgrade());
        if !ours {
            let _ = self.webview.focus_parent();
        }
    }
}

impl Deref for WebView {
    type Target = wry::WebView;

    fn deref(&self) -> &Self::Target {
        &self.webview
    }
}

impl Focusable for WebView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for WebView {}

impl EventEmitter<WebViewEvent> for WebView {}

impl Render for WebView {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity().clone();
        #[cfg(target_os = "linux")]
        self.popups.set_owner(view.downgrade());

        let root = div()
            .track_focus(&self.focus_handle)
            .size_full()
            .child({
                let view = cx.entity().clone();
                canvas(
                    move |bounds, _, cx| view.update(cx, |r, _| r.bounds = bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            })
            .child(WebViewElement::new(self.webview.clone(), view, window, cx));
        #[cfg(target_os = "linux")]
        let root = linux_input::attach(root, self, window, cx);
        root
    }
}

/// Keyboard input and `<select>` menus for a page rendered offscreen.
#[cfg(target_os = "linux")]
mod linux_input {
    use std::ops::Range;

    use gpui::{
        Bounds, ClipboardItem, Context, Div, EntityInputHandler, FontWeight,
        InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent, ParentElement as _, Pixels,
        StatefulInteractiveElement as _, Styled as _, UTF16Selection, Window, WindowAppearance,
        anchored, deferred, div, prelude::FluentBuilder as _, px, rgb,
    };

    use super::WebView;

    /// A clipboard shortcut handled through GPUI's clipboard.
    enum Clipboard {
        Copy,
        Cut,
        Paste,
    }

    fn clipboard_shortcut(keystroke: &gpui::Keystroke) -> Option<Clipboard> {
        let m = keystroke.modifiers;
        let control = m.control && !m.alt && !m.platform && !m.function;
        let shift = m.shift && !m.control && !m.alt && !m.platform;
        match keystroke.key.as_str() {
            "c" | "insert" if control && !m.shift => Some(Clipboard::Copy),
            "x" if control && !m.shift => Some(Clipboard::Cut),
            "v" if control => Some(Clipboard::Paste),
            "delete" if shift => Some(Clipboard::Cut),
            "insert" if shift => Some(Clipboard::Paste),
            _ => None,
        }
    }

    pub(super) fn attach(
        root: Div,
        webview: &WebView,
        window: &mut Window,
        cx: &mut Context<WebView>,
    ) -> Div {
        if webview.offscreen.is_none() {
            return root;
        }
        root.on_key_down(cx.listener(WebView::offscreen_key_down))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, cx| {
                if let Some(host) = &this.offscreen
                    && host.key(false, &event.keystroke)
                {
                    cx.stop_propagation();
                }
            }))
            .children(option_menu(webview, window, cx))
    }

    impl WebView {
        fn offscreen_key_down(
            &mut self,
            event: &KeyDownEvent,
            _: &mut Window,
            cx: &mut Context<Self>,
        ) {
            let Some(host) = self.offscreen.clone() else {
                return;
            };
            if host.option_menu().is_some() {
                if event.keystroke.key == "escape" {
                    host.close_option_menu(None);
                    cx.stop_propagation();
                    cx.notify();
                }
                return;
            }
            match clipboard_shortcut(&event.keystroke) {
                Some(Clipboard::Copy) => host.copy_selection(&self.webview, false, cx),
                Some(Clipboard::Cut) => host.copy_selection(&self.webview, true, cx),
                Some(Clipboard::Paste) => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        host.insert_text(&text);
                    }
                }
                None => {
                    if !host.key(true, &event.keystroke) {
                        return;
                    }
                }
            }
            cx.stop_propagation();
        }
    }

    /// The open `<select>` menu, drawn by GPUI below the element.
    fn option_menu(
        webview: &WebView,
        window: &Window,
        cx: &mut Context<WebView>,
    ) -> Option<impl IntoElement + use<>> {
        let host = webview.offscreen.clone()?;
        let (items, anchor) = host.option_menu()?;
        let dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let (bg, fg, muted, border, hover, selected) = if dark {
            (0x262626, 0xf5f5f5, 0x8a8a8a, 0x404040, 0x3a3a3a, 0x1e4f8a)
        } else {
            (0xffffff, 0x171717, 0x737373, 0xd4d4d4, 0xf0f0f0, 0xd6e6fb)
        };
        let close = {
            let host = host.clone();
            cx.listener(move |_, _: &gpui::MouseDownEvent, _, cx| {
                host.close_option_menu(None);
                cx.notify();
            })
        };
        let rows = items.into_iter().enumerate().map(|(index, item)| {
            let choosable = item.enabled && !item.group_label;
            let host = host.clone();
            div()
                .id(index)
                .px_2()
                .py(px(3.))
                .whitespace_nowrap()
                .when(item.group_child, |row| row.pl_5())
                .when(item.group_label, |row| row.font_weight(FontWeight::BOLD))
                .when(!item.enabled, |row| row.text_color(rgb(muted)))
                .when(item.selected, |row| row.bg(rgb(selected)))
                .when(choosable, |row| {
                    row.cursor_pointer()
                        .hover(|style| style.bg(rgb(hover)))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            host.close_option_menu(Some(index));
                            cx.notify();
                        }))
                })
                .child(item.label)
        });
        Some(
            deferred(
                anchored()
                    .position(anchor.bottom_left())
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("webview-option-menu")
                            .occlude()
                            .min_w(anchor.size.width)
                            .max_h(px(360.))
                            .overflow_y_scroll()
                            .py_1()
                            .bg(rgb(bg))
                            .text_color(rgb(fg))
                            .text_sm()
                            .border_1()
                            .border_color(rgb(border))
                            .rounded_md()
                            .shadow_md()
                            .on_mouse_down_out(close)
                            .children(rows),
                    ),
            )
            .with_priority(1),
        )
    }

    /// Committed text from an input method; plain typing arrives as key events instead.
    impl EntityInputHandler for WebView {
        fn text_for_range(
            &mut self,
            _: Range<usize>,
            _: &mut Option<Range<usize>>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<String> {
            None
        }

        fn selected_text_range(
            &mut self,
            _: bool,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<UTF16Selection> {
            None
        }

        fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
            None
        }

        fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

        fn paste(&mut self, item: ClipboardItem, _: &mut Window, _: &mut Context<Self>) {
            if let (Some(host), Some(text)) = (&self.offscreen, item.text()) {
                host.insert_text(&text);
            }
        }

        fn replace_text_in_range(
            &mut self,
            _: Option<Range<usize>>,
            text: &str,
            _: &mut Window,
            _: &mut Context<Self>,
        ) {
            if let Some(host) = &self.offscreen {
                host.insert_text(text);
            }
        }

        fn replace_and_mark_text_in_range(
            &mut self,
            _: Option<Range<usize>>,
            _: &str,
            _: Option<Range<usize>>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) {
        }

        fn bounds_for_range(
            &mut self,
            _: Range<usize>,
            element_bounds: Bounds<Pixels>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<Bounds<Pixels>> {
            Some(element_bounds)
        }

        fn character_index_for_point(
            &mut self,
            _: gpui::Point<Pixels>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<usize> {
            None
        }
    }
}

/// A webview element can display a wry webview.
pub struct WebViewElement {
    parent: Entity<WebView>,
    view: Rc<wry::WebView>,
}

impl WebViewElement {
    /// Create a new webview element from a wry WebView.
    pub fn new(
        view: Rc<wry::WebView>,
        parent: Entity<WebView>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self {
        Self { view, parent }
    }
}

impl IntoElement for WebViewElement {
    type Element = WebViewElement;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for WebViewElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: Size::full(),
            flex_shrink: 1.,
            ..Default::default()
        };

        // If the parent view is no longer visible, we don't need to layout the webview
        let id = window.request_layout(style, [], cx);
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if !self.parent.read(cx).visible() {
            return None;
        }

        let parent = self.parent.read(cx);
        parent.sync_focus(window, cx);
        #[cfg(all(feature = "gpui-fast", any(target_os = "macos", target_os = "linux")))]
        if let Some(surface) = &parent.composition {
            surface.set_scale_factor(window.scale_factor());
        }
        #[cfg(target_os = "linux")]
        if parent.scale_factor.replace(window.scale_factor()) != window.scale_factor() {
            linux::match_scale_factor(&parent.webview, window.scale_factor(), parent.zoom.get());
        }
        #[cfg(target_os = "linux")]
        parent.origin.set(bounds.origin);
        let clip = window.content_mask().bounds;
        if let Err(error) = parent.set_geometry(bounds, clip) {
            if !parent.geometry_failed.replace(true) {
                log::warn!("WebView geometry update failed: {error:#}");
            }
            let _ = parent.set_native_visible(false);
            return None;
        }
        parent.geometry_failed.set(false);
        let visible = bounds.intersect(&clip);
        if visible.is_empty() {
            return None;
        }

        #[cfg(target_os = "linux")]
        if parent.offscreen.is_some() {
            // GPUI paints the page, so it gets input like any other element.
            return Some(window.insert_hitbox(visible, gpui::HitboxBehavior::Normal));
        }
        // The native view covers this area, so GPUI content below it must not receive input.
        Some(window.insert_hitbox(visible, gpui::HitboxBehavior::BlockMouse))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        #[cfg(target_os = "linux")]
        if let Some(host) = self.parent.read(cx).offscreen.clone() {
            if let Some(hitbox) = hitbox.as_ref() {
                paint_offscreen(&host, &self.parent, bounds, hitbox, window, cx);
            }
            return;
        }

        let bounds = hitbox.clone().map(|h| h.bounds).unwrap_or(bounds);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let webview = self.view.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, _, _| {
                if phase == gpui::DispatchPhase::Bubble && !bounds.contains(&event.position) {
                    // Click white space to blur the input focus
                    let _ = webview.focus_parent();
                }
            });
        });
        // A page click forwarded to GPUI (Linux X11) focuses the WebView, so focus leaving it
        // later can be noticed.
        let focus = self.parent.read(cx).focus_handle.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == gpui::DispatchPhase::Bubble && bounds.contains(&event.position) {
                window.focus(&focus, cx);
            }
        });
    }
}

/// Paint the offscreen page and route GPUI input to it.
#[cfg(target_os = "linux")]
fn paint_offscreen(
    host: &Rc<offscreen::OffscreenView>,
    parent: &Entity<WebView>,
    bounds: Bounds<Pixels>,
    hitbox: &Hitbox,
    window: &mut Window,
    cx: &mut App,
) {
    use gpui::{
        DispatchPhase, ElementInputHandler, MouseExitEvent, MouseMoveEvent, MouseUpEvent,
        ScrollWheelEvent, px, size,
    };

    if let Some(image) = host.frame(window) {
        // The frame keeps its own size while a resize is in flight, rather than stretching.
        let scale = window.scale_factor();
        let frame = image.size(0);
        let image_bounds = Bounds::new(
            bounds.origin,
            size(
                px(frame.width.0 as f32 / scale),
                px(frame.height.0 as f32 / scale),
            ),
        );
        let _ = window.paint_image(bounds, image_bounds, Default::default(), image, 0, false);
    }
    window.set_cursor_style(host.cursor(), hitbox);

    let focus = parent.read(cx).focus_handle.clone();
    window.on_mouse_event({
        let host = host.clone();
        let hitbox = hitbox.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                return;
            }
            window.focus(&focus, cx);
            host.close_option_menu(None);
            host.mouse_down(event.position, event.button, event.modifiers);
            cx.stop_propagation();
        }
    });
    window.on_mouse_event({
        let host = host.clone();
        move |event: &MouseUpEvent, phase, _, _| {
            if phase == DispatchPhase::Bubble {
                host.mouse_up(event.position, event.button, event.modifiers);
            }
        }
    });
    window.on_mouse_event({
        let host = host.clone();
        let hitbox = hitbox.clone();
        move |event: &MouseMoveEvent, phase, window, _| {
            if phase == DispatchPhase::Bubble {
                host.mouse_move(event.position, hitbox.is_hovered(window), event.modifiers);
            }
        }
    });
    window.on_mouse_event({
        let host = host.clone();
        move |event: &MouseExitEvent, phase, _, _| {
            if phase == DispatchPhase::Bubble {
                host.mouse_exit(event.position, event.modifiers);
            }
        }
    });
    window.on_mouse_event({
        let host = host.clone();
        let hitbox = hitbox.clone();
        move |event: &ScrollWheelEvent, phase, window, cx| {
            // Scrolling right after typing must work too, which `is_hovered` would refuse.
            if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                host.scroll(event.position, event.delta, event.modifiers);
                cx.stop_propagation();
            }
        }
    });
    let focus = parent.read(cx).focus_handle.clone();
    window.handle_input(&focus, ElementInputHandler::new(bounds, parent.clone()), cx);
}
