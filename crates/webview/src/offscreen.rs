//! Native Wayland hosts Wry's WebKitGTK view in a GTK offscreen window.
//!
//! GTK cannot embed into a Wayland surface it does not own, so the page renders offscreen:
//! each damaged frame is copied into a GPUI image painted in the element's bounds, and GPUI
//! input is forwarded to the page as synthesized GDK events. Because the page is ordinary GPUI
//! content, overlays render above it and containers clip it.
//!
//! GTK runs on XWayland when it can: its own Wayland connection never has keyboard focus, so
//! the compositor rejects its selections, and WebKit drops a text selection as soon as it fails
//! to claim the primary selection with it. What GTK would show as windows of its own is done by
//! GPUI instead: `<select>` menus and file choosers open as GPUI UI, and copy and paste use
//! GPUI's clipboard.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Arc, Mutex},
};

use gpui::{
    AnyWindowHandle, App, AsyncApp, Bounds, CursorStyle, Keystroke, Modifiers, MouseButton,
    NavigationDirection, PathPromptOptions, Pixels, Point, RenderImage, ScrollDelta, Window, point,
    px, size,
};
use gtk::{cairo, gdk, glib, glib::prelude::ObjectExt as _, prelude::*};
use webkit2gtk::{FileChooserRequestExt as _, OptionMenuExt as _, WebViewExt as _};
use wry::{WebViewBuilderExtUnix as _, WebViewExtUnix as _};

use crate::toplevel::OffscreenToplevel;

/// The environment switch that keeps WebKitGTK off its DMA-BUF renderer, which needs a GL
/// context that GTK offscreen windows on Wayland cannot create.
pub(crate) const DMABUF_ENV: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";

/// WebKitGTK scrolls this many pixels per unit of smooth scroll delta.
const PIXELS_PER_SCROLL_UNIT: f32 = 40.;
/// GPUI reports one wheel notch as this many lines; GTK as one unit.
const LINES_PER_NOTCH: f32 = 3.;

/// One entry of a `<select>` menu the page asked to show.
#[derive(Clone, Debug)]
pub(crate) struct OptionItem {
    pub label: String,
    pub enabled: bool,
    pub group_label: bool,
    pub group_child: bool,
    pub selected: bool,
}

/// A `<select>` menu waiting for GPUI to show it.
pub(crate) struct OptionMenu {
    menu: webkit2gtk::OptionMenu,
    /// The `<select>` element, in GTK pixels relative to the page.
    anchor: Bounds<f32>,
    pub items: Vec<OptionItem>,
}

/// State GTK signal handlers share with the view.
#[derive(Default)]
struct Shared {
    /// The page painted since the last capture.
    dirty: Cell<bool>,
    /// A window refresh is already scheduled.
    refresh_pending: Cell<bool>,
    cursor: Cell<CursorStyle>,
    option_menu: RefCell<Option<OptionMenu>>,
}

/// The offscreen half of a WebView on Wayland.
pub(crate) struct OffscreenView {
    window: OffscreenToplevel,
    view: webkit2gtk::WebView,
    shared: Rc<Shared>,
    frame: RefCell<Option<Arc<RenderImage>>>,
    /// Frames replaced last paint, removed from the atlas on the next one.
    retired: RefCell<Vec<Arc<RenderImage>>>,
    /// The page size last requested, in GTK pixels.
    size: Cell<(i32, i32)>,
    /// The element's window-relative origin.
    origin: Cell<Point<Pixels>>,
    /// GPUI's scale factor over GTK's, converting GPUI pixels to GTK pixels.
    ratio: Cell<f32>,
    hovered: Cell<bool>,
    /// The GDK button mask of the buttons pressed on the page.
    buttons: Cell<u32>,
    focused: Cell<bool>,
    window_handle: AnyWindowHandle,
    cx: AsyncApp,
}

