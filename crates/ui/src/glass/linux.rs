//! Asking a Linux compositor to blur behind the window.
//!
//! GPUI's `Blurred` background only speaks `org_kde_kwin_blur`, which KWin 6.7
//! removed for the standard `ext-background-effect-v1`. Compositors announce
//! that protocol and whether blur is on right now, so Irori asks them rather
//! than guess from the desktop's name, and binds it itself. A Wayland surface
//! can only be named on the connection that made it, so the requests go over
//! GPUI's own connection, through the window's raw handles.

use std::env;
use std::ffi::c_void;
use std::mem::ManuallyDrop;

use anyhow::Context as _;
use gpui_kit::component::window_paddings;
use gpui_kit::{App, Edges, Global, Pixels, Size, Window, WindowBackgroundAppearance, WindowId};
use once_cell::sync::Lazy;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_compositor, wl_region, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1 as manager, ext_background_effect_surface_v1 as effect,
};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, PropMode};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// How this compositor blurs what is behind a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Blur {
    /// `ext-background-effect-v1`, asked for here.
    BackgroundEffect {
        /// KWin 6.7 measures the blur region from the window geometry, not
        /// from the surface as the protocol says.
        from_window_geometry: bool,
    },
    /// `org_kde_kwin_blur`, which GPUI asks for: KWin before 6.7.
    KwinProtocol,
    /// `_KDE_NET_WM_BLUR_BEHIND_REGION`, asked for here: KWin on X11 and for
    /// X11 windows under Xwayland.
    KwinHint,
    /// Blurs anything see-through unasked (Hyprland before the protocol).
    Unasked,
    None,
}

impl Blur {
    pub(super) fn appearance(self) -> WindowBackgroundAppearance {
        match self {
            Blur::KwinProtocol => WindowBackgroundAppearance::Blurred,
            _ => WindowBackgroundAppearance::Transparent,
        }
    }
}

/// Detected once at start. Blur switched on later in the desktop's settings
/// is picked up by the next start.
pub(super) static BLUR: Lazy<Blur> = Lazy::new(|| {
    let blur = probe_wayland().or_else(probe_x11).unwrap_or(Blur::None);
    tracing::info!("desktop blur: {blur:?}");
    blur
});

/// Ask for blur behind the visible part of `window`, or stop asking. Cheap
/// when nothing changed, so it can follow every resize. Failing leaves the
/// window see-through without blur, so it is only logged.
pub(super) fn request(on: bool, window: &Window, cx: &mut App) {
    if !matches!(*BLUR, Blur::BackgroundEffect { .. } | Blur::KwinHint) {
        return;
    }
    let frame = on.then(|| visible_frame(window.viewport_size(), window_paddings(window)));
    let wanted = Requested(window.window_handle().window_id(), frame);
    if cx.try_global::<Requested>() == Some(&wanted) {
        return;
    }
    cx.set_global(wanted);
    let result = match *BLUR {
        Blur::BackgroundEffect {
            from_window_geometry,
        } => {
            let frame = frame.map(|f| surface_region(f, from_window_geometry));
            request_background_effect(frame, window, cx)
        }
        _ => request_kwin_hint(frame, window, cx),
    };
    if let Err(e) = result {
        tracing::warn!("asking the compositor for blur: {e:#}");
    }
}

/// What the compositor was last asked for.
#[derive(PartialEq)]
struct Requested(WindowId, Option<Frame>);

impl Global for Requested {}

/// `[x, y, width, height]` in logical pixels.
type Frame = [i32; 4];

/// The window as the user sees it, inside its surface. Client-side decorated
/// windows keep a margin around it for their shadow. Compositors only clip
/// blur to the surface, so blurring all of it leaves a band of bare blur
/// around the window.
fn visible_frame(surface: Size<Pixels>, paddings: Edges<Pixels>) -> Frame {
    let pixels = |value: Pixels| f32::from(value).round() as i32;
    [
        pixels(paddings.left),
        pixels(paddings.top),
        pixels(surface.width - paddings.left - paddings.right),
        pixels(surface.height - paddings.top - paddings.bottom),
    ]
}

/// `frame` in the coordinates a compositor expects of a blur region.
fn surface_region(frame: Frame, from_window_geometry: bool) -> Frame {
    let [x, y, width, height] = frame;
    if from_window_geometry {
        [0, 0, width, height]
    } else {
        [x, y, width, height]
    }
}

const KWIN_HINT: &[u8] = b"_KDE_NET_WM_BLUR_BEHIND_REGION";

