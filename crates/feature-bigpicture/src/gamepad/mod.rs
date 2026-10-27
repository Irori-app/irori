//! Controller input for the whole application.
//!
//! A dedicated thread reads controllers through gilrs (SDL mappings for Xbox,
//! PlayStation including DualSense, Switch Pro and Steam Deck / Steam Input
//! pads), normalizes buttons and sticks into [`GamepadInput`]s with
//! repeat timing from the settings, and sends them over a channel to a GPUI
//! task. That task routes each input to the focused element of the active
//! window:
//!
//! 1. If the focused element (or an ancestor) handles the matching action
//!    from this module ([`Up`], [`Confirm`], …), the action is dispatched.
//!    Gamepad-first views such as Big Picture handle these directly.
//! 2. Otherwise the input is translated for ordinary screens: directions
//!    send arrow keys (so lists, menus and grids navigate as with a
//!    keyboard) and fall back to moving focus to the previous/next Tab stop;
//!    Confirm presses Enter (activating the focused control), Back presses
//!    Escape and falls back to navigating back, Alt sends Ctrl+Enter (play),
//!    Menu opens the command palette (Ctrl+K) and the shoulders page.
//! 3. Guide toggles Big Picture from anywhere.
//!
//! Input is ignored while `disable_controller` is set. When Irori is not the
//! active window only Guide is honoured, and only while no game is running:
//! it brings Irori to the front in Big Picture instead of acting on input
//! meant for a game. Guide acts on a short press, so holding it to switch a
//! controller off does nothing.
//!
//! Steam Input must not make a press count twice: controllers Steam lists in
//! `SDL_GAMECONTROLLER_IGNORE_DEVICES` (it drives them through a virtual
//! controller) are not read, and an arrow, Enter or Escape key press that
//! echoes a controller press just handled (Steam's desktop layout types
//! them) is dropped, as is a controller press echoing such a key.

mod input;
mod reader;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use futures::StreamExt as _;
use gpui_kit::{
    Action, AnyWindowHandle, App, AppContext as _, Entity, Global, KeyUpEvent, Keystroke,
    PlatformInput, Window, actions,
};
use irori_ui::Route;
use irori_ui::i18n::{t, t_args};
use irori_ui::nav::{self, navigator};
use parking_lot::RwLock;

pub use input::{
    ControllerKind, Deduper, EchoFilter, GamepadInput, GuideTap, InputSource, PadButton, Repeater,
    SteamIgnoreList, StickAxis, is_ignored_device, keyboard_equivalent, map_button,
};
use reader::{Message, SharedConfig};

actions!(
    irori_gamepad,
    [
        /// Move up (D-pad, left stick, or the Up arrow in Big Picture).
        Up,
        /// Move down.
        Down,
        /// Move left.
        Left,
        /// Move right.
        Right,
        /// Activate the focused item (A / Cross).
        Confirm,
        /// Close the topmost layer or go back (B / Circle).
        Back,
        /// Secondary action of the focused item, e.g. play directly (X / Square).
        Alt,
        /// Open the context or quick menu (Y / Triangle / Start).
        Menu,
        /// Previous page (LB / L1).
        PrevPage,
        /// Next page (RB / R1).
        NextPage,
        /// Guide / PS / Home button.
        Guide
    ]
);

/// Charge state of a controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Battery {
    #[default]
    Unknown,
    Wired,
    /// Running on battery, level in percent.
    Discharging(u8),
    /// Charging, level in percent.
    Charging(u8),
    Full,
}

impl Battery {
    /// Short translated text such as "80%" or "Charging 40%"; `None` when
    /// unknown or wired.
    pub fn label(self) -> Option<String> {
        crate::register_catalogs();
        let text = match self {
            Battery::Discharging(level) => t_args(
                "bigpicture.battery.level",
                &[("level", level.to_string().as_str())],
            ),
            Battery::Charging(level) => t_args(
                "bigpicture.battery.charging",
                &[("level", level.to_string().as_str())],
            ),
            Battery::Full => t("bigpicture.battery.full"),
            Battery::Unknown | Battery::Wired => return None,
        };
        Some(text.to_string())
    }

