//! Slint user interfaces on a layer surface of the compositor (`zwlr_layer_shell_v1`),
//! drawn with Slint's software renderer into shared memory: oh-my-ime's switcher, and
//! the floating panels of engines.
//!
//! [`install`] makes the overlay Slint's platform. The application then creates its
//! Slint component, shows it (`ComponentHandle::show`) and runs `slint::run_event_loop`.
//! The component appears on the screen when the application calls [`Overlay::show`]
//! with the size it needs, and goes away with [`Overlay::hide`]. Ordinary overlays keep
//! the keyboard with the application; [`install_modal`] takes it while shown.
//! With [`Pointer::PassThrough`]
//! (the switcher) the pointer reaches what is below the surface; with [`Pointer::Take`]
//! (a floating panel) the component gets it: hovering, clicks and scrolling.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
};
use slint::platform::{
    EventLoopProxy, PlatformError, PointerEventButton, WindowAdapter, WindowEvent,
};
use slint::{EventLoopError, LogicalPosition, PhysicalSize};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::{EventLoop, channel};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, registry_handlers,
};
use tracing::{debug, warn};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy as _, QueueHandle, WEnum};
use xkbcommon::xkb;

/// Where the overlay is on the output the compositor puts it on.
///
/// An edge with a margin is one the overlay keeps to, that far away in logical pixels;
/// along an axis with neither edge, it is centred.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Placement {
    pub top: Option<i32>,
    pub bottom: Option<i32>,
    pub left: Option<i32>,
    pub right: Option<i32>,
    /// Above fullscreen windows (the overlay layer), rather than below them.
    pub above_fullscreen: bool,
}

/// What the overlay does with the pointer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pointer {
    /// Clicks go through to what is below, as for a notice.
    #[default]
    PassThrough,
    /// The component gets the pointer, as a panel with buttons needs.
    Take,
}

