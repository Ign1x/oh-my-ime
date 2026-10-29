//! Engine windows for X11 applications.
//!
//! Compositors show an input popup only while a Wayland text field has focus. X11
//! applications (XIM, or the Fcitx5 module under XWayland) have none, so the bridge
//! shows the active engine's window on a surface of the daemon's own instead: a mirror,
//! a layer surface placed at the application's text cursor (see [`crate::placement`]).
//! It shows the engine's buffers with the scale, transform and viewport of the engine's
//! surface, which stays unmapped meanwhile; frame callbacks go to the mirror, and
//! pointer events on the mirror reach the engine as events on its own surface.

use std::rc::{Rc, Weak};

use wl_proxy::fixed::Fixed;
use wl_proxy::object::{Object, ObjectCoreApi, ObjectUtils};
use wl_proxy::protocols::fractional_scale_v1::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wl_proxy::protocols::fractional_scale_v1::wp_fractional_scale_v1::{
    WpFractionalScaleV1, WpFractionalScaleV1Handler,
};
use wl_proxy::protocols::viewporter::wp_viewport::WpViewport;
use wl_proxy::protocols::viewporter::wp_viewporter::WpViewporter;
use wl_proxy::protocols::wayland::wl_buffer::WlBuffer;
use wl_proxy::protocols::wayland::wl_compositor::WlCompositor;
use wl_proxy::protocols::wayland::wl_output::{WlOutput, WlOutputTransform};
use wl_proxy::protocols::wayland::wl_surface::{WlSurface, WlSurfaceHandler};
use wl_proxy::protocols::wlr_layer_shell_unstable_v1::zwlr_layer_shell_v1::{
    ZwlrLayerShellV1, ZwlrLayerShellV1Layer,
};
use wl_proxy::protocols::wlr_layer_shell_unstable_v1::zwlr_layer_surface_v1::{
    ZwlrLayerSurfaceV1, ZwlrLayerSurfaceV1Anchor, ZwlrLayerSurfaceV1Handler,
};

use super::{BridgeSurface, Shared};
use crate::placement::{self, Rect};

/// The layer surface namespace, for compositor rules (for example, to turn off
/// Hyprland's animations: `layerrule = noanim, omime-popup`).
const NAMESPACE: &str = "omime-popup";

/// Where engine windows go while an X11 application has focus.
#[derive(Debug, Clone)]
pub struct X11Target {
    /// The output the text cursor is on.
    pub output: Rc<WlOutput>,
    /// The output's logical geometry.
    pub output_rect: Rect,
    /// The text cursor, in compositor coordinates.
    pub cursor: Rect,
}

impl PartialEq for X11Target {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.output, &other.output)
            && self.output_rect == other.output_rect
            && self.cursor == other.cursor
    }
}

/// The daemon's own globals that mirrors are made of.
#[derive(Debug, Default, Clone)]
pub struct OwnGlobals {
    pub compositor: Option<Rc<WlCompositor>>,
    pub layer_shell: Option<Rc<ZwlrLayerShellV1>>,
    pub viewporter: Option<Rc<WpViewporter>>,
    pub fractional_scale: Option<Rc<WpFractionalScaleManagerV1>>,
}

impl OwnGlobals {
    pub fn can_mirror(&self) -> bool {
        self.compositor.is_some() && self.layer_shell.is_some()
    }
}

/// The double-buffered state of an engine surface that decides how its buffer is shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceState {
    pub scale: i32,
    pub transform: WlOutputTransform,
    /// The viewport's source rectangle: x, y, width, height.
    pub source: Option<[Fixed; 4]>,
    /// The viewport's destination size.
    pub destination: Option<(i32, i32)>,
}

impl Default for SurfaceState {
    fn default() -> Self {
        Self {
            scale: 1,
            transform: WlOutputTransform::NORMAL,
            source: None,
            destination: None,
        }
    }
}

impl SurfaceState {
    /// The size of the surface, in surface coordinates, with a buffer of `buffer` pixels.
    pub fn logical_size(&self, buffer: (i32, i32)) -> (i32, i32) {
        if let Some(size) = self.destination {
            return size;
        }
        if let Some([_, _, width, height]) = self.source {
            return (
                width.to_i32_round_towards_nearest(),
                height.to_i32_round_towards_nearest(),
            );
        }
        // Odd transforms turn the buffer by 90 or 270 degrees.
        let (width, height) = if self.transform.0 % 2 == 1 {
            (buffer.1, buffer.0)
        } else {
            buffer
        };
        let scale = self.scale.max(1);
        (width / scale, height / scale)
    }
}

