//! Steam's sign-in, drawn by Irori: a QR code the Steam Mobile app scans, or
//! the account name and password followed by Steam Guard (approve in the
//! app, or a code from the app or an email). Steam answers with tokens; the
//! password is sent to Steam and never kept. Polling stops when the view is
//! dropped (the dialog closed).

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, Focusable as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window, black, div,
    px, white,
};
use irori_steam::signin::{
    Confirmation, CredentialsSession, GuardKind, Poll, SignIn, SteamError, login_input,
};
use irori_ui::backend;
use irori_ui::i18n::{t, t_args};

/// Side of one QR module. The code is art: it needs exact pixels and black on
/// white whatever the theme, or phones fail to read it.
const MODULE: f32 = 5.;
/// White border around the code, in modules, as the QR standard asks.
const QUIET: usize = 4;

/// Steam finished the sign-in: what [`irori_core::Store::login`] takes.
pub(crate) struct SteamSignedIn(pub String);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Qr,
    Password,
}

enum QrState {
    Loading,
    Ready { width: usize, dark: Vec<bool> },
    Failed(SharedString),
}

enum PasswordStage {
    Form,
    /// Steam asks for a Steam Guard confirmation.
    Guard {
        session: CredentialsSession,
        confirmations: Vec<Confirmation>,
    },
}

