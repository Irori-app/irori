//! End-to-end check of the WebView on the session's windowing system: opens a window with
//! a test page, drives it through GPUI input (as a user would), reads the page back with
//! JavaScript and prints one PASS/FAIL line per check. Exits non-zero on any failure.
//!
//! cargo run -p gpui-webview --example offscreen_check

use std::time::Duration;

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, Bounds, ClipboardItem, Context, Entity,
    EntityInputHandler, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, PlatformInput, Point, Render,
    ScrollDelta, ScrollWheelEvent, Styled as _, TouchPhase, Window, WindowBounds, WindowOptions,
    div, point, px, size,
};
use std::cell::Cell;

use gpui_kit::Subscription;
use gpui_webview::{WebView, WebViewEvent};

const PAGE: &str = r##"<!doctype html>
<html><body style="margin:0;font:16px sans-serif;height:3000px">
<input id="name" style="position:absolute;left:40px;top:40px;width:300px;height:30px">
<select id="pick" style="position:absolute;left:40px;top:100px;width:200px;height:30px">
  <option>Alpha</option><option>Beta</option><option>Gamma</option>
</select>
<a id="link" href="#x" style="position:absolute;left:40px;top:160px;font-size:24px">a link</a>
<p id="words" style="position:absolute;left:40px;top:220px;font-size:24px">double clickable</p>
<button id="open" style="position:absolute;left:40px;top:300px;height:30px">open pop-up</button>
<script>
  window.got = [];
  addEventListener('message', e => got.push(e.data));
  document.getElementById('open').onclick = () => {
    const w = open('', 'pop', 'width=400,height=300');
    w.document.write('<script>opener.postMessage("hello", "*"); setTimeout(() => close(), 300)<\/script>');
    w.document.close();
  };
</script>
</body></html>"##;

struct Harness {
    webview: Entity<WebView>,
    popups: Vec<Entity<WebView>>,
    _subscriptions: Vec<Subscription>,
}

thread_local! {
    static POPUPS_OPENED: Cell<u32> = const { Cell::new(0) };
    static POPUPS_CLOSED: Cell<u32> = const { Cell::new(0) };
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.webview.clone())
    }
}

fn main() {
    // SAFETY: Nothing else runs yet.
    unsafe { gpui_webview::prepare_process() };
    gpui_kit::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(800.), px(600.)), cx);
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    let webview = cx.new(|cx| {
                        let builder = wry::WebViewBuilder::new().with_html(PAGE);
                        WebView::build(builder, window, cx).expect("build webview")
                    });
                    webview.read(cx).set_popups_enabled(std::env::var_os("NOPOP").is_none());
                    cx.new(|cx| {
                        let subscription = cx.subscribe(&webview, |this: &mut Harness, _, event, cx| {
                            if let WebViewEvent::Popup(popup) = event {
                                POPUPS_OPENED.with(|n| n.set(n.get() + 1));
                                if std::env::var_os("KEEP").is_some() {
                                    this.popups.push(popup.clone());
                                }
                                this._subscriptions.push(cx.subscribe(popup, |_, _, event, _| {
                                    if let WebViewEvent::Close = event {
                                        POPUPS_CLOSED.with(|n| n.set(n.get() + 1));
                                    }
                                }));
                            }
                        });
                        Harness {
                            webview,
                            popups: Vec::new(),
                            _subscriptions: vec![subscription],
                        }
                    })
                },
            )
            .expect("open window");
        let webview = handle
            .update(cx, |harness, _, _| harness.webview.clone())
            .unwrap();
        let window: AnyWindowHandle = handle.into();
        cx.spawn(async move |cx| {
            let failures = run(window, webview, cx).await;
            println!("{failures} failure(s)");
            cx.update(|cx| cx.quit());
            std::process::exit(i32::from(failures > 0));
        })
        .detach();
    });
}

async fn sleep(cx: &AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// Evaluate `script` in the page and return its JSON result.
async fn js(webview: &Entity<WebView>, script: &str, cx: &mut AsyncApp) -> serde_json::Value {
    let (tx, rx) = futures::channel::oneshot::channel();
    let tx = std::sync::Mutex::new(Some(tx));
    cx.update(|cx| {
        webview
            .read(cx)
            .evaluate_script_with_callback(script, move |result| {
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(result);
                }
            })
            .expect("evaluate");
    });
    let result = rx.await.unwrap_or_default();
    serde_json::from_str(&result).unwrap_or(serde_json::Value::Null)
}

/// The window position of the centre of the element `id`.
async fn center(webview: &Entity<WebView>, id: &str, cx: &mut AsyncApp) -> Point<gpui_kit::Pixels> {
    let rect = js(
        webview,
        &format!(
            "(() => {{ const r = document.getElementById('{id}').getBoundingClientRect(); \
             return [r.left + r.width / 2, r.top + r.height / 2]; }})()"
        ),
        cx,
    )
    .await;
    let origin = cx.update(|cx| webview.read(cx).bounds().origin);
    let x = rect[0].as_f64().unwrap_or_default() as f32;
    let y = rect[1].as_f64().unwrap_or_default() as f32;
    origin + point(px(x), px(y))
}

