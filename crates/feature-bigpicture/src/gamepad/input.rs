//! Controller-independent input model: normalized inputs, controller families,
//! directional repeat timing, cross-device and keyboard de-duplication, and
//! the devices Steam asks other programs to ignore. Pure logic, no I/O, so it
//! is unit-tested without hardware.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ember_ui::i18n::t;
use gpui_kit::SharedString;

/// A normalized controller input, independent of the physical layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GamepadInput {
    Up,
    Down,
    Left,
    Right,
    /// The "accept" face button (A on Xbox and Nintendo, Cross on PlayStation).
    Confirm,
    /// The "cancel" face button (B on Xbox and Nintendo, Circle on PlayStation).
    Back,
    /// X on Xbox and Nintendo, Square on PlayStation.
    Alt,
    /// Y on Xbox and Nintendo, Triangle on PlayStation, or Start/Select.
    Menu,
    /// Left shoulder (LB / L1 / L).
    PrevPage,
    /// Right shoulder (RB / R1 / R).
    NextPage,
    /// Guide / PS / Home / Steam button.
    Guide,
}

impl GamepadInput {
    /// Directional inputs repeat while held; buttons fire once per press.
    pub fn is_directional(self) -> bool {
        matches!(
            self,
            GamepadInput::Up | GamepadInput::Down | GamepadInput::Left | GamepadInput::Right
        )
    }
}

/// Controller family, used for button glyphs and layout quirks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ControllerKind {
    Xbox,
    PlayStation4,
    PlayStation5,
    /// Switch Pro / Joy-Con: face buttons are labelled by position the other
    /// way round (A is east, B is south).
    Nintendo,
    SteamDeck,
    #[default]
    Generic,
}

impl ControllerKind {
    /// Detect the family from USB ids and the device name. PlayStation 5
    /// controllers are matched before PlayStation 4 ones, so a DualSense is
    /// never mistaken for a DualShock.
    pub fn detect(vendor: Option<u16>, product: Option<u16>, name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        match vendor {
            Some(0x28de) => return ControllerKind::SteamDeck,
            Some(0x054c) => {
                return if matches!(product, Some(0x0ce6 | 0x0df2)) || name.contains("dualsense") {
                    ControllerKind::PlayStation5
                } else {
                    ControllerKind::PlayStation4
                };
            }
            Some(0x057e) => return ControllerKind::Nintendo,
            Some(0x045e) => return ControllerKind::Xbox,
            _ => {}
        }
        if name.contains("dualsense") || name.contains("ps5") {
            ControllerKind::PlayStation5
        } else if name.contains("dualshock")
            || name.contains("ps4")
            || name.contains("ps3")
            || name.contains("playstation")
        {
            ControllerKind::PlayStation4
        } else if name.contains("xbox") || name.contains("x-box") || name.contains("microsoft") {
            ControllerKind::Xbox
        } else if name.contains("switch") || name.contains("joy-con") || name.contains("nintendo") {
            ControllerKind::Nintendo
        } else if name.contains("steam") {
            ControllerKind::SteamDeck
        } else {
            ControllerKind::Generic
        }
    }

    /// Human-readable, translated family name. Sony's product names
    /// (DualShock 4, DualSense) are brands and stay as they are.
    pub fn label(self) -> SharedString {
        crate::register_catalogs();
        match self {
            ControllerKind::Xbox => t("bigpicture.controller.xbox"),
            ControllerKind::PlayStation4 => SharedString::new_static("DualShock 4"),
            ControllerKind::PlayStation5 => SharedString::new_static("DualSense"),
            ControllerKind::Nintendo => t("bigpicture.controller.nintendo"),
            ControllerKind::SteamDeck => t("bigpicture.controller.steam"),
            ControllerKind::Generic => t("bigpicture.controller.generic"),
        }
    }