pub(crate) struct SteamSignInView {
    mode: Mode,
    signin: Option<SignIn>,
    qr: QrState,
    stage: PasswordStage,
    account: Entity<InputState>,
    password: Entity<InputState>,
    code: Entity<InputState>,
    busy: bool,
    error: Option<SharedString>,
    /// The sign-in being polled; replacing it stops the previous one.
    poll: Option<Task<()>>,
    _load: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SteamSignedIn> for SteamSignInView {}

impl SteamSignInView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let account =
            cx.new(|cx| InputState::new(window, cx).placeholder(t("manage.steam.account")));
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(t("manage.steam.password"))
        });
        let code = cx
            .new(|cx| InputState::new(window, cx).placeholder(t("manage.steam.code_placeholder")));
        let mut subscriptions = Vec::new();
        for input in [&account, &password] {
            subscriptions.push(
                cx.subscribe_in(input, window, |this, _, event, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.sign_in_with_password(window, cx);
                    }
                }),
            );
        }
        subscriptions.push(
            cx.subscribe_in(&code, window, |this, _, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit_code(window, cx);
                }
            }),
        );
        // Reading this device's identity touches the disk.
        let load = backend(cx).spawn_blocking(SignIn::new);
        let load = cx.spawn_in(window, async move |this, cx| {
            let signin = load
                .await
                .map_err(anyhow::Error::from)
                .and_then(|signin| signin);
            let _ = this.update_in(cx, |this, window, cx| {
                match signin {
                    Ok(signin) => {
                        this.signin = Some(signin);
                        this.start_qr(window, cx);
                    }
                    Err(e) => this.qr = QrState::Failed(format!("{e:#}").into()),
                }
                cx.notify();
            });
        });
        Self {
            mode: Mode::Qr,
            signin: None,
            qr: QrState::Loading,
            stage: PasswordStage::Form,
            account,
            password,
            code,
            busy: false,
            error: None,
            poll: None,
            _load: load,
            _subscriptions: subscriptions,
        }
    }

    fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.error = None;
        self.poll = None;
        self.busy = false;
        self.stage = PasswordStage::Form;
        match mode {
            Mode::Qr => self.start_qr(window, cx),
            Mode::Password => self.account.read(cx).focus_handle(cx).focus(window, cx),
        }
        cx.notify();
    }

    /// Ask Steam for a QR code and wait for the phone to approve it.
    fn start_qr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(signin) = self.signin.clone() else {
            return;
        };
        self.qr = QrState::Loading;
        self.error = None;
        let task = irori_ui::run(cx, async move { signin.begin_qr().await });
        self.poll = Some(cx.spawn_in(window, async move |this, cx| {
            let started = task.await;
            let session = this.update_in(cx, |this, _, cx| {
                let session = match started {
                    Ok(session) => {
                        this.show_qr(&session.challenge_url);
                        Some(session)
                    }
                    Err(e) => {
                        this.qr = QrState::Failed(error_text(&e));
                        None
                    }
                };
                cx.notify();
                session
            });
            let Ok(Some(session)) = session else { return };
            let (mut client_id, request_id, interval) =
                (session.client_id, session.request_id, session.interval);
            loop {
                cx.background_executor().timer(interval).await;
                let Ok(signin) = this.update(cx, |this, _| this.signin.clone()) else {
                    return;
                };
                let Some(signin) = signin else { return };
                let rid = request_id.clone();
                let Ok(task) = this.update(cx, |_, cx| {
                    irori_ui::run(cx, async move { signin.poll(client_id, &rid).await })
                }) else {
                    return;
                };
                let polled = task.await;
                let next = this.update_in(cx, |this, window, cx| {
                    let next = match polled {
                        Ok(Poll::Pending {
                            new_challenge_url,
                            new_client_id,
                        }) => {
                            if let Some(url) = new_challenge_url {
                                this.show_qr(&url);
                            }
                            Some(new_client_id)
                        }
                        Ok(Poll::Done(tokens)) => {
                            cx.emit(SteamSignedIn(login_input(tokens)));
                            None
                        }
                        // Codes expire after a few minutes: show a new one.
                        Err(e) if is_expired(&e) => {
                            this.start_qr(window, cx);
                            None
                        }
                        Err(e) => {
                            this.qr = QrState::Failed(error_text(&e));
                            None
                        }
                    };
                    cx.notify();
                    next
                });
                match next {
                    Ok(Some(new_id)) => {
                        if let Some(id) = new_id {
                            client_id = id;
                        }
                    }
                    _ => return,
                }
            }
        }));
    }

    fn show_qr(&mut self, url: &str) {
        self.qr = match qrcode::QrCode::new(url.as_bytes()) {
            Ok(code) => QrState::Ready {
                width: code.width(),
                dark: code
                    .to_colors()
                    .into_iter()
                    .map(|c| c == qrcode::Color::Dark)
                    .collect(),
            },
            Err(e) => QrState::Failed(e.to_string().into()),
        };
    }

    fn sign_in_with_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let account = self.account.read(cx).value().trim().to_string();
        let password = self.password.read(cx).value().to_string();
        if self.busy || account.is_empty() || password.is_empty() {
            return;
        }
        let Some(signin) = self.signin.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let task = irori_ui::run(cx, async move {
            signin.begin_password(&account, &password).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let started = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match started {
                    Ok(session) => {
                        // The password has done its job.
                        this.password
                            .update(cx, |s, cx| s.set_value("", window, cx));
                        let confirmations = session.confirmations.clone();
                        let interval = session.interval;
                        let (client_id, request_id) =
                            (session.client_id, session.request_id.clone());
                        this.stage = PasswordStage::Guard {
                            session,
                            confirmations,
                        };
                        this.poll_password(client_id, request_id, interval, window, cx);
                        this.code.read(cx).focus_handle(cx).focus(window, cx);
                    }
                    Err(e) => this.error = Some(error_text(&e)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Wait for Steam Guard: the phone's approval, an email link, or a code
    /// typed here (which Steam then reports through the same poll).
    fn poll_password(
        &mut self,
        mut client_id: u64,
        request_id: Vec<u8>,
        interval: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.poll = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                let Ok(Some(signin)) = this.update(cx, |this, _| this.signin.clone()) else {
                    return;
                };
                let rid = request_id.clone();
                let Ok(task) = this.update(cx, |_, cx| {
                    irori_ui::run(cx, async move { signin.poll(client_id, &rid).await })
                }) else {
                    return;
                };
                let polled = task.await;
                let next = this.update_in(cx, |this, _, cx| {
                    let next = match polled {
                        Ok(Poll::Pending { new_client_id, .. }) => Some(new_client_id),
                        Ok(Poll::Done(tokens)) => {
                            cx.emit(SteamSignedIn(login_input(tokens)));
                            None
                        }
                        Err(e) => {
                            this.stage = PasswordStage::Form;
                            this.error = Some(error_text(&e));
                            None
                        }
                    };
                    cx.notify();
                    next
                });
                match next {
                    Ok(Some(new_id)) => {
                        if let Some(id) = new_id {
                            client_id = id;
                        }
                    }
                    _ => return,
                }
            }
        }));
    }

    fn submit_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let code = self.code.read(cx).value().trim().to_string();
        let PasswordStage::Guard {
            session,
            confirmations,
        } = &self.stage
        else {
            return;
        };
        let Some(kind) = code_kind(confirmations) else {
            return;
        };
        if self.busy || code.is_empty() {
            return;
        }
        let Some(signin) = self.signin.clone() else {
            return;
        };
        let session = session.clone();
        self.busy = true;
        self.error = None;
        cx.notify();
        let task = irori_ui::run(cx, async move {
            signin.submit_code(&session, &code, kind).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let sent = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if let Err(e) = sent {
                    this.error = Some(error_text(&e));
                    this.code.update(cx, |s, cx| s.set_value("", window, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn back_to_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.poll = None;
        self.stage = PasswordStage::Form;
        self.error = None;
        self.busy = false;
        self.code.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    fn render_qr(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let code: AnyElement = match &self.qr {
            QrState::Loading => h_flex()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Spinner::new().small())
                .child(t("manage.steam.qr_loading"))
                .into_any_element(),
            QrState::Failed(message) => v_flex()
                .gap_2()
                .items_start()
                .child(Alert::error("steam-qr-failed", message.clone()))
                .child(
                    Button::new("steam-qr-retry")
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label(t("manage.steam.qr_retry"))
                        .on_click(cx.listener(|this, _, window, cx| this.start_qr(window, cx))),
                )
                .into_any_element(),
            QrState::Ready { width, dark } => qr_code(*width, dark),
        };
        v_flex()
            .gap_3()
            .items_center()
            .child(code)
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .text_sm()
                    .text_color(muted)
                    .child(Icon::new(IconName::Smartphone).small())
                    .child(div().flex_1().min_w_0().child(t("manage.steam.qr_hint"))),
            )
            .into_any_element()
    }

    fn render_password(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        match &self.stage {
            PasswordStage::Form => v_flex()
                .gap_3()
                .child(Input::new(&self.account).disabled(self.busy))
                .child(Input::new(&self.password).mask_toggle().disabled(self.busy))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(t("manage.steam.password_note")),
                )
                .child(
                    h_flex().justify_end().child(
                        Button::new("steam-sign-in")
                            .primary()
                            .icon(IconName::LogIn)
                            .label(t("manage.steam.sign_in"))
                            .loading(self.busy)
                            .disabled(self.busy || self.signin.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.sign_in_with_password(window, cx)
                            })),
                    ),
                )
                .into_any_element(),
            PasswordStage::Guard { confirmations, .. } => {
                let wants_code = code_kind(confirmations).is_some();
                v_flex()
                    .gap_3()
                    .children(confirmations.iter().filter_map(guard_text).map(|text| {
                        h_flex()
                            .gap_2()
                            .items_start()
                            .text_sm()
                            .child(Icon::new(IconName::ShieldCheck).small())
                            .child(div().flex_1().min_w_0().child(text))
                    }))
                    .when(wants_code, |this| {
                        this.child(Input::new(&self.code).disabled(self.busy))
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Spinner::new().small())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(t("manage.steam.waiting")),
                            )
                            .child(
                                Button::new("steam-guard-back")
                                    .ghost()
                                    .label(t("manage.steam.back"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.back_to_form(window, cx)
                                    })),
                            )
                            .when(wants_code, |row| {
                                row.child(
                                    Button::new("steam-guard-submit")
                                        .primary()
                                        .label(t("manage.steam.submit_code"))
                                        .loading(self.busy)
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.submit_code(window, cx)
                                        })),
                                )
                            }),
                    )
                    .into_any_element()
            }
        }
    }
}