/// `None` without a Wayland session to ask, which is GPUI's cue for X11 too.
fn probe_wayland() -> Option<Blur> {
    if env::var_os("WAYLAND_DISPLAY").is_none_or(|v| v.is_empty()) {
        return None;
    }
    let conn = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<Capabilities>(&conn).ok()?;
    let has = |interface: &str| {
        let list = globals.contents();
        list.with_list(|list| list.iter().any(|g| g.interface == interface))
    };
    let mut capabilities = Capabilities::default();
    if globals
        .bind::<manager::ExtBackgroundEffectManagerV1, _, _>(&queue.handle(), 1..=1, ())
        .is_ok()
    {
        queue.roundtrip(&mut capabilities).ok()?;
        if capabilities.blur {
            return Some(Blur::BackgroundEffect {
                from_window_geometry: has("org_kde_plasma_shell"),
            });
        }
    }
    Some(if has("org_kde_kwin_blur_manager") {
        Blur::KwinProtocol
    } else if env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        Blur::Unasked
    } else {
        Blur::None
    })
}

/// KWin announces the hint as a property of the root window.
fn probe_x11() -> Option<Blur> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let hint = conn.intern_atom(true, KWIN_HINT).ok()?.reply().ok()?.atom;
    let announced = conn.list_properties(root).ok()?.reply().ok()?.atoms;
    Some(if hint != 0 && announced.contains(&hint) {
        Blur::KwinHint
    } else {
        Blur::None
    })
}

fn request_kwin_hint(frame: Option<Frame>, window: &Window, cx: &mut App) -> anyhow::Result<()> {
    let handle = HasWindowHandle::window_handle(window).context("reading the window handle")?;
    let RawWindowHandle::Xcb(handle) = handle.as_raw() else {
        return Ok(());
    };
    let id = handle.window.get();
    if !cx.has_global::<KwinHint>() {
        let hint = KwinHint::connect()?;
        cx.set_global(hint);
    }
    let KwinHint { conn, hint } = cx.global::<KwinHint>();
    match frame {
        Some(frame) => conn
            .change_property32(
                PropMode::REPLACE,
                id,
                *hint,
                AtomEnum::CARDINAL,
                &device_frame(frame, window.scale_factor()),
            )?
            .check()?,
        None => conn.delete_property(id, *hint)?.check()?,
    }
    Ok(())
}

/// X11 regions are in device pixels.
fn device_frame(frame: Frame, scale: f32) -> [u32; 4] {
    frame.map(|value| (value as f32 * scale).round().max(0.) as u32)
}

/// A connection of its own: X11 window properties belong to the server, so
/// it needs nothing of GPUI's.
struct KwinHint {
    conn: RustConnection,
    hint: Atom,
}

impl Global for KwinHint {}

impl KwinHint {
    fn connect() -> anyhow::Result<Self> {
        let (conn, _) = x11rb::connect(None).context("connecting to X11")?;
        let hint = conn.intern_atom(false, KWIN_HINT)?.reply()?.atom;
        Ok(Self { conn, hint })
    }
}

fn request_background_effect(
    frame: Option<Frame>,
    window: &Window,
    cx: &mut App,
) -> anyhow::Result<()> {
    let id = window.window_handle().window_id();
    let (surface, display) = wayland_handles(window)?;
    if !cx.has_global::<BackgroundEffect>() {
        let effects = BackgroundEffect::connect(display)?;
        cx.set_global(effects);
    }
    cx.global_mut::<BackgroundEffect>().set(id, surface, frame)
}

fn wayland_handles(window: &Window) -> anyhow::Result<(*mut c_void, *mut c_void)> {
    let surface = HasWindowHandle::window_handle(window).context("reading the window handle")?;
    let display = HasDisplayHandle::display_handle(window).context("reading the display")?;
    match (surface.as_raw(), display.as_raw()) {
        (RawWindowHandle::Wayland(surface), RawDisplayHandle::Wayland(display)) => {
            Ok((surface.surface.as_ptr(), display.display.as_ptr()))
        }
        _ => anyhow::bail!("the window is not a Wayland surface"),
    }
}

/// The background effect objects, on GPUI's Wayland connection.
struct BackgroundEffect(ManuallyDrop<Client>);

impl Global for BackgroundEffect {}

