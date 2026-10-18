//! Title bars for X11 windows, for `xwayland.mode: 'builtin'`.
//!
//! `psbox decorations` runs inside the sandbox between `xwayland-satellite`
//! (the bridge, in this module's names) and the host compositor.
//! Satellite's Wayland connection is descriptor 3 (the supervisor accepts it
//! on the socket satellite's `WAYLAND_DISPLAY` names), and the proxy
//! connects to the compositor like any other client. Everything passes
//! through, except that when the compositor draws no title bars (GNOME), the
//! proxy offers satellite server-side decorations
//! (`zxdg_decoration_manager_v1`) and draws them itself on each X11 window
//! that asks: an Adwaita title bar with resize borders, made of subsurfaces
//! on the proxy's connection.

mod frame;
mod interfaces;
mod proxy;
mod scale;
pub mod wire;

use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;

use anyhow::{Context, Result};
use sctk_adwaita::FrameConfig;
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::shm::Shm;
use smithay_client_toolkit::subcompositor::SubcompositorState;
use wayland_client::Connection;
use wayland_client::backend::WaylandError;
use wayland_client::globals::registry_queue_init;

use self::frame::{Kit, State};
use self::proxy::{Proxy, lock};
use crate::spec::XwaylandScale;
use crate::sys::fd::poll_events;

/// Runs until satellite closes its connection, or the compositor's ends.
pub fn run(key: &str, scale: XwaylandScale) -> Result<i32> {
    match serve(key, scale) {
        Ok(()) => Ok(0),
        Err(e) => {
            eprintln!("psbox[{key}]: decorations: {e:#}");
            Ok(1)
        }
    }
}

fn serve(key: &str, scale: XwaylandScale) -> Result<()> {
    // SAFETY: the supervisor passes satellite's connection as descriptor 3,
    // and nothing else in this process uses it.
    let bridge = unsafe { OwnedFd::from_raw_fd(3) };
    // SAFETY: fcntl on a descriptor this process owns.
    crate::sys::cvt_int(unsafe { libc::fcntl(bridge.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) })
        .context("take xwayland-satellite's connection")?;
    // Polled while the proxy holds the socket itself.
    let bridge_fd = bridge.try_clone().context("take xwayland-satellite's connection")?;

    let conn = Connection::connect_to_env().context("connect to the compositor")?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).context("list the compositor's globals")?;
    let qh = queue.handle();
    let compositor = Arc::new(CompositorState::bind(&globals, &qh).context("bind wl_compositor")?);
    let subcompositor = Arc::new(
        SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh).context("bind wl_subcompositor")?,
    );
    // The mode satellite is told is the logical size at a scale, which
    // takes xdg-output.
    let has_xdg_output = globals
        .contents()
        .with_list(|list| list.iter().any(|g| g.interface == "zxdg_output_manager_v1"));
    let scale = match scale {
        XwaylandScale::Native => scale,
        _ if has_xdg_output => scale,
        _ => {
            eprintln!(
                "psbox[{key}]: decorations: the compositor has no zxdg_output_manager_v1, which xwayland.scale \"{scale}\" needs; X11 apps scale as with \"native\""
            );
            XwaylandScale::Native
        }
    };
    let decorate = !globals
        .contents()
        .with_list(|list| list.iter().any(|g| g.interface == "zxdg_decoration_manager_v1"));
    let kit = match decorate {
        true => Some(Kit {
            conn: conn.clone(),
            qh: qh.clone(),
            compositor: compositor.clone(),
            subcompositor,
            shm: Shm::bind(&globals, &qh).context("bind wl_shm")?,
            // Asks the settings portal for the color scheme.
            config: FrameConfig::auto(),
        }),
        false => None,
    };
    let bridge = Proxy::new(conn.backend(), wire::Socket::new(bridge), decorate, kit, scale, key);
    let mut state = State {
        bridge: bridge.clone(),
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        shm: Shm::bind(&globals, &qh).context("bind wl_shm")?,
        pointer: None,
        focus: None,
    };
    // The outputs' modes and logical sizes (from xdg-output, which needs a
    // second round trip), before the bridge's first request is read.
    for _ in 0..2 {
        queue.roundtrip(&mut state).context("list the compositor's outputs")?;
    }

    loop {
        queue
            .dispatch_pending(&mut state)
            .context("handle the compositor's events")?;
        let bridge_writes = {
            let mut b = lock(&bridge);
            if let Some(e) = b.failed.take() {
                return Err(e);
            }
            b.bridge.flush().context("write to xwayland-satellite")?;
            b.bridge.wants_write()
        };
        let compositor_writes = match conn.flush() {
            Ok(()) => false,
            Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => true,
            Err(e) => return Err(e).context("write to the compositor"),
        };
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let ready = poll_events(
            &[
                (guard.connection_fd(), interest(compositor_writes)),
                (bridge_fd.as_fd(), interest(bridge_writes)),
            ],
            -1,
        )
        .context("wait for messages")?;
        if ready[0] & !libc::POLLOUT != 0 {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e).context("read from the compositor"),
            }
        } else {
            drop(guard);
        }
        if ready[1] & !libc::POLLOUT != 0 && !lock(&bridge).read_bridge()? {
            return Ok(());
        }
    }
}

fn interest(writes: bool) -> libc::c_short {
    match writes {
        true => libc::POLLIN | libc::POLLOUT,
        false => libc::POLLIN,
    }
}