impl Drop for OffscreenView {
    fn drop(&mut self) {
        if let Some(menu) = self.shared.option_menu.take() {
            menu.menu.close();
        }
        let frames: Vec<_> = self
            .retired
            .take()
            .into_iter()
            .chain(self.frame.take())
            .collect();
        if !frames.is_empty() {
            let handle = self.window_handle;
            self.cx
                .spawn(async move |cx| {
                    let _ = handle.update(cx, |_, window, _| {
                        for image in frames {
                            let _ = window.drop_image(image);
                        }
                    });
                })
                .detach();
        }
    }
}

/// Whether `window` is a native Wayland window, which needs the offscreen host.
pub(crate) fn is_wayland(window: &Window) -> bool {
    use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    matches!(
        HasWindowHandle::window_handle(window).map(|h| h.as_raw()),
        Ok(RawWindowHandle::Wayland(_))
    )
}

/// A page built offscreen but not yet attached to a GPUI window.
pub(crate) struct OffscreenParts {
    pub webview: wry::WebView,
    window: OffscreenToplevel,
}

/// Build `builder` into an offscreen window. Only GTK is involved, so this also works
/// inside a WebKit signal handler, where GPUI's app state may be borrowed.
pub(crate) fn prepare(builder: wry::WebViewBuilder) -> wry::Result<OffscreenParts> {
    let gtk_on_wayland = gdk::Display::default()
        .is_some_and(|display| display.type_().name() == "GdkWaylandDisplay");
    if gtk_on_wayland && std::env::var_os(DMABUF_ENV).is_none() {
        return Err(wry::Error::Io(std::io::Error::other(
            "WebView on Wayland needs WebKitGTK's DMA-BUF renderer disabled before it starts: \
             call `gpui_webview::prepare_process()` at the start of `main`",
        )));
    }
    let window = OffscreenToplevel::new();
    window.set_default_size(1, 1);
    let webview = builder.build_gtk(&window)?;
    window.show_all();
    // The window lives as long as the page in it, which raw handles can keep past the
    // `WebView` entity.
    webview.webview().connect_destroy({
        let window = window.downgrade();
        move |_| {
            if let Some(window) = window.upgrade() {
                // SAFETY: The window only hosts this page, which is going away.
                unsafe { window.destroy() };
            }
        }
    });
    Ok(OffscreenParts { webview, window })
}

impl OffscreenView {
    /// Attach a prepared page to `window`, which repaints when the page changes.
    pub(crate) fn attach(
        parts: OffscreenParts,
        window: &Window,
        cx: &App,
    ) -> (wry::WebView, Rc<Self>) {
        let OffscreenParts {
            webview,
            window: offscreen,
        } = parts;
        let view = webview.webview();
        let shared = Rc::new(Shared::default());
        let notify = refresher(shared.clone(), window, cx);

        offscreen.connect_damage_event({
            let shared = shared.clone();
            let notify = notify.clone();
            move |_, _| {
                shared.dirty.set(true);
                notify();
                false
            }
        });
        track_cursor(&view, &shared, &notify);
        view.connect_show_option_menu({
            let shared = shared.clone();
            let notify = notify.clone();
            move |_, menu, _, rect| {
                show_option_menu(&shared, menu, rect, &notify);
                true
            }
        });
        // GTK would open the context menu as a popup of the offscreen window, which has no
        // place on screen.
        view.connect_context_menu(|_, _, _, _| true);
        view.connect_run_file_chooser({
            let cx = cx.to_async();
            move |_, request| {
                choose_files(request.clone(), &cx);
                true
            }
        });

        let this = Rc::new(Self {
            window: offscreen,
            view,
            shared,
            frame: RefCell::default(),
            retired: RefCell::default(),
            size: Cell::new((1, 1)),
            origin: Cell::default(),
            ratio: Cell::new(1.),
            hovered: Cell::new(false),
            buttons: Cell::new(0),
            focused: Cell::new(false),
            window_handle: window.window_handle(),
            cx: cx.to_async(),
        });
        (webview, this)
    }