/// A layer surface showing an engine window.
pub struct Mirror {
    pub surface: Rc<WlSurface>,
    layer: Rc<ZwlrLayerSurfaceV1>,
    viewport: Option<Rc<WpViewport>>,
    fractional_scale: Option<Rc<WpFractionalScaleV1>>,
    pub target: X11Target,
    /// Whether the compositor configured the layer surface, which must happen before
    /// it gets a buffer.
    pub configured: bool,
    /// The buffer attached to the surface.
    pub current: Option<Rc<WlBuffer>>,
    /// What the compositor was told, so that only changes are sent.
    state: SurfaceState,
    size: Option<(i32, i32)>,
    position: Option<(i32, i32)>,
    /// The text cursor relative to the window, as last reported to the engine.
    pub reported_cursor: Option<Rect>,
}

impl Mirror {
    /// Creates a mirror for `window`, whose size is `size`, and asks the compositor to
    /// configure it.
    pub fn create(
        own: &OwnGlobals,
        shared: &Shared,
        window: &Rc<WlSurface>,
        target: X11Target,
        size: (i32, i32),
    ) -> Option<Self> {
        let (Some(compositor), Some(layer_shell)) = (&own.compositor, &own.layer_shell) else {
            return None;
        };
        let surface = compositor.new_send_create_surface();
        surface.set_forward_to_client(false);
        surface.set_handler(MirrorSurface {
            window: Rc::downgrade(window),
        });
        let layer = layer_shell.new_send_get_layer_surface(
            &surface,
            Some(&target.output),
            ZwlrLayerShellV1Layer::OVERLAY,
            NAMESPACE,
        );
        layer.set_forward_to_client(false);
        layer.set_handler(MirrorLayer {
            window: Rc::downgrade(window),
        });
        layer.send_set_anchor(ZwlrLayerSurfaceV1Anchor::TOP | ZwlrLayerSurfaceV1Anchor::LEFT);
        // Margins count from the edges of the output, not from panels.
        layer.send_set_exclusive_zone(-1);
        let viewport = own.viewporter.as_ref().map(|viewporter| {
            let viewport = viewporter.new_send_get_viewport(&surface);
            viewport.set_forward_to_client(false);
            viewport
        });
        let fractional_scale = own.fractional_scale.as_ref().map(|manager| {
            let scale = manager.new_send_get_fractional_scale(&surface);
            scale.set_forward_to_client(false);
            scale.set_handler(MirrorScale {
                window: Rc::downgrade(window),
                shared: shared.clone(),
            });
            scale
        });
        let mut mirror = Self {
            surface,
            layer,
            viewport,
            fractional_scale,
            target,
            configured: false,
            current: None,
            state: SurfaceState::default(),
            size: None,
            position: None,
            reported_cursor: None,
        };
        mirror.place(size);
        // The first commit has no buffer; the compositor answers with a configure.
        mirror.surface.send_commit();
        Some(mirror)
    }

    /// Moves the mirror to where a window of `size` goes for the text cursor. Returns
    /// whether anything changed.
    fn place(&mut self, size: (i32, i32)) -> bool {
        let output = self.target.output_rect;
        let position = placement::place(self.target.cursor, size, output);
        let mut changed = false;
        if self.size != Some(size) {
            let (width, height) = (size.0.max(1), size.1.max(1));
            self.layer
                .send_set_size(width.unsigned_abs(), height.unsigned_abs());
            self.size = Some(size);
            changed = true;
        }
        if self.position != Some(position) {
            let (left, top) = (position.0 - output.0, position.1 - output.1);
            self.layer.send_set_margin(top, 0, 0, left);
            self.position = Some(position);
            changed = true;
        }
        changed
    }

    /// The text cursor relative to the window.
    pub fn cursor(&self) -> Rect {
        let (x, y, width, height) = self.target.cursor;
        let (left, top) = self.position.unwrap_or((x, y));
        (x - left, y - top, width, height)
    }

