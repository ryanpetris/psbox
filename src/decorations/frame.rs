//! Title bars: an Adwaita frame (sctk-adwaita) for each X11 window that
//! asks for server-side decorations, drawn as subsurfaces of its surface,
//! and the proxy's own seat, whose pointer drives them.

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use sctk_adwaita::{AdwaitaFrame, FrameConfig};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::csd_frame::{
    DecorationsFrame, FrameAction, FrameClick, ResizeEdge, WindowManagerCapabilities, WindowState,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{
    BTN_LEFT, BTN_RIGHT, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::subcompositor::SubcompositorState;
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};
use wayland_backend::client::ObjectId;
use wayland_client::protocol::{wl_output, wl_pointer, wl_seat, wl_surface};
use wayland_client::{Connection, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel};

use super::proxy::{Proxy as Bridge, lock};
use super::scale::OutputGeometry;

/// What every frame is made with.
pub struct Kit {
    pub conn: Connection,
    pub qh: QueueHandle<State>,
    pub compositor: Arc<CompositorState>,
    pub subcompositor: Arc<SubcompositorState>,
    pub shm: Shm,
    pub config: FrameConfig,
}

/// A forwarded surface as the base of a frame.
struct Base(wl_surface::WlSurface);

impl WaylandSurface for Base {
    fn wl_surface(&self) -> &wl_surface::WlSurface {
        &self.0
    }
}

pub struct Frame {
    /// The bridge's id for the window's surface.
    pub surface: u32,
    xdg_surface: xdg_surface::XdgSurface,
    toplevel: xdg_toplevel::XdgToplevel,
    inner: AdwaitaFrame<State>,
    /// The window's size without the frame, once it has content.
    content: Option<(NonZeroU32, NonZeroU32)>,
    /// The window geometry last set, which includes the title bar.
    geometry: Option<(i32, i32, i32, i32)>,
    /// The window geometry the bridge asked for, within its surface.
    pub bridge_geometry: Option<(i32, i32, i32, i32)>,
}

impl Frame {
    pub fn new(
        kit: &Kit,
        surface: u32,
        surface_up: &ObjectId,
        xdg_up: &ObjectId,
        toplevel_up: &ObjectId,
    ) -> Result<Frame> {
        let lost = |_| anyhow!("the window is gone");
        let base = Base(wl_surface::WlSurface::from_id(&kit.conn, surface_up.clone()).map_err(lost)?);
        let inner = AdwaitaFrame::new(
            &base,
            &kit.shm,
            kit.compositor.clone(),
            kit.subcompositor.clone(),
            kit.qh.clone(),
            kit.config.clone(),
        )
        .map_err(|e| anyhow!("{e}"))?;
        Ok(Frame {
            surface,
            xdg_surface: xdg_surface::XdgSurface::from_id(&kit.conn, xdg_up.clone()).map_err(lost)?,
            toplevel: xdg_toplevel::XdgToplevel::from_id(&kit.conn, toplevel_up.clone()).map_err(lost)?,
            inner,
            content: None,
            geometry: None,
            bridge_geometry: None,
        })
    }

    /// The height of the title bar, 0 while it is not shown (fullscreen).
    pub fn offset(&self) -> i32 {
        -self.inner.location().1
    }

    pub fn set_title(&mut self, title: &str) {
        self.inner.set_title(title);
    }

    pub fn set_scale(&mut self, scale: i32) {
        self.inner.set_scaling_factor(f64::from(scale.max(1)));
    }

    /// Takes the toplevel's new states, and returns the size to pass on to
    /// the bridge: the compositor's, without the title bar.
    pub fn configure(&mut self, states: &[u8], width: i32, height: i32) -> (i32, i32) {
        let state = window_state(states);
        self.inner.update_state(state);
        let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) else {
            return (width, height);
        };
        match (NonZeroU32::new(w), NonZeroU32::new(h)) {
            (Some(w), Some(h)) => match self.inner.subtract_borders(w, h) {
                (Some(w), Some(h)) => (w.get() as i32, h.get() as i32),
                (Some(w), None) => (w.get() as i32, 1),
                _ => (width, height),
            },
            _ => (width, height),
        }
    }

    pub fn set_capabilities(&mut self, caps: &[u8]) {
        self.inner.update_wm_capabilities(capabilities(caps));
    }

    /// Before the window's surface commits: follows its size, draws what
    /// changed, and sets the window geometry to take in the title bar. The
    /// frame's subsurfaces are synchronized, so all of it shows with that
    /// commit.
    pub fn commit(&mut self, size: Option<(i32, i32)>) {
        let size = size.and_then(|(w, h)| Some((clamp_size(w)?, clamp_size(h)?)));
        if let Some((w, h)) = size
            && self.content != Some((w, h))
        {
            self.inner.resize(w, h);
            self.content = Some((w, h));
        }
        let Some((w, h)) = self.content else {
            return;
        };
        if self.inner.is_dirty() {
            self.inner.draw();
        }
        let (x, y, bw, bh) = self.bridge_geometry.unwrap_or((0, 0, w.get() as i32, h.get() as i32));
        let (lx, ly) = self.inner.location();
        let (bw, bh) = (
            clamp_size(bw).unwrap_or(NonZeroU32::MIN),
            clamp_size(bh).unwrap_or(NonZeroU32::MIN),
        );
        let (gw, gh) = self.inner.add_borders(bw.get(), bh.get());
        let geometry = (x.saturating_add(lx), y.saturating_add(ly), gw as i32, gh as i32);
        if self.geometry != Some(geometry) {
            self.xdg_surface
                .set_window_geometry(geometry.0, geometry.1, geometry.2, geometry.3);
            self.geometry = Some(geometry);
        }
    }

    /// Draws what changed since the window's last commit: button highlights
    /// under the pointer, or the title.
    pub fn redraw(&mut self) {
        if self.content.is_some() && self.inner.is_dirty() {
            self.inner.draw();
        }
    }

    /// Takes the frame off its window, which keeps the window geometry the
    /// bridge asked for, or its whole surface.
    pub fn remove(self) {
        if self.geometry.is_some()
            && let Some((w, h)) = self.content
        {
            let (x, y, w, h) = self.bridge_geometry.unwrap_or((0, 0, w.get() as i32, h.get() as i32));
            self.xdg_surface.set_window_geometry(x, y, w, h);
        }
    }

    pub fn act(&self, action: FrameAction, seat: &wl_seat::WlSeat, serial: u32) {
        match action {
            FrameAction::Minimize => self.toplevel.set_minimized(),
            FrameAction::Maximize => self.toplevel.set_maximized(),
            FrameAction::UnMaximize => self.toplevel.unset_maximized(),
            FrameAction::Move => self.toplevel._move(seat, serial),
            FrameAction::ShowMenu(x, y) => self.toplevel.show_window_menu(seat, serial, x, y),
            FrameAction::Resize(edge) => {
                use xdg_toplevel::ResizeEdge as Edge;
                let edge = match edge {
                    ResizeEdge::Top => Edge::Top,
                    ResizeEdge::Bottom => Edge::Bottom,
                    ResizeEdge::Left => Edge::Left,
                    ResizeEdge::TopLeft => Edge::TopLeft,
                    ResizeEdge::BottomLeft => Edge::BottomLeft,
                    ResizeEdge::Right => Edge::Right,
                    ResizeEdge::TopRight => Edge::TopRight,
                    ResizeEdge::BottomRight => Edge::BottomRight,
                    _ => return,
                };
                self.toplevel.resize(seat, serial, edge);
            }
            _ => {}
        }
    }
}