struct Client {
    conn: Connection,
    /// Never dispatched after setup, but its objects need it to stay alive.
    queue: EventQueue<Capabilities>,
    manager: manager::ExtBackgroundEffectManagerV1,
    compositor: wl_compositor::WlCompositor,
    /// One per surface or the compositor kills the connection. A window that
    /// was closed and reopened is a new surface.
    surface_effect: Option<(WindowId, effect::ExtBackgroundEffectSurfaceV1)>,
}

impl BackgroundEffect {
    fn connect(display: *mut c_void) -> anyhow::Result<Self> {
        // SAFETY: `display` is GPUI's live wl_display, which outlives this
        // client: `ManuallyDrop` keeps these objects from touching it while
        // the application shuts down. Requests and the one roundtrip here run
        // on the main thread, like all of GPUI's Wayland traffic.
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, queue) =
            registry_queue_init::<Capabilities>(&conn).context("listing Wayland globals")?;
        let qh = queue.handle();
        let manager = globals
            .bind(&qh, 1..=1, ())
            .context("binding the background effect manager")?;
        let compositor = globals
            .bind(&qh, 1..=1, ())
            .context("binding wl_compositor")?;
        Ok(Self(ManuallyDrop::new(Client {
            conn,
            queue,
            manager,
            compositor,
            surface_effect: None,
        })))
    }

    fn set(
        &mut self,
        id: WindowId,
        surface: *mut c_void,
        frame: Option<Frame>,
    ) -> anyhow::Result<()> {
        let client = &mut *self.0;
        let qh = client.queue.handle();
        if let Some((known, effect)) = &client.surface_effect
            && *known != id
        {
            effect.destroy();
            client.surface_effect = None;
        }
        let effect = match &client.surface_effect {
            Some((_, effect)) => effect,
            None => {
                // SAFETY: `surface` is the live wl_surface of the window the
                // caller holds, read from its raw handle just now.
                let surface = unsafe {
                    ObjectId::from_ptr(wl_surface::WlSurface::interface(), surface.cast())
                }
                .context("reading the window's surface")?;
                let surface = wl_surface::WlSurface::from_id(&client.conn, surface)
                    .context("reading the window's surface")?;
                let effect = client.manager.get_background_effect(&surface, &qh, ());
                &client.surface_effect.insert((id, effect)).1
            }
        };
        match frame {
            Some([x, y, width, height]) => {
                let region = client.compositor.create_region(&qh, ());
                region.add(x, y, width, height);
                effect.set_blur_region(Some(&region));
                region.destroy();
            }
            None => effect.set_blur_region(None),
        }
        // Takes effect on the surface's next commit, GPUI's next frame.
        client.conn.flush().context("talking to the compositor")
    }
}

#[derive(Default)]
struct Capabilities {
    blur: bool,
}

impl Dispatch<manager::ExtBackgroundEffectManagerV1, ()> for Capabilities {
    fn event(
        state: &mut Self,
        _: &manager::ExtBackgroundEffectManagerV1,
        event: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let manager::Event::Capabilities {
            flags: WEnum::Value(flags),
        } = event
        {
            state.blur = flags.contains(manager::Capability::Blur);
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Capabilities {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Capabilities: ignore wl_compositor::WlCompositor);
delegate_noop!(Capabilities: ignore wl_region::WlRegion);
delegate_noop!(Capabilities: ignore effect::ExtBackgroundEffectSurfaceV1);

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{px, size};

    #[test]
    fn the_frame_leaves_out_the_shadow_margin() {
        let frame = visible_frame(size(px(1440.), px(900.)), Edges::all(px(20.)));
        assert_eq!(frame, [20, 20, 1400, 860]);
    }

    #[test]
    fn a_maximized_window_is_all_frame() {
        let frame = visible_frame(size(px(1920.), px(1080.)), Edges::all(px(0.)));
        assert_eq!(frame, [0, 0, 1920, 1080]);
    }

    #[test]
    fn a_tiled_edge_has_no_margin() {
        let paddings = Edges {
            top: px(0.),
            right: px(20.),
            bottom: px(0.),
            left: px(0.),
        };
        let frame = visible_frame(size(px(960.), px(1080.)), paddings);
        assert_eq!(frame, [0, 0, 940, 1080]);
    }

    #[test]
    fn only_kwin_measures_from_the_window_geometry() {
        let frame = [20, 20, 1400, 860];
        assert_eq!(surface_region(frame, false), frame);
        assert_eq!(surface_region(frame, true), [0, 0, 1400, 860]);
    }

    #[test]
    fn x11_regions_are_scaled_to_device_pixels() {
        assert_eq!(device_frame([20, 20, 1400, 860], 1.5), [30, 30, 2100, 1290]);
    }
}
