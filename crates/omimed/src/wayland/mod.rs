//! The Wayland frontend: the daemon's own objects on the compositor connection.
//!
//! All Wayland objects live in a single [`wl_proxy::state::State`]. Handlers installed on
//! our objects never act directly: they push [`Event`]s into a queue that is drained
//! after every dispatch. This keeps all state changes in one place and avoids
//! re-entrancy into wl-proxy.
//!
//! The frontend reports what the core needs to know as [`FrontendEvent`]s and carries
//! out the core's decisions through methods such as [`Frontend::finish_key`] and
//! [`Frontend::apply_actions`].

mod bridge;
mod seat;
mod toplevels;

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::time::Instant;

use anyhow::{anyhow, bail};
use omime_proto::engine::{Action, ContextId, ContextInfo, KeyEvent, Rect};
use tracing::{debug, error, info, warn};
use wl_proxy::baseline::Baseline;
use wl_proxy::object::{Object, ObjectCoreApi};
use wl_proxy::protocols::ObjectInterface;
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_keyboard_grab_v2::{
    ZwpInputMethodKeyboardGrabV2, ZwpInputMethodKeyboardGrabV2Handler,
};
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_v2::{
    ZwpInputMethodV2, ZwpInputMethodV2Handler,
};
use wl_proxy::protocols::text_input_unstable_v3::zwp_text_input_v3::{
    ZwpTextInputV3ChangeCause, ZwpTextInputV3ContentHint, ZwpTextInputV3ContentPurpose,
};
use wl_proxy::protocols::virtual_keyboard_unstable_v1::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wl_proxy::protocols::wayland::wl_callback::{WlCallback, WlCallbackHandler};
use wl_proxy::protocols::wayland::wl_keyboard::{WlKeyboardKeyState, WlKeyboardKeymapFormat};
use wl_proxy::protocols::wayland::wl_registry::{WlRegistry, WlRegistryHandler};
use wl_proxy::protocols::wayland::wl_seat::{WlSeat, WlSeatCapability, WlSeatHandler};
use wl_proxy::protocols::wlr_foreign_toplevel_management_unstable_v1::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1;
use wl_proxy::state::{State, StateError, StateHandler};

use crate::keyboard::{ModifierState, RepeatInfo};
use bridge::Bridge;
pub use bridge::EngineId;
use seat::SeatSession;
use toplevels::{ToplevelEvent, Toplevels};

/// Identifies a seat by the registry name of its `wl_seat` global.
pub type SeatId = u32;

/// Events produced by handlers of the daemon's own Wayland objects.
#[derive(Debug)]
pub enum Event {
    Global {
        name: u32,
        interface: ObjectInterface,
        version: u32,
    },
    GlobalRemove {
        name: u32,
    },
    InitialSyncDone,
    SeatName {
        seat: SeatId,
        name: String,
    },
    Im {
        seat: SeatId,
        generation: u32,
        event: ImEvent,
    },
    Grab {
        seat: SeatId,
        generation: u32,
        event: GrabEvent,
    },
    /// A window's state changed (see [`toplevels`]).
    Toplevel(ToplevelEvent),
    /// The text cursor rectangle relative to an engine's popup changed.
    PopupCursorRect {
        engine: EngineId,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    /// The compositor sent a fatal protocol error.
    ProtocolError {
        message: String,
    },
}

/// Events of a `zwp_input_method_v2`.
#[derive(Debug)]
pub enum ImEvent {
    Activate,
    Deactivate,
    SurroundingText {
        text: String,
        cursor: u32,
        anchor: u32,
    },
    TextChangeCause(u32),
    ContentType {
        hints: u32,
        purpose: u32,
    },
    Done,
    Unavailable,
}

/// Events of a `zwp_input_method_keyboard_grab_v2`.
pub enum GrabEvent {
    Keymap {
        format: u32,
        fd: Rc<OwnedFd>,
        size: u32,
    },
    Key {
        time: u32,
        key: u32,
        state: u32,
    },
    Modifiers(ModifierState),
    RepeatInfo(RepeatInfo),
}

impl std::fmt::Debug for GrabEvent {
    // Key codes reveal what the user types, so they are never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keymap { format, size, .. } => f
                .debug_struct("Keymap")
                .field("format", format)
                .field("size", size)
                .finish(),
            Self::Key { state, .. } => f.debug_struct("Key").field("state", state).finish(),
            Self::Modifiers(_) => f.write_str("Modifiers"),
            Self::RepeatInfo(r) => f.debug_tuple("RepeatInfo").field(r).finish(),
        }
    }
}

