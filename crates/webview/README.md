# GPUI WebView (vendored)

A WebView for GPUI, based on [Wry](https://github.com/tauri-apps/wry).

Vendored from `gpui-webview` 0.8.0 (longbridge/gpui-kit, commit `1676826d`,
`crates/webview`), Apache-2.0: see `LICENSE-APACHE`. The changes below are Ember's.

| Platform | Engine | How the page is shown |
| --- | --- | --- |
| macOS | WKWebView | Native view in the window (overlays above it with `gpui-fast`) |
| Windows | WebView2 | Native view in the window; it covers overlays |
| Linux, X11 / XWayland | WebKitGTK | Native child window (overlays above it with `gpui-fast`) |
| Linux, native Wayland | WebKitGTK | Rendered offscreen and painted by GPUI |

Call `gpui_webview::prepare_process()` first thing in `main`, then build a WebView
with `WebView::build(builder, window, cx)`.

## Changes from upstream

- **Native Wayland.** Upstream refuses Wayland windows. Here the page renders into
  an offscreen GTK window (`src/offscreen.rs`). Each repaint is copied into a GPUI
  image, and GPUI's mouse, scroll, keyboard and input-method events are forwarded
  to WebKit as GDK events.
  - Because GPUI paints the page, its overlays render above it and scrolling
    containers clip it, without `gpui-fast`.
  - `<select>` menus open as GPUI popups, and file inputs use GPUI's file picker.
  - Copy, cut and paste go through GPUI's clipboard. The cursor follows the page.
  - The offscreen window is a `GtkWindow` subclass (`src/toplevel.rs`), not a
    `GtkOffscreenWindow`. WebKitGTK only treats a page as "in a window" for the
    former, and outside a window it holds back media and image animation, among
    other things.
  - GTK runs on XWayland when it can. Its own Wayland connection never has keyboard
    focus, so the compositor rejects its primary-selection claims. When a claim
    fails, WebKit collapses the page's text selection.
  - WebKit's DMA-BUF renderer needs a GL context that GTK offscreen windows on
    Wayland cannot create. `prepare_process()` turns it off through
    `WEBKIT_DISABLE_DMABUF_RENDERER`.
- **Keyboard focus (upstream #1787).** When GPUI focus moves to another element,
  the native view now gives up the keyboard, so GPUI inputs receive typing again.
  On X11, a click on the page also focuses the WebView in GPUI.
- **GTK main loop.** It is driven only while a WebView is alive. Upstream drove it
  every 16 ms forever after the first WebView.
- **Native visibility.** It is applied only when it changes, rather than on every
  frame.
- **Startup notification.** GTK no longer reports the application's startup as
  complete on behalf of its hidden windows.

## Limits on native Wayland

- **Not supported:** drag and drop, the page's context menu (GTK's has nowhere to
  appear), and `title` tooltips.
- **Input methods:** committed text is inserted. Text still being composed is not
  shown in the page.
- **Copy:** works from the focused text field or the main document's selection,
  but not from inside iframes.
- **Without XWayland:** selecting text with the mouse doesn't stick (see above),
  and pages can't write to the system clipboard themselves.
- **Upstream Wry bug:** IPC messages panic when posted from a page whose address
  isn't a valid HTTP URI, such as a `data:` URL. Use `with_html` or `http(s)` URLs.

## Checking it

`cargo run -p gpui-webview --example offscreen_check` opens a window with a test
page and drives it through GPUI input: clicking, typing, paste, input-method text,
double-click selection, the link cursor, `<select>` menus, right click and wheel
scrolling. It prints PASS or FAIL for each check. Run it in a Wayland session for
the offscreen path, or with `WAYLAND_DISPLAY` unset for the X11 path.