    /// Size the page to `bounds` and remember where it is, for input coordinates.
    pub(crate) fn set_geometry(&self, bounds: Bounds<Pixels>, scale_factor: f32) {
        self.origin.set(bounds.origin);
        let ratio = scale_factor / self.view.scale_factor().max(1) as f32;
        self.ratio.set(ratio);
        let gtk = |value: Pixels| (f32::from(value) * ratio).round().max(1.) as i32;
        let size = (gtk(bounds.size.width), gtk(bounds.size.height));
        if self.size.replace(size) != size {
            // The window follows its child's requested size.
            self.view.set_size_request(size.0, size.1);
        }
    }

    /// The latest frame, captured again when the page painted since.
    pub(crate) fn frame(&self, window: &mut Window) -> Option<Arc<RenderImage>> {
        for image in self.retired.borrow_mut().drain(..) {
            let _ = window.drop_image(image);
        }
        if (self.shared.dirty.replace(false) || self.frame.borrow().is_none())
            && let Some(image) = self.capture()
            && let Some(old) = self.frame.replace(Some(image))
        {
            self.retired.borrow_mut().push(old);
        }
        self.frame.borrow().clone()
    }

    /// Copy the offscreen window's surface into a BGRA image at device resolution.
    fn capture(&self) -> Option<Arc<RenderImage>> {
        let surface = self.window.surface()?;
        let scale = self.window.scale_factor().max(1);
        let width = self.window.allocated_width() * scale;
        let height = self.window.allocated_height() * scale;
        if width <= 0 || height <= 0 {
            return None;
        }
        let mut target = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).ok()?;
        target.set_device_scale(f64::from(scale), f64::from(scale));
        {
            let cr = cairo::Context::new(&target).ok()?;
            cr.set_operator(cairo::Operator::Source);
            cr.set_source_surface(&surface, 0., 0.).ok()?;
            cr.paint().ok()?;
        }
        target.flush();
        let (width, height) = (width as usize, height as usize);
        let stride = target.stride() as usize;
        let data = target.data().ok()?;
        // Cairo's ARGB32 is BGRA in memory on little-endian machines, which is what GPUI wants.
        let mut pixels = Vec::with_capacity(width * height * 4);
        for row in data.chunks(stride).take(height) {
            pixels.extend_from_slice(&row[..width * 4]);
        }
        let buffer = image::RgbaImage::from_raw(width as u32, height as u32, pixels)?;
        Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
    }

    /// The page's mouse cursor.
    pub(crate) fn cursor(&self) -> CursorStyle {
        self.shared.cursor.get()
    }

    /// The device size of the last captured frame.
    pub(crate) fn frame_size(&self) -> Option<gpui::Size<gpui::DevicePixels>> {
        self.frame.borrow().as_ref().map(|image| image.size(0))
    }

    /// Convert a window position to GTK pixels on the page.
    fn page_position(&self, position: Point<Pixels>) -> (f64, f64) {
        let local = position - self.origin.get();
        let ratio = self.ratio.get();
        (
            f64::from(f32::from(local.x) * ratio),
            f64::from(f32::from(local.y) * ratio),
        )
    }

    /// Convert GTK pixels on the page to a window position.
    pub(crate) fn window_position(&self, x: f32, y: f32) -> Point<Pixels> {
        let ratio = self.ratio.get();
        self.origin.get() + point(px(x / ratio), px(y / ratio))
    }

    pub(crate) fn mouse_down(
        &self,
        position: Point<Pixels>,
        button: MouseButton,
        modifiers: Modifiers,
    ) {
        let Some(button) = gdk_button(button) else {
            return;
        };
        let (x, y) = self.page_position(position);
        let state = modifier_state(modifiers) | self.buttons.get();
        self.buttons.set(self.buttons.get() | button_mask(button));
        // WebKit counts clicks itself from the presses' times and positions; GDK's extra
        // double-click press would count as one more.
        self.send_button(gdk::ffi::GDK_BUTTON_PRESS, button, x, y, state);
    }

    pub(crate) fn mouse_up(
        &self,
        position: Point<Pixels>,
        button: MouseButton,
        modifiers: Modifiers,
    ) {
        let Some(button) = gdk_button(button) else {
            return;
        };
        let mask = button_mask(button);
        if self.buttons.get() & mask == 0 {
            return;
        }
        let (x, y) = self.page_position(position);
        let state = modifier_state(modifiers) | self.buttons.get();
        self.buttons.set(self.buttons.get() & !mask);
        self.send_button(gdk::ffi::GDK_BUTTON_RELEASE, button, x, y, state);
    }

    /// Whether a button pressed on the page is still down, so moves outside it still drag.
    pub(crate) fn dragging(&self) -> bool {
        self.buttons.get() != 0
    }

    /// Forward a move; `hovered` is whether the pointer is over the visible page.
    pub(crate) fn mouse_move(&self, position: Point<Pixels>, hovered: bool, modifiers: Modifiers) {
        let (x, y) = self.page_position(position);
        let state = modifier_state(modifiers) | self.buttons.get();
        if hovered != self.hovered.get() && (hovered || !self.dragging()) {
            self.hovered.set(hovered);
            let kind = if hovered {
                gdk::ffi::GDK_ENTER_NOTIFY
            } else {
                gdk::ffi::GDK_LEAVE_NOTIFY
            };
            self.send_crossing(kind, x, y, state);
        }
        if hovered || self.dragging() {
            self.send(
                gdk::ffi::GDK_MOTION_NOTIFY,
                Device::Pointer,
                |event| unsafe {
                    let motion = &mut (*event).motion;
                    motion.x = x;
                    motion.y = y;
                    motion.x_root = x;
                    motion.y_root = y;
                    motion.state = state;
                },
            );
        }
    }

    /// The pointer left the page without a move over it, such as when the window lost it.
    pub(crate) fn mouse_exit(&self, position: Point<Pixels>, modifiers: Modifiers) {
        if self.hovered.replace(false) {
            let (x, y) = self.page_position(position);
            self.send_crossing(gdk::ffi::GDK_LEAVE_NOTIFY, x, y, modifier_state(modifiers));
        }
    }

    pub(crate) fn scroll(&self, position: Point<Pixels>, delta: ScrollDelta, modifiers: Modifiers) {
        // GPUI scrolls content down for a positive delta; GTK for a negative one.
        let (dx, dy) = match delta {
            ScrollDelta::Pixels(delta) => {
                let ratio = self.ratio.get() / PIXELS_PER_SCROLL_UNIT;
                (-f32::from(delta.x) * ratio, -f32::from(delta.y) * ratio)
            }
            ScrollDelta::Lines(delta) => (-delta.x / LINES_PER_NOTCH, -delta.y / LINES_PER_NOTCH),
        };
        let (x, y) = self.page_position(position);
        let state = modifier_state(modifiers) | self.buttons.get();
        self.send(gdk::ffi::GDK_SCROLL, Device::Pointer, |event| unsafe {
            let scroll = &mut (*event).scroll;
            scroll.x = x;
            scroll.y = y;
            scroll.x_root = x;
            scroll.y_root = y;
            scroll.state = state;
            scroll.direction = gdk::ffi::GDK_SCROLL_SMOOTH;
            scroll.delta_x = f64::from(dx);
            scroll.delta_y = f64::from(dy);
        });
    }

    /// Forward a key press or release. Returns false for a key GDK has no keyval for.
    pub(crate) fn key(&self, pressed: bool, keystroke: &Keystroke) -> bool {
        let Some(keyval) = keyval(keystroke) else {
            return false;
        };
        let state = modifier_state(keystroke.modifiers) | self.buttons.get();
        let kind = if pressed {
            gdk::ffi::GDK_KEY_PRESS
        } else {
            gdk::ffi::GDK_KEY_RELEASE
        };
        self.send(kind, Device::Keyboard, |event| unsafe {
            let key = &mut (*event).key;
            key.keyval = keyval;
            key.state = state;
            let keymap = gdk::ffi::gdk_keymap_get_for_display(gdk::ffi::gdk_display_get_default());
            let mut keys: *mut gdk::ffi::GdkKeymapKey = std::ptr::null_mut();
            let mut count = 0;
            if gdk::ffi::gdk_keymap_get_entries_for_keyval(keymap, keyval, &mut keys, &mut count)
                != 0
            {
                if count > 0 {
                    key.hardware_keycode = (*keys).keycode as u16;
                    key.group = (*keys).group as u8;
                }
                glib::ffi::g_free(keys.cast());
            }
        });
        true
    }

    /// Give the page keyboard focus, or take it away.
    pub(crate) fn set_focused(&self, focused: bool) {
        if self.focused.replace(focused) == focused {
            return;
        }
        if focused {
            self.window.set_focus(Some(&self.view));
        }
        let Some(target) = self.window.window() else {
            return;
        };
        send_event(
            &target,
            gdk::ffi::GDK_FOCUS_CHANGE,
            Device::Keyboard,
            |event| unsafe {
                (*event).focus_change.in_ = i16::from(focused);
            },
        );
    }

    /// Insert text at the page's caret, as typing it would.
    pub(crate) fn insert_text(&self, text: &str) {
        if !text.is_empty() {
            self.view
                .execute_editing_command_with_argument("InsertText", text);
        }
    }

    /// Put the page's selected text on GPUI's clipboard, deleting it with `cut`. Password
    /// fields are never copied.
    pub(crate) fn copy_selection(&self, webview: &wry::WebView, cut: bool, cx: &App) {
        const SELECTION: &str = "(() => { const e = document.activeElement; \
            if (e && (e.tagName === 'INPUT' || e.tagName === 'TEXTAREA') \
                && typeof e.selectionStart === 'number') \
              return e.type === 'password' ? '' : e.value.substring(e.selectionStart, e.selectionEnd); \
            return String(window.getSelection()); })()";
        let (tx, rx) = futures::channel::oneshot::channel::<String>();
        let tx = Mutex::new(Some(tx));
        let result = webview.evaluate_script_with_callback(SELECTION, move |json| {
            let text = serde_json::from_str::<String>(&json).unwrap_or_default();
            if let Some(tx) = tx.lock().ok().and_then(|mut tx| tx.take()) {
                let _ = tx.send(text);
            }
        });
        if let Err(error) = result {
            log::warn!("WebView copy failed: {error}");
            return;
        }
        let view = self.view.clone();
        cx.spawn(async move |cx| {
            let Ok(text) = rx.await else {
                return;
            };
            if text.is_empty() {
                return;
            }
            cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)));
            if cut {
                view.execute_editing_command("Delete");
            }
        })
        .detach();
    }

    /// The `<select>` menu the page wants shown, with its anchor as a window bounds.
    pub(crate) fn option_menu(&self) -> Option<(Vec<OptionItem>, Bounds<Pixels>)> {
        let menu = self.shared.option_menu.borrow();
        let menu = menu.as_ref()?;
        let ratio = self.ratio.get();
        let anchor = Bounds::new(
            self.window_position(menu.anchor.origin.x, menu.anchor.origin.y),
            size(
                px(menu.anchor.size.width / ratio),
                px(menu.anchor.size.height / ratio),
            ),
        );
        Some((menu.items.clone(), anchor))
    }

    /// Choose `index` in the open `<select>` menu, or dismiss it with `None`.
    pub(crate) fn close_option_menu(&self, index: Option<usize>) {
        let Some(menu) = self.shared.option_menu.take() else {
            return;
        };
        if let Some(index) = index {
            menu.menu.activate_item(index as u32);
        }
        menu.menu.close();
    }
}