/// Queue shared between wl-proxy handlers and the frontend.
pub type EventQueue = Rc<RefCell<VecDeque<Event>>>;

/// Identifies a key the core has to decide about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyRef {
    pub seat: SeatId,
    /// The text field the key was typed into (0 if none).
    pub ctx: ContextId,
    pub keycode: u32,
    pub time: u32,
    pub released: bool,
    pub repeat: bool,
}

/// What the frontend reports to the core.
#[derive(Debug)]
pub enum FrontendEvent {
    /// A (non-sensitive) text field got focus.
    Activated {
        seat: SeatId,
        ctx: ContextId,
        info: ContextInfo,
    },
    /// Properties of the focused field changed. `reset` means the application changed
    /// the text on its own.
    Updated {
        ctx: ContextId,
        info: ContextInfo,
        reset: bool,
    },
    /// The text field lost focus.
    Deactivated { ctx: ContextId },
    /// A key needs a decision: call [`Frontend::finish_key`].
    Key { key: KeyRef, event: KeyEvent },
    /// The text cursor relative to an engine's popup changed.
    CursorRect { engine: EngineId, rect: Rect },
    /// Another application got focus (`None` when unknown). Sent after the `Updated`
    /// events of the focused fields, which carry the new app id.
    FocusedApp(Option<String>),
}

/// Queue of [`FrontendEvent`]s, shared with the seat sessions.
pub type Outputs = Rc<RefCell<VecDeque<FrontendEvent>>>;

const SEAT_VERSION: u32 = 7;

struct BoundGlobal<T> {
    name: u32,
    object: Rc<T>,
}

/// The daemon's connection to the compositor.
pub struct Frontend {
    state: Rc<State>,
    queue: EventQueue,
    registry: Rc<WlRegistry>,
    im_manager: Option<BoundGlobal<ZwpInputMethodManagerV2>>,
    vk_manager: Option<BoundGlobal<ZwpVirtualKeyboardManagerV1>>,
    toplevel_manager: Option<BoundGlobal<ZwlrForeignToplevelManagerV1>>,
    toplevels: Toplevels,
    focused_app: Option<String>,
    /// Bound `wl_seat`s by registry name, before and after a session exists.
    seat_objects: BTreeMap<SeatId, Rc<WlSeat>>,
    seats: BTreeMap<SeatId, SeatSession>,
    initial_sync_done: bool,
    bridge: Bridge,
    active_engine: Option<EngineId>,
    outputs: Outputs,
    /// Last context id handed out; ids are never reused within a daemon's lifetime.
    last_ctx: ContextId,
}

