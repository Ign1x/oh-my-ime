//! The UI bridge: engine UI connections multiplexed onto the daemon's connection.
//!
//! Engines connect their UI toolkit to a private socket served by the daemon. All their
//! requests are forwarded on the daemon's own compositor connection, so their surfaces
//! belong to the input method as far as the compositor is concerned, and can be given
//! the `zwp_input_popup_surface_v2` role. See `docs/architecture.md` for the design.
//!
//! The rules implemented here:
//!
//! - Engines see a virtual registry with a whitelist of globals. Binds are performed on
//!   the daemon's registry after validating the version.
//! - `xdg_wm_base` is virtual. A toplevel is configured immediately with `0x0` (the
//!   client picks its size) and its surface becomes an input popup when it is mapped.
//! - Only the active engine's windows are mapped, once the engine asked for it. Hiding
//!   injects `attach(NULL)`; while hidden, buffers and frame callbacks are held by the
//!   bridge instead of the compositor.
//! - For X11 applications, which have no Wayland text field for a popup to follow, a
//!   window is shown on a layer surface of the daemon's own instead (see [`mirror`]).
//!   Frame callbacks of windows are always answered by the bridge, so they follow a
//!   window between its popup and its mirror.
//! - Popup roles are destroyed before their surfaces, and everything an engine leaves
//!   behind is destroyed in dependency order when it disconnects.
//! - Hosted input methods, unmodified Wayland input methods running as engines, get a
//!   virtual input method and virtual keyboard (see [`hosted`]); their popup surfaces
//!   are engine windows too.

mod hosted;
mod mirror;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use tracing::{debug, warn};
use wl_proxy::client::{Client, ClientHandler};
use wl_proxy::fixed::Fixed;
use wl_proxy::object::{Object, ObjectCoreApi, ObjectRcUtils, ObjectUtils};
use wl_proxy::protocols::ObjectInterface;
use wl_proxy::protocols::cursor_shape_v1::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;
use wl_proxy::protocols::cursor_shape_v1::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wl_proxy::protocols::fractional_scale_v1::wp_fractional_scale_manager_v1::{
    WpFractionalScaleManagerV1, WpFractionalScaleManagerV1Handler,
};
use wl_proxy::protocols::fractional_scale_v1::wp_fractional_scale_v1::{
    WpFractionalScaleV1, WpFractionalScaleV1Handler,
};
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_v2::ZwpInputMethodV2;
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_popup_surface_v2::{
    ZwpInputPopupSurfaceV2, ZwpInputPopupSurfaceV2Handler,
};
use wl_proxy::protocols::linux_dmabuf_v1::zwp_linux_buffer_params_v1::{
    ZwpLinuxBufferParamsV1, ZwpLinuxBufferParamsV1Flags, ZwpLinuxBufferParamsV1Handler,
};
use wl_proxy::protocols::linux_dmabuf_v1::zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1;
use wl_proxy::protocols::linux_dmabuf_v1::zwp_linux_dmabuf_v1::{
    ZwpLinuxDmabufV1, ZwpLinuxDmabufV1Handler,
};
use wl_proxy::protocols::presentation_time::wp_presentation::WpPresentation;
use wl_proxy::protocols::single_pixel_buffer_v1::wp_single_pixel_buffer_manager_v1::{
    WpSinglePixelBufferManagerV1, WpSinglePixelBufferManagerV1Handler,
};
use wl_proxy::protocols::viewporter::wp_viewport::{WpViewport, WpViewportHandler};
use wl_proxy::protocols::viewporter::wp_viewporter::{WpViewporter, WpViewporterHandler};
use wl_proxy::protocols::virtual_keyboard_unstable_v1::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wl_proxy::protocols::wayland::wl_buffer::{WlBuffer, WlBufferHandler};
use wl_proxy::protocols::wayland::wl_callback::{WlCallback, WlCallbackHandler};
use wl_proxy::protocols::wayland::wl_compositor::{WlCompositor, WlCompositorHandler};
use wl_proxy::protocols::wayland::wl_data_device::{WlDataDevice, WlDataDeviceHandler};
use wl_proxy::protocols::wayland::wl_data_device_manager::{
    WlDataDeviceManager, WlDataDeviceManagerHandler,
};
use wl_proxy::protocols::wayland::wl_data_source::{WlDataSource, WlDataSourceHandler};
use wl_proxy::protocols::wayland::wl_display::{WlDisplay, WlDisplayHandler};
use wl_proxy::protocols::wayland::wl_keyboard::WlKeyboard;
use wl_proxy::protocols::wayland::wl_output::{WlOutput, WlOutputTransform};
use wl_proxy::protocols::wayland::wl_pointer::{
    WlPointer, WlPointerAxis, WlPointerAxisRelativeDirection, WlPointerAxisSource,
    WlPointerButtonState, WlPointerHandler,
};
use wl_proxy::protocols::wayland::wl_region::WlRegion;
use wl_proxy::protocols::wayland::wl_registry::{WlRegistry, WlRegistryHandler};
use wl_proxy::protocols::wayland::wl_seat::{WlSeat, WlSeatCapability, WlSeatHandler};
use wl_proxy::protocols::wayland::wl_shm::{WlShm, WlShmFormat, WlShmHandler};
use wl_proxy::protocols::wayland::wl_shm_pool::{WlShmPool, WlShmPoolHandler};
use wl_proxy::protocols::wayland::wl_subcompositor::WlSubcompositor;
use wl_proxy::protocols::wayland::wl_subsurface::WlSubsurface;
use wl_proxy::protocols::wayland::wl_surface::{WlSurface, WlSurfaceHandler};
use wl_proxy::protocols::wayland::wl_touch::WlTouch;
use wl_proxy::protocols::wlr_layer_shell_unstable_v1::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use wl_proxy::protocols::xdg_decoration_unstable_v1::zxdg_decoration_manager_v1::{
    ZxdgDecorationManagerV1, ZxdgDecorationManagerV1Handler,
};
use wl_proxy::protocols::xdg_decoration_unstable_v1::zxdg_toplevel_decoration_v1::{
    ZxdgToplevelDecorationV1, ZxdgToplevelDecorationV1Handler, ZxdgToplevelDecorationV1Mode,
};
use wl_proxy::protocols::xdg_output_unstable_v1::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wl_proxy::protocols::xdg_output_unstable_v1::zxdg_output_v1::ZxdgOutputV1;
use wl_proxy::protocols::xdg_shell::xdg_popup::{XdgPopup, XdgPopupHandler};
use wl_proxy::protocols::xdg_shell::xdg_positioner::{XdgPositioner, XdgPositionerHandler};
use wl_proxy::protocols::xdg_shell::xdg_surface::{XdgSurface, XdgSurfaceHandler};
use wl_proxy::protocols::xdg_shell::xdg_toplevel::{XdgToplevel, XdgToplevelHandler};
use wl_proxy::protocols::xdg_shell::xdg_wm_base::{XdgWmBase, XdgWmBaseHandler};
use wl_proxy::state::State;

use super::{Event, EventQueue, SeatId};
use crate::placement::Rect;
pub use hosted::HostedKey;
use hosted::{HostedPopup, SeatKeyboard, VirtualImManager, VirtualKeyboardManager};
pub use mirror::X11Target;
use mirror::{Mirror, OwnGlobals, SurfaceState};

/// Identifies an engine.
pub type EngineId = Rc<str>;

/// Registry names of virtual globals. Compositors hand out small names, so these never
/// collide.
const XDG_WM_BASE_NAME: u32 = 0xff00_0001;
const XDG_WM_BASE_VERSION: u32 = 6;
const DATA_DEVICE_MANAGER_NAME: u32 = 0xff00_0002;
const DATA_DEVICE_MANAGER_VERSION: u32 = 3;
const DECORATION_MANAGER_NAME: u32 = 0xff00_0003;
const DECORATION_MANAGER_VERSION: u32 = 1;

/// Globals exposed to engines and the highest version we handle for each.
fn exposed_version(interface: ObjectInterface) -> Option<u32> {
    use ObjectInterface as I;
    Some(match interface {
        // v7 adds wl_surface.get_release, which the hidden-window logic doesn't track.
        I::WlCompositor => 6,
        I::WlSubcompositor => 1,
        I::WlShm => 2,
        I::WlSeat => 7,
        I::WlOutput => 4,
        I::ZxdgOutputManagerV1 => 3,
        I::ZwpLinuxDmabufV1 => 4,
        I::WpViewporter => 1,
        I::WpFractionalScaleManagerV1 => 1,
        I::WpCursorShapeManagerV1 => 1,
        I::WpSinglePixelBufferManagerV1 => 1,
        I::WpPresentation => 1,
        _ => return None,
    })
}