/// Error setting up the overlay.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot connect to the compositor: {0}")]
    Connect(#[from] wayland_client::ConnectError),
    #[error("cannot read the compositor's globals: {0}")]
    Globals(#[from] wayland_client::globals::GlobalError),
    #[error("the compositor has no {0}")]
    Missing(&'static str),
    #[error("cannot set up the event loop: {0}")]
    EventLoop(String),
    #[error("cannot make the overlay Slint's platform: {0}")]
    Platform(#[from] slint::platform::SetPlatformError),
}

/// The overlay of the application: shows and hides its Slint component.
#[derive(Debug, Clone)]
pub struct Overlay {
    wanted: Rc<Cell<Option<(u32, u32)>>>,
}

impl Overlay {
    /// Shows the component on the screen, `width` × `height` logical pixels large, or
    /// resizes it.
    pub fn show(&self, width: u32, height: u32) {
        self.wanted.set(Some((width.max(1), height.max(1))));
    }

    /// Takes the component off the screen.
    pub fn hide(&self) {
        self.wanted.set(None);
    }
}

/// Connects to the compositor in `$WAYLAND_DISPLAY` and makes the overlay Slint's
/// platform, with layer surfaces in the namespace `namespace` (for compositor rules)
/// placed by `placement`, doing with the pointer what `pointer` says. Call it before
/// creating any Slint component.
pub fn install(namespace: &str, placement: Placement, pointer: Pointer) -> Result<Overlay, Error> {
    install_with_keyboard(namespace, placement, pointer, false)
}

/// A modal overlay: takes the keyboard only while shown. Losing keyboard focus
/// requests that the component close. Ordinary overlays remain non-focusable.
pub fn install_modal(namespace: &str, placement: Placement) -> Result<Overlay, Error> {
    install_with_keyboard(namespace, placement, Pointer::Take, true)
}

fn install_with_keyboard(
    namespace: &str,
    placement: Placement,
    pointer: Pointer,
    modal: bool,
) -> Result<Overlay, Error> {
    let conn = Connection::connect_to_env()?;
    let (globals, queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let compositor =
        CompositorState::bind(&globals, &qh).map_err(|_| Error::Missing("wl_compositor"))?;
    let layer_shell =
        LayerShell::bind(&globals, &qh).map_err(|_| Error::Missing("zwlr_layer_shell_v1"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|_| Error::Missing("wl_shm"))?;
    // The pointer's shape over the overlay, where the compositor lets clients choose it
    // by name; elsewhere the compositor's cursor stays as it was.
    let cursor_shapes = (pointer == Pointer::Take)
        .then(|| CursorShapeManager::bind(&globals, &qh).ok())
        .flatten();
    let pool = SlotPool::new(256 * 64 * 4, &shm).map_err(|e| Error::EventLoop(e.to_string()))?;

    let event_loop: EventLoop<'static, State> =
        EventLoop::try_new().map_err(|e| Error::EventLoop(e.to_string()))?;
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| Error::EventLoop(e.to_string()))?;
    let (sender, messages) = channel::channel::<Message>();
    event_loop
        .handle()
        .insert_source(messages, |event, _, state: &mut State| match event {
            channel::Event::Msg(Message::Invoke(f)) => f(),
            channel::Event::Msg(Message::Quit) | channel::Event::Closed => state.quit = true,
        })
        .map_err(|e| Error::EventLoop(e.to_string()))?;

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let wanted = Rc::new(Cell::new(None));
    let state = State {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        pointers: Vec::new(),
        pointer,
        modal,
        keyboards: Vec::new(),
        cursor_shapes,
        outputs: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pool,
        qh,
        conn,
        window: window.clone(),
        namespace: namespace.to_owned(),
        placement,
        layer: None,
        requested: None,
        size: (0, 0),
        scale: 1,
        configured: false,
        frame_pending: false,
        pixels: Vec::new(),
        quit: false,
    };
    slint::platform::set_platform(Box::new(OverlayPlatform {
        window,
        wanted: wanted.clone(),
        event_loop: RefCell::new(event_loop),
        state: RefCell::new(state),
        proxy: sender,
    }))?;
    Ok(Overlay { wanted })
}

/// What other threads ask of the event loop.
enum Message {
    Invoke(Box<dyn FnOnce() + Send>),
    Quit,
}

struct Proxy(Mutex<channel::Sender<Message>>);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        self.send(Message::Quit)
    }

    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), EventLoopError> {
        self.send(Message::Invoke(event))
    }
}

impl Proxy {
    fn send(&self, message: Message) -> Result<(), EventLoopError> {
        let sender = self
            .0
            .lock()
            .map_err(|_| EventLoopError::EventLoopTerminated)?;
        sender
            .send(message)
            .map_err(|_| EventLoopError::EventLoopTerminated)
    }
}

struct OverlayPlatform {
    window: Rc<MinimalSoftwareWindow>,
    wanted: Rc<Cell<Option<(u32, u32)>>>,
    event_loop: RefCell<EventLoop<'static, State>>,
    state: RefCell<State>,
    proxy: channel::Sender<Message>,
}

impl slint::platform::Platform for OverlayPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        let mut event_loop = self.event_loop.borrow_mut();
        let mut state = self.state.borrow_mut();
        state.quit = false;
        loop {
            slint::platform::update_timers_and_animations();
            state.follow(self.wanted.get());
            state.repeat_keys();
            state.draw();
            if let Err(e) = state.conn.flush() {
                return Err(PlatformError::Other(format!(
                    "the compositor went away: {e}"
                )));
            }
            if state.quit {
                return Ok(());
            }
            // While something moves, the frame callback wakes the loop for the next
            // frame; timers wake it in any case, also when the compositor sends no
            // frame callbacks (its outputs are off).
            let timeout = match (
                slint::platform::duration_until_next_timer_update(),
                state.repeat_deadline(),
            ) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            event_loop
                .dispatch(timeout, &mut state)
                .map_err(|e| PlatformError::Other(format!("the event loop failed: {e}")))?;
        }
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(Mutex::new(self.proxy.clone()))))
    }
}

/// A pixel of a `wl_shm` ARGB8888 buffer as it is in memory: blue first, premultiplied.
#[derive(Debug, Clone, Copy, Default)]
struct Bgra {
    b: u8,
    g: u8,
    r: u8,
    a: u8,
}

impl TargetPixel for Bgra {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let keep = u16::from(u8::MAX - color.alpha);
        let mix = |old: u8, new: u8| ((u16::from(old) * keep / 255) as u8).saturating_add(new);
        self.r = mix(self.r, color.red);
        self.g = mix(self.g, color.green);
        self.b = mix(self.b, color.blue);
        self.a = mix(self.a, color.alpha);
    }

    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { b, g, r, a: 255 }
    }

    /// Transparent: what the component does not draw shows what is below.
    fn background() -> Self {
        Self::default()
    }
}

