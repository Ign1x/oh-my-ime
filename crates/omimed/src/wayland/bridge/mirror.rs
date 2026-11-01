//! Engine windows for X11 applications.
//!
//! Compositors show an input popup only while a Wayland text field has focus. X11
//! applications (XIM, or the Fcitx5 module under XWayland) have none, nor do Wayland
//! applications that use the Fcitx5 module, so the bridge shows the active engine's
//! window on surfaces of the daemon's own instead: a mirror, placed at the application's
//! text cursor (see [`crate::placement`]). It shows the engine's buffers with the scale,
//! transform and viewport of the engine's surface, which stays unmapped meanwhile; frame
//! callbacks go to the mirror, and pointer events on the mirror reach the engine as
//! events on its own surface.
//!
//! Mirrors are subsurfaces of a [`Stage`]: a transparent layer surface over the whole
//! output that takes no input, which stays while such an application has focus.
//! Compositors animate layer surfaces that appear, move or change size (Hyprland
//! does), and candidate windows do all of that with every key; subsurfaces appear,
//! move and change size on the spot.

use std::rc::{Rc, Weak};

use wl_proxy::fixed::Fixed;
use wl_proxy::object::{Object, ObjectCoreApi, ObjectUtils};
use wl_proxy::protocols::fractional_scale_v1::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wl_proxy::protocols::fractional_scale_v1::wp_fractional_scale_v1::{
    WpFractionalScaleV1, WpFractionalScaleV1Handler,
};
use wl_proxy::protocols::single_pixel_buffer_v1::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1;
use wl_proxy::protocols::viewporter::wp_viewport::WpViewport;
use wl_proxy::protocols::viewporter::wp_viewporter::WpViewporter;
use wl_proxy::protocols::wayland::wl_buffer::WlBuffer;
use wl_proxy::protocols::wayland::wl_compositor::WlCompositor;
use wl_proxy::protocols::wayland::wl_output::{WlOutput, WlOutputTransform};
use wl_proxy::protocols::wayland::wl_subcompositor::WlSubcompositor;
use wl_proxy::protocols::wayland::wl_subsurface::WlSubsurface;
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
/// Hyprland's animations: `layerrule = no_anim on, match:namespace omime-popup`).
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
    pub subcompositor: Option<Rc<WlSubcompositor>>,
    pub layer_shell: Option<Rc<ZwlrLayerShellV1>>,
    pub viewporter: Option<Rc<WpViewporter>>,
    pub single_pixel: Option<Rc<WpSinglePixelBufferManagerV1>>,
    pub fractional_scale: Option<Rc<WpFractionalScaleManagerV1>>,
}