/// State shared by all bridge handlers.
pub(super) struct BridgeShared {
    upstream_registry: Rc<WlRegistry>,
    /// Exposed upstream globals: name → (interface, highest version an engine may bind).
    globals: BTreeMap<u32, (ObjectInterface, u32)>,
    /// Virtual registries handed to engines, for global announcements.
    registries: Vec<Weak<WlRegistry>>,
    /// Input method used to create popups.
    popup_host: Option<Rc<ZwpInputMethodV2>>,
    /// Whether a text input is focused and the input method active.
    im_active: bool,
    active_engine: Option<EngineId>,
    /// Engines that asked for their windows to be shown (`SetPopupVisible(true)`).
    /// Windows stay hidden until their engine asks.
    shown_engines: std::collections::HashSet<EngineId>,
    /// Where windows go while an X11 application has focus.
    x11: Option<X11Target>,
    /// The daemon's own globals, for mirrors.
    own: OwnGlobals,
    /// Surfaces that have been given an `xdg_toplevel` or, by a hosted input method, a
    /// popup role: engine windows.
    windows: Vec<Weak<WlSurface>>,
    configure_serial: u32,
    /// The last preferred fractional scale the compositor sent for an engine window.
    /// Compositors send it only once a popup is mapped; handing it to new surfaces
    /// right away saves engines a blurry first frame.
    last_scale: Option<u32>,
    /// The input methods of hosted engines, one per engine and seat.
    hosted: BTreeMap<(EngineId, SeatId), Weak<ZwpInputMethodV2>>,
    /// The keyboards of seats, for the keyboard grabs of hosted input methods.
    keyboards: BTreeMap<SeatId, SeatKeyboard>,
    /// XKB's default keymap, for X11 applications typing before any Wayland field.
    default_keymap: Option<Rc<str>>,
    events: EventQueue,
}

type Shared = Rc<RefCell<BridgeShared>>;

/// How an engine window is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presentation {
    Hidden,
    /// As an input popup, which the compositor places at the focused text field.
    Popup,
    /// On a mirror at the text cursor of an X11 application.
    Mirror,
}

impl BridgeShared {
    fn presentation(&self, engine: &EngineId) -> Presentation {
        if self.active_engine.as_ref() != Some(engine) || !self.shown_engines.contains(engine) {
            Presentation::Hidden
        } else if self.im_active && self.popup_host.is_some() {
            Presentation::Popup
        } else if self.x11.is_some() && self.own.can_mirror() {
            Presentation::Mirror
        } else {
            Presentation::Hidden
        }
    }

    fn next_serial(&mut self) -> u32 {
        self.configure_serial = self.configure_serial.wrapping_add(1);
        self.configure_serial
    }
}

/// The bridge, owned by the [`Frontend`](super::Frontend). Clones share state.
#[derive(Clone)]
pub struct Bridge {
    state: Rc<State>,
    shared: Shared,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge").finish_non_exhaustive()
    }
}

impl Bridge {
    pub fn new(state: Rc<State>, upstream_registry: Rc<WlRegistry>, events: EventQueue) -> Self {
        let shared = Rc::new(RefCell::new(BridgeShared {
            upstream_registry,
            globals: BTreeMap::new(),
            registries: Vec::new(),
            popup_host: None,
            im_active: false,
            active_engine: None,
            shown_engines: std::collections::HashSet::new(),
            x11: None,
            own: OwnGlobals::default(),
            windows: Vec::new(),
            configure_serial: 0,
            last_scale: None,
            hosted: BTreeMap::new(),
            keyboards: BTreeMap::new(),
            default_keymap: None,
            events,
        }));
        Self { state, shared }
    }

    /// An upstream global appeared.
    pub fn add_global(&self, name: u32, interface: ObjectInterface, version: u32) {
        let Some(max) = exposed_version(interface) else {
            return;
        };
        let version = version.min(max);
        let registries = {
            let mut shared = self.shared.borrow_mut();
            shared.globals.insert(name, (interface, version));
            shared.registries.retain(|r| r.strong_count() > 0);
            shared.registries.clone()
        };
        for registry in registries.iter().filter_map(Weak::upgrade) {
            registry.send_global(name, interface, version);
        }
    }

    /// Hands the bridge a global the daemon bound for its own surfaces: `wl_compositor`,
    /// `zwlr_layer_shell_v1`, `wp_viewporter` or `wp_fractional_scale_manager_v1`.
    pub fn add_own_global(&self, object: Rc<dyn Object>) {
        let own = &mut self.shared.borrow_mut().own;
        if let Some(compositor) = object.clone().try_downcast::<WlCompositor>() {
            own.compositor = Some(compositor);
        } else if let Some(layer_shell) = object.clone().try_downcast::<ZwlrLayerShellV1>() {
            own.layer_shell = Some(layer_shell);
        } else if let Some(viewporter) = object.clone().try_downcast::<WpViewporter>() {
            own.viewporter = Some(viewporter);
        } else if let Some(manager) = object.try_downcast::<WpFractionalScaleManagerV1>() {
            own.fractional_scale = Some(manager);
        }
    }

    /// An upstream global disappeared.
    pub fn remove_global(&self, name: u32) {
        let registries = {
            let mut shared = self.shared.borrow_mut();
            if shared.globals.remove(&name).is_none() {
                return;
            }
            shared.registries.clone()
        };
        for registry in registries.iter().filter_map(Weak::upgrade) {
            registry.send_global_remove(name);
        }
    }

    /// Accepts a UI connection of `engine`. A `hosted` engine is a Wayland input method
    /// and finds a virtual input method on the connection.
    pub fn add_client(
        &self,
        engine: EngineId,
        hosted: bool,
        socket: std::os::fd::OwnedFd,
    ) -> anyhow::Result<()> {
        let client = self
            .state
            .add_client(&Rc::new(socket))
            .map_err(|e| anyhow::anyhow!("cannot add a UI client: {e}"))?;
        client.display().set_handler(BridgeDisplay {
            shared: self.shared.clone(),
            engine: engine.clone(),
            hosted,
        });
        client.set_handler(BridgeClient {
            client: Rc::downgrade(&client),
            shared: self.shared.clone(),
            engine: engine.clone(),
        });
        debug!(engine = &*engine, "UI client connected");
        Ok(())
    }

    /// Records whether `engine` wants its windows shown. Takes effect with the next
    /// [`Self::set_policy`].
    pub fn set_engine_visible(&self, engine: EngineId, visible: bool) {
        let mut shared = self.shared.borrow_mut();
        if visible {
            shared.shown_engines.insert(engine);
        } else {
            shared.shown_engines.remove(&engine);
        }
    }

    /// Updates who may show windows and where, and refreshes them. `x11` is where
    /// windows go while no Wayland text field but an X11 application has focus.
    pub fn set_policy(
        &self,
        popup_host: Option<Rc<ZwpInputMethodV2>>,
        im_active: bool,
        active_engine: Option<EngineId>,
        x11: Option<X11Target>,
    ) {
        let (host_changed, windows) = {
            let mut shared = self.shared.borrow_mut();
            let host_changed = match (&shared.popup_host, &popup_host) {
                (Some(a), Some(b)) => !Rc::ptr_eq(a, b),
                (None, None) => false,
                _ => true,
            };
            shared.popup_host = popup_host;
            shared.im_active = im_active;
            shared.active_engine = active_engine;
            shared.x11 = x11;
            shared.windows.retain(|w| w.strong_count() > 0);
            (host_changed, shared.windows.clone())
        };
        for surface in windows.iter().filter_map(Weak::upgrade) {
            let Ok(mut handler) = surface.try_get_handler_mut::<BridgeSurface>() else {
                continue;
            };
            if host_changed {
                // Popups belong to the old input method object; start over.
                handler.drop_popup(&surface);
            }
            handler.refresh(&surface);
        }
    }
}

/// Per-connection handler; cleans up when the engine goes away.
struct BridgeClient {
    client: Weak<Client>,
    shared: Shared,
    engine: EngineId,
}

impl ClientHandler for BridgeClient {
    fn disconnected(self: Box<Self>) {
        debug!(engine = &*self.engine, "UI client disconnected");
        if let Some(client) = self.client.upgrade() {
            destroy_client_objects(&client);
        }
        let mut shared = self.shared.borrow_mut();
        shared.registries.retain(|r| r.strong_count() > 0);
        shared.windows.retain(|w| w.strong_count() > 0);
    }
}