struct State {
    registry: RegistryState,
    seats: SeatState,
    /// The pointers of the seats, with their cursor-shape devices, while the component
    /// takes the pointer.
    pointers: Vec<(wl_pointer::WlPointer, Option<WpCursorShapeDeviceV1>)>,
    pointer: Pointer,
    modal: bool,
    keyboards: Vec<Keyboard>,
    cursor_shapes: Option<CursorShapeManager>,
    outputs: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    qh: QueueHandle<Self>,
    conn: Connection,
    window: Rc<MinimalSoftwareWindow>,
    namespace: String,
    placement: Placement,
    /// The layer surface while the component is shown.
    layer: Option<LayerSurface>,
    /// The size last asked of the compositor, in logical pixels.
    requested: Option<(u32, u32)>,
    /// The size the compositor gave, in logical pixels.
    size: (u32, u32),
    /// The buffer scale of the outputs the surface is on.
    scale: u32,
    configured: bool,
    /// Whether a frame callback is on its way: the next frame waits for it.
    frame_pending: bool,
    pixels: Vec<Bgra>,
    quit: bool,
}

impl State {
    /// Creates, resizes or destroys the layer surface as the application wants.
    fn follow(&mut self, wanted: Option<(u32, u32)>) {
        match (wanted, &self.layer) {
            (None, None) => {}
            (None, Some(_)) => {
                debug!("hiding the overlay");
                self.layer = None;
                self.requested = None;
                self.configured = false;
                self.frame_pending = false;
                for keyboard in &mut self.keyboards {
                    keyboard.repeat = None;
                    keyboard.focused = false;
                }
            }
            (Some(size), None) => self.create(size),
            (Some(size), Some(layer)) => {
                if self.requested != Some(size) {
                    layer.set_size(size.0, size.1);
                    layer.commit();
                    self.requested = Some(size);
                }
            }
        }
    }