    /// Whether the level is low enough to warn about.
    pub fn is_low(self) -> bool {
        matches!(self, Battery::Discharging(level) if level <= 20)
    }
}

/// A connected controller.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ControllerInfo {
    /// Device name as reported by the driver or SDL mapping.
    pub name: String,
    pub kind: ControllerKind,
    pub battery: Battery,
}

static CONNECTED: RwLock<Vec<ControllerInfo>> = RwLock::new(Vec::new());

pub(crate) fn set_connected(list: Vec<ControllerInfo>) {
    *CONNECTED.write() = list;
}

/// Names of the connected controllers, for the settings page. Empty until
/// [`install`] ran and a controller is plugged in.
pub fn connected_controllers() -> Vec<String> {
    CONNECTED.read().iter().map(|c| c.name.clone()).collect()
}

/// Details of the connected controllers.
pub fn controllers() -> Vec<ControllerInfo> {
    CONNECTED.read().clone()
}

/// Observable controller state: connected devices and the family of the
/// controller used last (for button glyphs). Owned by the gamepad global.
#[derive(Default)]
pub struct GamepadModel {
    controllers: Vec<ControllerInfo>,
    last_kind: Option<ControllerKind>,
}

impl GamepadModel {
    pub fn controllers(&self) -> &[ControllerInfo] {
        &self.controllers
    }

    /// Whether any controller is connected.
    pub fn is_connected(&self) -> bool {
        !self.controllers.is_empty()
    }

    /// Family of the controller used most recently, else of the first
    /// connected one; `None` without controllers.
    pub fn active_kind(&self) -> Option<ControllerKind> {
        if self.controllers.is_empty() {
            return None;
        }
        self.last_kind
            .or_else(|| self.controllers.first().map(|c| c.kind))
    }

    /// The controller to show in status indicators.
    pub fn primary(&self) -> Option<&ControllerInfo> {
        let kind = self.active_kind()?;
        self.controllers
            .iter()
            .find(|c| c.kind == kind)
            .or_else(|| self.controllers.first())
    }
}

struct GamepadGlobal {
    model: Entity<GamepadModel>,
}

impl Global for GamepadGlobal {}

/// The controller model, if [`install`] ran.
pub fn model(cx: &App) -> Option<Entity<GamepadModel>> {
    cx.try_global::<GamepadGlobal>().map(|g| g.model.clone())
}

/// Start controller support: the reader thread and the GPUI task that routes
/// its inputs. Call once after `irori_ui::init`; later calls do nothing.
/// Follows `disable_controller`, `gamepad_initial_repeat_delay` and
/// `gamepad_repeat_delay` live.
pub fn install(cx: &mut App) {
    if cx.has_global::<GamepadGlobal>() {
        return;
    }
    let settings = cx
        .try_global::<irori_ui::Models>()
        .map(|m| m.settings.clone());
    let shared = {
        let (disabled, initial, repeat) = settings
            .as_ref()
            .map(|s| {
                let app = &s.read(cx).app;
                (
                    app.disable_controller,
                    app.gamepad_initial_repeat_delay,
                    app.gamepad_repeat_delay,
                )
            })
            .unwrap_or((false, 300, 50));
        Arc::new(SharedConfig::new(disabled, initial, repeat))
    };
    if let Some(settings) = settings {
        let shared = shared.clone();
        cx.observe(&settings, move |settings, cx| {
            let app = &settings.read(cx).app;
            shared
                .disabled
                .store(app.disable_controller, Ordering::Relaxed);
            shared
                .initial_ms
                .store(app.gamepad_initial_repeat_delay, Ordering::Relaxed);
            shared
                .repeat_ms
                .store(app.gamepad_repeat_delay, Ordering::Relaxed);
        })
        .detach();
    }

    let model = cx.new(|_| GamepadModel::default());
    cx.set_global(GamepadGlobal {
        model: model.clone(),
    });

    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    let thread_shared = shared.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("irori-gamepad".into())
        .spawn(move || reader::run(thread_shared, tx))
    {
        tracing::warn!("starting the controller reader: {e}");
        return;
    }
    let echoes = Rc::new(RefCell::new(EchoFilter::default()));
    {
        let echoes = echoes.clone();
        cx.intercept_keystrokes(move |event, _, cx| {
            let keystroke = &event.keystroke;
            if SYNTHESIZING.get() || keystroke.modifiers.modified() {
                return;
            }
            if let Some(input) = keyboard_equivalent(&keystroke.key)
                && !echoes
                    .borrow_mut()
                    .accept(InputSource::Keyboard, input, Instant::now())
            {
                cx.stop_propagation();
            }
        })
        .detach();
    }
    cx.spawn(async move |cx| {
        while let Some(message) = rx.next().await {
            let disabled = shared.disabled.load(Ordering::Relaxed);
            cx.update(|cx| on_message(message, disabled, &model, &echoes, cx));
        }
    })
    .detach();
}