impl std::fmt::Debug for Frontend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frontend")
            .field("seats", &self.seats.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Frontend {
    /// Connects to the compositor named by `display` (or `WAYLAND_DISPLAY`).
    pub fn connect(display: Option<&str>, last_ctx: ContextId) -> anyhow::Result<Self> {
        let mut builder = State::builder(Baseline::V6);
        if let Some(display) = display {
            builder = builder.with_server_display_name(display);
        }
        let state = builder
            .with_logging(std::env::var_os("OMIME_WL_DEBUG").is_some())
            .build()
            .map_err(|e| wl_error("failed to connect to the Wayland compositor", e))?;

        let queue = EventQueue::default();
        state.set_handler(StateEvents {
            queue: queue.clone(),
        });

        let display = state.display();
        let registry = display.new_send_get_registry();
        registry.set_forward_to_client(false);
        let bridge = Bridge::new(state.clone(), registry.clone(), queue.clone());
        // Engines may connect at any moment; they must see globals no later than their
        // first roundtrip completes, so the bridge learns about them right away.
        registry.set_handler(RegistryEvents {
            queue: queue.clone(),
            bridge: bridge.clone(),
        });
        let sync = display.new_send_sync();
        sync.set_forward_to_client(false);
        sync.set_handler(SyncEvents {
            queue: queue.clone(),
        });
        Ok(Self {
            state,
            queue,
            registry,
            im_manager: None,
            vk_manager: None,
            toplevel_manager: None,
            toplevels: Toplevels::default(),
            focused_app: None,
            seat_objects: BTreeMap::new(),
            seats: BTreeMap::new(),
            initial_sync_done: false,
            bridge,
            active_engine: None,
            outputs: Outputs::default(),
            last_ctx,
        })
    }

    pub fn state(&self) -> &Rc<State> {
        &self.state
    }

    /// Flushes outgoing messages. Must be called before waiting for the poll fd.
    pub fn flush(&self) -> anyhow::Result<()> {
        self.state
            .before_poll()
            .map_err(|e| wl_error("failed to flush the Wayland connection", e))
    }

    /// Reads and dispatches everything that is available, then handles the resulting
    /// events.
    pub fn dispatch(&mut self) -> anyhow::Result<()> {
        while self
            .state
            .dispatch_available()
            .map_err(|e| wl_error("failed to dispatch Wayland messages", e))?
        {}
        self.process_events()
    }

    /// The earliest moment at which [`Self::on_timer`] wants to run.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.seats.values().filter_map(SeatSession::deadline).min()
    }

    pub fn on_timer(&mut self, now: Instant) -> anyhow::Result<()> {
        if let Some(manager) = &self.im_manager {
            for seat in self.seats.values_mut() {
                seat.on_timer(now, &manager.object);
            }
        }
        self.process_events()
    }

    /// Whether the initial roundtrip finished and input method sessions exist.
    pub fn is_ready(&self) -> bool {
        self.initial_sync_done
    }

    /// The last context id handed out, to continue numbering after a reconnect.
    pub fn last_context_id(&self) -> ContextId {
        self.last_ctx
    }

    /// Takes the events accumulated for the core.
    pub fn take_events(&mut self) -> Vec<FrontendEvent> {
        self.outputs.borrow_mut().drain(..).collect()
    }

    /// Forwards or swallows a key reported in [`FrontendEvent::Key`].
    pub fn finish_key(&mut self, key: &KeyRef, handled: bool) {
        if let Some(seat) = self.seats.get_mut(&key.seat) {
            seat.finish_key(key, handled);
        }
    }

    /// Applies an engine's actions to the text field `ctx` on `seat`.
    pub fn apply_actions(&mut self, seat: SeatId, ctx: ContextId, actions: &[Action]) {
        if let Some(seat) = self.seats.get_mut(&seat) {
            seat.apply_actions(ctx, actions);
        }
    }

    /// Removes any preedit from the text field `ctx` on `seat`.
    pub fn clear_preedit(&mut self, seat: SeatId, ctx: ContextId) {
        if let Some(seat) = self.seats.get_mut(&seat) {
            seat.clear_preedit(ctx);
        }
    }

    /// Whether `engine` wants its windows shown while it is active.
    pub fn set_engine_visible(&mut self, engine: EngineId, visible: bool) {
        self.bridge.set_engine_visible(engine, visible);
        self.update_bridge();
    }

    /// Hands a UI connection of `engine` to the bridge.
    pub fn add_ui_client(&self, engine: EngineId, socket: OwnedFd) -> anyhow::Result<()> {
        self.bridge.add_client(engine, socket)
    }

    /// Selects the engine whose windows may be shown.
    pub fn set_active_engine(&mut self, engine: Option<EngineId>) {
        self.active_engine = engine;
        self.update_bridge();
    }

    /// Tells the bridge which input method hosts popups and whether it is active.
    fn update_bridge(&self) {
        let focused = |s: &&SeatSession| s.context().is_some();
        let host = self
            .seats
            .values()
            .find(focused)
            .or_else(|| self.seats.values().find(|s| s.input_method().is_some()));
        self.bridge.set_policy(
            host.and_then(|s| s.input_method().cloned()),
            host.is_some_and(|s| s.context().is_some()),
            self.active_engine.clone(),
        );
    }

    /// Releases everything we hold on the compositor (keys, grabs, input methods).
    pub fn shutdown(&mut self) {
        for seat in self.seats.values_mut() {
            seat.shutdown();
        }
        self.seats.clear();
    }

    fn process_events(&mut self) -> anyhow::Result<()> {
        let mut handled = false;
        loop {
            let Some(event) = self.queue.borrow_mut().pop_front() else {
                break;
            };
            handled = true;
            self.handle_event(event)?;
        }
        if handled {
            self.update_bridge();
        }
        Ok(())
    }

    fn handle_event(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Global {
                name,
                interface,
                version,
            } => self.on_global(name, interface, version),
            Event::GlobalRemove { name } => self.on_global_remove(name),
            Event::InitialSyncDone => {
                self.initial_sync_done = true;
                if self.im_manager.is_none() {
                    bail!(
                        "the compositor does not support zwp_input_method_manager_v2; \
                         oh-my-ime needs a compositor with input-method-v2 (e.g. Hyprland or sway)"
                    );
                }
                if self.vk_manager.is_none() {
                    bail!(
                        "the compositor does not support zwp_virtual_keyboard_manager_v1, \
                         which oh-my-ime needs to pass keys through"
                    );
                }
                self.start_sessions();
            }
            Event::SeatName { seat, name } => {
                if let Some(session) = self.seats.get_mut(&seat) {
                    session.set_name(name);
                }
            }
            Event::Im {
                seat,
                generation,
                event,
            } => {
                if let Some(session) = self.seats.get_mut(&seat) {
                    session.handle_im(generation, event, &mut self.last_ctx);
                }
            }
            Event::Grab {
                seat,
                generation,
                event,
            } => {
                if let Some(session) = self.seats.get_mut(&seat) {
                    session.handle_grab(generation, event);
                }
            }
            Event::Toplevel(event) => {
                self.toplevels.handle(event);
                self.update_focused_app();
            }
            Event::PopupCursorRect {
                engine,
                x,
                y,
                width,
                height,
            } => {
                self.outputs
                    .borrow_mut()
                    .push_back(FrontendEvent::CursorRect {
                        engine,
                        rect: (x, y, width, height),
                    });
            }
            Event::ProtocolError { message } => {
                bail!("the compositor closed the connection: {message}");
            }
        }
        Ok(())
    }

    fn on_global(&mut self, name: u32, interface: ObjectInterface, version: u32) {
        match interface {
            ObjectInterface::ZwpInputMethodManagerV2 => {
                let object = self.state.create_object::<ZwpInputMethodManagerV2>(1);
                object.set_forward_to_client(false);
                self.registry.send_bind(name, object.clone());
                self.im_manager = Some(BoundGlobal { name, object });
            }
            ObjectInterface::ZwpVirtualKeyboardManagerV1 => {
                let object = self.state.create_object::<ZwpVirtualKeyboardManagerV1>(1);
                object.set_forward_to_client(false);
                self.registry.send_bind(name, object.clone());
                self.vk_manager = Some(BoundGlobal { name, object });
            }
            ObjectInterface::ZwlrForeignToplevelManagerV1 => {
                let object = self
                    .state
                    .create_object::<ZwlrForeignToplevelManagerV1>(version.min(toplevels::VERSION));
                object.set_forward_to_client(false);
                object.set_handler(toplevels::ManagerEvents::new(self.queue.clone()));
                self.registry.send_bind(name, object.clone());
                self.toplevel_manager = Some(BoundGlobal { name, object });
            }
            ObjectInterface::WlSeat => {
                let seat = self
                    .state
                    .create_object::<WlSeat>(version.min(SEAT_VERSION));
                seat.set_forward_to_client(false);
                seat.set_handler(SeatEvents {
                    seat: name,
                    queue: self.queue.clone(),
                });
                self.registry.send_bind(name, seat.clone());
                self.seat_objects.insert(name, seat);
                if self.initial_sync_done {
                    self.start_sessions();
                }
            }
            _ => {}
        }
    }

    fn on_global_remove(&mut self, name: u32) {
        if let Some(mut session) = self.seats.remove(&name) {
            info!(seat = session.name(), "seat removed");
            session.shutdown();
        }
        if let Some(seat) = self.seat_objects.remove(&name)
            && seat.version() >= 5
        {
            seat.send_release();
        }
        if self.im_manager.as_ref().is_some_and(|g| g.name == name) {
            warn!("the compositor removed zwp_input_method_manager_v2");
            self.im_manager = None;
        }
        if self.vk_manager.as_ref().is_some_and(|g| g.name == name) {
            warn!("the compositor removed zwp_virtual_keyboard_manager_v1");
            self.vk_manager = None;
        }
        if self
            .toplevel_manager
            .as_ref()
            .is_some_and(|g| g.name == name)
        {
            self.toplevel_manager = None;
            self.toplevels.handle(ToplevelEvent::Finished);
            self.update_focused_app();
        }
    }

    /// Tells the seats and the core when another application got focus.
    fn update_focused_app(&mut self) {
        let app = self.toplevels.focused_app().map(str::to_owned);
        if app == self.focused_app {
            return;
        }
        debug!(
            app = app.as_deref().unwrap_or("unknown"),
            "focused application changed"
        );
        self.focused_app = app.clone();
        for seat in self.seats.values_mut() {
            seat.set_app_id(app.clone());
        }
        self.outputs
            .borrow_mut()
            .push_back(FrontendEvent::FocusedApp(app));
    }

    fn start_sessions(&mut self) {
        let (Some(im_manager), Some(vk_manager)) = (&self.im_manager, &self.vk_manager) else {
            return;
        };
        for (&id, seat) in &self.seat_objects {
            if self.seats.contains_key(&id) {
                continue;
            }
            debug!(seat = id, "starting input method session");
            let mut session = SeatSession::new(
                id,
                seat.clone(),
                &im_manager.object,
                &vk_manager.object,
                self.queue.clone(),
                self.outputs.clone(),
            );
            session.set_app_id(self.focused_app.clone());
            self.seats.insert(id, session);
        }
    }
}