fn dispatch(window: AnyWindowHandle, input: PlatformInput, cx: &mut AsyncApp) {
    let _ = window.update(cx, |_, window, cx| {
        window.dispatch_event(input, cx);
    });
}

fn click(
    window: AnyWindowHandle,
    position: Point<gpui_kit::Pixels>,
    count: usize,
    cx: &mut AsyncApp,
) {
    dispatch(
        window,
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::default(),
        }),
        cx,
    );
    for click_count in 1..=count {
        dispatch(
            window,
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count,
                first_mouse: false,
            }),
            cx,
        );
        dispatch(
            window,
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count,
            }),
            cx,
        );
    }
}

fn press(window: AnyWindowHandle, keystroke: &str, cx: &mut AsyncApp) {
    let mut keystroke = Keystroke::parse(keystroke).expect("keystroke");
    if keystroke.key.chars().count() == 1 && keystroke.key_char.is_none() {
        let c = if keystroke.modifiers.shift {
            keystroke.key.to_uppercase()
        } else {
            keystroke.key.clone()
        };
        if !keystroke.modifiers.control {
            keystroke.key_char = Some(c);
        }
    }
    dispatch(
        window,
        PlatformInput::KeyDown(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        }),
        cx,
    );
    dispatch(window, PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
}

fn check(failures: &mut u32, name: &str, ok: bool, detail: impl std::fmt::Debug) {
    println!("{} {name}: {detail:?}", if ok { "PASS" } else { "FAIL" });
    if !ok {
        *failures += 1;
    }
}