    /// Prepares the next commit: shows `buffer` (or keeps showing the current one) with
    /// `state`, as a window of `size`. Returns whether anything changed.
    pub fn present(
        &mut self,
        buffer: Option<Rc<WlBuffer>>,
        state: &SurfaceState,
        size: (i32, i32),
    ) -> bool {
        let mut changed = self.place(size);
        if state.scale != self.state.scale {
            self.surface.send_set_buffer_scale(state.scale);
        }
        if state.transform != self.state.transform {
            self.surface.send_set_buffer_transform(state.transform);
        }
        if let Some(viewport) = &self.viewport {
            if state.source != self.state.source {
                let unset = Fixed::from_i32_saturating(-1);
                let [x, y, width, height] = state.source.unwrap_or([unset; 4]);
                viewport.send_set_source(x, y, width, height);
            }
            if state.destination != self.state.destination {
                let (width, height) = state.destination.unwrap_or((-1, -1));
                viewport.send_set_destination(width, height);
            }
        }
        changed |= *state != self.state;
        self.state = *state;
        if let Some(buffer) = buffer {
            self.surface.send_attach(Some(&buffer), 0, 0);
            self.surface.send_damage_buffer(0, 0, i32::MAX, i32::MAX);
            self.current = Some(buffer);
            changed = true;
        }
        changed
    }

    /// Destroys the mirror. Returns the buffer it showed, which the compositor releases.
    pub fn destroy(self) -> Option<Rc<WlBuffer>> {
        if let Some(scale) = &self.fractional_scale {
            scale.unset_handler();
            scale.send_destroy();
        }
        if let Some(viewport) = &self.viewport {
            viewport.send_destroy();
        }
        self.layer.unset_handler();
        self.layer.send_destroy();
        self.surface.unset_handler();
        self.surface.send_destroy();
        self.current
    }
}

/// The engine window a mirror shows.
pub fn mirrored_window(surface: &Rc<WlSurface>) -> Option<Rc<WlSurface>> {
    surface
        .try_get_handler_ref::<MirrorSurface>()
        .ok()
        .and_then(|mirror| mirror.window.upgrade())
}

/// Calls `f` with the bridge state of `window`, if it still has one.
fn with_window(window: &Weak<WlSurface>, f: impl FnOnce(&mut BridgeSurface, &Rc<WlSurface>)) {
    let Some(window) = window.upgrade() else {
        return;
    };
    if let Ok(mut handler) = window.try_get_handler_mut::<BridgeSurface>() {
        f(&mut handler, &window);
    }
}

struct MirrorSurface {
    window: Weak<WlSurface>,
}

impl WlSurfaceHandler for MirrorSurface {
    fn handle_preferred_buffer_scale(&mut self, _slf: &Rc<WlSurface>, factor: i32) {
        if let Some(window) = self.window.upgrade()
            && window.version() >= 6
        {
            window.send_preferred_buffer_scale(factor);
        }
    }
}

struct MirrorLayer {
    window: Weak<WlSurface>,
}

impl ZwlrLayerSurfaceV1Handler for MirrorLayer {
    fn handle_configure(
        &mut self,
        slf: &Rc<ZwlrLayerSurfaceV1>,
        serial: u32,
        _width: u32,
        _height: u32,
    ) {
        slf.send_ack_configure(serial);
        with_window(&self.window, BridgeSurface::mirror_configured);
    }

    fn handle_closed(&mut self, _slf: &Rc<ZwlrLayerSurfaceV1>) {
        with_window(&self.window, |handler, _| handler.mirror_closed());
    }
}

/// Hands the scale the compositor prefers for a mirror to the engine.
struct MirrorScale {
    window: Weak<WlSurface>,
    shared: Shared,
}

impl WpFractionalScaleV1Handler for MirrorScale {
    fn handle_preferred_scale(&mut self, _slf: &Rc<WpFractionalScaleV1>, scale: u32) {
        self.shared.borrow_mut().last_scale = Some(scale);
        with_window(&self.window, |handler, _| {
            if let Some(engine_scale) = handler.fractional_scale.as_ref().and_then(Weak::upgrade) {
                engine_scale.send_preferred_scale(scale);
                handler.mirror_scale = true;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_viewport_decides_the_size_before_the_buffer() {
        let mut state = SurfaceState {
            scale: 2,
            ..SurfaceState::default()
        };
        assert_eq!(state.logical_size((300, 100)), (150, 50));
        state.transform = WlOutputTransform::_90;
        assert_eq!(state.logical_size((300, 100)), (50, 150));
        state.source = Some([
            Fixed::ZERO,
            Fixed::ZERO,
            Fixed::from_i32_saturating(40),
            Fixed::from_i32_saturating(30),
        ]);
        assert_eq!(state.logical_size((300, 100)), (40, 30));
        state.destination = Some((200, 60));
        assert_eq!(state.logical_size((300, 100)), (200, 60));
    }
}