/// A callback that refreshes `window` once per batch of GTK notifications. GTK signals can
/// fire while GPUI dispatches the event that caused them, so the refresh runs afterwards.
fn refresher(shared: Rc<Shared>, window: &Window, cx: &App) -> Rc<dyn Fn()> {
    let handle = window.window_handle();
    let cx = cx.to_async();
    Rc::new(move || {
        if shared.refresh_pending.replace(true) {
            return;
        }
        let shared = shared.clone();
        cx.spawn(async move |cx| {
            shared.refresh_pending.set(false);
            let _ = handle.update(cx, |_, window, _| window.refresh());
        })
        .detach();
    })
}

/// Follow the cursor WebKit sets on its GDK window. GDK hands out one cursor per name, so the
/// cursor is recognised by comparing it with the named cursors GPUI can show.
fn track_cursor(view: &webkit2gtk::WebView, shared: &Rc<Shared>, notify: &Rc<dyn Fn()>) {
    const NAMES: &[(&str, CursorStyle)] = &[
        ("text", CursorStyle::IBeam),
        ("pointer", CursorStyle::PointingHand),
        ("crosshair", CursorStyle::Crosshair),
        ("grab", CursorStyle::OpenHand),
        ("grabbing", CursorStyle::ClosedHand),
        ("col-resize", CursorStyle::ResizeColumn),
        ("row-resize", CursorStyle::ResizeRow),
        ("ew-resize", CursorStyle::ResizeLeftRight),
        ("ns-resize", CursorStyle::ResizeUpDown),
        ("e-resize", CursorStyle::ResizeRight),
        ("w-resize", CursorStyle::ResizeLeft),
        ("n-resize", CursorStyle::ResizeUp),
        ("s-resize", CursorStyle::ResizeDown),
        ("nwse-resize", CursorStyle::ResizeUpLeftDownRight),
        ("nesw-resize", CursorStyle::ResizeUpRightDownLeft),
        ("not-allowed", CursorStyle::OperationNotAllowed),
        ("copy", CursorStyle::DragCopy),
        ("alias", CursorStyle::DragLink),
        ("vertical-text", CursorStyle::IBeamCursorForVerticalLayout),
    ];
    let Some(gdk_window) = view.window() else {
        return;
    };
    let display = gdk_window.display();
    let known: Vec<(gdk::Cursor, CursorStyle)> = NAMES
        .iter()
        .filter_map(|(name, style)| Some((gdk::Cursor::from_name(&display, name)?, *style)))
        .collect();
    let shared = shared.clone();
    let notify = notify.clone();
    gdk_window.connect_notify_local(Some("cursor"), move |gdk_window, _| {
        let style = gdk_window
            .cursor()
            .and_then(|cursor| known.iter().find(|(c, _)| *c == cursor))
            .map_or(CursorStyle::Arrow, |(_, style)| *style);
        if shared.cursor.replace(style) != style {
            notify();
        }
    });
}