    /// Glyph printed on the button that produces `input` on this family.
    pub fn glyph(self, input: GamepadInput) -> &'static str {
        let playstation = matches!(
            self,
            ControllerKind::PlayStation4 | ControllerKind::PlayStation5
        );
        match input {
            GamepadInput::Confirm if playstation => "✕",
            GamepadInput::Back if playstation => "○",
            GamepadInput::Alt if playstation => "□",
            GamepadInput::Menu if playstation => "△",
            GamepadInput::PrevPage if playstation => "L1",
            GamepadInput::NextPage if playstation => "R1",
            GamepadInput::PrevPage if self == ControllerKind::Nintendo => "L",
            GamepadInput::NextPage if self == ControllerKind::Nintendo => "R",
            GamepadInput::Confirm => "A",
            GamepadInput::Back => "B",
            GamepadInput::Alt => "X",
            GamepadInput::Menu => "Y",
            GamepadInput::PrevPage => "LB",
            GamepadInput::NextPage => "RB",
            GamepadInput::Guide => "Guide",
            GamepadInput::Up | GamepadInput::Down | GamepadInput::Left | GamepadInput::Right => {
                "D-pad"
            }
        }
    }

    /// Whether the face buttons are labelled the Nintendo way, which swaps
    /// the positional south/east and west/north meanings.
    pub fn swaps_face_buttons(self) -> bool {
        self == ControllerKind::Nintendo
    }
}

/// Devices that report as gamepads but must never drive the UI.
pub fn is_ignored_device(vendor: Option<u16>, product: Option<u16>, name: &str) -> bool {
    // Logitech G29 racing wheel.
    if vendor == Some(0x046d) && product == Some(0xc24f) {
        return true;
    }
    let name = name.to_ascii_lowercase();
    name.contains("motion sensors") || name.contains("touchpad")
}

/// A face or shoulder button by position (SDL / gilrs convention: south is the
/// bottom face button).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadButton {
    South,
    East,
    West,
    North,
    LeftShoulder,
    RightShoulder,
    Start,
    Select,
    Guide,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
}

/// The normalized input a physical button produces on a controller family.
pub fn map_button(button: PadButton, kind: ControllerKind) -> GamepadInput {
    let swap = kind.swaps_face_buttons();
    match button {
        PadButton::South if swap => GamepadInput::Back,
        PadButton::East if swap => GamepadInput::Confirm,
        PadButton::North if swap => GamepadInput::Alt,
        PadButton::West if swap => GamepadInput::Menu,
        PadButton::South => GamepadInput::Confirm,
        PadButton::East => GamepadInput::Back,
        PadButton::West => GamepadInput::Alt,
        PadButton::North => GamepadInput::Menu,
        PadButton::Start | PadButton::Select => GamepadInput::Menu,
        PadButton::LeftShoulder => GamepadInput::PrevPage,
        PadButton::RightShoulder => GamepadInput::NextPage,
        PadButton::Guide => GamepadInput::Guide,
        PadButton::DPadUp => GamepadInput::Up,
        PadButton::DPadDown => GamepadInput::Down,
        PadButton::DPadLeft => GamepadInput::Left,
        PadButton::DPadRight => GamepadInput::Right,
    }
}

/// Analog stick deflection beyond which a direction is pressed.
pub const STICK_PRESS: f32 = 0.5;
/// Deflection below which a pressed direction is released (hysteresis, so a
/// stick resting near the threshold does not chatter).
pub const STICK_RELEASE: f32 = 0.35;

/// One stick axis turned into a digital direction with hysteresis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StickAxis {
    pressed: Option<bool>,
}

/// Change of a stick axis's digital state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisChange {
    /// The positive (`true`) or negative (`false`) direction was pressed.
    Press(bool),
    /// The previously pressed direction was released.
    Release(bool),
}

impl StickAxis {
    /// Feed a new axis value in `-1.0..=1.0`; returns the transitions it
    /// causes (a flick from one side to the other releases, then presses).
    pub fn update(&mut self, value: f32) -> Vec<AxisChange> {
        let mut out = Vec::new();
        let target = if value >= STICK_PRESS {
            Some(true)
        } else if value <= -STICK_PRESS {
            Some(false)
        } else if value.abs() < STICK_RELEASE {
            None
        } else {
            self.pressed
        };
        if target != self.pressed {
            if let Some(old) = self.pressed {
                out.push(AxisChange::Release(old));
            }
            if let Some(new) = target {
                out.push(AxisChange::Press(new));
            }
            self.pressed = target;
        }
        out
    }
}