/// Destroys the compositor-side objects of a vanished client in dependency order.
fn destroy_client_objects(client: &Client) {
    let mut objects = Vec::new();
    client.objects(&mut objects);
    objects.sort_by_key(|o| destroy_rank(o.interface()));
    for object in &objects {
        destroy_upstream(object);
        object.unset_handler();
    }
}

/// Lower ranks are destroyed first: role and extension objects before surfaces,
/// surfaces before buffers, buffers before pools, everything before globals.
fn destroy_rank(interface: ObjectInterface) -> u8 {
    use ObjectInterface as I;
    match interface {
        I::WpViewport | I::WpFractionalScaleV1 | I::WpCursorShapeDeviceV1 => 0,
        I::WlSubsurface => 1,
        I::WlSurface => 2,
        I::WlBuffer | I::ZwpLinuxBufferParamsV1 | I::ZwpLinuxDmabufFeedbackV1 => 3,
        I::WlShmPool | I::WlRegion => 4,
        I::WlPointer | I::WlKeyboard | I::WlTouch | I::ZxdgOutputV1 => 5,
        _ => 6,
    }
}

fn destroy_upstream(object: &Rc<dyn Object>) {
    if object.server_id().is_none() {
        return;
    }
    macro_rules! destroy {
        ($ty:ty, $method:ident) => {
            if let Some(o) = object.try_downcast::<$ty>() {
                o.$method();
                return;
            }
        };
        ($ty:ty, $method:ident, since $v:expr) => {
            if let Some(o) = object.try_downcast::<$ty>() {
                if o.version() >= $v {
                    o.$method();
                }
                return;
            }
        };
    }
    if let Some(surface) = object.try_downcast::<WlSurface>() {
        if let Ok(mut handler) = surface.try_get_handler_mut::<BridgeSurface>() {
            handler.drop_popup(&surface);
            handler.drop_mirror(false);
        }
        surface.send_destroy();
        return;
    }
    destroy!(WpViewport, send_destroy);
    destroy!(WpFractionalScaleV1, send_destroy);
    destroy!(WpCursorShapeDeviceV1, send_destroy);
    destroy!(WlSubsurface, send_destroy);
    destroy!(WlBuffer, send_destroy);
    destroy!(ZwpLinuxBufferParamsV1, send_destroy);
    destroy!(ZwpLinuxDmabufFeedbackV1, send_destroy);
    destroy!(WlShmPool, send_destroy);
    destroy!(WlRegion, send_destroy);
    destroy!(WlPointer, send_release, since 3);
    destroy!(WlKeyboard, send_release, since 3);
    destroy!(WlTouch, send_release, since 3);
    destroy!(ZxdgOutputV1, send_destroy);
    destroy!(WlSeat, send_release, since 5);
    destroy!(WlOutput, send_release, since 3);
    destroy!(WlShm, send_release, since 2);
    destroy!(WlSubcompositor, send_destroy);
    destroy!(ZxdgOutputManagerV1, send_destroy);
    destroy!(ZwpLinuxDmabufV1, send_destroy);
    destroy!(WpViewporter, send_destroy);
    destroy!(WpFractionalScaleManagerV1, send_destroy);
    destroy!(WpCursorShapeManagerV1, send_destroy);
    destroy!(WpSinglePixelBufferManagerV1, send_destroy);
    destroy!(WpPresentation, send_destroy);
    // wl_compositor (< v7), wl_callback and server-destroyed objects have no
    // destructor; the compositor frees them with our connection.
}

fn same_client(a: &dyn Object, b: &dyn Object) -> bool {
    match (a.client(), b.client()) {
        (Some(a), Some(b)) => Rc::ptr_eq(&a, &b),
        _ => false,
    }
}

/// Kills a misbehaving engine connection without touching the shared connection.
fn kill_client(object: &dyn Object, reason: &str) {
    let Some(client) = object.client() else {
        return;
    };
    warn!("disconnecting a UI client: {reason}");
    destroy_client_objects(&client);
    client.disconnect();
}

struct BridgeDisplay {
    shared: Shared,
    engine: EngineId,
    hosted: bool,
}

impl WlDisplayHandler for BridgeDisplay {
    fn handle_get_registry(&mut self, _slf: &Rc<WlDisplay>, registry: &Rc<WlRegistry>) {
        // The registry is virtual: it never exists on the compositor.
        registry.set_forward_to_server(false);
        registry.set_handler(BridgeRegistry {
            shared: self.shared.clone(),
            engine: self.engine.clone(),
            hosted: self.hosted,
        });
        let globals: Vec<_> = {
            let mut shared = self.shared.borrow_mut();
            shared.registries.push(Rc::downgrade(registry));
            shared
                .globals
                .iter()
                .map(|(&n, &(i, v))| (n, i, v))
                .collect()
        };
        for (name, interface, version) in globals {
            registry.send_global(name, interface, version);
        }
        registry.send_global(
            XDG_WM_BASE_NAME,
            ObjectInterface::XdgWmBase,
            XDG_WM_BASE_VERSION,
        );
        registry.send_global(
            DATA_DEVICE_MANAGER_NAME,
            ObjectInterface::WlDataDeviceManager,
            DATA_DEVICE_MANAGER_VERSION,
        );
        registry.send_global(
            DECORATION_MANAGER_NAME,
            ObjectInterface::ZxdgDecorationManagerV1,
            DECORATION_MANAGER_VERSION,
        );
        if self.hosted {
            registry.send_global(
                hosted::INPUT_METHOD_MANAGER_NAME,
                ObjectInterface::ZwpInputMethodManagerV2,
                hosted::INPUT_METHOD_MANAGER_VERSION,
            );
            registry.send_global(
                hosted::VIRTUAL_KEYBOARD_MANAGER_NAME,
                ObjectInterface::ZwpVirtualKeyboardManagerV1,
                hosted::VIRTUAL_KEYBOARD_MANAGER_VERSION,
            );
        }
    }
}

struct BridgeRegistry {
    shared: Shared,
    engine: EngineId,
    hosted: bool,
}

impl BridgeRegistry {
    /// Binds the virtual input method globals of hosted engines. Returns `false` for
    /// other globals.
    fn bind_hosted(&self, name: u32, id: &Rc<dyn Object>) -> bool {
        if !self.hosted {
            return false;
        }
        if name == hosted::INPUT_METHOD_MANAGER_NAME {
            match id.clone().try_downcast::<ZwpInputMethodManagerV2>() {
                Some(manager) if manager.version() <= hosted::INPUT_METHOD_MANAGER_VERSION => {
                    manager.set_forward_to_server(false);
                    manager.set_handler(VirtualImManager {
                        shared: self.shared.clone(),
                        engine: self.engine.clone(),
                    });
                }
                _ => kill_client(&**id, "bound zwp_input_method_manager_v2 wrongly"),
            }
            return true;
        }
        if name == hosted::VIRTUAL_KEYBOARD_MANAGER_NAME {
            match id.clone().try_downcast::<ZwpVirtualKeyboardManagerV1>() {
                Some(manager) if manager.version() <= hosted::VIRTUAL_KEYBOARD_MANAGER_VERSION => {
                    manager.set_forward_to_server(false);
                    manager.set_handler(VirtualKeyboardManager {
                        shared: self.shared.clone(),
                        engine: self.engine.clone(),
                    });
                }
                _ => kill_client(&**id, "bound zwp_virtual_keyboard_manager_v1 wrongly"),
            }
            return true;
        }
        false
    }
}