/// The largest window side the frame is drawn for, which keeps its buffer
/// sizes in range at any scale.
const MAX_SIZE: u32 = 32767;

/// A size from the bridge, in `1..=MAX_SIZE`; `None` when it has none.
fn clamp_size(v: i32) -> Option<NonZeroU32> {
    NonZeroU32::new(u32::try_from(v).ok()?.min(MAX_SIZE))
}

/// `xdg_toplevel` states, from a `configure` event's array.
fn window_state(states: &[u8]) -> WindowState {
    let bits = states.as_chunks::<4>().0.iter().map(|c| u32::from_ne_bytes(*c));
    bits.fold(WindowState::empty(), |s, v| {
        s | match v {
            1 => WindowState::MAXIMIZED,
            2 => WindowState::FULLSCREEN,
            3 => WindowState::RESIZING,
            4 => WindowState::ACTIVATED,
            5 => WindowState::TILED_LEFT,
            6 => WindowState::TILED_RIGHT,
            7 => WindowState::TILED_TOP,
            8 => WindowState::TILED_BOTTOM,
            9 => WindowState::SUSPENDED,
            _ => WindowState::empty(),
        }
    })
}

/// What the compositor offers, from a `wm_capabilities` event's array.
fn capabilities(caps: &[u8]) -> WindowManagerCapabilities {
    let bits = caps.as_chunks::<4>().0.iter().map(|c| u32::from_ne_bytes(*c));
    bits.fold(WindowManagerCapabilities::empty(), |s, v| {
        s | match v {
            1 => WindowManagerCapabilities::WINDOW_MENU,
            2 => WindowManagerCapabilities::MAXIMIZE,
            3 => WindowManagerCapabilities::FULLSCREEN,
            4 => WindowManagerCapabilities::MINIMIZE,
            _ => WindowManagerCapabilities::empty(),
        }
    })
}

