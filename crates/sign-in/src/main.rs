//! `ember-sign-in`: a store's sign-in page in a window of its own.
//!
//! Ember runs this as a separate process so its own window never embeds a web
//! engine. The page is watched for what the store's login returns: every address
//! it visits and the text of every page it loads are matched against the
//! `--capture` pattern. The first match is printed to stdout as one JSON line,
//! `{"text": "..."}`, and the window closes. Closing the window without a match
//! exits with status 2.
//!
//! Usage: ember-sign-in --url <url> --capture <regex> [--title <window title>]

use std::io::Write as _;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use regex::Regex;
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::{PageLoadEvent, WebViewBuilder};

/// Reads the visible text of the page.
const PAGE_TEXT: &str = "document.body ? document.body.innerText : ''";

/// How often the page's text is read again, for pages that build their content
/// after loading.
const RECHECK: Duration = Duration::from_secs(1);

/// Exit status when the window was closed without signing in.
const CANCELLED: u8 = 2;

/// What the page reported to the event loop.
#[derive(Debug)]
enum PageEvent {
    /// The page finished loading; its text is read next.
    Loaded,
    /// Text or an address that matched the capture pattern.
    Matched(String),
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    url: String,
    capture: String,
    title: String,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args> {
    let (mut url, mut capture, mut title) = (None, None, None);
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let value = |it: &mut dyn Iterator<Item = String>| {
            it.next().with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--url" => url = Some(value(&mut it)?),
            "--capture" => capture = Some(value(&mut it)?),
            "--title" => title = Some(value(&mut it)?),
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(Args {
        url: url.context("--url is required")?,
        capture: capture.context("--capture is required")?,
        title: title.unwrap_or_else(|| "Sign in".into()),
    })
}

/// Whether `text` holds what the store's login returns: the pattern's first
/// group matches something non-empty.
fn matches(re: &Regex, text: &str) -> bool {
    re.captures(text)
        .and_then(|c| c.get(1))
        .is_some_and(|m| !m.as_str().trim().is_empty())
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("ember-sign-in: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ember-sign-in: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<()> {
    let re = Regex::new(&args.capture).context("invalid --capture pattern")?;
    let event_loop = EventLoopBuilder::<PageEvent>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title(&args.title)
        .with_inner_size(LogicalSize::new(520., 760.))
        .with_min_inner_size(LogicalSize::new(380., 480.))
        .build(&event_loop)
        .context("opening the sign-in window")?;

    let navigated = event_loop.create_proxy();
    let loaded = event_loop.create_proxy();
    let nav_re = re.clone();
    let builder = WebViewBuilder::new()
        .with_url(&args.url)
        // A fresh session each time: nothing from an earlier sign-in is kept.
        .with_incognito(true)
        .with_navigation_handler(move |address| {
            if matches(&nav_re, &address) {
                let _ = navigated.send_event(PageEvent::Matched(address));
            }
            true
        })
        .with_on_page_load_handler(move |event, address| {
            if matches!(event, PageLoadEvent::Finished) {
                let _ = loaded.send_event(PageEvent::Matched(address));
                let _ = loaded.send_event(PageEvent::Loaded);
            }
        })
        .with_new_window_req_handler(popups::handler());

    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix as _;
        use wry::WebViewBuilderExtUnix as _;
        let vbox = window
            .default_vbox()
            .context("the sign-in window has no container")?;
        builder.build_gtk(vbox)?
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window)?;

    let proxy = event_loop.create_proxy();
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + RECHECK);
        match event {
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                read_page_text(&webview, re.clone(), proxy.clone());
            }
            Event::UserEvent(PageEvent::Loaded) => {
                read_page_text(&webview, re.clone(), proxy.clone());
            }
            Event::UserEvent(PageEvent::Matched(text)) => {
                if matches(&re, &text) {
                    let line = serde_json::json!({ "text": text }).to_string();
                    let mut out = std::io::stdout().lock();
                    let _ = writeln!(out, "{line}");
                    let _ = out.flush();
                    std::process::exit(0);
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => std::process::exit(i32::from(CANCELLED)),
            _ => {}
        }
    })
}

/// Read the loaded page's text and report it when it matches.
fn read_page_text(webview: &wry::WebView, re: Regex, proxy: EventLoopProxy<PageEvent>) {
    let result = webview.evaluate_script_with_callback(PAGE_TEXT, move |json| {
        let text = serde_json::from_str::<String>(&json).unwrap_or_default();
        if matches(&re, &text) {
            let _ = proxy.send_event(PageEvent::Matched(text));
        }
    });
    if let Err(e) = result {
        eprintln!("ember-sign-in: reading the page failed: {e}");
    }
}

/// Pages the sign-in page opens as new windows, such as "Sign in with Google".
mod popups {
    use wry::{NewWindowFeatures, NewWindowResponse};

    /// On Linux each pop-up is a window of its own, related to its opener so the
    /// two can talk, and it closes when the page closes it (`window.close()`).
    #[cfg(target_os = "linux")]
    pub fn handler() -> impl Fn(String, NewWindowFeatures) -> NewWindowResponse + Send + Sync {
        |url, features| {
            use gtk::prelude::*;
            use webkit2gtk::WebViewExt as _;

            let view = webkit2gtk::WebView::with_related_view(&features.opener.webview);
            let window = gtk::Window::new(gtk::WindowType::Toplevel);
            window.set_title(&url);
            window.set_default_size(500, 680);
            window.add(&view);
            let closing = window.clone();
            view.connect_close(move |_| {
                // SAFETY: The window only hosts this page, which asked to close.
                unsafe { closing.destroy() };
            });
            let shown = window.clone();
            view.connect_ready_to_show(move |_| shown.show_all());
            NewWindowResponse::Create { webview: view }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn handler() -> impl Fn(String, NewWindowFeatures) -> NewWindowResponse + Send + Sync {
        |_, _| NewWindowResponse::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args> {
        parse_args(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn arguments() {
        let parsed = args(&[
            "--url",
            "https://x",
            "--capture",
            "c=(\\w+)",
            "--title",
            "T",
        ])
        .unwrap();
        assert_eq!(parsed.url, "https://x");
        assert_eq!(parsed.title, "T");
        assert_eq!(
            args(&["--url", "https://x", "--capture", "c"])
                .unwrap()
                .title,
            "Sign in"
        );
        assert!(args(&["--url", "https://x"]).is_err());
        assert!(args(&["--url"]).is_err());
        assert!(args(&["--bogus"]).is_err());
    }

    #[test]
    fn matching_needs_a_non_empty_group() {
        let re = Regex::new(r#""authorizationCode"\s*:\s*"([0-9a-f]*)""#).unwrap();
        assert!(matches(&re, r#"{"authorizationCode": "0123abcd"}"#));
        assert!(!matches(&re, r#"{"authorizationCode": ""}"#));
        assert!(!matches(&re, "nothing here"));
    }
}