/// Repeat timing of a held direction: the press fires immediately, the first
/// repeat after `initial`, then every `interval`.
#[derive(Debug, Clone)]
pub struct Repeater {
    held: Option<(GamepadInput, Instant)>,
    initial: Duration,
    interval: Duration,
}

/// Lower bound of the repeat interval, so a zero setting cannot flood the UI.
const MIN_INTERVAL: Duration = Duration::from_millis(5);

impl Repeater {
    pub fn new(initial: Duration, interval: Duration) -> Self {
        Self {
            held: None,
            initial,
            interval: interval.max(MIN_INTERVAL),
        }
    }

    /// Change the timing; a direction being held keeps its next deadline.
    pub fn set_timing(&mut self, initial: Duration, interval: Duration) {
        self.initial = initial;
        self.interval = interval.max(MIN_INTERVAL);
    }

    /// Start holding `input`. Returns the input to emit now.
    pub fn press(&mut self, input: GamepadInput, now: Instant) -> GamepadInput {
        self.held = Some((input, now + self.initial));
        input
    }

    /// Stop holding `input` (ignored if another direction took over).
    pub fn release(&mut self, input: GamepadInput) {
        if self.held.is_some_and(|(held, _)| held == input) {
            self.held = None;
        }
    }

    /// Forget any held direction (controller disconnected, input disabled).
    pub fn reset(&mut self) {
        self.held = None;
    }

    pub fn held(&self) -> Option<GamepadInput> {
        self.held.map(|(input, _)| input)
    }

    /// When the next repeat is due, if a direction is held.
    pub fn deadline(&self) -> Option<Instant> {
        self.held.map(|(_, at)| at)
    }

    /// Emit a repeat when due. After a stall (the reader was blocked for longer
    /// than one interval) a single repeat fires and the schedule restarts from
    /// `now`, instead of bursting the missed repeats.
    pub fn poll(&mut self, now: Instant) -> Option<GamepadInput> {
        let (input, at) = self.held?;
        if now < at {
            return None;
        }
        let mut next = at + self.interval;
        if next <= now {
            next = now + self.interval;
        }
        self.held = Some((input, next));
        Some(input)
    }
}

/// Drops the same press arriving from a second device within a short window.
///
/// Steam Input exposes a virtual pad next to the physical one, so every press
/// arrives twice. Rather than guessing which device is the duplicate, the
/// second identical press from a different device within [`Self::WINDOW`] is
/// ignored.
#[derive(Debug, Default)]
pub struct Deduper {
    last: Option<(GamepadInput, usize, Instant)>,
}

impl Deduper {
    pub const WINDOW: Duration = Duration::from_millis(40);

    /// Whether a press of `input` from device `device` at `now` is new.
    pub fn accept(&mut self, input: GamepadInput, device: usize, now: Instant) -> bool {
        if let Some((last, last_device, at)) = self.last
            && last == input
            && last_device != device
            && now.saturating_duration_since(at) < Self::WINDOW
        {
            return false;
        }
        self.last = Some((input, device, now));
        true
    }
}

/// Controllers Steam asks the programs it starts to leave alone. While
/// Steam Input drives a controller it offers a virtual one instead and lists
/// the physical device in `SDL_GAMECONTROLLER_IGNORE_DEVICES` (or only the
/// virtual one in `SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT`). Reading both
/// would act on every press twice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SteamIgnoreList {
    ignore: Vec<(u16, u16)>,
    /// When set, every device not listed is ignored.
    only: Option<Vec<(u16, u16)>>,
}

impl SteamIgnoreList {
    /// The lists from this process's environment.
    pub fn from_env() -> Self {
        let var = |key: &str| std::env::var(key).ok();
        Self::parse(
            var("SDL_GAMECONTROLLER_IGNORE_DEVICES").as_deref(),
            var("SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT").as_deref(),
        )
    }