fn show_option_menu(
    shared: &Rc<Shared>,
    menu: &webkit2gtk::OptionMenu,
    rect: &gdk::Rectangle,
    notify: &Rc<dyn Fn()>,
) {
    let items = (0..menu.n_items())
        .filter_map(|i| menu.item(i))
        .map(|mut item| OptionItem {
            label: item.label().map(|l| l.to_string()).unwrap_or_default(),
            enabled: item.is_enabled(),
            group_label: item.is_group_label(),
            group_child: item.is_group_child(),
            selected: item.is_selected(),
        })
        .collect();
    let anchor = Bounds::new(
        point(rect.x() as f32, rect.y() as f32),
        size(rect.width() as f32, rect.height() as f32),
    );
    if let Some(old) = shared.option_menu.replace(Some(OptionMenu {
        menu: menu.clone(),
        anchor,
        items,
    })) {
        old.menu.close();
    }
    // The page closes the menu itself when the `<select>` goes away.
    let weak = Rc::downgrade(shared);
    let notify_close = notify.clone();
    menu.connect_close(move |menu| {
        if let Some(shared) = weak.upgrade() {
            let mut current = shared.option_menu.borrow_mut();
            if current.as_ref().is_some_and(|m| &m.menu == menu) {
                *current = None;
                drop(current);
                notify_close();
            }
        }
    });
    notify();
}