impl WlRegistryHandler for BridgeRegistry {
    fn handle_bind(&mut self, _slf: &Rc<WlRegistry>, name: u32, id: Rc<dyn Object>) {
        if self.bind_hosted(name, &id) {
            return;
        }
        if name == XDG_WM_BASE_NAME {
            let Some(wm_base) = id.try_downcast::<XdgWmBase>() else {
                return kill_client(&*id, "bound the wrong interface for xdg_wm_base");
            };
            if wm_base.version() > XDG_WM_BASE_VERSION {
                return kill_client(&*id, "bound xdg_wm_base with an unsupported version");
            }
            wm_base.set_forward_to_server(false);
            wm_base.set_handler(VirtualWmBase {
                shared: self.shared.clone(),
                engine: self.engine.clone(),
            });
            return;
        }
        if name == DATA_DEVICE_MANAGER_NAME {
            let Some(manager) = id.try_downcast::<WlDataDeviceManager>() else {
                return kill_client(&*id, "bound the wrong interface for wl_data_device_manager");
            };
            if manager.version() > DATA_DEVICE_MANAGER_VERSION {
                return kill_client(
                    &*id,
                    "bound wl_data_device_manager with an unsupported version",
                );
            }
            manager.set_forward_to_server(false);
            manager.set_handler(VirtualDataDeviceManager);
            return;
        }
        if name == DECORATION_MANAGER_NAME {
            let Some(manager) = id.try_downcast::<ZxdgDecorationManagerV1>() else {
                return kill_client(
                    &*id,
                    "bound the wrong interface for zxdg_decoration_manager_v1",
                );
            };
            if manager.version() > DECORATION_MANAGER_VERSION {
                return kill_client(
                    &*id,
                    "bound zxdg_decoration_manager_v1 with an unsupported version",
                );
            }
            manager.set_forward_to_server(false);
            manager.set_handler(VirtualDecorationManager);
            return;
        }
        let (upstream, global) = {
            let shared = self.shared.borrow();
            (
                shared.upstream_registry.clone(),
                shared.globals.get(&name).copied(),
            )
        };
        let Some((interface, max_version)) = global else {
            return kill_client(&*id, "bound an unknown global");
        };
        if id.interface() != interface || id.version() == 0 || id.version() > max_version {
            return kill_client(&*id, "bound a global with a wrong interface or version");
        }
        upstream.send_bind(name, id.clone());
        if let Some(compositor) = id.try_downcast::<WlCompositor>() {
            compositor.set_handler(BridgeCompositor {
                shared: self.shared.clone(),
                engine: self.engine.clone(),
                hosted: self.hosted,
            });
        } else if let Some(seat) = id.try_downcast::<WlSeat>() {
            seat.set_handler(BridgeSeat { name });
        } else if let Some(shm) = id.try_downcast::<WlShm>() {
            shm.set_handler(BridgeShm);
        } else if let Some(manager) = id.try_downcast::<WpFractionalScaleManagerV1>() {
            manager.set_handler(BridgeFractionalScaleManager {
                shared: self.shared.clone(),
            });
        } else if let Some(viewporter) = id.try_downcast::<WpViewporter>() {
            viewporter.set_handler(BridgeViewporter);
        } else if let Some(dmabuf) = id.try_downcast::<ZwpLinuxDmabufV1>() {
            dmabuf.set_handler(BridgeDmabuf);
        } else if let Some(manager) = id.try_downcast::<WpSinglePixelBufferManagerV1>() {
            manager.set_handler(BridgeSinglePixelBuffers);
        }
    }
}

struct BridgeCompositor {
    shared: Shared,
    engine: EngineId,
    hosted: bool,
}

impl WlCompositorHandler for BridgeCompositor {
    fn handle_create_surface(&mut self, slf: &Rc<WlCompositor>, id: &Rc<WlSurface>) {
        slf.send_create_surface(id);
        id.set_handler(BridgeSurface::new(
            self.shared.clone(),
            self.engine.clone(),
            self.hosted,
        ));
    }
}

struct BridgeFractionalScaleManager {
    shared: Shared,
}

impl WpFractionalScaleManagerV1Handler for BridgeFractionalScaleManager {
    fn handle_get_fractional_scale(
        &mut self,
        slf: &Rc<WpFractionalScaleManagerV1>,
        id: &Rc<WpFractionalScaleV1>,
        surface: &Rc<WlSurface>,
    ) {
        slf.send_get_fractional_scale(id, surface);
        id.set_handler(BridgeFractionalScale {
            shared: self.shared.clone(),
            preferred: None,
        });
        if let Ok(mut handler) = surface.try_get_handler_mut::<BridgeSurface>() {
            handler.fractional_scale = Some(Rc::downgrade(id));
        }
        if let Some(scale) = self.shared.borrow().last_scale {
            id.send_preferred_scale(scale);
        }
    }
}

struct BridgeFractionalScale {
    shared: Shared,
    /// The scale the compositor prefers for the engine's surface.
    preferred: Option<u32>,
}

impl WpFractionalScaleV1Handler for BridgeFractionalScale {
    fn handle_preferred_scale(&mut self, slf: &Rc<WpFractionalScaleV1>, scale: u32) {
        self.shared.borrow_mut().last_scale = Some(scale);
        self.preferred = Some(scale);
        slf.send_preferred_scale(scale);
    }
}

/// Returns the size of the file behind `fd`, if it can be determined.
fn file_size(fd: &std::os::fd::OwnedFd) -> Option<i64> {
    rustix::fs::fstat(fd).ok().map(|stat| stat.st_size)
}

/// Validates shm requests that compositors treat as fatal protocol errors (or that
/// could make them read past the end of a file), so that a broken engine is
/// disconnected on its own instead of breaking the shared connection.
struct BridgeShm;

impl WlShmHandler for BridgeShm {
    fn handle_create_pool(
        &mut self,
        slf: &Rc<WlShm>,
        id: &Rc<WlShmPool>,
        fd: &Rc<std::os::fd::OwnedFd>,
        size: i32,
    ) {
        if size <= 0 || file_size(fd).is_none_or(|len| len < i64::from(size)) {
            return kill_client(&**slf, "invalid wl_shm.create_pool");
        }
        slf.send_create_pool(id, fd, size);
        id.set_handler(BridgeShmPool {
            fd: fd.clone(),
            size,
        });
    }
}

struct BridgeShmPool {
    fd: Rc<std::os::fd::OwnedFd>,
    size: i32,
}

impl WlShmPoolHandler for BridgeShmPool {
    fn handle_create_buffer(
        &mut self,
        slf: &Rc<WlShmPool>,
        id: &Rc<WlBuffer>,
        offset: i32,
        width: i32,
        height: i32,
        stride: i32,
        format: WlShmFormat,
    ) {
        // The same checks wlroots and Hyprland perform before raising invalid_stride.
        let valid = offset >= 0
            && width > 0
            && height > 0
            && stride >= width
            && i64::from(offset) + i64::from(stride) * i64::from(height) <= i64::from(self.size);
        if !valid {
            return kill_client(&**slf, "invalid wl_shm_pool.create_buffer");
        }
        slf.send_create_buffer(id, offset, width, height, stride, format);
        id.set_handler(BufferShim::sized(width, height));
    }

    fn handle_resize(&mut self, slf: &Rc<WlShmPool>, size: i32) {
        if size < self.size || file_size(&self.fd).is_none_or(|len| len < i64::from(size)) {
            return kill_client(&**slf, "invalid wl_shm_pool.resize");
        }
        self.size = size;
        slf.send_resize(size);
    }
}

/// Wraps wl_seat: engines never get keyboard focus, so they don't see a keyboard.
struct BridgeSeat {
    /// The registry name of the seat, which identifies it.
    name: SeatId,
}

impl WlSeatHandler for BridgeSeat {
    fn handle_capabilities(&mut self, slf: &Rc<WlSeat>, capabilities: WlSeatCapability) {
        slf.send_capabilities(WlSeatCapability(
            capabilities.0 & !WlSeatCapability::KEYBOARD.0,
        ));
    }

    fn handle_get_pointer(&mut self, slf: &Rc<WlSeat>, id: &Rc<WlPointer>) {
        slf.send_get_pointer(id);
        id.set_handler(BridgePointer::default());
    }
}

/// Delivers pointer events only to the engine whose surface the pointer is over.
///
/// The compositor sends pointer events to every `wl_pointer` of our connection, which
/// includes the pointers of all engines.
#[derive(Default)]
struct BridgePointer {
    /// The engine's surface the pointer entered, as far as the engine knows.
    entered: Option<Rc<WlSurface>>,
    frame_pending: bool,
}

impl BridgePointer {
    fn forwarding(&mut self) -> bool {
        if self.entered.is_some() {
            self.frame_pending = true;
        }
        self.entered.is_some()
    }
}

impl WlPointerHandler for BridgePointer {
    fn handle_enter(
        &mut self,
        slf: &Rc<WlPointer>,
        serial: u32,
        surface: &Rc<WlSurface>,
        surface_x: Fixed,
        surface_y: Fixed,
    ) {
        // A mirror stands for the engine window it shows, at the same size.
        let surface = mirror::mirrored_window(surface).unwrap_or_else(|| surface.clone());
        self.entered = same_client(&**slf, &*surface).then_some(surface);
        if self.forwarding()
            && let Some(surface) = &self.entered
        {
            slf.send_enter(serial, surface, surface_x, surface_y);
        }
    }

    fn handle_leave(&mut self, slf: &Rc<WlPointer>, serial: u32, _surface: &Rc<WlSurface>) {
        // The surface left may be a mirror that is gone by now; the engine learns that
        // the pointer left the surface it entered.
        if let Some(surface) = self.entered.take()
            && surface.client().is_some()
        {
            self.frame_pending = true;
            slf.send_leave(serial, &surface);
        }
    }

