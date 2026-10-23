//! Store login.
//!
//! The store's sign-in page opens in Ember's sign-in window (`window`), a
//! helper process that reports what the login returned, so signing in needs
//! nothing copied. Without the helper, or on request, the page opens in the
//! default browser instead: the dialog shows the store's (English)
//! instructions on what to copy, accepts the pasted code or address, and
//! watches the clipboard, signing in as soon as the user copies something
//! matching the store's capture pattern.

mod capture;
mod window;

use std::time::Duration;

use ember_core::{DynStore, LoginFlow, Runner};
use ember_ui::backend;
use ember_ui::i18n::{t, t_args};
use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, Focusable as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window, div,
};
use regex::Regex;

use capture::{ClipboardWatcher, compile_capture};
use ember_core::process::CancellationToken;
use window::WindowOutcome;

use crate::common::notify_success;

/// How often the clipboard is checked while the dialog is open.
const CLIPBOARD_POLL: Duration = Duration::from_millis(700);

/// Open the login dialog for `runner`. On success the dialog closes, a
/// notification names the account, and the library refreshes.
pub fn open_login_dialog(runner: Runner, window: &mut Window, cx: &mut App) {
    crate::i18n::register();
    let dialog = cx.new(|cx| LoginDialog::new(runner, window, cx));
    let focus = dialog.read(cx).input.focus_handle(cx);
    focus.focus(window, cx);
    let width = window.rem_size() * 34.;
    window.open_dialog(cx, move |d, _, _| {
        let on_close = dialog.clone();
        d.title(t_args(
            "manage.login.title",
            &[("store", runner.store_name())],
        ))
        .width(width)
        .child(dialog.clone())
        .on_close(move |_, _, cx| on_close.update(cx, |d, _| d.close()))
    });
}

enum FlowState {
    Loading,
    Ready {
        url: String,
        instructions: String,
        capture: Option<Regex>,
    },
    /// Sign-in cannot start; `title` (when set) heads the message.
    Unavailable {
        title: Option<SharedString>,
        message: SharedString,
    },
}

impl FlowState {
    fn unavailable(message: impl Into<SharedString>) -> Self {
        FlowState::Unavailable {
            title: None,
            message: message.into(),
        }
    }
}

/// The sign-in window's part of the dialog.
enum SignInWindow {
    /// Not used: the browser flow is shown.
    None,
    /// Open; cancelling the token closes it.
    Open {
        cancel: CancellationToken,
        _task: Task<()>,
    },
    /// The user closed it before signing in.
    Closed,
}