    fn create(&mut self, (width, height): (u32, u32)) {
        debug!(width, height, "showing the overlay");
        let surface = self.compositor.create_surface(&self.qh);
        let placement = self.placement;
        let layer = if placement.above_fullscreen {
            Layer::Overlay
        } else {
            Layer::Top
        };
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            layer,
            Some(self.namespace.clone()),
            None,
        );
        let mut anchor = Anchor::empty();
        for (margin, edge) in [
            (placement.top, Anchor::TOP),
            (placement.bottom, Anchor::BOTTOM),
            (placement.left, Anchor::LEFT),
            (placement.right, Anchor::RIGHT),
        ] {
            if margin.is_some() {
                anchor |= edge;
            }
        }
        layer.set_anchor(anchor);
        layer.set_margin(
            placement.top.unwrap_or(0),
            placement.right.unwrap_or(0),
            placement.bottom.unwrap_or(0),
            placement.left.unwrap_or(0),
        );
        layer.set_size(width, height);
        layer.set_keyboard_interactivity(if self.modal {
            KeyboardInteractivity::Exclusive
        } else {
            KeyboardInteractivity::None
        });
        if self.pointer == Pointer::PassThrough {
            // An empty input region: clicks go through.
            match Region::new(&self.compositor) {
                Ok(region) => layer
                    .wl_surface()
                    .set_input_region(Some(region.wl_region())),
                Err(e) => warn!("the overlay takes clicks: {e}"),
            }
        }
        // The first commit has no buffer; the compositor answers with a configure.
        layer.commit();
        self.layer = Some(layer);
        self.requested = Some((width, height));
        self.configured = false;
        self.frame_pending = false;
    }

    /// The size of the buffer, in physical pixels.
    fn buffer_size(&self) -> (u32, u32) {
        (self.size.0 * self.scale, self.size.1 * self.scale)
    }

    fn resize_window(&self) {
        let (width, height) = self.buffer_size();
        self.window.set_size(PhysicalSize::new(width, height));
        self.window.request_redraw();
    }

    /// Draws the component if it changed and the compositor is ready for a frame.
    fn draw(&mut self) {
        let Some(layer) = &self.layer else { return };
        if !self.configured || self.frame_pending {
            return;
        }
        let (width, height) = self.buffer_size();
        if width == 0 || height == 0 {
            return;
        }
        let pixels = &mut self.pixels;
        let drawn = self.window.draw_if_needed(|renderer| {
            pixels.clear();
            pixels.resize(width as usize * height as usize, Bgra::default());
            renderer.render(&mut pixels[..], width as usize);
        });
        if !drawn {
            return;
        }
        let stride = width as i32 * 4;
        let (buffer, canvas) = match self.pool.create_buffer(
            width as i32,
            height as i32,
            stride,
            wl_shm::Format::Argb8888,
        ) {
            Ok(created) => created,
            Err(e) => {
                warn!("cannot draw the overlay: {e}");
                return;
            }
        };
        for (bytes, pixel) in canvas.as_chunks_mut::<4>().0.iter_mut().zip(&self.pixels) {
            *bytes = [pixel.b, pixel.g, pixel.r, pixel.a];
        }
        let surface = layer.wl_surface();
        surface.set_buffer_scale(self.scale as i32);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        if self.window.has_active_animations() {
            surface.frame(&self.qh, surface.clone());
            self.frame_pending = true;
        }
        if let Err(e) = buffer.attach_to(surface) {
            warn!("cannot show the overlay's frame: {e}");
            return;
        }
        layer.commit();
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        let scale = u32::try_from(factor).unwrap_or(1).max(1);
        if scale != self.scale {
            self.scale = scale;
            self.window
                .window()
                .dispatch_event(WindowEvent::ScaleFactorChanged {
                    scale_factor: scale as f32,
                });
            self.resize_window();
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        self.window.request_redraw();
    }

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

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        // The output went away, say. Shown again on the next `show`.
        debug!("the compositor closed the overlay");
        self.layer = None;
        self.requested = None;
        self.configured = false;
        self.frame_pending = false;
        if self.modal {
            self.window
                .window()
                .dispatch_event(WindowEvent::CloseRequested);
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (width, height) = configure.new_size;
        let requested = self.requested.unwrap_or((1, 1));
        self.size = (
            if width == 0 { requested.0 } else { width },
            if height == 0 { requested.1 } else { height },
        );
        self.configured = true;
        self.resize_window();
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.modal {
            self.keyboards.push(Keyboard {
                object: seat.get_keyboard(qh, seat.clone()),
                state: None,
                focused: false,
                modifiers: [false; 4],
                rate: 0,
                delay: 0,
                repeat: None,
            });
            return;
        }
        if capability != Capability::Pointer || self.pointer != Pointer::Take {
            return;
        }
        match self.seats.get_pointer(qh, &seat) {
            Ok(pointer) => {
                let shape = self
                    .cursor_shapes
                    .as_ref()
                    .map(|shapes| shapes.get_shape_device(&pointer, qh));
                self.pointers.push((pointer, shape));
            }
            Err(e) => warn!("the overlay cannot follow a pointer: {e}"),
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            self.keyboards.retain(|keyboard| {
                if keyboard.object.data::<wl_seat::WlSeat>() == Some(&seat) {
                    if keyboard.focused {
                        self.window
                            .window()
                            .dispatch_event(WindowEvent::CloseRequested);
                    }
                    keyboard.object.release();
                    false
                } else {
                    true
                }
            });
            return;
        }
        if capability != Capability::Pointer {
            return;
        }
        self.pointers.retain(|(pointer, shape)| {
            let ours = pointer
                .data::<smithay_client_toolkit::seat::pointer::PointerData>()
                .is_some_and(|data| data.seat() == &seat);
            if ours {
                if let Some(shape) = shape {
                    shape.destroy();
                }
                pointer.release();
            }
            !ours
        });
    }

    fn remove_seat(&mut self, conn: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.remove_capability(conn, qh, seat.clone(), Capability::Pointer);
        self.remove_capability(conn, qh, seat, Capability::Keyboard);
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let ours = |surface: &wl_surface::WlSurface| {
            self.layer
                .as_ref()
                .is_some_and(|layer| layer.wl_surface() == surface)
        };
        for event in events {
            if !ours(&event.surface) {
                continue;
            }
            // Logical pixels, as Slint takes them.
            let position = LogicalPosition::new(event.position.0 as f32, event.position.1 as f32);
            let window = self.window.window();
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some((_, Some(shape))) = self.pointers.iter().find(|(p, _)| p == pointer)
                    {
                        shape.set_shape(serial, Shape::Default);
                    }
                    window.dispatch_event(WindowEvent::PointerMoved { position });
                }
                PointerEventKind::Leave { .. } => window.dispatch_event(WindowEvent::PointerExited),
                PointerEventKind::Motion { .. } => {
                    window.dispatch_event(WindowEvent::PointerMoved { position });
                }
                PointerEventKind::Press { button, .. } => {
                    window.dispatch_event(WindowEvent::PointerPressed {
                        position,
                        button: button_of(button),
                    });
                }
                PointerEventKind::Release { button, .. } => {
                    window.dispatch_event(WindowEvent::PointerReleased {
                        position,
                        button: button_of(button),
                    });
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    // Slint scrolls content the other way round from Wayland's axes.
                    window.dispatch_event(WindowEvent::PointerScrolled {
                        position,
                        delta_x: -horizontal.absolute as f32,
                        delta_y: -vertical.absolute as f32,
                    });
                }
            }
        }
    }
}