    fn handle_motion(&mut self, slf: &Rc<WlPointer>, time: u32, x: Fixed, y: Fixed) {
        if self.forwarding() {
            slf.send_motion(time, x, y);
        }
    }

    fn handle_button(
        &mut self,
        slf: &Rc<WlPointer>,
        serial: u32,
        time: u32,
        button: u32,
        state: WlPointerButtonState,
    ) {
        if self.forwarding() {
            slf.send_button(serial, time, button, state);
        }
    }

    fn handle_axis(&mut self, slf: &Rc<WlPointer>, time: u32, axis: WlPointerAxis, value: Fixed) {
        if self.forwarding() {
            slf.send_axis(time, axis, value);
        }
    }

    fn handle_frame(&mut self, slf: &Rc<WlPointer>) {
        if std::mem::take(&mut self.frame_pending) {
            slf.send_frame();
        }
    }

    fn handle_axis_source(&mut self, slf: &Rc<WlPointer>, source: WlPointerAxisSource) {
        if self.forwarding() {
            slf.send_axis_source(source);
        }
    }

    fn handle_axis_stop(&mut self, slf: &Rc<WlPointer>, time: u32, axis: WlPointerAxis) {
        if self.forwarding() {
            slf.send_axis_stop(time, axis);
        }
    }

    fn handle_axis_discrete(&mut self, slf: &Rc<WlPointer>, axis: WlPointerAxis, discrete: i32) {
        if self.forwarding() {
            slf.send_axis_discrete(axis, discrete);
        }
    }

    fn handle_axis_relative_direction(
        &mut self,
        slf: &Rc<WlPointer>,
        axis: WlPointerAxis,
        direction: WlPointerAxisRelativeDirection,
    ) {
        if self.forwarding() {
            slf.send_axis_relative_direction(axis, direction);
        }
    }
}

/// The xdg role of a surface (virtual; never sent to the compositor).
struct WindowRole {
    xdg_surface: Weak<XdgSurface>,
    toplevel: Option<Weak<XdgToplevel>>,
    configured: bool,
}

/// A buffer attach the bridge has not passed to the compositor yet.
#[derive(Clone)]
struct PendingAttach {
    buffer: Option<Rc<WlBuffer>>,
    x: i32,
    y: i32,
}

/// Lets the bridge hold an engine's buffer while its window is hidden.
///
/// Hiding a window detaches its buffer on the compositor, which then releases it. If the
/// engine saw that release it could reuse the buffer, and showing the window again would
/// need a new frame that toolkits only draw when their content changes. Instead, the
/// bridge swallows that release and keeps the buffer until the window is shown again or
/// the engine commits a newer one.
#[derive(Default)]
struct BufferShim {
    /// A release from the compositor is in flight for an attachment we undid; drop it.
    swallow_release: bool,
    /// The size in pixels, if the bridge saw the buffer being created.
    size: Option<(i32, i32)>,
}

impl BufferShim {
    fn sized(width: i32, height: i32) -> Self {
        Self {
            swallow_release: false,
            size: Some((width, height)),
        }
    }
}

/// The size of `buffer` in pixels, if known.
fn buffer_size(buffer: &Rc<WlBuffer>) -> Option<(i32, i32)> {
    buffer.try_get_handler_ref::<BufferShim>().ok()?.size
}

impl WlBufferHandler for BufferShim {
    fn handle_release(&mut self, slf: &Rc<WlBuffer>) {
        if std::mem::take(&mut self.swallow_release) || slf.client().is_none() {
            return;
        }
        slf.send_release();
    }
}

fn shim(buffer: &Rc<WlBuffer>) -> Option<wl_proxy::handler::HandlerMut<'_, BufferShim>> {
    if buffer.try_get_handler_ref::<BufferShim>().is_err() {
        buffer.set_handler(BufferShim::default());
    }
    buffer.try_get_handler_mut::<BufferShim>().ok()
}

struct BridgeSurface {
    shared: Shared,
    engine: EngineId,
    /// Whether the surface belongs to a hosted input method, whose windows are its
    /// popup surfaces.
    hosted: bool,
    window: Option<WindowRole>,
    /// The popup role a hosted input method gave the surface.
    input_popup: Option<Weak<ZwpInputPopupSurfaceV2>>,
    /// The input popup role on the compositor, created when the window is first mapped.
    popup: Option<Rc<ZwpInputPopupSurfaceV2>>,
    /// Attach since the last commit (window surfaces only).
    pending_attach: Option<PendingAttach>,
    /// The buffer attached on the compositor side; `Some` means the popup is mapped.
    current: Option<Rc<WlBuffer>>,
    /// The engine's newest buffer while the window is hidden, or not yet on its mirror.
    held: Option<Rc<WlBuffer>>,
    /// The state set since the last commit, and as of the last commit.
    pending_state: PendingState,
    state: SurfaceState,
    /// The frame callbacks of a window: requested since the last commit, then waiting
    /// until the window is shown, then handed to the surface showing it.
    pending_frames: Vec<Rc<WlCallback>>,
    frames: Vec<Rc<WlCallback>>,
    armed: Vec<Rc<WlCallback>>,
    /// The engine's fractional scale object for this surface.
    fractional_scale: Option<Weak<WpFractionalScaleV1>>,
    /// Whether the engine last got a scale meant for the mirror.
    mirror_scale: bool,
    /// The mirror showing the window while an X11 application has focus.
    mirror: Option<Mirror>,
    /// What the compositor closed a mirror for; no new mirror until the target changes.
    closed_for: Option<X11Target>,
}

/// Changes to a surface's [`SurfaceState`] since its last commit.
#[derive(Default)]
struct PendingState {
    scale: Option<i32>,
    transform: Option<WlOutputTransform>,
    source: Option<Option<[Fixed; 4]>>,
    destination: Option<Option<(i32, i32)>>,
}

/// The size of a window showing `buffer`, in surface coordinates.
fn window_size(state: &SurfaceState, buffer: &Rc<WlBuffer>) -> (i32, i32) {
    // A buffer the bridge did not see being created is placed as if it were tiny.
    buffer_size(buffer).map_or((1, 1), |size| state.logical_size(size))
}

impl BridgeSurface {
    fn new(shared: Shared, engine: EngineId, hosted: bool) -> Self {
        Self {
            shared,
            engine,
            hosted,
            window: None,
            input_popup: None,
            popup: None,
            pending_attach: None,
            current: None,
            held: None,
            pending_state: PendingState::default(),
            state: SurfaceState::default(),
            pending_frames: Vec::new(),
            frames: Vec::new(),
            armed: Vec::new(),
            fractional_scale: None,
            mirror_scale: false,
            mirror: None,
            closed_for: None,
        }
    }

    fn is_toplevel(&self) -> bool {
        self.window.as_ref().is_some_and(|w| w.toplevel.is_some())
    }

    /// Whether the surface is an engine window, whose buffers the bridge shows.
    fn is_window(&self) -> bool {
        self.is_toplevel() || self.input_popup.is_some()
    }

    fn presentation(&self) -> Presentation {
        // Hosted input methods show their popups; any other window of theirs would be
        // a toplevel window, which the bridge cannot show.
        let shown = if self.hosted {
            self.input_popup.is_some()
        } else {
            self.is_toplevel()
        };
        if shown {
            self.shared.borrow().presentation(&self.engine)
        } else {
            Presentation::Hidden
        }
    }

    fn ensure_popup(&mut self, surface: &Rc<WlSurface>) -> bool {
        if self.popup.is_some() {
            return true;
        }
        let Some(host) = self.shared.borrow().popup_host.clone() else {
            return false;
        };
        let popup = host.new_send_get_input_popup_surface(surface);
        popup.set_forward_to_client(false);
        popup.set_handler(PopupEvents {
            window: Rc::downgrade(surface),
            last: None,
        });
        self.popup = Some(popup);
        true
    }

    /// Unmaps the window on the compositor and holds on to its buffer.
    fn hide(&mut self, surface: &Rc<WlSurface>) {
        let Some(buffer) = self.current.take() else {
            return;
        };
        self.disarm_frames();
        surface.send_attach(None, 0, 0);
        surface.send_commit();
        if let Some(mut shim) = shim(&buffer) {
            shim.swallow_release = true;
        }
        self.hold(buffer);
    }