/// The code kind to send when the user types one: the app's code first.
fn code_kind(confirmations: &[Confirmation]) -> Option<GuardKind> {
    [GuardKind::DeviceCode, GuardKind::EmailCode]
        .into_iter()
        .find(|kind| confirmations.iter().any(|c| c.kind == *kind))
}

/// What to tell the user about one way Steam Guard accepts the sign-in.
fn guard_text(confirmation: &Confirmation) -> Option<SharedString> {
    let domain = confirmation
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    match confirmation.kind {
        GuardKind::DeviceConfirmation => Some(t("manage.steam.approve_on_phone")),
        GuardKind::DeviceCode => Some(t("manage.steam.device_code")),
        GuardKind::EmailCode => Some(match domain {
            Some(domain) => t_args("manage.steam.email_code", &[("domain", domain)]),
            None => t("manage.steam.email_code_plain"),
        }),
        GuardKind::EmailConfirmation => Some(t("manage.steam.email_confirm")),
        _ => None,
    }
}

fn is_expired(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<SteamError>(),
        Some(SteamError::LoginExpired)
    )
}

/// Steam's refusals in the user's language; anything else as Steam said it.
fn error_text(e: &anyhow::Error) -> SharedString {
    match e.downcast_ref::<SteamError>() {
        Some(SteamError::InvalidPassword) => t("manage.steam.error.password"),
        Some(SteamError::InvalidCode) => t("manage.steam.error.code"),
        Some(SteamError::RateLimited) => t("manage.steam.error.rate_limited"),
        Some(SteamError::LoginExpired) => t("manage.steam.error.expired"),
        _ => format!("{e:#}").into(),
    }
}