/// Ask GPUI for the files the page's `<input type=file>` wants.
fn choose_files(request: webkit2gtk::FileChooserRequest, cx: &AsyncApp) {
    let multiple = request.selects_multiple();
    cx.spawn(async move |cx| {
        let paths = cx.update(|cx| {
            cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple,
                prompt: None,
            })
        });
        match paths.await {
            Ok(Ok(Some(paths))) if !paths.is_empty() => {
                let paths: Vec<String> = paths
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
                request.select_files(&paths);
            }
            _ => request.cancel(),
        }
    })
    .detach();
}

#[derive(Clone, Copy)]
enum Device {
    Pointer,
    Keyboard,
}

impl OffscreenView {
    fn send(
        &self,
        kind: gdk::ffi::GdkEventType,
        device: Device,
        fill: impl FnOnce(*mut gdk::ffi::GdkEvent),
    ) {
        if let Some(target) = self.view.window() {
            send_event(&target, kind, device, fill);
        }
    }

    fn send_button(&self, kind: gdk::ffi::GdkEventType, button: u32, x: f64, y: f64, state: u32) {
        self.send(kind, Device::Pointer, |event| unsafe {
            let press = &mut (*event).button;
            press.x = x;
            press.y = y;
            press.x_root = x;
            press.y_root = y;
            press.button = button;
            press.state = state;
        });
    }