    /// Maps the window again with the held buffer, if it is still usable.
    fn show(&mut self, surface: &Rc<WlSurface>) {
        if self.current.is_some() {
            return;
        }
        match self.held.take() {
            Some(buffer) if buffer.server_id().is_some() && self.ensure_popup(surface) => {
                surface.send_attach(Some(&buffer), 0, 0);
                self.arm_frames(surface);
                surface.send_commit();
                self.current = Some(buffer);
                self.restore_popup_reports();
            }
            held => {
                self.held = held;
                // Nothing to show: ask the engine for a frame.
                self.send_configure();
            }
        }
    }

    /// Makes `buffer` the held buffer, releasing the previous one to the engine.
    fn hold(&mut self, buffer: Rc<WlBuffer>) {
        if self
            .held
            .as_ref()
            .is_some_and(|held| Rc::ptr_eq(held, &buffer))
        {
            return;
        }
        self.release_held();
        self.held = Some(buffer);
    }

    /// Gives the held buffer back to the engine.
    fn release_held(&mut self) {
        let Some(buffer) = self.held.take() else {
            return;
        };
        let release_in_flight =
            shim(&buffer).is_some_and(|mut s| std::mem::take(&mut s.swallow_release));
        // If the compositor's release is still on its way, simply let it through.
        // Otherwise the compositor is long done with the buffer and we release it.
        if !release_in_flight && buffer.client().is_some() {
            buffer.send_release();
        }
    }

    /// Destroys the popup role. Must happen before the surface is destroyed.
    fn drop_popup(&mut self, surface: &Rc<WlSurface>) {
        self.hide(surface);
        if let Some(popup) = self.popup.take() {
            popup.unset_handler();
            popup.send_destroy();
        }
    }

    /// Hides or shows the window to match the bridge policy.
    fn refresh(&mut self, surface: &Rc<WlSurface>) {
        match self.presentation() {
            Presentation::Popup => {
                self.drop_mirror(true);
                self.show(surface);
            }
            Presentation::Mirror => {
                self.hide(surface);
                self.update_mirror(surface);
            }
            Presentation::Hidden => {
                self.hide(surface);
                self.drop_mirror(true);
            }
        }
    }

    /// Hands the window's committed frame callbacks to `target`, the surface that shows
    /// the window with its next commit.
    fn arm_frames(&mut self, target: &Rc<WlSurface>) {
        self.armed.retain(|request| {
            request
                .try_get_handler_ref::<FrameRequest>()
                .is_ok_and(|request| !request.frames.is_empty())
        });
        if self.frames.is_empty() {
            return;
        }
        let request = target.new_send_frame();
        request.set_forward_to_client(false);
        request.set_handler(FrameRequest {
            frames: std::mem::take(&mut self.frames),
        });
        self.armed.push(request);
    }

    /// Takes back the frame callbacks handed to a surface that stops showing the window.
    fn disarm_frames(&mut self) {
        let mut frames = Vec::new();
        for request in self.armed.drain(..) {
            if let Ok(mut request) = request.try_get_handler_mut::<FrameRequest>() {
                frames.append(&mut request.frames);
            }
        }
        frames.append(&mut self.frames);
        self.frames = frames;
    }

    /// Answers every frame callback, for a window that goes away.
    fn finish_frames(&mut self) {
        self.disarm_frames();
        self.frames.append(&mut self.pending_frames);
        FrameRequest::done(std::mem::take(&mut self.frames), 0);
    }

    /// Tells the engine where the text cursor is relative to its window.
    fn report_cursor(&self, (x, y, width, height): Rect) {
        if let Some(popup) = self.input_popup.as_ref().and_then(Weak::upgrade) {
            // A hosted input method learns it from its popup role, as on a compositor.
            return HostedPopup::report(&popup, (x, y, width, height));
        }
        let shared = self.shared.borrow();
        shared
            .events
            .borrow_mut()
            .push_back(Event::PopupCursorRect {
                engine: self.engine.clone(),
                x,
                y,
                width,
                height,
            });
    }

    /// Tells the engine again what the compositor said about the popup (where the text
    /// cursor is relative to it, and the scale to draw at), which a mirror may have
    /// contradicted since.
    fn restore_popup_reports(&mut self) {
        let last = self.popup.as_ref().and_then(|popup| {
            popup
                .try_get_handler_ref::<PopupEvents>()
                .ok()
                .and_then(|events| events.last)
        });
        if let Some(cursor) = last {
            self.report_cursor(cursor);
        }
        if std::mem::take(&mut self.mirror_scale)
            && let Some(scale) = self.fractional_scale.as_ref().and_then(Weak::upgrade)
            && let Some(preferred) = scale
                .try_get_handler_ref::<BridgeFractionalScale>()
                .ok()
                .and_then(|handler| handler.preferred)
        {
            scale.send_preferred_scale(preferred);
        }
    }

    /// Shows the window on a mirror at the X11 application's text cursor: creates the
    /// mirror there, then shows the engine's newest buffer on it.
    fn update_mirror(&mut self, window: &Rc<WlSurface>) {
        let (target, own) = {
            let shared = self.shared.borrow();
            (shared.x11.clone(), shared.own.clone())
        };
        let Some(target) = target else {
            return self.drop_mirror(true);
        };
        if self
            .mirror
            .as_ref()
            .is_some_and(|mirror| !Rc::ptr_eq(&mirror.target.output, &target.output))
        {
            // A layer surface stays on its output.
            self.drop_mirror(true);
        }
        let Some(mirror) = &mut self.mirror else {
            if self.closed_for.as_ref() == Some(&target) {
                return;
            }
            self.closed_for = None;
            match &self.held {
                Some(buffer) => {
                    let size = window_size(&self.state, buffer);
                    self.mirror = Mirror::create(&own, &self.shared, window, target, size);
                }
                // Nothing to show: ask the engine for a frame.
                None => self.send_configure(),
            }
            return;
        };
        mirror.target = target;
        if !mirror.configured {
            return;
        }
        let buffer = self
            .held
            .take()
            .filter(|buffer| buffer.server_id().is_some());
        let Some(shown) = buffer.clone().or_else(|| mirror.current.clone()) else {
            return self.send_configure();
        };
        let changed = mirror.present(buffer, &self.state, window_size(&self.state, &shown));
        let surface = mirror.surface.clone();
        let cursor = mirror.cursor();
        let moved = mirror.reported_cursor.replace(cursor) != Some(cursor);
        let frames = !self.frames.is_empty();
        self.arm_frames(&surface);
        if changed || frames {
            surface.send_commit();
        }
        if moved {
            self.report_cursor(cursor);
        }
    }

    /// Destroys the mirror. With `keep_buffer`, the buffer it showed is held for showing
    /// the window again.
    fn drop_mirror(&mut self, keep_buffer: bool) {
        let Some(mirror) = self.mirror.take() else {
            return;
        };
        self.disarm_frames();
        if let Some(buffer) = mirror.destroy()
            && keep_buffer
            && self.held.is_none()
        {
            // The compositor releases the buffer of a destroyed surface.
            if let Some(mut shim) = shim(&buffer) {
                shim.swallow_release = true;
            }
            self.held = Some(buffer);
        }
    }

    /// The compositor configured the mirror: it may get a buffer now.
    fn mirror_configured(&mut self, window: &Rc<WlSurface>) {
        let Some(mirror) = &mut self.mirror else {
            return;
        };
        if mirror.configured {
            // Later configures only answer changes of the size.
            return;
        }
        mirror.configured = true;
        if self.presentation() == Presentation::Mirror {
            self.update_mirror(window);
        }
    }

    /// The compositor will not show the mirror any more, for example because its output
    /// went away.
    fn mirror_closed(&mut self) {
        self.closed_for = self.mirror.as_ref().map(|mirror| mirror.target.clone());
        self.drop_mirror(true);
    }

    fn apply_pending_state(&mut self) {
        let pending = std::mem::take(&mut self.pending_state);
        if let Some(scale) = pending.scale {
            self.state.scale = scale;
        }
        if let Some(transform) = pending.transform {
            self.state.transform = transform;
        }
        if let Some(source) = pending.source {
            self.state.source = source;
        }
        if let Some(destination) = pending.destination {
            self.state.destination = destination;
        }
    }

    /// The surface stops being a window (its role was destroyed): it is unmapped and
    /// gives back everything it held.
    fn lose_window(&mut self, surface: &Rc<WlSurface>) {
        self.hide(surface);
        self.drop_mirror(false);
        self.release_held();
        self.finish_frames();
        self.pending_attach = None;
    }