    /// Parse SDL's `0xVVVV/0xPPPP,…` lists. An empty "except" list means no
    /// restriction, as in SDL.
    pub fn parse(ignore: Option<&str>, except: Option<&str>) -> Self {
        let only = except.map(parse_ids).filter(|ids| !ids.is_empty());
        Self {
            ignore: ignore.map(parse_ids).unwrap_or_default(),
            only,
        }
    }

    /// Whether the device with these USB ids must be ignored. Devices that
    /// report no ids are kept.
    pub fn ignores(&self, vendor: Option<u16>, product: Option<u16>) -> bool {
        let (Some(vendor), Some(product)) = (vendor, product) else {
            return false;
        };
        let id = (vendor, product);
        self.only.as_ref().is_some_and(|only| !only.contains(&id)) || self.ignore.contains(&id)
    }
}

fn parse_ids(list: &str) -> Vec<(u16, u16)> {
    let hex = |s: &str| {
        let s = s.trim();
        let s = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);
        u16::from_str_radix(s, 16).ok()
    };
    list.split(',')
        .filter_map(|entry| {
            let (vendor, product) = entry.split_once('/')?;
            Some((hex(vendor)?, hex(product)?))
        })
        .collect()
}

/// The Guide button acts on a short press, when released: holding it is how
/// many controllers switch off, which must not toggle Big Picture.
#[derive(Debug, Default)]
pub struct GuideTap {
    pressed: Vec<(usize, Instant)>,
}

impl GuideTap {
    /// Longest hold that still counts as a press.
    pub const MAX_HOLD: Duration = Duration::from_millis(600);

    pub fn press(&mut self, device: usize, now: Instant) {
        self.pressed.retain(|(d, _)| *d != device);
        self.pressed.push((device, now));
    }

    /// Whether the release at `now` ends a short press on `device`.
    pub fn release(&mut self, device: usize, now: Instant) -> bool {
        let Some(ix) = self.pressed.iter().position(|(d, _)| *d == device) else {
            return false;
        };
        let (_, at) = self.pressed.swap_remove(ix);
        now.saturating_duration_since(at) <= Self::MAX_HOLD
    }
}

/// Where an input came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSource {
    Controller,
    Keyboard,
}

/// Steam Input can turn controller presses into key presses (its desktop
/// layout sends arrows, Enter and Escape) while Ember reads the same
/// controller. Whichever source reports an input first wins; the same input
/// from the other source right after is an echo and dropped.
#[derive(Debug, Default)]
pub struct EchoFilter {
    controller: HashMap<GamepadInput, Instant>,
    keyboard: HashMap<GamepadInput, Instant>,
}

impl EchoFilter {
    pub const WINDOW: Duration = Duration::from_millis(150);

    /// Whether `input` from `source` at `now` is new rather than an echo.
    pub fn accept(&mut self, source: InputSource, input: GamepadInput, now: Instant) -> bool {
        let (mine, other) = match source {
            InputSource::Controller => (&mut self.controller, &self.keyboard),
            InputSource::Keyboard => (&mut self.keyboard, &self.controller),
        };
        if other
            .get(&input)
            .is_some_and(|at| now.saturating_duration_since(*at) < Self::WINDOW)
        {
            return false;
        }
        mine.insert(input, now);
        true
    }
}

