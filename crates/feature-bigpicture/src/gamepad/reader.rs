//! The controller reader thread. It blocks in gilrs (epoll on Linux) until an
//! event arrives or the next directional repeat / battery refresh is due, so
//! an idle controller costs no CPU.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use gilrs::{Axis, Button, EventType, GamepadId, Gilrs, GilrsBuilder, PowerInfo};

use super::input::{
    AxisChange, ControllerKind, Deduper, GamepadInput, PadButton, Repeater, StickAxis,
    is_ignored_device, map_button,
};
use super::{Battery, ControllerInfo};

/// How often battery levels are re-read while a controller is connected.
const BATTERY_REFRESH: Duration = Duration::from_secs(30);

/// Settings shared with the GPUI side, updated live when settings change.
pub(crate) struct SharedConfig {
    pub disabled: AtomicBool,
    pub initial_ms: AtomicU32,
    pub repeat_ms: AtomicU32,
}

impl SharedConfig {
    pub fn new(disabled: bool, initial_ms: u32, repeat_ms: u32) -> Self {
        Self {
            disabled: AtomicBool::new(disabled),
            initial_ms: AtomicU32::new(initial_ms),
            repeat_ms: AtomicU32::new(repeat_ms),
        }
    }

    fn timing(&self) -> (Duration, Duration) {
        (
            Duration::from_millis(self.initial_ms.load(Ordering::Relaxed) as u64),
            Duration::from_millis(self.repeat_ms.load(Ordering::Relaxed) as u64),
        )
    }
}

/// What the reader reports to the GPUI side.
pub(crate) enum Message {
    Input {
        input: GamepadInput,
        kind: ControllerKind,
    },
    Controllers(Vec<ControllerInfo>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    DPad,
    Stick,
}

struct Device {
    kind: ControllerKind,
    ignored: bool,
    stick_x: StickAxis,
    stick_y: StickAxis,
}

struct Reader {
    shared: Arc<SharedConfig>,
    tx: UnboundedSender<Message>,
    devices: HashMap<GamepadId, Device>,
    held: Vec<(GamepadId, Source, GamepadInput)>,
    repeater: Repeater,
    dedupe: Deduper,
    controllers: Vec<ControllerInfo>,
    last_battery: Instant,
}

/// Run the reader until the GPUI side drops its receiver.
pub(crate) fn run(shared: Arc<SharedConfig>, tx: UnboundedSender<Message>) {
    let mut gilrs = match GilrsBuilder::new().with_force_feedback(false).build() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!("controller support unavailable: {e}");
            return;
        }
    };
    let (initial, interval) = shared.timing();
    let mut reader = Reader {
        shared,
        tx,
        devices: HashMap::new(),
        held: Vec::new(),
        repeater: Repeater::new(initial, interval),
        dedupe: Deduper::default(),
        controllers: Vec::new(),
        last_battery: Instant::now(),
    };
    reader.refresh_devices(&gilrs);

    loop {
        if reader.tx.is_closed() {
            return;
        }
        let (initial, interval) = reader.shared.timing();
        reader.repeater.set_timing(initial, interval);

        let now = Instant::now();
        let battery_due =
            (!reader.controllers.is_empty()).then(|| reader.last_battery + BATTERY_REFRESH);
        let deadline = match (reader.repeater.deadline(), battery_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let timeout = deadline.map(|d| d.saturating_duration_since(now));

        let started = Instant::now();
        match gilrs.next_event_blocking(timeout) {
            Some(event) => reader.handle(event.id, event.event, &gilrs),
            None if timeout.is_none() && started.elapsed() < Duration::from_millis(1) => {
                // The backend failed without blocking (epoll error); back off
                // instead of spinning.
                std::thread::sleep(Duration::from_millis(250));
            }
            None => {}
        }

        let now = Instant::now();
        if let Some(input) = reader.repeater.poll(now) {
            let kind = reader.held_kind();
            reader.emit(input, kind);
        }
        if battery_due.is_some_and(|due| now >= due) {
            reader.last_battery = now;
            reader.publish_controllers(&gilrs);
        }
    }
}

fn pad_button(button: Button) -> Option<PadButton> {
    Some(match button {
        Button::South => PadButton::South,
        Button::East => PadButton::East,
        Button::West => PadButton::West,
        Button::North => PadButton::North,
        Button::LeftTrigger => PadButton::LeftShoulder,
        Button::RightTrigger => PadButton::RightShoulder,
        Button::Start => PadButton::Start,
        Button::Select => PadButton::Select,
        Button::Mode => PadButton::Guide,
        Button::DPadUp => PadButton::DPadUp,
        Button::DPadDown => PadButton::DPadDown,
        Button::DPadLeft => PadButton::DPadLeft,
        Button::DPadRight => PadButton::DPadRight,
        _ => return None,
    })
}

fn battery(info: PowerInfo) -> Battery {
    match info {
        PowerInfo::Wired => Battery::Wired,
        PowerInfo::Discharging(level) => Battery::Discharging(level),
        PowerInfo::Charging(level) => Battery::Charging(level),
        PowerInfo::Charged => Battery::Full,
        PowerInfo::Unknown => Battery::Unknown,
    }
}