    fn send_configure(&mut self) {
        let Some(window) = &mut self.window else {
            return;
        };
        let (Some(xdg_surface), Some(toplevel)) = (
            window.xdg_surface.upgrade(),
            window.toplevel.as_ref().and_then(Weak::upgrade),
        ) else {
            return;
        };
        if !window.configured && toplevel.version() >= 5 {
            // We support none of the window-management capabilities.
            toplevel.send_wm_capabilities(&[]);
        }
        toplevel.send_configure(0, 0, &[]);
        let serial = self.shared.borrow_mut().next_serial();
        xdg_surface.send_configure(serial);
        window.configured = true;
    }

    fn commit_window(&mut self, surface: &Rc<WlSurface>) {
        if !self.window.as_ref().is_some_and(|w| w.configured) {
            // The initial commit of a new toplevel: answer with a configure.
            self.send_configure();
        }
        self.apply_pending_state();
        self.frames.append(&mut self.pending_frames);
        let presentation = self.presentation();
        let mut unmapped = false;
        if let Some(attach) = self.pending_attach.take() {
            match attach.buffer {
                Some(buffer)
                    if presentation == Presentation::Popup && self.ensure_popup(surface) =>
                {
                    self.release_held();
                    surface.send_attach(Some(&buffer), attach.x, attach.y);
                    self.current = Some(buffer);
                }
                // Hidden, or shown on a mirror, which takes the newest frame from here.
                Some(buffer) => self.hold(buffer),
                None => {
                    // The engine unmapped its window.
                    unmapped = true;
                    self.release_held();
                    if self.current.take().is_some() {
                        surface.send_attach(None, attach.x, attach.y);
                    }
                    self.drop_mirror(false);
                }
            }
        }
        if presentation == Presentation::Popup && self.current.is_some() {
            self.arm_frames(surface);
        }
        surface.send_commit();
        if presentation == Presentation::Mirror && !unmapped {
            self.update_mirror(surface);
        }
    }
}

impl WlSurfaceHandler for BridgeSurface {
    fn handle_destroy(&mut self, slf: &Rc<WlSurface>) {
        self.drop_popup(slf);
        self.drop_mirror(false);
        self.release_held();
        self.finish_frames();
        slf.send_destroy();
    }

    fn handle_attach(
        &mut self,
        slf: &Rc<WlSurface>,
        buffer: Option<&Rc<WlBuffer>>,
        x: i32,
        y: i32,
    ) {
        if self.is_window() {
            if let Some(buffer) = buffer {
                shim(buffer);
            }
            self.pending_attach = Some(PendingAttach {
                buffer: buffer.cloned(),
                x,
                y,
            });
        } else {
            slf.send_attach(buffer, x, y);
        }
    }

    fn handle_frame(&mut self, slf: &Rc<WlSurface>, callback: &Rc<WlCallback>) {
        if self.is_window() {
            // Answered once the window is shown, on its popup or on its mirror.
            self.pending_frames.push(callback.clone());
        } else {
            slf.send_frame(callback);
        }
    }

    fn handle_set_buffer_transform(&mut self, slf: &Rc<WlSurface>, transform: WlOutputTransform) {
        self.pending_state.transform = Some(transform);
        slf.send_set_buffer_transform(transform);
    }

    fn handle_set_buffer_scale(&mut self, slf: &Rc<WlSurface>, scale: i32) {
        self.pending_state.scale = Some(scale);
        slf.send_set_buffer_scale(scale);
    }

    fn handle_commit(&mut self, slf: &Rc<WlSurface>) {
        if self.is_window() {
            self.commit_window(slf);
        } else {
            self.apply_pending_state();
            slf.send_commit();
        }
    }

    fn handle_enter(&mut self, slf: &Rc<WlSurface>, output: &Rc<WlOutput>) {
        if same_client(&**slf, &**output) {
            slf.send_enter(output);
        }
    }

    fn handle_leave(&mut self, slf: &Rc<WlSurface>, output: &Rc<WlOutput>) {
        if same_client(&**slf, &**output) {
            slf.send_leave(output);
        }
    }
}

/// A frame callback the bridge asked for on the surface showing an engine window; when
/// it is done, so are the window's frame callbacks it stands for.
struct FrameRequest {
    frames: Vec<Rc<WlCallback>>,
}

impl FrameRequest {
    fn done(frames: Vec<Rc<WlCallback>>, time: u32) {
        for frame in frames {
            if frame.client().is_some() {
                frame.send_done(time);
                frame.delete_id();
            }
        }
    }
}

impl WlCallbackHandler for FrameRequest {
    fn handle_done(&mut self, _slf: &Rc<WlCallback>, time: u32) {
        Self::done(std::mem::take(&mut self.frames), time);
    }
}

/// Events of a popup role we created for an engine window.
struct PopupEvents {
    window: Weak<WlSurface>,
    last: Option<(i32, i32, i32, i32)>,
}

impl ZwpInputPopupSurfaceV2Handler for PopupEvents {
    fn handle_text_input_rectangle(
        &mut self,
        _slf: &Rc<ZwpInputPopupSurfaceV2>,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) {
        // Hyprland re-sends the rectangle on every commit; only report changes, or an
        // engine that redraws on each update would loop forever.
        let rect = (x, y, width, height);
        if self.last.replace(rect) != Some(rect)
            && let Some(window) = self.window.upgrade()
            && let Ok(handler) = window.try_get_handler_ref::<BridgeSurface>()
        {
            handler.report_cursor(rect);
        }
    }
}

/// Records the viewports of engine surfaces, which mirrors copy.
struct BridgeViewporter;

impl WpViewporterHandler for BridgeViewporter {
    fn handle_get_viewport(
        &mut self,
        slf: &Rc<WpViewporter>,
        id: &Rc<WpViewport>,
        surface: &Rc<WlSurface>,
    ) {
        slf.send_get_viewport(id, surface);
        id.set_handler(BridgeViewport {
            surface: Rc::downgrade(surface),
        });
    }
}

struct BridgeViewport {
    surface: Weak<WlSurface>,
}

impl BridgeViewport {
    fn pending(&self, f: impl FnOnce(&mut PendingState)) {
        if let Some(surface) = self.surface.upgrade()
            && let Ok(mut handler) = surface.try_get_handler_mut::<BridgeSurface>()
        {
            f(&mut handler.pending_state);
        }
    }
}

impl WpViewportHandler for BridgeViewport {
    fn handle_destroy(&mut self, slf: &Rc<WpViewport>) {
        // The surface loses its viewport with its next commit.
        self.pending(|state| {
            state.source = Some(None);
            state.destination = Some(None);
        });
        slf.send_destroy();
    }

    fn handle_set_source(
        &mut self,
        slf: &Rc<WpViewport>,
        x: Fixed,
        y: Fixed,
        width: Fixed,
        height: Fixed,
    ) {
        let source = [x, y, width, height];
        let unset = source == [Fixed::from_i32_saturating(-1); 4];
        self.pending(|state| state.source = Some((!unset).then_some(source)));
        slf.send_set_source(x, y, width, height);
    }

    fn handle_set_destination(&mut self, slf: &Rc<WpViewport>, width: i32, height: i32) {
        let unset = (width, height) == (-1, -1);
        self.pending(|state| state.destination = Some((!unset).then_some((width, height))));
        slf.send_set_destination(width, height);
    }
}

/// Records the sizes of dmabuf buffers.
struct BridgeDmabuf;

impl ZwpLinuxDmabufV1Handler for BridgeDmabuf {
    fn handle_create_params(
        &mut self,
        slf: &Rc<ZwpLinuxDmabufV1>,
        params_id: &Rc<ZwpLinuxBufferParamsV1>,
    ) {
        slf.send_create_params(params_id);
        params_id.set_handler(BridgeBufferParams::default());
    }
}

#[derive(Default)]
struct BridgeBufferParams {
    /// The size passed to `create`, for the buffer that `created` brings.
    size: Option<(i32, i32)>,
}

impl ZwpLinuxBufferParamsV1Handler for BridgeBufferParams {
    fn handle_create(
        &mut self,
        slf: &Rc<ZwpLinuxBufferParamsV1>,
        width: i32,
        height: i32,
        format: u32,
        flags: ZwpLinuxBufferParamsV1Flags,
    ) {
        self.size = Some((width, height));
        slf.send_create(width, height, format, flags);
    }

    fn handle_created(&mut self, slf: &Rc<ZwpLinuxBufferParamsV1>, buffer: &Rc<WlBuffer>) {
        if let Some((width, height)) = self.size {
            buffer.set_handler(BufferShim::sized(width, height));
        }
        slf.send_created(buffer);
    }