/// `StateError` is not `Send`, so it cannot be wrapped by `anyhow` directly.
fn wl_error(context: &str, error: StateError) -> anyhow::Error {
    anyhow!("{context}: {error}")
}

struct StateEvents {
    queue: EventQueue,
}

impl StateHandler for StateEvents {
    fn display_error(
        self: Box<Self>,
        object: Option<&Rc<dyn Object>>,
        server_id: u32,
        error: u32,
        msg: &str,
    ) {
        let interface = object.map_or("<deleted>", |o| o.interface().name());
        let message = format!("protocol error {error} on {interface}#{server_id}: {msg}");
        error!("{message}");
        self.queue
            .borrow_mut()
            .push_back(Event::ProtocolError { message });
    }
}

struct RegistryEvents {
    queue: EventQueue,
    bridge: Bridge,
}

impl WlRegistryHandler for RegistryEvents {
    fn handle_global(
        &mut self,
        _slf: &Rc<WlRegistry>,
        name: u32,
        interface: ObjectInterface,
        version: u32,
    ) {
        self.bridge.add_global(name, interface, version);
        self.queue.borrow_mut().push_back(Event::Global {
            name,
            interface,
            version,
        });
    }

    fn handle_global_remove(&mut self, _slf: &Rc<WlRegistry>, name: u32) {
        self.bridge.remove_global(name);
        self.queue
            .borrow_mut()
            .push_back(Event::GlobalRemove { name });
    }
}