    fn send_crossing(&self, kind: gdk::ffi::GdkEventType, x: f64, y: f64, state: u32) {
        self.send(kind, Device::Pointer, |event| unsafe {
            let crossing = &mut (*event).crossing;
            crossing.x = x;
            crossing.y = y;
            crossing.x_root = x;
            crossing.y_root = y;
            crossing.state = state;
            crossing.mode = gdk::ffi::GDK_CROSSING_NORMAL;
            crossing.detail = gdk::ffi::GDK_NOTIFY_NONLINEAR;
        });
    }
}

/// Synthesize a GDK event of `kind` on `target` and dispatch it through GTK. `fill` sets the
/// fields of the event's variant; the target, time and device are set here.
fn send_event(
    target: &gdk::Window,
    kind: gdk::ffi::GdkEventType,
    device: Device,
    fill: impl FnOnce(*mut gdk::ffi::GdkEvent),
) {
    use glib::translate::ToGlibPtr as _;

    // SAFETY: The event is a fresh GDK event of `kind`; `fill` writes only that variant's
    // plain fields. GDK takes the window reference and frees it with the event.
    unsafe {
        let event = gdk::ffi::gdk_event_new(kind);
        let any = &mut (*event).any;
        any.window = target.to_glib_full();
        any.send_event = 1;
        let time = (glib::monotonic_time() / 1000) as u32;
        match kind {
            gdk::ffi::GDK_KEY_PRESS | gdk::ffi::GDK_KEY_RELEASE => (*event).key.time = time,
            gdk::ffi::GDK_MOTION_NOTIFY => (*event).motion.time = time,
            gdk::ffi::GDK_SCROLL => (*event).scroll.time = time,
            gdk::ffi::GDK_ENTER_NOTIFY | gdk::ffi::GDK_LEAVE_NOTIFY => {
                (*event).crossing.time = time
            }
            gdk::ffi::GDK_FOCUS_CHANGE => {}
            _ => (*event).button.time = time,
        }
        fill(event);
        let seat = gdk::ffi::gdk_display_get_default_seat(gdk::ffi::gdk_window_get_display(
            target.to_glib_none().0,
        ));
        if !seat.is_null() {
            let device = match device {
                Device::Pointer => gdk::ffi::gdk_seat_get_pointer(seat),
                Device::Keyboard => gdk::ffi::gdk_seat_get_keyboard(seat),
            };
            if !device.is_null() {
                gdk::ffi::gdk_event_set_device(event, device);
                // WebKit reads the source device to tell a wheel from a touchpad.
                gdk::ffi::gdk_event_set_source_device(event, device);
            }
        }
        gtk::ffi::gtk_main_do_event(event);
        gdk::ffi::gdk_event_free(event);
    }
}