/// Keyboard state belongs to its seat; no keyboard is bound for passive overlays.
struct Keyboard {
    object: wl_keyboard::WlKeyboard,
    state: Option<xkb::State>,
    focused: bool,
    modifiers: [bool; 4],
    rate: u32,
    delay: u32,
    repeat: Option<(u32, slint::SharedString, Instant)>,
}

impl Keyboard {
    fn sync_modifiers(&mut self, window: &slint::Window) {
        use slint::platform::Key;
        for (i, (name, key)) in [
            ("Shift", Key::Shift),
            ("Control", Key::Control),
            ("Mod1", Key::Alt),
            ("Mod4", Key::Meta),
        ]
        .into_iter()
        .enumerate()
        {
            let active = self.focused
                && self
                    .state
                    .as_ref()
                    .is_some_and(|s| s.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE));
            if active != self.modifiers[i] {
                self.modifiers[i] = active;
                window.dispatch_event(if active {
                    WindowEvent::KeyPressed { text: key.into() }
                } else {
                    WindowEvent::KeyReleased { text: key.into() }
                });
            }
        }
    }
}

impl State {
    fn repeat_deadline(&self) -> Option<Duration> {
        self.keyboards
            .iter()
            .filter_map(|k| {
                k.repeat
                    .as_ref()
                    .map(|(_, _, at)| at.saturating_duration_since(Instant::now()))
            })
            .min()
    }

    fn repeat_keys(&mut self) {
        for keyboard in &mut self.keyboards {
            if let Some((_, text, at)) = &mut keyboard.repeat
                && keyboard.focused
                && *at <= Instant::now()
            {
                self.window
                    .window()
                    .dispatch_event(WindowEvent::KeyPressRepeated { text: text.clone() });
                *at =
                    Instant::now() + Duration::from_millis(1000 / u64::from(keyboard.rate.max(1)));
            }
        }
    }
}