struct SyncEvents {
    queue: EventQueue,
}

impl WlCallbackHandler for SyncEvents {
    fn handle_done(&mut self, _slf: &Rc<WlCallback>, _callback_data: u32) {
        self.queue.borrow_mut().push_back(Event::InitialSyncDone);
    }
}

struct SeatEvents {
    seat: SeatId,
    queue: EventQueue,
}

impl WlSeatHandler for SeatEvents {
    fn handle_capabilities(&mut self, _slf: &Rc<WlSeat>, _capabilities: WlSeatCapability) {}

    fn handle_name(&mut self, _slf: &Rc<WlSeat>, name: &str) {
        self.queue.borrow_mut().push_back(Event::SeatName {
            seat: self.seat,
            name: name.to_owned(),
        });
    }
}

struct ImEvents {
    seat: SeatId,
    generation: u32,
    queue: EventQueue,
}

impl ImEvents {
    fn push(&self, event: ImEvent) {
        self.queue.borrow_mut().push_back(Event::Im {
            seat: self.seat,
            generation: self.generation,
            event,
        });
    }
}

impl ZwpInputMethodV2Handler for ImEvents {
    fn handle_activate(&mut self, _slf: &Rc<ZwpInputMethodV2>) {
        self.push(ImEvent::Activate);
    }

