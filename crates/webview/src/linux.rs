//! Linux hosts Wry's WebKitGTK view in an X11 child window of the GPUI window, or, when GPUI
//! runs natively on Wayland, in an offscreen window painted by GPUI (see `offscreen`).

use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{
    App, Bounds, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, PlatformInput,
    Point, Window, point, px,
};
use gtk::glib::prelude::ObjectExt as _;
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, WindowHandle, XlibWindowHandle,
};

/// How often pending GTK events are dispatched between GPUI frames.
const GTK_POLL_INTERVAL: Duration = Duration::from_millis(16);

thread_local! {
    /// WebViews alive on this (the UI) thread; GTK's main loop is driven while any are.
    static LIVE_WEBVIEWS: Cell<usize> = const { Cell::new(0) };
    /// Whether the task driving GTK's main loop is running.
    static GTK_POLLING: Cell<bool> = const { Cell::new(false) };
}

/// The GDK backend GTK needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GdkBackend {
    /// X11, to parent the page into the GPUI window's X11 window.
    X11,
    /// Any, for a page rendered offscreen. X11 (XWayland) comes first: there GTK can own
    /// selections without keyboard focus, which WebKit's text selection depends on.
    Offscreen,
}

/// Keeps GTK's main loop driven while a WebView is alive.
pub(crate) struct GtkUse(());

impl Drop for GtkUse {
    fn drop(&mut self) {
        LIVE_WEBVIEWS.with(|live| live.set(live.get().saturating_sub(1)));
    }
}

/// Initialize GTK on `backend` once and drive its main loop from GPUI while WebViews live.
pub(crate) fn ensure_gtk(backend: GdkBackend, cx: &mut App) -> wry::Result<GtkUse> {
    let error = |message: &str| wry::Error::Io(std::io::Error::other(message.to_string()));
    if !gtk::is_initialized_main_thread() {
        // Wry downcasts the default GDK display to X11 for child windows.
        gtk::gdk::set_allowed_backends(match backend {
            GdkBackend::X11 => "x11",
            GdkBackend::Offscreen => "x11,wayland",
        });
        gtk::init().map_err(|e| error(&e.to_string()))?;
        // GTK must not announce the application's startup as done for its hidden windows.
        gtk::Window::set_auto_startup_notification(false);
    }
    let display = gtk::gdk::Display::default().ok_or_else(|| error("GTK has no display"))?;
    if backend == GdkBackend::X11 && display.type_().name() == "GdkWaylandDisplay" {
        return Err(error(
            "GTK already runs on Wayland, but a WebView in an X11 window needs it on X11",
        ));
    }

    LIVE_WEBVIEWS.with(|live| live.set(live.get() + 1));
    if !GTK_POLLING.with(|polling| polling.replace(true)) {
        cx.spawn(async move |cx| {
            while LIVE_WEBVIEWS.with(Cell::get) > 0 {
                while gtk::events_pending() {
                    gtk::main_iteration_do(false);
                }
                cx.background_executor().timer(GTK_POLL_INTERVAL).await;
            }
            // Let GTK finish tearing down the last WebView.
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            GTK_POLLING.with(|polling| polling.set(false));
        })
        .detach();
    }
    Ok(GtkUse(()))
}

/// WebKitGTK answers `prefers-color-scheme` from GTK's preference, which GTK 3 does not
/// read from the desktop, so without this every page renders light.
pub(crate) fn set_theme(theme: wry::Theme) {
    use gtk::prelude::GtkSettingsExt as _;
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(matches!(theme, wry::Theme::Dark));
    }
}

/// The GPUI window as the Xlib parent Wry requires; GPUI reports its X11 window through XCB.
pub(crate) struct X11Parent(XlibWindowHandle);

impl X11Parent {
    pub(crate) fn new(window: &Window) -> wry::Result<Self> {
        Self::from_raw(HasWindowHandle::window_handle(window)?.as_raw())
    }

    fn from_raw(handle: RawWindowHandle) -> wry::Result<Self> {
        match handle {
            RawWindowHandle::Xcb(handle) => {
                let mut xlib = XlibWindowHandle::new(handle.window.get().into());
                xlib.visual_id = handle.visual_id.map_or(0, |id| id.get().into());
                Ok(Self(xlib))
            }
            RawWindowHandle::Xlib(handle) => Ok(Self(handle)),
            _ => Err(wry::Error::Io(std::io::Error::other(
                "WebView on Linux requires an X11 window; start the application with \
                 `gpui_kit::platform::linux(WindowingModes::X11)`",
            ))),
        }
    }
}

impl HasWindowHandle for X11Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: The X11 window outlives the webview, which is destroyed with the GPUI window.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Xlib(self.0)) })
    }
}

/// Wry scales bounds by GDK's factor, which ignores GPUI's, so pass device pixels.
pub(crate) fn device_bounds(bounds: Bounds<Pixels>, scale_factor: f32) -> wry::Rect {
    let device = |value: Pixels| f32::from(value) * scale_factor;
    wry::Rect {
        position: PhysicalPosition::new(device(bounds.origin.x), device(bounds.origin.y)).into(),
        size: PhysicalSize::new(device(bounds.size.width), device(bounds.size.height)).into(),
    }
}

