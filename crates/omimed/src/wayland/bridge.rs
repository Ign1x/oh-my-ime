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
//! - Only the active engine's windows are mapped. Hiding injects `attach(NULL)`; while
//!   hidden, buffers and frame callbacks are held by the bridge instead of the compositor.
//! - Popup roles are destroyed before their surfaces, and everything an engine leaves
//!   behind is destroyed in dependency order when it disconnects.

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
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_v2::ZwpInputMethodV2;
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_popup_surface_v2::{
    ZwpInputPopupSurfaceV2, ZwpInputPopupSurfaceV2Handler,
};
use wl_proxy::protocols::linux_dmabuf_v1::zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1;
use wl_proxy::protocols::linux_dmabuf_v1::zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1;
use wl_proxy::protocols::linux_dmabuf_v1::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wl_proxy::protocols::presentation_time::wp_presentation::WpPresentation;
use wl_proxy::protocols::single_pixel_buffer_v1::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1;
use wl_proxy::protocols::viewporter::wp_viewport::WpViewport;
use wl_proxy::protocols::viewporter::wp_viewporter::WpViewporter;
use wl_proxy::protocols::wayland::wl_buffer::{WlBuffer, WlBufferHandler};
use wl_proxy::protocols::wayland::wl_compositor::{WlCompositor, WlCompositorHandler};
use wl_proxy::protocols::wayland::wl_data_device::{WlDataDevice, WlDataDeviceHandler};
use wl_proxy::protocols::wayland::wl_data_device_manager::{
    WlDataDeviceManager, WlDataDeviceManagerHandler,
};
use wl_proxy::protocols::wayland::wl_data_source::{WlDataSource, WlDataSourceHandler};
use wl_proxy::protocols::wayland::wl_display::{WlDisplay, WlDisplayHandler};
use wl_proxy::protocols::wayland::wl_keyboard::WlKeyboard;
use wl_proxy::protocols::wayland::wl_output::WlOutput;
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

use super::{Event, EventQueue};

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
    /// Surfaces that have been given an `xdg_toplevel`, i.e. engine windows.
    windows: Vec<Weak<WlSurface>>,
    configure_serial: u32,
    /// The last preferred fractional scale the compositor sent for an engine window.
    /// Compositors send it only once a popup is mapped; handing it to new surfaces
    /// right away saves engines a blurry first frame.
    last_scale: Option<u32>,
    events: EventQueue,
}

type Shared = Rc<RefCell<BridgeShared>>;