thread_local! {
    /// Set while Irori types a key itself for a controller input, so the
    /// echo filter lets it through.
    static SYNTHESIZING: Cell<bool> = const { Cell::new(false) };
}

fn on_message(
    message: Message,
    disabled: bool,
    model: &Entity<GamepadModel>,
    echoes: &RefCell<EchoFilter>,
    cx: &mut App,
) {
    match message {
        Message::Controllers(list) => model.update(cx, |m, cx| {
            if list.is_empty() {
                m.last_kind = None;
            }
            m.controllers = list;
            cx.notify();
        }),
        Message::Input { input, kind } => {
            if disabled
                || !echoes
                    .borrow_mut()
                    .accept(InputSource::Controller, input, Instant::now())
            {
                return;
            }
            model.update(cx, |m, cx| {
                if m.last_kind != Some(kind) {
                    m.last_kind = Some(kind);
                    cx.notify();
                }
            });
            dispatch_input(input, cx);
        }
    }
}

/// The action dispatched for an input when the focused element handles it.
pub fn action_for(input: GamepadInput) -> Box<dyn Action> {
    match input {
        GamepadInput::Up => Box::new(Up),
        GamepadInput::Down => Box::new(Down),
        GamepadInput::Left => Box::new(Left),
        GamepadInput::Right => Box::new(Right),
        GamepadInput::Confirm => Box::new(Confirm),
        GamepadInput::Back => Box::new(Back),
        GamepadInput::Alt => Box::new(Alt),
        GamepadInput::Menu => Box::new(Menu),
        GamepadInput::PrevPage => Box::new(PrevPage),
        GamepadInput::NextPage => Box::new(NextPage),
        GamepadInput::Guide => Box::new(Guide),
    }
}

fn game_running(cx: &App) -> bool {
    cx.try_global::<irori_ui::Models>()
        .is_some_and(|m| m.statuses.read(cx).playing().next().is_some())
}

fn active_window(cx: &mut App) -> Option<AnyWindowHandle> {
    if let Some(window) = cx.active_window() {
        return Some(window);
    }
    cx.windows().into_iter().find(|handle| {
        handle
            .update(cx, |_, window, _| window.is_window_active())
            .unwrap_or(false)
    })
}

/// Route one input to the active window (see the module docs).
pub fn dispatch_input(input: GamepadInput, cx: &mut App) {
    match active_window(cx) {
        Some(handle) => {
            let _ = handle.update(cx, |_, window, cx| route_in_window(input, window, cx));
        }
        None if input == GamepadInput::Guide && !game_running(cx) => {
            if let Some(handle) = cx.windows().into_iter().next() {
                let _ = handle.update(cx, |_, window, _| window.activate_window());
                if navigator(cx).read(cx).current() != &Route::BigPicture {
                    nav::navigate(Route::BigPicture, cx);
                }
            }
        }
        None => {}
    }
}

