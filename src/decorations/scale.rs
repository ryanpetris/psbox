//! The display scale xwayland-satellite is told (`xwayland-scale`).
//!
//! Satellite sizes the X screen from an output's current mode and works at
//! the scale the compositor gives it: `wl_output.scale`, overridden by a
//! surface's `wp_fractional_scale_v1.preferred_scale`. X11 apps get that
//! scale as `Xft.dpi` and, rounded down, as GTK's window scale. To change
//! the scale X11 apps work at, the proxy rewrites those events, and makes
//! the mode the output's logical size at that scale, so the X screen
//! matches the windows satellite sizes at it.

use crate::spec::XwaylandScale;

/// An output's logical size, as the proxy's own connection sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputGeometry {
    /// The size in the compositor's logical coordinates.
    pub logical: (i32, i32),
    /// Turned a quarter: the logical size runs across the mode.
    pub rotated: bool,
}

/// The current mode satellite is told for an output whose mode is `mode`,
/// logical size `logical` (in the compositor's orientation), and whole
/// scale `whole`: the compositor's `wl_output.scale`, a fractional scale
/// rounded up, or when it sends none, the mode's size over the logical
/// size, rounded to the 120ths fractional scales come in and then up.
pub fn mode(
    scale: XwaylandScale,
    mode: (i32, i32),
    logical: Option<(i32, i32)>,
    rotated: bool,
    whole: Option<i32>,
) -> (i32, i32) {
    let Some((lw, lh)) = logical.filter(|(w, h)| *w > 0 && *h > 0) else {
        return mode;
    };
    let (lw, lh) = if rotated { (lh, lw) } else { (lw, lh) };
    match scale {
        XwaylandScale::Native => mode,
        XwaylandScale::RoundUp => {
            let ratio = || ((mode.0 as f64 / lw as f64) * 120.0).round() / 120.0;
            let s = whole.unwrap_or_else(|| ratio().ceil() as i32).clamp(1, 16);
            let (w, h) = (lw.saturating_mul(s), lh.saturating_mul(s));
            // At a whole scale the mode already is the logical size times
            // it, give or take the compositor's rounding of the logical size.
            match (mode.0 - w).abs() < s && (mode.1 - h).abs() < s {
                true => mode,
                false => (w, h),
            }
        }
        XwaylandScale::Compositor => (lw, lh),
    }
}

/// A `wp_fractional_scale_v1.preferred_scale`, in 120ths.
pub fn preferred(scale: XwaylandScale, preferred: u32) -> u32 {
    match scale {
        XwaylandScale::Native => preferred,
        XwaylandScale::RoundUp => preferred.div_ceil(120).max(1).saturating_mul(120),
        XwaylandScale::Compositor => 120,
    }
}

/// A whole scale: `wl_output.scale` or `wl_surface.preferred_buffer_scale`.
pub fn whole(scale: XwaylandScale, factor: i32) -> i32 {
    match scale {
        XwaylandScale::Native | XwaylandScale::RoundUp => factor,
        XwaylandScale::Compositor => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_150_percent_panel_in_each_mode() {
        let panel = |s| mode(s, (2880, 1920), Some((1920, 1280)), false, Some(2));
        assert_eq!(panel(XwaylandScale::Native), (2880, 1920));
        assert_eq!(panel(XwaylandScale::RoundUp), (3840, 2560));
        assert_eq!(panel(XwaylandScale::Compositor), (1920, 1280));
        assert_eq!(
            [180, 240, 120].map(|p| preferred(XwaylandScale::RoundUp, p)),
            [240, 240, 120]
        );
        assert_eq!(preferred(XwaylandScale::Native, 180), 180);
        assert_eq!(preferred(XwaylandScale::Compositor, 180), 120);
        assert_eq!(whole(XwaylandScale::RoundUp, 2), 2);
        assert_eq!(whole(XwaylandScale::Compositor, 2), 1);
    }

    #[test]
    fn whole_scales_and_turned_outputs() {
        let round_up = |m, l, r, s| mode(XwaylandScale::RoundUp, m, Some(l), r, s);
        assert_eq!(round_up((3840, 2160), (1920, 1080), false, Some(2)), (3840, 2160));
        // A logical size the compositor rounded: still the whole scale 3,
        // given or worked out.
        assert_eq!(round_up((1366, 768), (455, 256), false, Some(3)), (1366, 768));
        assert_eq!(round_up((1366, 768), (455, 256), false, None), (1366, 768));
        assert_eq!(round_up((2880, 1920), (1920, 1280), false, None), (3840, 2560));
        assert_eq!(round_up((2880, 1920), (1280, 1920), true, Some(2)), (3840, 2560));
        let compositor = |l, r| mode(XwaylandScale::Compositor, (2880, 1920), l, r, Some(2));
        assert_eq!(compositor(Some((1280, 1920)), true), (1920, 1280));
        // Without the logical size, the mode stays as it is.
        assert_eq!(compositor(None, false), (2880, 1920));
    }
}