impl OwnGlobals {
    pub fn can_mirror(&self) -> bool {
        self.compositor.is_some()
            && self.subcompositor.is_some()
            && self.layer_shell.is_some()
            && self.viewporter.is_some()
            && self.single_pixel.is_some()
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

/// A transparent layer surface over the whole output, which mirrors are subsurfaces of.
pub struct Stage {
    pub surface: Rc<WlSurface>,
    layer: Rc<ZwlrLayerSurfaceV1>,
    viewport: Rc<WpViewport>,
    buffer: Rc<WlBuffer>,
    fractional_scale: Option<Rc<WpFractionalScaleV1>>,
    pub output: Rc<WlOutput>,
    /// Whether the compositor configured the stage and it is mapped: mirrors may show
    /// windows on it.
    pub configured: bool,
    /// The fractional scale the compositor prefers for the output, as it last said.
    pub scale: Option<u32>,
}

impl Stage {
    /// Creates a stage over `output` and asks the compositor to configure it.
    pub fn create(own: &OwnGlobals, shared: &Shared, output: &Rc<WlOutput>) -> Option<Self> {
        let (Some(compositor), Some(layer_shell), Some(viewporter), Some(single_pixel)) = (
            &own.compositor,
            &own.layer_shell,
            &own.viewporter,
            &own.single_pixel,
        ) else {
            return None;
        };
        let surface = compositor.new_send_create_surface();
        surface.set_forward_to_client(false);
        // Clicks go through to the windows below, except on the mirrors.
        let region = compositor.new_send_create_region();
        region.set_forward_to_client(false);
        surface.send_set_input_region(Some(&region));
        region.send_destroy();
        let layer = layer_shell.new_send_get_layer_surface(
            &surface,
            Some(output),
            ZwlrLayerShellV1Layer::OVERLAY,
            NAMESPACE,
        );
        layer.set_forward_to_client(false);
        layer.set_handler(StageLayer {
            shared: shared.clone(),
        });
        layer.send_set_anchor(
            ZwlrLayerSurfaceV1Anchor::TOP
                | ZwlrLayerSurfaceV1Anchor::BOTTOM
                | ZwlrLayerSurfaceV1Anchor::LEFT
                | ZwlrLayerSurfaceV1Anchor::RIGHT,
        );
        // Over panels too: candidates may open anywhere.
        layer.send_set_exclusive_zone(-1);
        let viewport = viewporter.new_send_get_viewport(&surface);
        viewport.set_forward_to_client(false);
        let buffer = single_pixel.new_send_create_u32_rgba_buffer(0, 0, 0, 0);
        buffer.set_forward_to_client(false);
        let fractional_scale = own.fractional_scale.as_ref().map(|manager| {
            let scale = manager.new_send_get_fractional_scale(&surface);
            scale.set_forward_to_client(false);
            scale.set_handler(StageScale {
                shared: shared.clone(),
            });
            scale
        });
        // The first commit has no buffer; the compositor answers with a configure.
        surface.send_commit();
        Some(Self {
            surface,
            layer,
            viewport,
            buffer,
            fractional_scale,
            output: output.clone(),
            configured: false,
            scale: None,
        })
    }

    /// The compositor configured the stage `width` × `height`: it shows its transparent
    /// pixel over all of it.
    fn configure(&mut self, width: u32, height: u32) {
        let size = |value: u32| i32::try_from(value).unwrap_or(i32::MAX).max(1);
        self.viewport
            .send_set_destination(size(width), size(height));
        if !self.configured {
            self.surface.send_attach(Some(&self.buffer), 0, 0);
            self.surface.send_damage_buffer(0, 0, 1, 1);
        }
        self.surface.send_commit();
        self.configured = true;
    }

    /// Destroys the stage. Its mirrors must be gone.
    pub fn destroy(self) {
        if let Some(scale) = &self.fractional_scale {
            scale.unset_handler();
            scale.send_destroy();
        }
        self.viewport.send_destroy();
        self.layer.unset_handler();
        self.layer.send_destroy();
        self.surface.send_destroy();
        self.buffer.send_destroy();
    }
}

/// A subsurface of the stage showing an engine window.
pub struct Mirror {
    pub surface: Rc<WlSurface>,
    subsurface: Rc<WlSubsurface>,
    /// The surface of the stage it is on.
    pub stage: Rc<WlSurface>,
    viewport: Option<Rc<WpViewport>>,
    pub target: X11Target,
    /// The buffer attached to the surface.
    pub current: Option<Rc<WlBuffer>>,
    /// What the compositor was told, so that only changes are sent.
    state: SurfaceState,
    position: Option<(i32, i32)>,
    /// The text cursor relative to the window, as last reported to the engine.
    pub reported_cursor: Option<Rect>,
}

impl Mirror {
    /// Creates a mirror for `window` on the stage `stage`.
    pub fn create(
        own: &OwnGlobals,
        window: &Rc<WlSurface>,
        stage: &Rc<WlSurface>,
        target: X11Target,
    ) -> Option<Self> {
        let (Some(compositor), Some(subcompositor)) = (&own.compositor, &own.subcompositor) else {
            return None;
        };
        let surface = compositor.new_send_create_surface();
        surface.set_forward_to_client(false);
        surface.set_handler(MirrorSurface {
            window: Rc::downgrade(window),
        });
        let subsurface = subcompositor.new_send_get_subsurface(&surface, stage);
        subsurface.set_forward_to_client(false);
        // Its frames show as they come, not with the stage's commits.
        subsurface.send_set_desync();
        let viewport = own.viewporter.as_ref().map(|viewporter| {
            let viewport = viewporter.new_send_get_viewport(&surface);
            viewport.set_forward_to_client(false);
            viewport
        });
        Some(Self {
            surface,
            subsurface,
            stage: stage.clone(),
            viewport,
            target,
            current: None,
            state: SurfaceState::default(),
            position: None,
            reported_cursor: None,
        })
    }

    /// Moves the mirror to where a window of `size` goes for the text cursor. Returns
    /// whether it moved; the move takes effect with the stage's next commit.
    fn place(&mut self, size: (i32, i32)) -> bool {
        let output = self.target.output_rect;
        let position = placement::place(self.target.cursor, size, output);
        if self.position == Some(position) {
            return false;
        }
        let (left, top) = (position.0 - output.0, position.1 - output.1);
        self.subsurface.send_set_position(left, top);
        self.position = Some(position);
        true
    }

    /// The text cursor relative to the window.
    pub fn cursor(&self) -> Rect {
        let (x, y, width, height) = self.target.cursor;
        let (left, top) = self.position.unwrap_or((x, y));
        (x - left, y - top, width, height)
    }

    /// Prepares the next commit: shows `buffer` (or keeps showing the current one) with
    /// `state`, as a window of `size`. Returns whether the mirror's surface changed and
    /// whether it moved (which takes a commit of the stage).
    pub fn present(
        &mut self,
        buffer: Option<Rc<WlBuffer>>,
        state: &SurfaceState,
        size: (i32, i32),
    ) -> (bool, bool) {
        let moved = self.place(size);
        let mut changed = false;
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
        (changed, moved)
    }

    /// Destroys the mirror. Returns the buffer it showed, which the compositor releases.
    pub fn destroy(self) -> Option<Rc<WlBuffer>> {
        if let Some(viewport) = &self.viewport {
            viewport.send_destroy();
        }
        self.subsurface.send_destroy();
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

struct StageLayer {
    shared: Shared,
}

impl ZwlrLayerSurfaceV1Handler for StageLayer {
    fn handle_configure(
        &mut self,
        slf: &Rc<ZwlrLayerSurfaceV1>,
        serial: u32,
        width: u32,
        height: u32,
    ) {
        slf.send_ack_configure(serial);
        let first = {
            let mut shared = self.shared.borrow_mut();
            let Some(stage) = shared.stage.as_mut() else {
                return;
            };
            let first = !stage.configured;
            stage.configure(width, height);
            first
        };
        if first {
            // Windows waiting for the stage can show now.
            super::refresh_windows(&self.shared);
        }
    }

    fn handle_closed(&mut self, _slf: &Rc<ZwlrLayerSurfaceV1>) {
        // Its output went away, say. No new stage until the target changes.
        {
            let mut shared = self.shared.borrow_mut();
            shared.stage_closed_for = shared.x11.clone();
        }
        super::retire_stage(&self.shared);
    }
}

/// Hands the scale the compositor prefers for the stage's output to the engines whose
/// windows are on it.
struct StageScale {
    shared: Shared,
}

impl WpFractionalScaleV1Handler for StageScale {
    fn handle_preferred_scale(&mut self, _slf: &Rc<WpFractionalScaleV1>, scale: u32) {
        let windows = {
            let mut shared = self.shared.borrow_mut();
            shared.last_scale = Some(scale);
            if let Some(stage) = shared.stage.as_mut() {
                stage.scale = Some(scale);
            }
            shared.windows.clone()
        };
        for window in windows.iter().filter_map(Weak::upgrade) {
            if let Ok(mut handler) = window.try_get_handler_mut::<BridgeSurface>()
                && handler.has_mirror()
            {
                handler.send_mirror_scale(scale);
            }
        }
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