async fn run(window: AnyWindowHandle, webview: Entity<WebView>, cx: &mut AsyncApp) -> u32 {
    let mut failures = 0;
    sleep(cx, 2500).await;

    let (offscreen, debug) = cx.update(|cx| {
        let view = webview.read(cx);
        (view.is_offscreen(), view.offscreen_debug())
    });
    println!("offscreen: {offscreen}");
    if !offscreen {
        // A native child window gets input from the windowing system, not from GPUI, so
        // only loading and scripting can be checked here.
        let loaded = js(
            &webview,
            "document.getElementById('pick').options.length",
            cx,
        )
        .await;
        check(&mut failures, "native page loads", loaded == 3, &loaded);
        return failures;
    }
    {
        let frame = debug.as_ref().and_then(|d| d.frame_size);
        check(&mut failures, "frame captured", frame.is_some(), frame);
    }

    if std::env::var_os("ONLYPOP").is_some() {
        popup_check(&mut failures, window, &webview, cx).await;
        return failures;
    }

    // Click into the input and type.
    let name = center(&webview, "name", cx).await;
    click(window, name, 1, cx);
    sleep(cx, 200).await;
    let active = js(&webview, "document.activeElement.id", cx).await;
    check(
        &mut failures,
        "click focuses input",
        active == "name",
        &active,
    );
    for key in [
        "shift-h",
        "i",
        "space",
        "t",
        "h",
        "e",
        "r",
        "e",
        "backspace",
    ] {
        press(window, key, cx);
    }
    sleep(cx, 300).await;
    let value = js(&webview, "document.getElementById('name').value", cx).await;
    check(&mut failures, "typing", value == "Hi ther", &value);

    // Paste from GPUI's clipboard, then copy everything back out.
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(" pasted".into())));
    sleep(cx, 200).await;
    let clipboard = cx
        .update(|cx| cx.read_from_clipboard().and_then(|c| c.text()))
        .unwrap_or_default();
    // Wayland only lets a focused window set the clipboard; then the paste is whatever is there.
    let writable = clipboard == " pasted";
    press(window, "ctrl-v", cx);
    sleep(cx, 300).await;
    let value = js(&webview, "document.getElementById('name').value", cx).await;
    let expected = format!("Hi ther{clipboard}");
    check(
        &mut failures,
        "paste",
        value.as_str() == Some(expected.as_str()),
        format!("{} chars", value.as_str().map_or(0, |v| v.chars().count())),
    );
    let before = value.as_str().unwrap_or_default().to_string();

    // Input method commits arrive through the input handler.
    let _ = window.update(cx, |_, window, cx| {
        webview.update(cx, |webview, cx| {
            EntityInputHandler::replace_text_in_range(webview, None, "ü", window, cx)
        })
    });
    sleep(cx, 300).await;
    let value = js(&webview, "document.getElementById('name').value", cx).await;
    let expected = format!("{before}ü");
    check(
        &mut failures,
        "input method commit",
        value.as_str() == Some(expected.as_str()),
        value.as_str().map(|v| v.ends_with('ü')),
    );
    press(window, "ctrl-a", cx);
    press(window, "ctrl-c", cx);
    sleep(cx, 500).await;
    let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|c| c.text()));
    if writable {
        check(
            &mut failures,
            "copy",
            copied.as_deref() == Some(expected.as_str()),
            copied.map(|c| c.chars().count()),
        );
    } else {
        println!("SKIP copy: this window may not set the clipboard (not focused)");
    }

    // Double click selects a word.
    let words = center(&webview, "words", cx).await;
    click(window, words, 2, cx);
    sleep(cx, 300).await;
    let selection = js(&webview, "String(window.getSelection())", cx).await;
    check(
        &mut failures,
        "double click selects a word",
        selection
            .as_str()
            .is_some_and(|s| !s.trim().is_empty() && !s.contains(' ')),
        &selection,
    );

    // Hovering a link shows the pointing hand.
    if offscreen {
        let link = center(&webview, "link", cx).await;
        dispatch(
            window,
            PlatformInput::MouseMove(MouseMoveEvent {
                position: link,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
            cx,
        );
        sleep(cx, 300).await;
        let cursor = cx.update(|cx| webview.read(cx).offscreen_debug().map(|d| d.cursor));
        check(
            &mut failures,
            "link cursor",
            cursor == Some(gpui_kit::CursorStyle::PointingHand),
            cursor,
        );
    }

    // A <select> opens a GPUI menu; choosing from it changes the value.
    if offscreen {
        let pick = center(&webview, "pick", cx).await;
        click(window, pick, 1, cx);
        sleep(cx, 400).await;
        let labels = cx.update(|cx| {
            webview
                .read(cx)
                .offscreen_debug()
                .and_then(|d| d.option_menu)
        });
        check(
            &mut failures,
            "select menu opens",
            labels.as_ref().is_some_and(|l| l.len() == 3),
            &labels,
        );
        press(window, "escape", cx);
        sleep(cx, 200).await;
        let labels = cx.update(|cx| {
            webview
                .read(cx)
                .offscreen_debug()
                .and_then(|d| d.option_menu)
        });
        check(
            &mut failures,
            "escape closes the menu",
            labels.is_none(),
            &labels,
        );

        // Open it again and click the second item, which GPUI draws below the <select>.
        click(window, pick, 1, cx);
        sleep(cx, 400).await;
        let bottom = js(
            &webview,
            "document.getElementById('pick').getBoundingClientRect().bottom",
            cx,
        )
        .await
        .as_f64()
        .unwrap_or_default() as f32;
        let origin = cx.update(|cx| webview.read(cx).bounds().origin);
        click(
            window,
            origin + point(pick.x - origin.x, px(bottom + 47.)),
            1,
            cx,
        );
        sleep(cx, 400).await;
        let value = js(&webview, "document.getElementById('pick').value", cx).await;
        check(
            &mut failures,
            "choosing from the menu",
            value == "Beta",
            &value,
        );
    }

    // A right click is swallowed (GTK's menu has nowhere to appear) and leaves the page usable.
    if std::env::var_os("SKIPRC").is_none() {
    dispatch(
        window,
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Right,
            position: name,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    );
    dispatch(
        window,
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Right,
            position: name,
            modifiers: Modifiers::default(),
            click_count: 1,
        }),
        cx,
    );
    sleep(cx, 300).await;
    let alive = js(&webview, "1 + 1", cx).await;
    check(&mut failures, "right click", alive == 2, &alive);
    }

    popup_check(&mut failures, window, &webview, cx).await;

    // Scrolling the wheel down scrolls the page down.
    let position = center(&webview, "words", cx).await;
    dispatch(
        window,
        PlatformInput::ScrollWheel(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Lines(point(0., -3.)),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        }),
        cx,
    );
    sleep(cx, 600).await;
    let scrolled = js(&webview, "window.scrollY", cx).await;
    check(
        &mut failures,
        "wheel scrolls down",
        scrolled.as_f64().is_some_and(|y| y > 0.),
        &scrolled,
    );

    failures
}

async fn popup_check(
    failures: &mut u32,
    window: AnyWindowHandle,
    webview: &Entity<WebView>,
    cx: &mut AsyncApp,
) {
    // A pop-up opens as a WebView of its own, can message its opener, and closes itself.
    let open = center(webview, "open", cx).await;
    click(window, open, 1, cx);
    sleep(cx, 1200).await;
    let got = js(webview, "JSON.stringify(window.got)", cx).await;
    let (opened, closed) = (POPUPS_OPENED.with(Cell::get), POPUPS_CLOSED.with(Cell::get));
    check(failures, "pop-up opens", opened == 1, opened);
    check(failures, "pop-up messages its opener", got == "[\"hello\"]", &got);
    check(failures, "pop-up closes itself", closed == 1, closed);

}
