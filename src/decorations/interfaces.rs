//! The globals the bridge may bind through the proxy: those
//! xwayland-satellite uses, at the highest version the proxy has message
//! signatures for. Everything else the compositor offers is hidden from the
//! bridge.

use wayland_backend::protocol::Interface;
use wayland_client::Proxy;
use wayland_client::protocol::{wl_compositor, wl_data_device_manager, wl_output, wl_seat, wl_shm, wl_subcompositor};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1;
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1;
use wayland_protocols::wp::linux_drm_syncobj::v1::client::wp_linux_drm_syncobj_manager_v1;
use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_pointer_constraints_v1;
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_manager_v1;
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_manager_v1;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_manager_v2;
use wayland_protocols::wp::viewporter::client::wp_viewporter;
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1;
use wayland_protocols::xdg::decoration::zv1::client::{zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1};
use wayland_protocols::xdg::shell::client::xdg_wm_base;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1;

const GLOBALS: &[fn() -> &'static Interface] = &[
    wl_compositor::WlCompositor::interface,
    wl_subcompositor::WlSubcompositor::interface,
    wl_shm::WlShm::interface,
    wl_seat::WlSeat::interface,
    wl_output::WlOutput::interface,
    wl_data_device_manager::WlDataDeviceManager::interface,
    xdg_wm_base::XdgWmBase::interface,
    zxdg_output_manager_v1::ZxdgOutputManagerV1::interface,
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1::interface,
    xdg_activation_v1::XdgActivationV1::interface,
    wp_viewporter::WpViewporter::interface,
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1::interface,
    zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1::interface,
    wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1::interface,
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1::interface,
    zwp_pointer_constraints_v1::ZwpPointerConstraintsV1::interface,
    zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1::interface,
    zwp_tablet_manager_v2::ZwpTabletManagerV2::interface,
];

/// The interface of a global the bridge may bind.
pub fn global(name: &str) -> Option<&'static Interface> {
    GLOBALS.iter().map(|f| f()).find(|i| i.name == name)
}

/// The decoration manager the proxy offers when the compositor has none.
pub fn decoration_manager() -> &'static Interface {
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1::interface()
}

pub fn toplevel_decoration() -> &'static Interface {
    zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1::interface()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globals_are_found_by_name() {
        assert_eq!(global("wl_seat").map(|i| i.name), Some("wl_seat"));
        assert_eq!(
            global("zxdg_decoration_manager_v1").map(|i| i.name),
            Some(decoration_manager().name)
        );
        assert!(global("wp_presentation").is_none());
    }
}