/// Forward presses on the page to GPUI, which does not otherwise see them, so overlays that
/// close on an outside click (popovers, menus) close when the page is clicked.
pub(crate) fn forward_mouse_down(
    webview: &wry::WebView,
    origin: Rc<Cell<Point<Pixels>>>,
    window: &Window,
    cx: &App,
) {
    use gtk::{gdk::EventType, glib::Propagation, prelude::WidgetExt as _};
    use wry::WebViewExtUnix as _;

    let handle = window.window_handle();
    let cx = cx.to_async();
    webview
        .webview()
        .connect_button_press_event(move |widget, event| {
            let button = match event.button() {
                1 => MouseButton::Left,
                2 => MouseButton::Middle,
                3 => MouseButton::Right,
                _ => return Propagation::Proceed,
            };
            if event.event_type() != EventType::ButtonPress {
                return Propagation::Proceed;
            }
            let (x, y) = event.position();
            let gdk_scale = widget.scale_factor() as f32;
            let origin = origin.get();
            // GTK dispatches this from the GTK main loop; update GPUI once it is done.
            cx.spawn(async move |cx| {
                handle.update(cx, |_, window, cx| {
                    let scale = gdk_scale / window.scale_factor();
                    let position = origin + point(px(x as f32 * scale), px(y as f32 * scale));
                    window.dispatch_event(
                        PlatformInput::MouseDown(MouseDownEvent {
                            button,
                            position,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                            first_mouse: false,
                        }),
                        cx,
                    );
                    window.dispatch_event(
                        PlatformInput::MouseUp(MouseUpEvent {
                            button,
                            position,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                        }),
                        cx,
                    );
                })
            })
            .detach();
            Propagation::Proceed
        });
}

/// Zoom the page so its CSS pixels match GPUI's scale (GDK only supports integer scales),
/// times the page zoom the application asked for.
pub(crate) fn match_scale_factor(webview: &wry::WebView, scale_factor: f32, zoom: f64) {
    use gtk::prelude::WidgetExt as _;
    use wry::WebViewExtUnix as _;

    let gdk_scale = webview.webview().scale_factor().max(1);
    let _ = webview.zoom(f64::from(scale_factor) / f64::from(gdk_scale) * zoom);
}

/// A GPUI Fast composition surface hosting the webview: an X11 child window that GPUI cuts
/// its overlays out of, so they render above the page.
#[cfg(feature = "gpui-fast")]
pub(crate) struct NativeWebViewSurface {
    surface: gpui::WindowCompositionSurface,
    scale_factor: std::cell::Cell<f32>,
    window: gpui::AnyWindowHandle,
    cx: gpui::AsyncApp,
}

#[cfg(feature = "gpui-fast")]
impl NativeWebViewSurface {
    pub(crate) fn new(window: &Window, cx: &App) -> anyhow::Result<Self> {
        let surface = window
            .enable_window_composition()?
            .create_native_surface()?;
        Ok(Self {
            surface,
            scale_factor: std::cell::Cell::new(window.scale_factor()),
            window: window.window_handle(),
            cx: cx.to_async(),
        })
    }

    /// The surface's X11 window, as the parent Wry builds the webview in.
    pub(crate) fn parent(&self) -> wry::Result<X11Parent> {
        let handle = self
            .surface
            .platform_surface()
            .and_then(|surface| surface.platform_handle())
            .map_err(|error| wry::Error::Io(std::io::Error::other(error)))?;
        match handle.downcast::<RawWindowHandle>() {
            Ok(handle) => X11Parent::from_raw(*handle),
            Err(_) => Err(wry::Error::UnsupportedWindowHandle),
        }
    }

    pub(crate) fn set_scale_factor(&self, scale_factor: f32) {
        self.scale_factor.set(scale_factor);
    }

    /// Position the surface in the window and fill it with the webview.
    pub(crate) fn set_bounds(&self, webview: &wry::WebView, rect: wry::Rect) -> anyhow::Result<()> {
        use gpui::{DevicePixels, point, size};

        let scale_factor = f64::from(self.scale_factor.get());
        let position = rect.position.to_physical::<i32>(scale_factor);
        let extent = rect.size.to_physical::<i32>(scale_factor);
        self.surface.platform_surface()?.set_bounds(Bounds::new(
            point(DevicePixels(position.x), DevicePixels(position.y)),
            size(DevicePixels(extent.width), DevicePixels(extent.height)),
        ))?;
        webview.set_bounds(wry::Rect {
            position: PhysicalPosition::new(0, 0).into(),
            size: PhysicalSize::new(extent.width, extent.height).into(),
        })?;
        Ok(())
    }

    pub(crate) fn set_geometry(
        &self,
        webview: &wry::WebView,
        document: Bounds<Pixels>,
        visible: Bounds<Pixels>,
    ) -> anyhow::Result<()> {
        self.surface
            .platform_surface()?
            .set_bounds(visible.to_device_pixels(self.scale_factor.get()))?;
        let child = Bounds::new(document.origin - visible.origin, document.size);
        webview.set_bounds(device_bounds(child, self.scale_factor.get()))?;
        Ok(())
    }

    pub(crate) fn set_visible(&self, visible: bool) -> anyhow::Result<()> {
        self.surface.platform_surface()?.set_visible(visible)
    }

    pub(crate) fn release(self, webview: std::rc::Rc<wry::WebView>) {
        // Entity destruction can occur while App is borrowed. Destroy Wry's X11 windows before
        // removing the surface window that contains them.
        self.cx
            .clone()
            .spawn(async move |cx| {
                drop(webview);
                let result = self.window.update(cx, |_, window, _| {
                    window
                        .enable_window_composition()?
                        .remove_surface(self.surface.id())
                });
                if let Ok(Err(error)) = result {
                    log::warn!("cannot release WebView composition surface: {error:#}");
                }
            })
            .detach();
    }
}