impl BridgeShared {
    fn window_visible(&self, engine: &EngineId) -> bool {
        self.im_active && self.popup_host.is_some() && self.active_engine.as_ref() == Some(engine)
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
            windows: Vec::new(),
            configure_serial: 0,
            last_scale: None,
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

    /// Accepts a UI connection of `engine`.
    pub fn add_client(&self, engine: EngineId, socket: std::os::fd::OwnedFd) -> anyhow::Result<()> {
        let client = self
            .state
            .add_client(&Rc::new(socket))
            .map_err(|e| anyhow::anyhow!("cannot add a UI client: {e}"))?;
        client.display().set_handler(BridgeDisplay {
            shared: self.shared.clone(),
            engine: engine.clone(),
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

    /// Updates who may show windows and refreshes their visibility.
    pub fn set_policy(
        &self,
        popup_host: Option<Rc<ZwpInputMethodV2>>,
        im_active: bool,
        active_engine: Option<EngineId>,
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
}

impl WlDisplayHandler for BridgeDisplay {
    fn handle_get_registry(&mut self, _slf: &Rc<WlDisplay>, registry: &Rc<WlRegistry>) {
        // The registry is virtual: it never exists on the compositor.
        registry.set_forward_to_server(false);
        registry.set_handler(BridgeRegistry {
            shared: self.shared.clone(),
            engine: self.engine.clone(),
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
    }
}

struct BridgeRegistry {
    shared: Shared,
    engine: EngineId,
}

impl WlRegistryHandler for BridgeRegistry {
    fn handle_bind(&mut self, _slf: &Rc<WlRegistry>, name: u32, id: Rc<dyn Object>) {
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
            });
        } else if let Some(seat) = id.try_downcast::<WlSeat>() {
            seat.set_handler(BridgeSeat);
        } else if let Some(shm) = id.try_downcast::<WlShm>() {
            shm.set_handler(BridgeShm);
        } else if let Some(manager) = id.try_downcast::<WpFractionalScaleManagerV1>() {
            manager.set_handler(BridgeFractionalScaleManager {
                shared: self.shared.clone(),
            });
        }
    }
}

struct BridgeCompositor {
    shared: Shared,
    engine: EngineId,
}

impl WlCompositorHandler for BridgeCompositor {
    fn handle_create_surface(&mut self, slf: &Rc<WlCompositor>, id: &Rc<WlSurface>) {
        slf.send_create_surface(id);
        id.set_handler(BridgeSurface::new(self.shared.clone(), self.engine.clone()));
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
        });
        if let Some(scale) = self.shared.borrow().last_scale {
            id.send_preferred_scale(scale);
        }
    }
}

struct BridgeFractionalScale {
    shared: Shared,
}

impl WpFractionalScaleV1Handler for BridgeFractionalScale {
    fn handle_preferred_scale(&mut self, slf: &Rc<WpFractionalScaleV1>, scale: u32) {
        self.shared.borrow_mut().last_scale = Some(scale);
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
struct BridgeSeat;

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
    focused: bool,
    frame_pending: bool,
}

impl BridgePointer {
    fn forwarding(&mut self) -> bool {
        if self.focused {
            self.frame_pending = true;
        }
        self.focused
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
        self.focused = same_client(&**slf, &**surface);
        if self.forwarding() {
            slf.send_enter(serial, surface, surface_x, surface_y);
        }
    }

    fn handle_leave(&mut self, slf: &Rc<WlPointer>, serial: u32, surface: &Rc<WlSurface>) {
        if self.forwarding() && same_client(&**slf, &**surface) {
            slf.send_leave(serial, surface);
        }
        self.focused = false;
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
    window: Option<WindowRole>,
    /// The input popup role on the compositor, created when the window is first mapped.
    popup: Option<Rc<ZwpInputPopupSurfaceV2>>,
    /// Attach since the last commit (window surfaces only).
    pending_attach: Option<PendingAttach>,
    /// The buffer attached on the compositor side; `Some` means the popup is mapped.
    current: Option<Rc<WlBuffer>>,
    /// The engine's newest buffer while the window is hidden; shown when we map again.
    held: Option<Rc<WlBuffer>>,
}

impl BridgeSurface {
    fn new(shared: Shared, engine: EngineId) -> Self {
        Self {
            shared,
            engine,
            window: None,
            popup: None,
            pending_attach: None,
            current: None,
            held: None,
        }
    }

    fn is_toplevel(&self) -> bool {
        self.window.as_ref().is_some_and(|w| w.toplevel.is_some())
    }

    fn visible(&self) -> bool {
        self.is_toplevel() && self.shared.borrow().window_visible(&self.engine)
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
            engine: self.engine.clone(),
            events: self.shared.borrow().events.clone(),
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
                surface.send_commit();
                self.current = Some(buffer);
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
        if self.visible() {
            self.show(surface);
        } else {
            self.hide(surface);
        }
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
        let Some(attach) = self.pending_attach.take() else {
            return surface.send_commit();
        };
        match attach.buffer {
            Some(buffer) if self.visible() && self.ensure_popup(surface) => {
                self.release_held();
                surface.send_attach(Some(&buffer), attach.x, attach.y);
                self.current = Some(buffer);
            }
            Some(buffer) => {
                // Hidden: keep the newest frame for when we are shown again.
                self.hold(buffer);
            }
            None => {
                self.release_held();
                if self.current.take().is_some() {
                    surface.send_attach(None, attach.x, attach.y);
                }
            }
        }
        surface.send_commit();
    }
}

impl WlSurfaceHandler for BridgeSurface {
    fn handle_destroy(&mut self, slf: &Rc<WlSurface>) {
        self.drop_popup(slf);
        self.release_held();
        slf.send_destroy();
    }

    fn handle_attach(
        &mut self,
        slf: &Rc<WlSurface>,
        buffer: Option<&Rc<WlBuffer>>,
        x: i32,
        y: i32,
    ) {
        if self.is_toplevel() {
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

    fn handle_commit(&mut self, slf: &Rc<WlSurface>) {
        if self.is_toplevel() {
            self.commit_window(slf);
        } else {
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

/// Events of a popup role we created for an engine window.
struct PopupEvents {
    engine: EngineId,
    events: EventQueue,
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
        if self.last.replace(rect) != Some(rect) {
            self.events.borrow_mut().push_back(Event::PopupCursorRect {
                engine: self.engine.clone(),
                x,
                y,
                width,
                height,
            });
        }
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
            handler.hide(&surface);
            handler.release_held();
            handler.pending_attach = None;
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