/// Content of the login dialog. Owns the sign-in window or the clipboard watch,
/// which end when the dialog closes.
pub(crate) struct LoginDialog {
    runner: Runner,
    store: Option<DynStore>,
    flow: FlowState,
    input: Entity<InputState>,
    window: SignInWindow,
    watcher: ClipboardWatcher,
    submitting: bool,
    error: Option<SharedString>,
    poll: Option<Task<()>>,
    _load: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LoginDialog {
    fn new(runner: Runner, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx
            .new(|cx| InputState::new(window, cx).placeholder(t("manage.login.input_placeholder")));
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit_input(window, cx),
                InputEvent::Change => {
                    this.error = None;
                    cx.notify();
                }
                _ => {}
            }),
        ];
        let initial = cx.read_from_clipboard().and_then(|c| c.text());
        let store = backend(cx).store(runner);
        let mut this = Self {
            runner,
            store: store.clone(),
            flow: FlowState::Loading,
            input,
            window: SignInWindow::None,
            watcher: ClipboardWatcher::new(initial.as_deref()),
            submitting: false,
            error: None,
            poll: None,
            _load: None,
            _subscriptions: subscriptions,
        };
        match store {
            None => {
                this.flow = FlowState::unavailable(t_args(
                    "manage.login.not_in_build",
                    &[("store", runner.store_name())],
                ));
            }
            Some(store) => {
                // `login_flow` may do blocking work (Amazon generates its sign-in
                // request), so it runs on the backend's blocking pool.
                let handle = backend(cx).spawn_blocking(move || store.login_flow());
                this._load = Some(cx.spawn_in(window, async move |this, cx| {
                    let flow = handle.await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        match flow {
                            Ok(flow) => this.set_flow(flow, window, cx),
                            Err(e) => {
                                this.flow = FlowState::Unavailable {
                                    title: Some(t("manage.login.prepare_failed")),
                                    message: format!("{e:#}").into(),
                                }
                            }
                        }
                        cx.notify();
                    });
                }));
            }
        }
        this
    }

    fn set_flow(&mut self, flow: LoginFlow, window: &mut Window, cx: &mut Context<Self>) {
        match flow {
            LoginFlow::None => {
                self.flow = FlowState::unavailable(t_args(
                    "manage.login.no_account",
                    &[("store", self.runner.store_name())],
                ));
            }
            LoginFlow::Browser {
                url,
                capture,
                instructions,
            } => {
                if url.is_empty() {
                    // The store explains why sign-in is unavailable in its instructions.
                    self.flow = FlowState::unavailable(instructions);
                    return;
                }
                let capture = compile_capture(&capture);
                // The sign-in window can only finish by itself with a pattern to
                // recognise the result by.
                let use_window = capture.is_some() && window::helper_path().is_some();
                self.flow = FlowState::Ready {
                    url,
                    instructions,
                    capture,
                };
                if use_window {
                    self.open_window(window, cx);
                } else {
                    self.use_browser(window, cx);
                }
            }
        }
    }

    /// Open the sign-in window; its result signs in, closing it offers the choice
    /// again, and a failure falls back to the browser.
    fn open_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let FlowState::Ready {
            url,
            capture: Some(re),
            ..
        } = &self.flow
        else {
            return;
        };
        let (url, pattern) = (url.clone(), re.as_str().to_string());
        let Some(helper) = window::helper_path() else {
            self.use_browser(window, cx);
            return;
        };
        self.close_window();
        let cancel = CancellationToken::new();
        let title = t_args("manage.login.title", &[("store", self.runner.store_name())]);
        let task = ember_ui::run(
            cx,
            window::sign_in(helper, url, pattern, title.to_string(), cancel.clone()),
        );
        let task = cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !matches!(this.window, SignInWindow::Open { .. }) {
                    return;
                }
                match outcome {
                    Ok(WindowOutcome::SignedIn(text)) => {
                        this.window = SignInWindow::Closed;
                        this.submit(text, window, cx);
                    }
                    Ok(WindowOutcome::Closed) => this.window = SignInWindow::Closed,
                    Err(e) => {
                        tracing::warn!("Sign-in window unavailable, using the browser: {e:#}");
                        this.use_browser(window, cx);
                    }
                }
                cx.notify();
            });
        });
        self.window = SignInWindow::Open {
            cancel,
            _task: task,
        };
        cx.notify();
    }

    /// Close the sign-in window if it is open.
    fn close_window(&mut self) {
        if let SignInWindow::Open { cancel, .. } =
            std::mem::replace(&mut self.window, SignInWindow::None)
        {
            cancel.cancel();
        }
    }

    /// Show the browser flow: open the page in the browser and watch the clipboard.
    fn use_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_window();
        let FlowState::Ready { url, capture, .. } = &self.flow else {
            return;
        };
        cx.open_url(url);
        if capture.is_some() {
            self.start_watching(window, cx);
        }
        cx.notify();
    }

    /// The dialog closed: close the sign-in window and stop watching the clipboard.
    pub(crate) fn close(&mut self) {
        self.close_window();
        self.stop_watching();
    }

    fn start_watching(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.poll = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(CLIPBOARD_POLL).await;
                let alive = this.update_in(cx, |this, window, cx| this.poll_clipboard(window, cx));
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    /// Stop the clipboard watch (the dialog closed).
    pub(crate) fn stop_watching(&mut self) {
        self.poll = None;
    }

    fn poll_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submitting {
            return;
        }
        let FlowState::Ready {
            capture: Some(re), ..
        } = &self.flow
        else {
            return;
        };
        let text = cx.read_from_clipboard().and_then(|c| c.text());
        if let Some(secret) = self.watcher.observe(re, text.as_deref()) {
            self.submit(secret, window, cx);
        }
    }

    fn submit_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() || self.submitting {
            return;
        }
        self.submit(text, window, cx);
    }

    fn submit(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        self.submitting = true;
        self.error = None;
        cx.notify();
        let runner = self.runner;
        let task = ember_ui::run(cx, async move { store.login(&text).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.submitting = false;
                match result {
                    Ok(user) => {
                        this.close();
                        window.close_dialog(cx);
                        let name = user.display_name.trim();
                        let store = runner.store_name();
                        let message = if name.is_empty() {
                            t_args("manage.login.signed_in", &[("store", store)])
                        } else {
                            t_args(
                                "manage.login.signed_in_as",
                                &[("store", store), ("name", name)],
                            )
                        };
                        notify_success(message, window, cx);
                        ember_ui::models::refresh_library(false, cx);
                    }
                    Err(e) => {
                        this.error = Some(format!("{e:#}").into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn copy_link(&mut self, cx: &mut Context<Self>) {
        if let FlowState::Ready { url, .. } = &self.flow {
            self.watcher.ignore(url);
            cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
        }
    }

    fn reopen(&mut self, cx: &mut Context<Self>) {
        if let FlowState::Ready { url, .. } = &self.flow {
            cx.open_url(url);
        }
    }
}

/// One numbered step of the flow.
fn step(n: usize, title: SharedString, body: impl IntoElement, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .flex_shrink_0()
                .size_6()
                .rounded_full()
                .bg(theme.muted)
                .text_xs()
                .font_semibold()
                .flex()
                .items_center()
                .justify_center()
                .child(n.to_string()),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(div().text_sm().font_semibold().child(title))
                .child(body),
        )
}

impl Render for LoginDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let body: AnyElement = match &self.flow {
            FlowState::Loading => h_flex()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Spinner::new().small())
                .child(t("manage.login.preparing"))
                .into_any_element(),
            FlowState::Unavailable { title, message } => {
                Alert::error("login-unavailable", message.clone())
                    .when_some(title.clone(), |a, title| a.title(title))
                    .into_any_element()
            }
            FlowState::Ready { .. } if !matches!(self.window, SignInWindow::None) => {
                let open = matches!(self.window, SignInWindow::Open { .. });
                let store = self.runner.store_name();
                v_flex()
                    .gap_4()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_start()
                            .when(open || self.submitting, |row| {
                                row.child(div().pt_0p5().child(Spinner::new().small()))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .child(if self.submitting {
                                        t("manage.login.window.finishing")
                                    } else if open {
                                        t_args("manage.login.window.waiting", &[("store", store)])
                                    } else {
                                        t("manage.login.window.closed")
                                    }),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .when(!open && !self.submitting, |row| {
                                row.child(
                                    Button::new("reopen-window")
                                        .primary()
                                        .label(t("manage.login.window.reopen"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_window(window, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("use-browser")
                                    .outline()
                                    .icon(IconName::ExternalLink)
                                    .label(t("manage.login.use_browser"))
                                    .disabled(self.submitting)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.use_browser(window, cx)
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            FlowState::Ready {
                instructions,
                capture,
                ..
            } => v_flex()
                .gap_4()
                .child(step(
                    1,
                    t("manage.login.step.sign_in"),
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child(t("manage.login.step.opened")),
                        )
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    Button::new("reopen")
                                        .small()
                                        .outline()
                                        .icon(IconName::ExternalLink)
                                        .label(t("manage.login.reopen"))
                                        .on_click(cx.listener(|this, _, _, cx| this.reopen(cx))),
                                )
                                .child(
                                    Button::new("copy-link")
                                        .small()
                                        .ghost()
                                        .label(t("manage.login.copy_link"))
                                        .on_click(cx.listener(|this, _, _, cx| this.copy_link(cx))),
                                ),
                        ),
                    cx,
                ))
                .child(step(
                    2,
                    t("manage.login.step.copy"),
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(instructions.clone()),
                    cx,
                ))
                .child(step(
                    3,
                    t("manage.login.step.paste"),
                    v_flex()
                        .gap_2()
                        .child(Input::new(&self.input).disabled(self.submitting))
                        .when(capture.is_some(), |this| {
                            this.child(
                                h_flex()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(Icon::new(IconName::ClipboardPaste).xsmall())
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(t("manage.login.auto_detect")),
                                    ),
                            )
                        }),
                    cx,
                ))
                .into_any_element(),
        };
        let ready = matches!(self.flow, FlowState::Ready { .. });
        let browser = matches!(self.window, SignInWindow::None);
        let has_input = !self.input.read(cx).value().trim().is_empty();
        v_flex()
            .gap_4()
            .w_full()
            .child(body)
            .when_some(self.error.clone(), |this, e| {
                this.child(Alert::error("login-error", e).title(t("manage.login.failed")))
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("login-cancel")
                            .label(t("manage.login.cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .when(browser, |row| {
                        row.child(
                            Button::new("login-submit")
                                .primary()
                                .label(t("manage.login.submit"))
                                .loading(self.submitting)
                                .disabled(!ready || !has_input || self.submitting)
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.submit_input(window, cx)
                                    }),
                                ),
                        )
                    }),
            )
    }
}