/// The proxy's own side of the compositor connection: its seat and the
/// frames' objects.
pub struct State {
    pub bridge: Arc<Mutex<Bridge>>,
    pub registry: RegistryState,
    pub seat_state: SeatState,
    /// Outputs, which sctk follows for the frames' surfaces.
    pub output_state: OutputState,
    pub compositor: Arc<CompositorState>,
    pub shm: Shm,
    pub pointer: Option<ThemedPointer>,
    /// The toplevel whose frame the pointer is over.
    pub focus: Option<u32>,
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        conn: &Connection,
        _: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let bridge = self.bridge.clone();
        let mut bridge = lock(&bridge);
        for event in events {
            let (x, y) = event.position;
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    let id = event.surface.id();
                    let over = bridge
                        .frames
                        .iter_mut()
                        .find_map(|(t, f)| f.inner.click_point_moved(Duration::ZERO, &id, x, y).map(|c| (*t, c)));
                    match over {
                        Some((toplevel, cursor)) => {
                            if let Some(old) = self.focus.filter(|f| *f != toplevel)
                                && let Some(frame) = bridge.frames.get_mut(&old)
                            {
                                frame.inner.click_point_left();
                            }
                            self.focus = Some(toplevel);
                            if let Some(p) = &self.pointer {
                                let _ = p.set_cursor(conn, cursor);
                            }
                        }
                        None => {
                            if let Some(frame) = self.focus.take().and_then(|old| bridge.frames.get_mut(&old)) {
                                frame.inner.click_point_left();
                            }
                        }
                    }
                }
                PointerEventKind::Leave { .. } => {
                    if let Some(frame) = self.focus.take().and_then(|old| bridge.frames.get_mut(&old)) {
                        frame.inner.click_point_left();
                    }
                }
                PointerEventKind::Press { button, serial, time }
                | PointerEventKind::Release { button, serial, time } => {
                    let pressed = matches!(event.kind, PointerEventKind::Press { .. });
                    let click = match button {
                        BTN_LEFT => FrameClick::Normal,
                        BTN_RIGHT => FrameClick::Alternate,
                        _ => continue,
                    };
                    let Some(toplevel) = self.focus else {
                        continue;
                    };
                    let action = bridge
                        .frames
                        .get_mut(&toplevel)
                        .and_then(|f| f.inner.on_click(Duration::from_millis(u64::from(time)), click, pressed));
                    if let (Some(action), Some(seat)) = (
                        action,
                        pointer
                            .data::<smithay_client_toolkit::seat::pointer::PointerData<()>>()
                            .map(|d| d.seat().clone()),
                    ) {
                        bridge.frame_action(toplevel, action, &seat, serial);
                    }
                }
                PointerEventKind::Axis { .. } => {}
            }
        }
        for frame in bridge.frames.values_mut() {
            frame.redraw();
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            let surface = self.compositor.create_surface(qh);
            self.pointer = self
                .seat_state
                .get_pointer_with_theme::<_, ()>(qh, &seat, self.shm.wl_shm(), surface, ThemeSpec::default())
                .ok();
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer
            && let Some(p) = self.pointer.take()
        {
            p.pointer().release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.take_output(&output);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.take_output(&output);
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if let Some(info) = self.output_state.info(&output) {
            lock(&self.bridge).set_output(info.id, None);
        }
    }
}

impl State {
    /// Passes an output's logical size on to the proxy, for the scale the
    /// bridge is told.
    fn take_output(&mut self, output: &wl_output::WlOutput) {
        let Some(info) = self.output_state.info(output) else {
            return;
        };
        use wl_output::Transform;
        let geometry = info.logical_size.map(|logical| OutputGeometry {
            logical,
            rotated: matches!(
                info.transform,
                Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270
            ),
        });
        lock(&self.bridge).set_output(info.id, geometry);
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(State);
delegate_dispatch2!(State);

#[cfg(test)]
mod tests {
    use super::*;

    fn array(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    #[test]
    fn bridge_sizes_are_clamped() {
        assert_eq!(clamp_size(0), None);
        assert_eq!(clamp_size(-5), None);
        assert_eq!(clamp_size(400).map(NonZeroU32::get), Some(400));
        assert_eq!(clamp_size(i32::MAX).map(NonZeroU32::get), Some(MAX_SIZE));
    }

    #[test]
    fn toplevel_arrays_become_frame_states() {
        assert_eq!(
            window_state(&array(&[4, 1, 99])),
            WindowState::ACTIVATED | WindowState::MAXIMIZED
        );
        assert_eq!(window_state(&[1, 0]), WindowState::empty());
        assert_eq!(
            capabilities(&array(&[2, 4])),
            WindowManagerCapabilities::MAXIMIZE | WindowManagerCapabilities::MINIMIZE
        );
    }
}