fn key_text(state: &xkb::State, key: u32) -> slint::SharedString {
    use slint::platform::Key;
    let code = xkb::Keycode::new(key + 8);
    match state.key_get_one_sym(code).raw() {
        xkb::keysyms::KEY_Escape => Key::Escape.into(),
        xkb::keysyms::KEY_Return | xkb::keysyms::KEY_KP_Enter => Key::Return.into(),
        xkb::keysyms::KEY_Up => Key::UpArrow.into(),
        xkb::keysyms::KEY_Down => Key::DownArrow.into(),
        xkb::keysyms::KEY_Left => Key::LeftArrow.into(),
        xkb::keysyms::KEY_Right => Key::RightArrow.into(),
        xkb::keysyms::KEY_Home => Key::Home.into(),
        xkb::keysyms::KEY_End => Key::End.into(),
        xkb::keysyms::KEY_Page_Up => Key::PageUp.into(),
        xkb::keysyms::KEY_Page_Down => Key::PageDown.into(),
        xkb::keysyms::KEY_Tab | xkb::keysyms::KEY_ISO_Left_Tab => Key::Tab.into(),
        xkb::keysyms::KEY_Shift_L | xkb::keysyms::KEY_Shift_R => Key::Shift.into(),
        xkb::keysyms::KEY_Control_L | xkb::keysyms::KEY_Control_R => Key::Control.into(),
        xkb::keysyms::KEY_Alt_L | xkb::keysyms::KEY_Alt_R => Key::Alt.into(),
        _ => state.key_get_utf8(code).into(),
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, wl_seat::WlSeat> for State {
    fn event(
        state: &mut Self,
        object: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &wl_seat::WlSeat,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let ours = state.layer.as_ref().map(|l| l.wl_surface().clone());
        let Some(keyboard) = state.keyboards.iter_mut().find(|k| &k.object == object) else {
            return;
        };
        let window = state.window.window();
        match event {
            wl_keyboard::Event::Keymap {
                format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                fd,
                size,
            } => {
                use std::io::Read as _;
                let mut bytes = Vec::new();
                let file = std::fs::File::from(fd);
                keyboard.state = None;
                keyboard.repeat = None;
                if size <= 16 * 1024 * 1024
                    && file.take(u64::from(size)).read_to_end(&mut bytes).is_ok()
                {
                    while bytes.last() == Some(&0) {
                        bytes.pop();
                    }
                    if let Ok(text) = String::from_utf8(bytes) {
                        keyboard.state = xkb::Keymap::new_from_string(
                            &xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
                            text,
                            xkb::KEYMAP_FORMAT_TEXT_V1,
                            xkb::KEYMAP_COMPILE_NO_FLAGS,
                        )
                        .map(|k| xkb::State::new(&k));
                    }
                }
            }
            wl_keyboard::Event::Enter { surface, .. } if Some(&surface) == ours.as_ref() => {
                keyboard.focused = true;
                window.dispatch_event(WindowEvent::WindowActiveChanged(true));
                keyboard.sync_modifiers(window);
                debug!("overlay keyboard focused");
            }
            wl_keyboard::Event::Leave { surface, .. } if Some(&surface) == ours.as_ref() => {
                keyboard.focused = false;
                keyboard.repeat = None;
                keyboard.sync_modifiers(window);
                window.dispatch_event(WindowEvent::WindowActiveChanged(false));
                window.dispatch_event(WindowEvent::CloseRequested);
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(xkb) = &mut keyboard.state {
                    xkb.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
                keyboard.sync_modifiers(window);
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                keyboard.rate = rate.clamp(0, 100) as u32;
                keyboard.delay = delay.max(0) as u32;
                if rate <= 0 {
                    keyboard.repeat = None;
                }
            }
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(pressed),
                ..
            } if keyboard.focused => {
                let Some(xkb) = &keyboard.state else {
                    return;
                };
                let text = key_text(xkb, key);
                if text.is_empty() {
                    return;
                }
                if pressed == wl_keyboard::KeyState::Pressed {
                    if keyboard.rate > 0 && xkb.get_keymap().key_repeats(xkb::Keycode::new(key + 8))
                    {
                        keyboard.repeat = Some((
                            key,
                            text.clone(),
                            Instant::now() + Duration::from_millis(u64::from(keyboard.delay)),
                        ));
                    }
                    window.dispatch_event(WindowEvent::KeyPressed { text });
                } else {
                    if keyboard.repeat.as_ref().is_some_and(|(k, _, _)| *k == key) {
                        keyboard.repeat = None;
                    }
                    window.dispatch_event(WindowEvent::KeyReleased { text });
                }
            }
            _ => {}
        }
    }
}

/// Slint's name for a Linux input event code of a pointer button.
fn button_of(code: u32) -> PointerEventButton {
    // linux/input-event-codes.h
    const BTN_LEFT: u32 = 0x110;
    const BTN_RIGHT: u32 = 0x111;
    const BTN_MIDDLE: u32 = 0x112;
    const BTN_SIDE: u32 = 0x113;
    const BTN_EXTRA: u32 = 0x114;
    match code {
        BTN_LEFT => PointerEventButton::Left,
        BTN_RIGHT => PointerEventButton::Right,
        BTN_MIDDLE => PointerEventButton::Middle,
        BTN_SIDE => PointerEventButton::Back,
        BTN_EXTRA => PointerEventButton::Forward,
        _ => PointerEventButton::Other,
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(State);
delegate_seat!(State);
delegate_pointer!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_layer!(State);
delegate_registry!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_blend_as_premultiplied_bgra() {
        let mut pixel = Bgra::background();
        assert_eq!((pixel.b, pixel.g, pixel.r, pixel.a), (0, 0, 0, 0));
        // Half transparent red over nothing.
        pixel.blend(PremultipliedRgbaColor {
            red: 128,
            green: 0,
            blue: 0,
            alpha: 128,
        });
        assert_eq!((pixel.b, pixel.g, pixel.r, pixel.a), (0, 0, 128, 128));
        // Opaque blue over it.
        pixel.blend(PremultipliedRgbaColor {
            red: 0,
            green: 0,
            blue: 255,
            alpha: 255,
        });
        assert_eq!((pixel.b, pixel.g, pixel.r, pixel.a), (255, 0, 0, 255));
        let white = Bgra::from_rgb(255, 255, 255);
        assert_eq!((white.b, white.g, white.r, white.a), (255, 255, 255, 255));
    }
}