    fn handle_create_immed(
        &mut self,
        slf: &Rc<ZwpLinuxBufferParamsV1>,
        buffer_id: &Rc<WlBuffer>,
        width: i32,
        height: i32,
        format: u32,
        flags: ZwpLinuxBufferParamsV1Flags,
    ) {
        slf.send_create_immed(buffer_id, width, height, format, flags);
        buffer_id.set_handler(BufferShim::sized(width, height));
    }
}

/// Records the size of single-pixel buffers.
struct BridgeSinglePixelBuffers;

impl WpSinglePixelBufferManagerV1Handler for BridgeSinglePixelBuffers {
    fn handle_create_u32_rgba_buffer(
        &mut self,
        slf: &Rc<WpSinglePixelBufferManagerV1>,
        id: &Rc<WlBuffer>,
        r: u32,
        g: u32,
        b: u32,
        a: u32,
    ) {
        slf.send_create_u32_rgba_buffer(id, r, g, b, a);
        id.set_handler(BufferShim::sized(1, 1));
    }
}

struct VirtualWmBase {
    shared: Shared,
    engine: EngineId,
}

impl XdgWmBaseHandler for VirtualWmBase {
    fn handle_destroy(&mut self, slf: &Rc<XdgWmBase>) {
        slf.delete_id();
    }

    fn handle_create_positioner(&mut self, _slf: &Rc<XdgWmBase>, id: &Rc<XdgPositioner>) {
        id.set_forward_to_server(false);
        id.set_handler(VirtualPositioner);
    }

    fn handle_get_xdg_surface(
        &mut self,
        _slf: &Rc<XdgWmBase>,
        id: &Rc<XdgSurface>,
        surface: &Rc<WlSurface>,
    ) {
        id.set_forward_to_server(false);
        id.set_handler(VirtualXdgSurface {
            shared: self.shared.clone(),
            engine: self.engine.clone(),
            surface: surface.clone(),
        });
        match surface.try_get_handler_mut::<BridgeSurface>() {
            Ok(mut handler) => {
                handler.window = Some(WindowRole {
                    xdg_surface: Rc::downgrade(id),
                    toplevel: None,
                    configured: false,
                });
            }
            Err(_) => kill_client(&**id, "xdg_surface for a foreign surface"),
        }
    }

    fn handle_pong(&mut self, _slf: &Rc<XdgWmBase>, _serial: u32) {}
}

struct VirtualPositioner;

impl XdgPositionerHandler for VirtualPositioner {
    fn handle_destroy(&mut self, slf: &Rc<XdgPositioner>) {
        slf.delete_id();
    }
}

struct VirtualXdgSurface {
    shared: Shared,
    engine: EngineId,
    surface: Rc<WlSurface>,
}

impl XdgSurfaceHandler for VirtualXdgSurface {
    fn handle_destroy(&mut self, slf: &Rc<XdgSurface>) {
        if let Ok(mut handler) = self.surface.try_get_handler_mut::<BridgeSurface>() {
            handler.window = None;
        }
        slf.delete_id();
    }

    fn handle_get_toplevel(&mut self, _slf: &Rc<XdgSurface>, id: &Rc<XdgToplevel>) {
        id.set_forward_to_server(false);
        id.set_handler(VirtualToplevel {
            surface: Rc::downgrade(&self.surface),
        });
        if let Ok(mut handler) = self.surface.try_get_handler_mut::<BridgeSurface>()
            && let Some(window) = &mut handler.window
        {
            window.toplevel = Some(Rc::downgrade(id));
            window.configured = false;
            self.shared
                .borrow_mut()
                .windows
                .push(Rc::downgrade(&self.surface));
            debug!(engine = &*self.engine, "engine window created");
        }
    }

    fn handle_get_popup(
        &mut self,
        _slf: &Rc<XdgSurface>,
        id: &Rc<XdgPopup>,
        _parent: Option<&Rc<XdgSurface>>,
        _positioner: &Rc<XdgPositioner>,
    ) {
        // Menus and tooltips inside an IME popup are not supported (yet): dismiss them.
        id.set_forward_to_server(false);
        id.set_handler(VirtualPopup);
        id.send_popup_done();
    }

    fn handle_ack_configure(&mut self, _slf: &Rc<XdgSurface>, _serial: u32) {}

    fn handle_set_window_geometry(
        &mut self,
        _slf: &Rc<XdgSurface>,
        _x: i32,
        _y: i32,
        _width: i32,
        _height: i32,
    ) {
    }
}

struct VirtualToplevel {
    surface: Weak<WlSurface>,
}

impl XdgToplevelHandler for VirtualToplevel {
    fn handle_destroy(&mut self, slf: &Rc<XdgToplevel>) {
        // Destroying the role unmaps the window; the popup role itself is kept so the
        // surface can become a window again.
        if let Some(surface) = self.surface.upgrade()
            && let Ok(mut handler) = surface.try_get_handler_mut::<BridgeSurface>()
        {
            handler.lose_window(&surface);
            if let Some(window) = &mut handler.window {
                window.toplevel = None;
            }
        }
        slf.delete_id();
    }
}

struct VirtualPopup;

impl XdgPopupHandler for VirtualPopup {
    fn handle_destroy(&mut self, slf: &Rc<XdgPopup>) {
        slf.delete_id();
    }
}

/// A clipboard that is always empty. Some toolkits refuse to start without a data
/// device manager, but engines must not see the user's clipboard through the bridge.
struct VirtualDataDeviceManager;

impl WlDataDeviceManagerHandler for VirtualDataDeviceManager {
    fn handle_create_data_source(&mut self, _slf: &Rc<WlDataDeviceManager>, id: &Rc<WlDataSource>) {
        id.set_forward_to_server(false);
        id.set_handler(VirtualDataSource);
    }

    fn handle_get_data_device(
        &mut self,
        _slf: &Rc<WlDataDeviceManager>,
        id: &Rc<WlDataDevice>,
        _seat: &Rc<WlSeat>,
    ) {
        id.set_forward_to_server(false);
        id.set_handler(VirtualDataDevice);
    }
}

struct VirtualDataSource;

impl WlDataSourceHandler for VirtualDataSource {
    fn handle_destroy(&mut self, slf: &Rc<WlDataSource>) {
        slf.delete_id();
    }
}

struct VirtualDataDevice;

impl WlDataDeviceHandler for VirtualDataDevice {
    fn handle_release(&mut self, slf: &Rc<WlDataDevice>) {
        slf.delete_id();
    }
}

/// Tells toolkits that the "compositor" draws decorations, so they drop their own
/// title bars. Popups have no decorations at all.
struct VirtualDecorationManager;

impl ZxdgDecorationManagerV1Handler for VirtualDecorationManager {
    fn handle_destroy(&mut self, slf: &Rc<ZxdgDecorationManagerV1>) {
        slf.delete_id();
    }

    fn handle_get_toplevel_decoration(
        &mut self,
        _slf: &Rc<ZxdgDecorationManagerV1>,
        id: &Rc<ZxdgToplevelDecorationV1>,
        toplevel: &Rc<XdgToplevel>,
    ) {
        id.set_forward_to_server(false);
        id.set_handler(VirtualDecoration {
            toplevel: Rc::downgrade(toplevel),
        });
        id.send_configure(ZxdgToplevelDecorationV1Mode::SERVER_SIDE);
    }
}

struct VirtualDecoration {
    toplevel: Weak<XdgToplevel>,
}

impl VirtualDecoration {
    fn reconfigure(&self, slf: &Rc<ZxdgToplevelDecorationV1>) {
        slf.send_configure(ZxdgToplevelDecorationV1Mode::SERVER_SIDE);
        // A decoration configure only takes effect with the next xdg_surface.configure.
        let Some(toplevel) = self.toplevel.upgrade() else {
            return;
        };
        let Some(surface) = toplevel
            .try_get_handler_ref::<VirtualToplevel>()
            .ok()
            .and_then(|h| h.surface.upgrade())
        else {
            return;
        };
        if let Ok(mut handler) = surface.try_get_handler_mut::<BridgeSurface>()
            && handler.window.as_ref().is_some_and(|w| w.configured)
        {
            handler.send_configure();
        }
    }
}

impl ZxdgToplevelDecorationV1Handler for VirtualDecoration {
    fn handle_destroy(&mut self, slf: &Rc<ZxdgToplevelDecorationV1>) {
        slf.delete_id();
    }

    fn handle_set_mode(
        &mut self,
        slf: &Rc<ZxdgToplevelDecorationV1>,
        _mode: ZxdgToplevelDecorationV1Mode,
    ) {
        self.reconfigure(slf);
    }

    fn handle_unset_mode(&mut self, slf: &Rc<ZxdgToplevelDecorationV1>) {
        self.reconfigure(slf);
    }
}