/// Route one input inside `window`: the matching action when the focused
/// element handles it, otherwise the keyboard/focus translation.
pub fn route_in_window(input: GamepadInput, window: &mut Window, cx: &mut App) {
    let action = action_for(input);
    if window.is_action_available(action.as_ref(), cx) {
        window.dispatch_action(action, cx);
        return;
    }
    match input {
        GamepadInput::Guide => crate::toggle_big_picture(cx),
        GamepadInput::Up | GamepadInput::Left => {
            let key = if input == GamepadInput::Up {
                "up"
            } else {
                "left"
            };
            if !press(window, key, cx) {
                window.focus_prev(cx);
            }
        }
        GamepadInput::Down | GamepadInput::Right => {
            let key = if input == GamepadInput::Down {
                "down"
            } else {
                "right"
            };
            if !press(window, key, cx) {
                window.focus_next(cx);
            }
        }
        GamepadInput::Confirm => activate(window, cx),
        GamepadInput::Back => {
            if !press(window, "escape", cx) {
                nav::go_back(cx);
            }
        }
        GamepadInput::Alt => {
            press(window, "ctrl-enter", cx);
        }
        GamepadInput::Menu => {
            press(window, "ctrl-k", cx);
        }
        GamepadInput::PrevPage => {
            press(window, "pageup", cx);
        }
        GamepadInput::NextPage => {
            press(window, "pagedown", cx);
        }
    }
}

/// Press a key as if typed; returns whether something handled it.
fn press(window: &mut Window, key: &str, cx: &mut App) -> bool {
    match Keystroke::parse(key) {
        Ok(keystroke) => synthesize(|| window.dispatch_keystroke(keystroke, cx)),
        Err(_) => false,
    }
}

/// Run `f`, which types keys for a controller input, past the echo filter.
fn synthesize<R>(f: impl FnOnce() -> R) -> R {
    let previous = SYNTHESIZING.replace(true);
    let result = f();
    SYNTHESIZING.set(previous);
    result
}

/// Press and release Enter, which activates the focused control (GPUI
/// clicks a focused element on the release of Enter after its press).
fn activate(window: &mut Window, cx: &mut App) {
    let Ok(keystroke) = Keystroke::parse("enter") else {
        return;
    };
    if synthesize(|| window.dispatch_keystroke(keystroke.clone(), cx)) {
        return;
    }
    window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
}

#[cfg(test)]
mod tests {
    use gpui_kit::{
        Context, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _, Render,
        TestAppContext, Window, div,
    };

    use super::*;

    /// An ordinary screen: two Tab stops, no gamepad handlers.
    struct Plain {
        first: FocusHandle,
        second: FocusHandle,
    }

    impl Render for Plain {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .child(div().id("first").track_focus(&self.first).child("First"))
                .child(div().id("second").track_focus(&self.second).child("Second"))
        }
    }

    /// A gamepad-aware screen that counts `Down` actions.
    struct Aware {
        focus: FocusHandle,
        downs: usize,
    }

    impl Render for Aware {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &Down, _, cx| {
                    this.downs += 1;
                    cx.notify();
                }))
                .child("Aware")
        }
    }

    #[gpui_kit::test]
    fn directions_move_focus_on_ordinary_screens(cx: &mut TestAppContext) {
        let window = cx.add_window(|_, cx| Plain {
            first: cx.focus_handle().tab_stop(true),
            second: cx.focus_handle().tab_stop(true),
        });
        let focused = |cx: &mut TestAppContext, second: bool| {
            window
                .update(cx, |plain, window, _| {
                    let handle = if second { &plain.second } else { &plain.first };
                    handle.is_focused(window)
                })
                .unwrap()
        };
        cx.update_window(window.into(), |_, window, cx| {
            route_in_window(GamepadInput::Down, window, cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(focused(cx, false));
        cx.update_window(window.into(), |_, window, cx| {
            route_in_window(GamepadInput::Right, window, cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(focused(cx, true));
        cx.update_window(window.into(), |_, window, cx| {
            route_in_window(GamepadInput::Up, window, cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(focused(cx, false));
    }

    #[gpui_kit::test]
    fn gamepad_aware_views_receive_actions(cx: &mut TestAppContext) {
        let window = cx.add_window(|window, cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            Aware { focus, downs: 0 }
        });
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            route_in_window(GamepadInput::Down, window, cx)
        })
        .unwrap();
        cx.run_until_parked();
        let downs = window.update(cx, |aware, _, _| aware.downs).unwrap();
        assert_eq!(downs, 1);
    }
}