/// The input an unmodified key press stands for, for the keys Steam Input's
/// desktop layout sends.
pub fn keyboard_equivalent(key: &str) -> Option<GamepadInput> {
    Some(match key {
        "up" => GamepadInput::Up,
        "down" => GamepadInput::Down,
        "left" => GamepadInput::Left,
        "right" => GamepadInput::Right,
        "enter" => GamepadInput::Confirm,
        "escape" => GamepadInput::Back,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_ignore_lists() {
        let none = SteamIgnoreList::parse(None, None);
        assert!(!none.ignores(Some(0x28de), Some(0x1205)));
        let list = SteamIgnoreList::parse(Some("0x28de/0x1205, 0X045E/0x028e,junk"), Some(""));
        assert!(list.ignores(Some(0x28de), Some(0x1205)));
        assert!(list.ignores(Some(0x045e), Some(0x028e)));
        assert!(!list.ignores(Some(0x28de), Some(0x11ff)));
        assert!(!list.ignores(None, None));
        let only = SteamIgnoreList::parse(None, Some("0x28de/0x11ff"));
        assert!(!only.ignores(Some(0x28de), Some(0x11ff)));
        assert!(only.ignores(Some(0x054c), Some(0x0ce6)));
    }

    #[test]
    fn guide_acts_on_short_presses_only() {
        let t0 = Instant::now();
        let mut tap = GuideTap::default();
        assert!(!tap.release(0, t0));
        tap.press(0, t0);
        assert!(tap.release(0, t0 + ms(200)));
        assert!(!tap.release(0, t0 + ms(250)));
        tap.press(1, t0);
        assert!(!tap.release(0, t0 + ms(10)));
        assert!(!tap.release(1, t0 + GuideTap::MAX_HOLD + ms(1)));
    }

    #[test]
    fn key_echoes_of_controller_presses_are_dropped() {
        use InputSource::{Controller, Keyboard};
        let t0 = Instant::now();
        let mut f = EchoFilter::default();
        assert!(f.accept(Controller, GamepadInput::Confirm, t0));
        assert!(!f.accept(Keyboard, GamepadInput::Confirm, t0 + ms(20)));
        assert!(f.accept(Keyboard, GamepadInput::Back, t0 + ms(30)));
        assert!(!f.accept(Controller, GamepadInput::Back, t0 + ms(40)));
        assert!(f.accept(Controller, GamepadInput::Confirm, t0 + ms(60)));
        assert!(f.accept(Keyboard, GamepadInput::Confirm, t0 + ms(300)));
        assert_eq!(keyboard_equivalent("escape"), Some(GamepadInput::Back));
        assert_eq!(keyboard_equivalent("a"), None);
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn repeat_fires_after_initial_delay_then_every_interval() {
        let t0 = Instant::now();
        let mut r = Repeater::new(ms(300), ms(50));
        assert_eq!(r.press(GamepadInput::Down, t0), GamepadInput::Down);
        assert_eq!(r.deadline(), Some(t0 + ms(300)));
        assert_eq!(r.poll(t0 + ms(299)), None);
        assert_eq!(r.poll(t0 + ms(300)), Some(GamepadInput::Down));
        assert_eq!(r.poll(t0 + ms(320)), None);
        assert_eq!(r.poll(t0 + ms(350)), Some(GamepadInput::Down));
        assert_eq!(r.deadline(), Some(t0 + ms(400)));
    }

    #[test]
    fn repeat_does_not_burst_after_a_stall() {
        let t0 = Instant::now();
        let mut r = Repeater::new(ms(300), ms(50));
        r.press(GamepadInput::Up, t0);
        assert_eq!(r.poll(t0 + ms(1000)), Some(GamepadInput::Up));
        assert_eq!(r.poll(t0 + ms(1001)), None);
        assert_eq!(r.deadline(), Some(t0 + ms(1050)));
    }

    #[test]
    fn release_only_stops_the_held_direction() {
        let t0 = Instant::now();
        let mut r = Repeater::new(ms(300), ms(50));
        r.press(GamepadInput::Left, t0);
        r.press(GamepadInput::Right, t0 + ms(10));
        r.release(GamepadInput::Left);
        assert_eq!(r.held(), Some(GamepadInput::Right));
        r.release(GamepadInput::Right);
        assert_eq!(r.held(), None);
        assert_eq!(r.poll(t0 + ms(2000)), None);
        assert_eq!(r.deadline(), None);
    }

    #[test]
    fn zero_interval_is_clamped() {
        let t0 = Instant::now();
        let mut r = Repeater::new(ms(0), ms(0));
        r.press(GamepadInput::Up, t0);
        assert_eq!(r.poll(t0), Some(GamepadInput::Up));
        assert_eq!(r.poll(t0 + ms(1)), None);
        assert_eq!(r.poll(t0 + MIN_INTERVAL), Some(GamepadInput::Up));
    }

    #[test]
    fn stick_has_hysteresis() {
        let mut x = StickAxis::default();
        assert!(x.update(0.4).is_empty());
        assert_eq!(x.update(0.6), vec![AxisChange::Press(true)]);
        assert!(x.update(0.4).is_empty(), "inside the hysteresis band");
        assert_eq!(x.update(0.2), vec![AxisChange::Release(true)]);
        x.update(0.9);
        assert_eq!(
            x.update(-0.9),
            vec![AxisChange::Release(true), AxisChange::Press(false)]
        );
    }

    #[test]
    fn duplicate_presses_from_a_second_device_are_dropped() {
        let t0 = Instant::now();
        let mut d = Deduper::default();
        assert!(d.accept(GamepadInput::Confirm, 1, t0));
        assert!(!d.accept(GamepadInput::Confirm, 2, t0 + ms(5)));
        assert!(
            d.accept(GamepadInput::Confirm, 1, t0 + ms(10)),
            "same device repeats"
        );
        assert!(d.accept(GamepadInput::Confirm, 2, t0 + ms(100)));
        assert!(d.accept(GamepadInput::Back, 1, t0 + ms(101)));
    }

    #[test]
    fn detects_playstation_5_before_4() {
        assert_eq!(
            ControllerKind::detect(
                Some(0x054c),
                Some(0x0ce6),
                "Sony Interactive Entertainment Wireless Controller"
            ),
            ControllerKind::PlayStation5
        );
        assert_eq!(
            ControllerKind::detect(
                Some(0x054c),
                Some(0x0df2),
                "DualSense Edge Wireless Controller"
            ),
            ControllerKind::PlayStation5
        );
        assert_eq!(
            ControllerKind::detect(Some(0x054c), Some(0x09cc), "Wireless Controller"),
            ControllerKind::PlayStation4
        );
        assert_eq!(
            ControllerKind::detect(None, None, "DualSense Wireless Controller (PS5 DualShock)"),
            ControllerKind::PlayStation5
        );
        assert_eq!(
            ControllerKind::detect(Some(0x057e), Some(0x2009), "Pro Controller"),
            ControllerKind::Nintendo
        );
        assert_eq!(
            ControllerKind::detect(Some(0x28de), Some(0x1205), "Steam Deck"),
            ControllerKind::SteamDeck
        );
        assert_eq!(
            ControllerKind::detect(Some(0x045e), Some(0x0b12), "Xbox Wireless Controller"),
            ControllerKind::Xbox
        );
        assert_eq!(
            ControllerKind::detect(None, None, "8BitDo"),
            ControllerKind::Generic
        );
    }

    #[test]
    fn nintendo_layout_swaps_face_buttons() {
        assert_eq!(
            map_button(PadButton::South, ControllerKind::Xbox),
            GamepadInput::Confirm
        );
        assert_eq!(
            map_button(PadButton::East, ControllerKind::Nintendo),
            GamepadInput::Confirm
        );
        assert_eq!(
            map_button(PadButton::South, ControllerKind::Nintendo),
            GamepadInput::Back
        );
        assert_eq!(
            map_button(PadButton::North, ControllerKind::PlayStation5),
            GamepadInput::Menu
        );
        assert_eq!(
            ControllerKind::PlayStation5.glyph(GamepadInput::Confirm),
            "✕"
        );
    }

    #[test]
    fn ignores_wheels_and_sensor_nodes() {
        assert!(is_ignored_device(Some(0x046d), Some(0xc24f), "G29"));
        assert!(is_ignored_device(
            Some(0x054c),
            Some(0x0ce6),
            "DualSense Wireless Controller Motion Sensors"
        ));
        assert!(!is_ignored_device(
            Some(0x054c),
            Some(0x0ce6),
            "DualSense Wireless Controller"
        ));
    }
}