/// The QR code as rows of dark and light runs on a white card.
fn qr_code(width: usize, dark: &[bool]) -> AnyElement {
    let module = px(MODULE);
    let rows = dark.chunks(width.max(1)).map(|row| {
        let mut runs: Vec<(bool, usize)> = Vec::new();
        for &d in row {
            match runs.last_mut() {
                Some((color, len)) if *color == d => *len += 1,
                _ => runs.push((d, 1)),
            }
        }
        h_flex().children(runs.into_iter().map(move |(d, len)| {
            div()
                .h(module)
                .w(module * len as f32)
                .when(d, |el| el.bg(black()))
        }))
    });
    div()
        .flex_shrink_0()
        .bg(white())
        .rounded_md()
        .p(module * QUIET as f32)
        .child(v_flex().children(rows))
        .into_any_element()
}

impl Render for SteamSignInView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.mode;
        v_flex()
            .gap_4()
            .child(
                TabBar::new("steam-sign-in-mode")
                    .segmented()
                    .small()
                    .selected_index(match mode {
                        Mode::Qr => 0,
                        Mode::Password => 1,
                    })
                    .child(Tab::new().label(t("manage.steam.tab_qr")))
                    .child(Tab::new().label(t("manage.steam.tab_password")))
                    .on_click(cx.listener(|this, ix: &usize, window, cx| {
                        let mode = if *ix == 0 { Mode::Qr } else { Mode::Password };
                        this.set_mode(mode, window, cx);
                    })),
            )
            .child(match mode {
                Mode::Qr => self.render_qr(cx),
                Mode::Password => self.render_password(cx),
            })
            .when_some(self.error.clone(), |this, e| {
                this.child(Alert::error("steam-sign-in-error", e).title(t("manage.login.failed")))
            })
    }
}