fn gdk_button(button: MouseButton) -> Option<u32> {
    Some(match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
        MouseButton::Navigate(NavigationDirection::Back) => 8,
        MouseButton::Navigate(NavigationDirection::Forward) => 9,
    })
}

fn button_mask(button: u32) -> u32 {
    match button {
        1 => gdk::ffi::GDK_BUTTON1_MASK,
        2 => gdk::ffi::GDK_BUTTON2_MASK,
        3 => gdk::ffi::GDK_BUTTON3_MASK,
        8 => gdk::ffi::GDK_BUTTON4_MASK,
        9 => gdk::ffi::GDK_BUTTON5_MASK,
        _ => 0,
    }
}

fn modifier_state(modifiers: Modifiers) -> u32 {
    let mut state = 0;
    if modifiers.shift {
        state |= gdk::ffi::GDK_SHIFT_MASK;
    }
    if modifiers.control {
        state |= gdk::ffi::GDK_CONTROL_MASK;
    }
    if modifiers.alt {
        state |= gdk::ffi::GDK_MOD1_MASK;
    }
    if modifiers.platform {
        state |= gdk::ffi::GDK_SUPER_MASK;
    }
    state
}

/// The GDK keyval of a GPUI keystroke.
pub(crate) fn keyval(keystroke: &Keystroke) -> Option<u32> {
    let named = match keystroke.key.as_str() {
        "backspace" => 0xff08,
        "tab" if keystroke.modifiers.shift => 0xfe20,
        "tab" => 0xff09,
        "enter" => 0xff0d,
        "escape" => 0xff1b,
        "delete" => 0xffff,
        "home" => 0xff50,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        "end" => 0xff57,
        "insert" => 0xff63,
        "menu" => 0xff67,
        "space" => 0x20,
        key => match key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
            Some(n @ 1..=35) => 0xffbe + n - 1,
            _ => 0,
        },
    };
    if named != 0 {
        return Some(named);
    }
    // Typed text first: it already reflects Shift and the keyboard layout.
    let text = keystroke
        .key_char
        .as_deref()
        .filter(|c| c.chars().count() == 1)
        .unwrap_or(&keystroke.key);
    let mut chars = text.chars();
    let (Some(mut c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    if keystroke.modifiers.shift {
        c = c.to_uppercase().next().unwrap_or(c);
    }
    // SAFETY: A pure lookup.
    let keyval = unsafe { gdk::ffi::gdk_unicode_to_keyval(c as u32) };
    (keyval != 0).then_some(keyval)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            key: key.into(),
            key_char: key_char.map(Into::into),
            modifiers,
        }
    }

    #[test]
    fn keyvals() {
        let none = Modifiers::default();
        let shift = Modifiers::shift();
        assert_eq!(keyval(&stroke("enter", None, none)), Some(0xff0d));
        assert_eq!(keyval(&stroke("tab", None, shift)), Some(0xfe20));
        assert_eq!(keyval(&stroke("f5", None, none)), Some(0xffc2));
        assert_eq!(keyval(&stroke("a", Some("a"), none)), Some('a' as u32));
        assert_eq!(keyval(&stroke("a", Some("A"), shift)), Some('A' as u32));
        assert_eq!(
            keyval(&stroke("c", None, Modifiers::control())),
            Some('c' as u32)
        );
        assert_eq!(keyval(&stroke("2", Some("@"), shift)), Some('@' as u32));
        assert_eq!(keyval(&stroke("unknownkey", None, none)), None);
    }
}