    fn handle_deactivate(&mut self, _slf: &Rc<ZwpInputMethodV2>) {
        self.push(ImEvent::Deactivate);
    }

    fn handle_surrounding_text(
        &mut self,
        _slf: &Rc<ZwpInputMethodV2>,
        text: &str,
        cursor: u32,
        anchor: u32,
    ) {
        self.push(ImEvent::SurroundingText {
            text: text.to_owned(),
            cursor,
            anchor,
        });
    }

    fn handle_text_change_cause(
        &mut self,
        _slf: &Rc<ZwpInputMethodV2>,
        cause: ZwpTextInputV3ChangeCause,
    ) {
        self.push(ImEvent::TextChangeCause(cause.0));
    }

    fn handle_content_type(
        &mut self,
        _slf: &Rc<ZwpInputMethodV2>,
        hint: ZwpTextInputV3ContentHint,
        purpose: ZwpTextInputV3ContentPurpose,
    ) {
        self.push(ImEvent::ContentType {
            hints: hint.0,
            purpose: purpose.0,
        });
    }

    fn handle_done(&mut self, _slf: &Rc<ZwpInputMethodV2>) {
        self.push(ImEvent::Done);
    }

    fn handle_unavailable(&mut self, _slf: &Rc<ZwpInputMethodV2>) {
        self.push(ImEvent::Unavailable);
    }
}

struct GrabEvents {
    seat: SeatId,
    generation: u32,
    queue: EventQueue,
}

impl GrabEvents {
    fn push(&self, event: GrabEvent) {
        self.queue.borrow_mut().push_back(Event::Grab {
            seat: self.seat,
            generation: self.generation,
            event,
        });
    }
}

impl ZwpInputMethodKeyboardGrabV2Handler for GrabEvents {
    fn handle_keymap(
        &mut self,
        _slf: &Rc<ZwpInputMethodKeyboardGrabV2>,
        format: WlKeyboardKeymapFormat,
        fd: &Rc<OwnedFd>,
        size: u32,
    ) {
        self.push(GrabEvent::Keymap {
            format: format.0,
            fd: fd.clone(),
            size,
        });
    }

    fn handle_key(
        &mut self,
        _slf: &Rc<ZwpInputMethodKeyboardGrabV2>,
        _serial: u32,
        time: u32,
        key: u32,
        state: WlKeyboardKeyState,
    ) {
        self.push(GrabEvent::Key {
            time,
            key,
            state: state.0,
        });
    }

    fn handle_modifiers(
        &mut self,
        _slf: &Rc<ZwpInputMethodKeyboardGrabV2>,
        _serial: u32,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    ) {
        self.push(GrabEvent::Modifiers(ModifierState {
            depressed: mods_depressed,
            latched: mods_latched,
            locked: mods_locked,
            group,
        }));
    }

    fn handle_repeat_info(
        &mut self,
        _slf: &Rc<ZwpInputMethodKeyboardGrabV2>,
        rate: i32,
        delay: i32,
    ) {
        self.push(GrabEvent::RepeatInfo(RepeatInfo { rate, delay }));
    }
}