impl Reader {
    fn enabled(&self) -> bool {
        !self.shared.disabled.load(Ordering::Relaxed)
    }

    fn held_kind(&self) -> ControllerKind {
        self.held
            .last()
            .and_then(|(id, ..)| self.devices.get(id))
            .map(|d| d.kind)
            .unwrap_or_default()
    }

    fn emit(&mut self, input: GamepadInput, kind: ControllerKind) {
        if !self.enabled() {
            self.repeater.reset();
            return;
        }
        let _ = self.tx.unbounded_send(Message::Input { input, kind });
    }

    fn refresh_devices(&mut self, gilrs: &Gilrs) {
        let mut devices = HashMap::new();
        for (id, pad) in gilrs.gamepads() {
            let (vendor, product, name) = (pad.vendor_id(), pad.product_id(), pad.name());
            let previous = self.devices.remove(&id);
            devices.insert(
                id,
                Device {
                    kind: ControllerKind::detect(vendor, product, name),
                    ignored: is_ignored_device(vendor, product, name),
                    stick_x: previous.as_ref().map(|d| d.stick_x).unwrap_or_default(),
                    stick_y: previous.as_ref().map(|d| d.stick_y).unwrap_or_default(),
                },
            );
        }
        self.devices = devices;
        let devices = &self.devices;
        self.held.retain(|(id, ..)| devices.contains_key(id));
        if self.held.is_empty() {
            self.repeater.reset();
        }
        self.publish_controllers(gilrs);
    }

    fn publish_controllers(&mut self, gilrs: &Gilrs) {
        let controllers: Vec<ControllerInfo> = gilrs
            .gamepads()
            .filter(|(id, _)| self.devices.get(id).is_some_and(|d| !d.ignored))
            .map(|(id, pad)| ControllerInfo {
                name: pad.name().to_string(),
                kind: self.devices.get(&id).map(|d| d.kind).unwrap_or_default(),
                battery: battery(pad.power_info()),
            })
            .collect();
        if controllers != self.controllers {
            self.controllers = controllers.clone();
            super::set_connected(controllers.clone());
            let _ = self.tx.unbounded_send(Message::Controllers(controllers));
        }
    }

    fn handle(&mut self, id: GamepadId, event: EventType, gilrs: &Gilrs) {
        match event {
            EventType::Connected | EventType::Disconnected => {
                self.refresh_devices(gilrs);
            }
            EventType::ButtonPressed(button, _) => {
                let Some(button) = pad_button(button) else {
                    return;
                };
                let Some(device) = self.devices.get(&id) else {
                    return;
                };
                if device.ignored {
                    return;
                }
                let input = map_button(button, device.kind);
                self.press(id, Source::DPad, input);
            }
            EventType::ButtonReleased(button, _) => {
                let Some(button) = pad_button(button) else {
                    return;
                };
                let Some(device) = self.devices.get(&id) else {
                    return;
                };
                let input = map_button(button, device.kind);
                if input.is_directional() {
                    self.release(id, Source::DPad, input);
                }
            }
            EventType::AxisChanged(axis @ (Axis::LeftStickX | Axis::LeftStickY), value, _) => {
                let Some(device) = self.devices.get_mut(&id) else {
                    return;
                };
                if device.ignored {
                    return;
                }
                let horizontal = axis == Axis::LeftStickX;
                let changes = if horizontal {
                    device.stick_x.update(value)
                } else {
                    device.stick_y.update(value)
                };
                for change in changes {
                    let (positive, pressed) = match change {
                        AxisChange::Press(p) => (p, true),
                        AxisChange::Release(p) => (p, false),
                    };
                    let input = match (horizontal, positive) {
                        (true, true) => GamepadInput::Right,
                        (true, false) => GamepadInput::Left,
                        (false, true) => GamepadInput::Up,
                        (false, false) => GamepadInput::Down,
                    };
                    if pressed {
                        self.press(id, Source::Stick, input);
                    } else {
                        self.release(id, Source::Stick, input);
                    }
                }
            }
            _ => {}
        }
    }

    fn press(&mut self, id: GamepadId, source: Source, input: GamepadInput) {
        let now = Instant::now();
        let kind = self.devices.get(&id).map(|d| d.kind).unwrap_or_default();
        if !self.dedupe.accept(input, usize::from(id), now) {
            return;
        }
        if input.is_directional() {
            self.held.retain(|h| !(h.0 == id && h.1 == source));
            self.held.push((id, source, input));
            let input = self.repeater.press(input, now);
            self.emit(input, kind);
        } else {
            self.emit(input, kind);
        }
    }

    fn release(&mut self, id: GamepadId, source: Source, input: GamepadInput) {
        self.held
            .retain(|h| !(h.0 == id && h.1 == source && h.2 == input));
        match self.held.last() {
            Some(&(_, _, still)) if still != input => {
                if self.repeater.held() == Some(input) {
                    self.repeater.press(still, Instant::now());
                }
            }
            Some(_) => {}
            None => self.repeater.release(input),
        }
    }
}
