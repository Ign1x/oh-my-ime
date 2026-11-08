//! XIM, the X Input Method protocol, for X11 applications that load no input method
//! module of their own: xterm, Java and Wine applications, GTK with `GTK_IM_MODULE=xim`
//! and others. They find omimed through `XMODIFIERS=@im=omime`.
//!
//! The `xim` crate speaks the protocol. Engines answer keys asynchronously, so omimed
//! takes every key an application forwards and, once the engine decided, sends back the
//! ones it did not consume, after the text the engine produced for them.
//!
//! libX11 hands a commit or a key sent back to the application by pushing an event onto
//! the front of its queue, so two in a row would arrive in the wrong order. Commits and
//! keys therefore go out one at a time as synchronous requests: libX11 answers with
//! `XIM_SYNC_REPLY` once the application consumed the event, and only then the next one
//! follows.

mod keymap;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::num::NonZeroU16;
use std::os::fd::AsFd as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use omime_proto::engine::{Action, ContextId, ContextInfo, Preedit};
use tokio::io::unix::AsyncFd;
use tracing::{debug, info, warn};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt as _, EventMask,
    KeyPressEvent, PropMode,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use xim::x11rb::{HasConnection as _, X11rbServer};
use xim::{
    CommitData, ErrorCode, Feedback, ForwardEventFlag, InputContext, InputStyle, PreeditDrawStatus,
    Request, Server as _, ServerCore as _, ServerError, ServerHandler, UserInputContext, XEvent,
    XimConnections, XimWrite as _,
};

use crate::daemon::{PollFd, watch};
use crate::frontend::{self, ContextIds, FrontendEvent, Origin};
use crate::placement::{self, Monitor};
use keymap::Keymap;

/// The name applications ask for with `XMODIFIERS=@im=omime`.
pub const SERVER_NAME: &str = "omime";
/// Other names omimed answers by, when no other XIM server holds them: the ones that
/// setups of Fcitx 5 and IBus leave in `XMODIFIERS`, so that applications started with
/// them reach omimed too.
pub const ALIASES: [&str; 2] = ["fcitx", "ibus"];
/// `KeyPressMask`: applications forward presses. Releases stay with them: every key
/// sent back costs a round trip, and applications need not answer one for a release.
const KEY_EVENTS: u32 = 1;
/// The major opcode of `XIM_SYNC_REPLY`.
const XIM_SYNC_REPLY: u8 = 62;
/// The major opcode of `XIM_CREATE_IC`.
const XIM_CREATE_IC: u8 = 50;
/// `xim` increments its ID before storing a context, and panics when it reaches zero.
/// Count all allocations on a connection, including contexts already destroyed.
const MAX_IC_ALLOCATIONS: u16 = u16::MAX - 1;
/// How long to wait before retiring an unresponsive application's output.
const SYNC_TIMEOUT: Duration = Duration::from_millis(300);
/// Bound output retained while another request is waiting for its acknowledgement.
const MAX_QUEUED_REQUESTS_PER_CLIENT: usize = 256;
const MAX_QUEUED_BYTES_PER_CLIENT: usize = 1024 * 1024;
/// How long omimed waits for the X server when it starts. Later attempts do not wait:
/// they would hold up keyboard input.
pub const STARTUP_CONNECT_WAIT: Duration = Duration::from_secs(3);
/// X11 event codes.
const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
/// X11 key codes are evdev key codes plus 8.
const X11_KEYCODE_OFFSET: u8 = 8;

/// An input context: client window, input method id, input context id.
type IcKey = (u32, u16, u16);

/// A key from an application, waiting for the engine's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyRef {
    pub ctx: ContextId,
    id: u64,
}

#[derive(Debug, Default)]
struct Ic {
    focused: bool,
    /// The core's context while the field has focus.
    ctx: Option<ContextId>,
    /// Whether the application draws the preedit (on-the-spot input style).
    preedit_callbacks: bool,
    preedit_started: bool,
    /// Characters of the preedit after the queued draws have been sent.
    preedit_len: usize,
    /// Last preedit actually sent, before any queued changes that a timeout can drop.
    sent_preedit: Option<usize>,
    /// The last key the application forwarded, a template for keys sent to it.
    last_event: Option<XEvent>,
    /// Where the application draws text, in X screen coordinates (the start of the
    /// baseline).
    spot: Option<(i32, i32)>,
}

impl Ic {
    /// What the core and engines learn about the field.
    fn info(&self) -> ContextInfo {
        ContextInfo {
            frontend: Some("xim".into()),
            // Applications of the other input styles leave drawing it to the input
            // method.
            client_preedit: self.preedit_callbacks,
            ..ContextInfo::default()
        }
    }
}

/// A key the engine has not answered yet.
#[derive(Debug)]
struct Pending {
    ic: IcKey,
    event: XEvent,
}

/// The `xim` crate's callbacks.
struct Handler {
    context_ids: ContextIds,
    /// The root window of the screen.
    root: u32,
    /// The communication window of the protocol message being handled.
    com_win: Option<u32>,
    /// The CreateIc being handled, for an error that libX11 can match to its request.
    create_ic_im: Option<NonZeroU16>,
    /// The communication window of each client window with input contexts.
    clients: BTreeMap<u32, u32>,
    ic_allocations: BTreeMap<u32, u16>,
    /// Connections already warned about their lifetime input context allocation limit.
    ic_limit_warned: BTreeSet<u32>,
    /// Timed-out clients stay suspended until their outstanding IC replies or ends.
    stalled: BTreeMap<u32, (u16, u16)>,
    /// A validated DestroyIc/Close removed contexts since the last output cleanup.
    destroyed_contexts: bool,
    keymap: Keymap,
    ics: BTreeMap<IcKey, Ic>,
    pending: BTreeMap<u64, Pending>,
    next_key: u64,
    events: VecDeque<FrontendEvent>,
}

fn ic_key(ic: &InputContext) -> IcKey {
    (
        ic.client_win(),
        ic.input_method_id().get(),
        ic.input_context_id().get(),
    )
}

impl Handler {
    fn reserve_ic(&mut self, com_win: u32) -> bool {
        let count = self.ic_allocations.entry(com_win).or_default();
        if *count >= MAX_IC_ALLOCATIONS {
            return false;
        }
        *count += 1;
        true
    }

    fn allocate_ic(&mut self, server: &mut impl xim::Server) -> Result<(), ServerError> {
        let com_win = self.com_win.ok_or(ServerError::ClientNotExists)?;
        if !self.reserve_ic(com_win) {
            if self.ic_limit_warned.insert(com_win) {
                warn!(
                    com_win,
                    "XIM: input context allocation limit reached; refusing CreateIc"
                );
            }
            // libX11 waits for an error with a valid, matching IMID. An error without
            // it is unrelated to the pending CreateIc and leaves the caller waiting.
            let im = self.create_ic_im.ok_or(ServerError::ClientNotExists)?;
            if let Some((&client_win, _)) = self.clients.iter().find(|(_, win)| **win == com_win) {
                server.error(
                    client_win,
                    ErrorCode::BadAlloc,
                    String::new(),
                    Some(im),
                    None,
                )?;
            }
            return Err(ServerError::XimError(ErrorCode::BadAlloc, String::new()));
        }
        Ok(())
    }

    fn forget_connection(
        &mut self,
        com_win: u32,
        queue: &mut VecDeque<Queued>,
        waiting: &mut Option<SyncWait>,
    ) {
        self.ic_allocations.remove(&com_win);
        self.ic_limit_warned.remove(&com_win);
        let clients: Vec<u32> = self
            .clients
            .iter()
            .filter_map(|(&client, &owner)| (owner == com_win).then_some(client))
            .collect();
        for client in clients {
            let keys: Vec<IcKey> = self
                .ics
                .keys()
                .filter(|key| key.0 == client)
                .copied()
                .collect();
            for key in keys {
                self.focus_out(key);
                self.ics.remove(&key);
            }
            self.pending.retain(|_, pending| pending.ic.0 != client);
            queue.retain(|queued| queued.ic.0 != client);
            if waiting.is_some_and(|waiting| waiting.ic.0 == client) {
                *waiting = None;
            }
            self.stalled.remove(&client);
            self.clients.remove(&client);
        }
    }

    fn focus_in(&mut self, key: IcKey) {
        if self.stalled.contains_key(&key.0) {
            return;
        }
        let Some(ic) = self.ics.get_mut(&key) else {
            return;
        };
        if ic.focused {
            return;
        }
        ic.focused = true;
        let ctx = self.context_ids.next();
        ic.ctx = Some(ctx);
        debug!(ctx, "XIM text field focused");
        self.events.push_back(FrontendEvent::Activated {
            origin: Origin::Xim,
            ctx,
            info: ic.info(),
        });
    }

    /// Learns where the field draws text, from its spot in its window.
    fn update_spot(&mut self, server: &Server, ic: &InputContext) {
        let Some(window) = ic.app_focus_win().or(ic.app_win()) else {
            return;
        };
        let spot = ic.preedit_spot();
        let translated = server
            .conn()
            .translate_coordinates(window.get(), self.root, spot.x, spot.y)
            .ok()
            .and_then(|cookie| cookie.reply().ok());
        if let (Some(reply), Some(state)) = (translated, self.ics.get_mut(&ic_key(ic))) {
            state.spot = Some((i32::from(reply.dst_x), i32::from(reply.dst_y)));
        }
    }

    fn focus_out(&mut self, key: IcKey) {
        let Some(ic) = self.ics.get_mut(&key) else {
            return;
        };
        ic.focused = false;
        if let Some(ctx) = ic.ctx.take() {
            debug!(ctx, "XIM text field left");
            self.events.push_back(FrontendEvent::Deactivated { ctx });
        }
    }

    fn retire_output(&mut self, key: IcKey, queue: &mut VecDeque<Queued>) {
        let (client, im, ic) = key;
        self.stalled.insert(client, (im, ic));
        queue.retain(|queued| queued.ic.0 != client);
        self.pending.retain(|_, pending| pending.ic.0 != client);
        // End the core contexts too: a late engine reply must not repopulate the
        // retired queue after recovery. A new focus or key activates a fresh context.
        let keys: Vec<_> = self
            .ics
            .keys()
            .filter(|key| key.0 == client)
            .copied()
            .collect();
        for key in keys {
            self.focus_out(key);
            if let Some(ic) = self.ics.get_mut(&key) {
                ic.preedit_started = ic.sent_preedit.is_some();
                ic.preedit_len = ic.sent_preedit.unwrap_or_default();
            }
        }
    }

    fn sync_reply(&mut self, com_win: u32, ids: (u16, u16), waiting: &mut Option<SyncWait>) {
        if waiting.is_some_and(|wait| {
            self.clients.get(&wait.ic.0) == Some(&com_win) && (wait.ic.1, wait.ic.2) == ids
        }) {
            *waiting = None;
        }
        // A delayed reply can recover its own suspended client while an unrelated
        // application's synchronous request remains outstanding.
        self.stalled.retain(|client, expected| {
            self.clients.get(client) != Some(&com_win) || *expected != ids
        });
    }

    fn destroy_ic(&mut self, key: IcKey) {
        self.focus_out(key);
        self.ics.remove(&key);
        self.pending.retain(|_, pending| pending.ic != key);
        // Close also invokes this callback for each of the IM's contexts. An ended
        // IC can no longer acknowledge its output; unrelated ICs keep the wait.
        if self.stalled.get(&key.0) == Some(&(key.1, key.2)) {
            self.stalled.remove(&key.0);
        }
        self.destroyed_contexts = true;
    }

    fn discard_destroyed_output(
        &mut self,
        queue: &mut VecDeque<Queued>,
        waiting: &mut Option<SyncWait>,
    ) {
        if !std::mem::take(&mut self.destroyed_contexts) {
            return;
        }
        queue.retain(|queued| self.ics.contains_key(&queued.ic));
        if waiting.is_some_and(|wait| !self.ics.contains_key(&wait.ic)) {
            *waiting = None;
        }
    }
}

type Server = X11rbServer<RustConnection>;

impl ServerHandler<Server> for Handler {
    type InputStyleArray = [InputStyle; 3];
    type InputContextData = ();

    fn new_ic_data(&mut self, server: &mut Server, _style: InputStyle) -> Result<(), ServerError> {
        self.allocate_ic(server)
    }

    fn input_styles(&self) -> Self::InputStyleArray {
        [
            // The application draws the preedit.
            InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_NOTHING,
            // Nobody draws it; the engine's own window may show the composition.
            InputStyle::PREEDIT_POSITION | InputStyle::STATUS_NOTHING,
            InputStyle::PREEDIT_NOTHING | InputStyle::STATUS_NOTHING,
        ]
    }

    fn filter_events(&self) -> u32 {
        KEY_EVENTS
    }

    fn handle_connect(&mut self, _server: &mut Server) -> Result<(), ServerError> {
        Ok(())
    }

    fn handle_create_ic(
        &mut self,
        server: &mut Server,
        user_ic: &mut UserInputContext<()>,
    ) -> Result<(), ServerError> {
        let ic = Ic {
            preedit_callbacks: user_ic
                .ic
                .input_style()
                .contains(InputStyle::PREEDIT_CALLBACKS),
            ..Ic::default()
        };
        self.ics.insert(ic_key(&user_ic.ic), ic);
        // Applications that exit without closing the input method (or crash) never
        // disconnect: watch their window to end their contexts when it goes away.
        let client_win = user_ic.ic.client_win();
        if let Some(com_win) = self.com_win
            && self.clients.insert(client_win, com_win).is_none()
        {
            let watch = ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY);
            server
                .conn()
                .change_window_attributes(client_win, &watch)
                .map_err(|e| ServerError::Internal(e.to_string()))?;
        }
        server.set_event_mask(&user_ic.ic, KEY_EVENTS, 0)
    }

    fn handle_destroy_ic(
        &mut self,
        _server: &mut Server,
        user_ic: UserInputContext<()>,
    ) -> Result<(), ServerError> {
        self.destroy_ic(ic_key(&user_ic.ic));
        Ok(())
    }

    fn handle_reset_ic(
        &mut self,
        _server: &mut Server,
        user_ic: &mut UserInputContext<()>,
    ) -> Result<String, ServerError> {
        let key = ic_key(&user_ic.ic);
        if let Some(ic) = self.ics.get_mut(&key) {
            // The application drops the preedit it shows.
            ic.preedit_started = false;
            ic.preedit_len = 0;
            ic.sent_preedit = None;
            if let Some(ctx) = ic.ctx {
                self.events.push_back(FrontendEvent::Updated {
                    ctx,
                    info: ic.info(),
                    reset: true,
                });
            }
        }
        Ok(String::new())
    }

    fn handle_set_focus(
        &mut self,
        server: &mut Server,
        user_ic: &mut UserInputContext<()>,
    ) -> Result<(), ServerError> {
        self.update_spot(server, &user_ic.ic);
        self.focus_in(ic_key(&user_ic.ic));
        Ok(())
    }

    fn handle_unset_focus(
        &mut self,
        _server: &mut Server,
        user_ic: &mut UserInputContext<()>,
    ) -> Result<(), ServerError> {
        self.focus_out(ic_key(&user_ic.ic));
        Ok(())
    }

    fn handle_set_ic_values(
        &mut self,
        server: &mut Server,
        user_ic: &mut UserInputContext<()>,
    ) -> Result<(), ServerError> {
        self.update_spot(server, &user_ic.ic);
        Ok(())
    }

    fn handle_forward_event(
        &mut self,
        _server: &mut Server,
        user_ic: &mut UserInputContext<()>,
        xev: &KeyPressEvent,
    ) -> Result<bool, ServerError> {
        let key = ic_key(&user_ic.ic);
        if self.stalled.contains_key(&key.0) {
            // Do not build new engine work or let the crate forward keys around the
            // outstanding synchronous request while this client is suspended.
            return Ok(true);
        }
        // Applications usually say when a field gets focus, but a key means it has.
        self.focus_in(key);
        let Some(ic) = self.ics.get_mut(&key) else {
            return Ok(false);
        };
        let Some(ctx) = ic.ctx else {
            return Ok(false);
        };
        let event = serialize_event(xev);
        ic.last_event = Some(event.clone());
        self.next_key += 1;
        let id = self.next_key;
        self.pending.insert(id, Pending { ic: key, event });
        let state = u16::from(xev.state);
        let keysym = self.keymap.keysym(xev.detail, state);
        let released = xev.response_type & 0x7f == KEY_RELEASE;
        let event = crate::keyboard::x11_key_event(
            keysym,
            u32::from(xev.detail),
            u32::from(state),
            released,
            xev.time,
        );
        self.events.push_back(FrontendEvent::Key {
            key: frontend::KeyRef::Xim(KeyRef { ctx, id }),
            event,
        });
        Ok(true)
    }
}

/// The inverse of the `xim` crate's conversion of forwarded events.
fn serialize_event(xev: &KeyPressEvent) -> XEvent {
    XEvent {
        response_type: xev.response_type,
        detail: xev.detail,
        sequence: xev.sequence,
        time: xev.time,
        root: xev.root,
        event: xev.event,
        child: xev.child,
        root_x: xev.root_x,
        root_y: xev.root_y,
        event_x: xev.event_x,
        event_y: xev.event_y,
        state: u16::from(xev.state),
        same_screen: xev.same_screen,
    }
}

/// The XIM server on the X display of `$DISPLAY`.
pub struct XimFrontend {
    server: Server,
    connections: XimConnections<()>,
    handler: Handler,
    fd: AsyncFd<PollFd>,
    /// The `_XIM_PROTOCOL` atom, the type of protocol messages.
    xim_protocol: u32,
    /// The monitors of the screen, as RandR describes them.
    monitors: Vec<Monitor>,
    queue: VecDeque<Queued>,
    waiting: Option<SyncWait>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SyncWait {
    since: Instant,
    ic: IcKey,
}

/// A request waiting for its turn.
#[derive(Debug)]
struct Queued {
    ic: IcKey,
    request: Request,
    /// Whether what follows waits until the application consumed this.
    sync: bool,
}

/// The wire size includes preedit feedback as well as text; also count the fixed
/// request storage so small messages cannot evade the byte budget.
fn enqueue(queue: &mut VecDeque<Queued>, queued: Queued) -> bool {
    let bytes = |queued: &Queued| {
        queued
            .request
            .size()
            .saturating_add(std::mem::size_of::<Queued>())
    };
    let (count, bytes) = queue
        .iter()
        .filter(|other| other.ic.0 == queued.ic.0)
        .fold((1, bytes(&queued)), |(count, total), other| {
            (count + 1, total.saturating_add(bytes(other)))
        });
    if count > MAX_QUEUED_REQUESTS_PER_CLIENT || bytes > MAX_QUEUED_BYTES_PER_CLIENT {
        return false;
    }
    queue.push_back(queued);
    true
}

impl std::fmt::Debug for XimFrontend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XimFrontend")
            .field("ics", &self.handler.ics.len())
            .finish_non_exhaustive()
    }
}

impl XimFrontend {
    /// Connects to the X display in `$DISPLAY` and registers the XIM server
    /// [`SERVER_NAME`], waiting at most `wait` for the X server to answer.
    pub fn connect(context_ids: ContextIds, wait: Duration) -> anyhow::Result<Self> {
        let (conn, screen) = connect_display(wait)?;
        let keymap = Keymap::load(&conn).context("cannot read the keyboard mapping")?;
        let xim_protocol = conn.intern_atom(false, b"_XIM_PROTOCOL")?.reply()?.atom;
        let root = conn
            .setup()
            .roots
            .get(screen)
            .context("no such screen")?
            .root;
        let mask = randr::NotifyMask::SCREEN_CHANGE
            | randr::NotifyMask::CRTC_CHANGE
            | randr::NotifyMask::OUTPUT_CHANGE;
        // Without RandR, X11 coordinates are taken for compositor coordinates.
        let _ = conn.randr_select_input(root, mask);
        let monitors = load_monitors(&conn, root);
        let fd = rustix::io::dup(conn.stream().as_fd())?;
        let server = Server::init(conn, screen, SERVER_NAME, xim::ALL_LOCALES)
            .map_err(|e| anyhow::anyhow!("cannot register the XIM server: {e}"))?;
        info!(name = SERVER_NAME, "serving XIM");
        for alias in ALIASES {
            match register_alias(server.conn(), root, alias) {
                Ok(true) => info!(name = alias, "serving XIM"),
                Ok(false) => info!(
                    name = alias,
                    "another XIM server answers by this name; leaving it"
                ),
                Err(e) => warn!(name = alias, "cannot serve XIM by this name: {e:#}"),
            }
        }
        Ok(Self {
            server,
            connections: XimConnections::new(),
            handler: Handler {
                context_ids,
                root,
                com_win: None,
                create_ic_im: None,
                clients: BTreeMap::new(),
                ic_allocations: BTreeMap::new(),
                ic_limit_warned: BTreeSet::new(),
                stalled: BTreeMap::new(),
                destroyed_contexts: false,
                keymap,
                ics: BTreeMap::new(),
                pending: BTreeMap::new(),
                next_key: 0,
                events: VecDeque::new(),
            },
            fd: watch(PollFd(Rc::new(fd)))?,
            xim_protocol,
            monitors,
            queue: VecDeque::new(),
            waiting: None,
        })
    }

    /// Waits until the X server sent something.
    pub async fn readable(&self) -> std::io::Result<()> {
        let mut guard = self.fd.readable().await?;
        guard.clear_ready();
        Ok(())
    }

    /// Handles everything the X server sent.
    pub fn dispatch(&mut self) -> anyhow::Result<()> {
        self.process()
    }

    /// Handles what the X server sent and sends what is queued, until neither is left.
    ///
    /// Sending can wait for a reply (the `xim` crate interns an atom for every long
    /// message), and x11rb queues whatever arrives meanwhile. The socket does not wake us
    /// again for that, so an application's request could wait for the next unrelated
    /// event, and the application with it.
    fn process(&mut self) -> anyhow::Result<()> {
        loop {
            while let Some(event) = self.server.conn().poll_for_event()? {
                self.handle_event(&event)?;
            }
            if !self.send_queued() {
                return self.flush();
            }
        }
    }

    fn handle_event(&mut self, event: &Event) -> anyhow::Result<()> {
        self.handler.com_win = None;
        self.handler.create_ic_im = None;
        match event {
            Event::MappingNotify(_) => {
                self.handler.keymap = Keymap::load(self.server.conn())?;
            }
            Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => {
                self.monitors = load_monitors(self.server.conn(), self.handler.root);
            }
            Event::ClientMessage(msg) if msg.type_ == self.xim_protocol => {
                self.handler.com_win = Some(msg.window);
                if let Some(header) = protocol_header(self.server.conn(), msg) {
                    self.handler.create_ic_im = create_ic_im(&header);
                    // The `xim` crate ignores this answer. Only its connection and
                    // matching context may release a synchronous output request.
                    if let Some(ids) = sync_reply_ids(&header) {
                        self.handler.sync_reply(msg.window, ids, &mut self.waiting);
                    }
                }
            }
            Event::DestroyNotify(notify) => self.client_gone(notify.window),
            _ => {}
        }
        let connected = self
            .handler
            .com_win
            .is_some_and(|win| self.connections.get_connection(win).is_some());
        // Open has no allocation hook. Its ID counter can panic too, even in release
        // builds; keep panic=unwind and discard the affected connection before reuse.
        let panicked = match catch_unwind(AssertUnwindSafe(|| {
            self.server
                .filter_event(event, &mut self.connections, &mut self.handler)
        })) {
            Ok(Ok(_)) => false,
            Ok(Err(e)) => {
                debug!("XIM: {e}");
                false
            }
            Err(_) => {
                warn!("XIM: protocol handler panicked; disconnecting client");
                true
            }
        };
        self.handler
            .discard_destroyed_output(&mut self.queue, &mut self.waiting);
        if let Some(com_win) = self.handler.com_win.take()
            && connected
            && (panicked || self.connections.get_connection(com_win).is_none())
        {
            self.disconnect_client(com_win);
        }
        Ok(())
    }

    /// Ends the input contexts of an application whose client window is gone.
    fn client_gone(&mut self, client_win: u32) {
        let Some(&com_win) = self.handler.clients.get(&client_win) else {
            return;
        };
        debug!(client_win, "XIM: an application went away");
        self.disconnect_client(com_win);
    }

    /// Discards protocol state and output belonging only to this connection, including
    /// an Open-only client that never registered an input context with the handler.
    fn disconnect_client(&mut self, com_win: u32) {
        self.connections.remove_connection(com_win);
        self.handler
            .forget_connection(com_win, &mut self.queue, &mut self.waiting);
        // The transport window belongs to omimed, not the application. Destroy it so
        // the client can detect the disconnect, including after an Open panic.
        let _ = self.server.conn().destroy_window(com_win);
    }

    /// When to give up on an application that did not answer a synchronous request.
    pub fn deadline(&self) -> Option<Instant> {
        self.waiting.map(|wait| wait.since + SYNC_TIMEOUT)
    }

    pub fn on_timer(&mut self, now: Instant) -> anyhow::Result<()> {
        if self.deadline().is_some_and(|deadline| deadline <= now)
            && let Some(wait) = self.waiting.take()
        {
            debug!("XIM: an application did not answer in time");
            self.handler.retire_output(wait.ic, &mut self.queue);
        }
        self.process()
    }

    /// Queues `request` for `ic`, for [`Self::process`] to send. A `sync` request
    /// holds back what follows until the application consumed it.
    fn send(&mut self, ic: IcKey, request: Request, sync: bool) {
        if self.handler.stalled.contains_key(&ic.0) || !self.handler.ics.contains_key(&ic) {
            return;
        }
        if !enqueue(&mut self.queue, Queued { ic, request, sync })
            && let Some(&com_win) = self.handler.clients.get(&ic.0)
        {
            // No acknowledgement may be outstanding for an over-budget client.
            // Disconnect it rather than leave an incomplete output sequence alive.
            warn!("XIM: output queue limit reached; disconnecting client");
            self.disconnect_client(com_win);
        }
    }

    /// Sends queued requests up to the next synchronous one. Returns whether it sent any.
    fn send_queued(&mut self) -> bool {
        let mut sent = false;
        while self.waiting.is_none() {
            let Some(queued) = self.queue.pop_front() else {
                break;
            };
            sent = true;
            // draw_preedit sends full replacements with one feedback per character.
            let preedit = match &queued.request {
                Request::PreeditStart { .. } => Some(Some(0)),
                Request::PreeditDraw { feedbacks, .. } => Some(Some(feedbacks.len())),
                Request::PreeditDone { .. } => Some(None),
                _ => None,
            };
            if let Err(e) = self.server.send_req(queued.ic.0, queued.request) {
                debug!("XIM: {e}");
                continue;
            }
            if let Some(preedit) = preedit
                && let Some(ic) = self.handler.ics.get_mut(&queued.ic)
            {
                ic.sent_preedit = preedit;
            }
            if queued.sync {
                self.waiting = Some(SyncWait {
                    since: Instant::now(),
                    ic: queued.ic,
                });
            }
        }
        sent
    }

    fn flush(&self) -> anyhow::Result<()> {
        self.server.conn().flush()?;
        Ok(())
    }

    /// Takes the events for the core.
    pub fn take_events(&mut self) -> Vec<FrontendEvent> {
        self.handler.events.drain(..).collect()
    }

    /// The monitors of the X screen.
    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
    }

    /// The most recently focused field and its text cursor, in X screen coordinates.
    /// XIM only tells where the baseline starts; the rectangle assumes a common line
    /// height.
    pub fn focused_cursor(&self) -> Option<(ContextId, Option<placement::Rect>)> {
        const ASCENT: i32 = 16;
        const LINE_HEIGHT: i32 = 22;
        self.handler
            .ics
            .values()
            .filter_map(|ic| Some((ic.ctx?, ic.spot)))
            .max_by_key(|(ctx, _)| *ctx)
            .map(|(ctx, spot)| {
                (
                    ctx,
                    spot.map(|(x, y)| {
                        placement::clamp_rect((
                            x,
                            placement::saturate_i32(i64::from(y) - i64::from(ASCENT)),
                            1,
                            LINE_HEIGHT,
                        ))
                    }),
                )
            })
    }

    /// Ends every context, before the connection goes away.
    pub fn end_contexts(&mut self) -> Vec<FrontendEvent> {
        let keys: Vec<IcKey> = self.handler.ics.keys().copied().collect();
        for key in keys {
            self.handler.focus_out(key);
        }
        self.take_events()
    }

    /// Gives a key the engine did not consume back to the application.
    pub fn finish_key(&mut self, key: &KeyRef, handled: bool) {
        let Some(pending) = self.handler.pending.remove(&key.id) else {
            return;
        };
        if !handled {
            self.forward(pending.ic, pending.event);
            let _ = self.process();
        }
    }

    fn forward(&mut self, (client_win, im, ic): IcKey, xev: XEvent) {
        let request = Request::ForwardEvent {
            input_method_id: im,
            input_context_id: ic,
            serial_number: 0,
            flag: ForwardEventFlag::SYNCHRONOUS,
            xev,
        };
        self.send((client_win, im, ic), request, true);
    }

    /// Applies an engine's actions to the field `ctx`.
    pub fn apply_actions(
        &mut self,
        ctx: ContextId,
        actions: &[Action],
        reply_key: Option<&KeyRef>,
    ) {
        let event = reply_key
            .and_then(|key| self.handler.pending.get(&key.id))
            .map(|pending| pending.event.clone());
        self.apply_actions_with_key(ctx, actions, None, event.as_ref());
    }

    pub fn forward_key(&mut self, ctx: ContextId, key: &frontend::ForwardedKey) {
        self.apply_actions_with_key(
            ctx,
            &[Action::ForwardKey {
                keycode: key.keycode,
                pressed: key.pressed,
            }],
            Some(key),
            None,
        );
    }

    fn apply_actions_with_key(
        &mut self,
        ctx: ContextId,
        actions: &[Action],
        forwarded: Option<&frontend::ForwardedKey>,
        reply_event: Option<&XEvent>,
    ) {
        let Some(key) = self.find(ctx) else {
            return;
        };
        for action in actions {
            match action {
                // Mode confirmations are consumed by Core, never sent to applications.
                Action::InputMode { .. } => {}
                Action::Commit(text) => {
                    // A commit replaces the preedit.
                    self.draw_preedit(key, &Preedit::default());
                    let (client_win, im, ic) = key;
                    let request = Request::Commit {
                        input_method_id: im,
                        input_context_id: ic,
                        data: CommitData::Chars {
                            commited: xim_ctext::utf8_to_compound_text(text),
                            syncronous: true,
                        },
                    };
                    self.send((client_win, im, ic), request, true);
                }
                Action::Preedit(preedit) => self.draw_preedit(key, preedit),
                // XIM has no way to tell about text around the cursor.
                Action::DeleteSurrounding { .. } => {}
                Action::ForwardKey { keycode, pressed } => {
                    // XIM passes only presses on, so the application gets its releases
                    // from the X server. A release sent back would be worse than useless:
                    // libX11 marks the next event as the one the input method sent back,
                    // and with no filter for releases the mark stays, so the next real
                    // press bypasses the input method and types its letter. (Hosted
                    // input methods send back every release they do not consume.)
                    if !*pressed {
                        continue;
                    }
                    let template = reply_event.cloned().or_else(|| {
                        self.handler
                            .ics
                            .get(&key)
                            .and_then(|ic| ic.last_event.clone())
                    });
                    let detail = u8::try_from(*keycode)
                        .ok()
                        .and_then(|code| code.checked_add(X11_KEYCODE_OFFSET));
                    // Replies use the pending event's state, time and window metadata;
                    // host calls retain the most recently reported event as before.
                    if let (Some(mut template), Some(detail)) = (template, detail) {
                        if let Some(forwarded) = forwarded {
                            template.state = forwarded.modifiers.x11_bits() as u16;
                            template.time = forwarded.time;
                        }
                        let xev = XEvent {
                            response_type: if *pressed { KEY_PRESS } else { KEY_RELEASE },
                            detail,
                            ..template
                        };
                        self.forward(key, xev);
                    }
                }
            }
        }
        let _ = self.process();
    }

    /// Removes any preedit from the field `ctx`.
    pub fn clear_preedit(&mut self, ctx: ContextId) {
        if let Some(key) = self.find(ctx) {
            self.draw_preedit(key, &Preedit::default());
            let _ = self.process();
        }
    }

    fn find(&self, ctx: ContextId) -> Option<IcKey> {
        self.handler
            .ics
            .iter()
            .find(|(_, ic)| ic.ctx == Some(ctx))
            .map(|(key, _)| *key)
    }

    /// Shows `preedit` in applications that draw it themselves.
    fn draw_preedit(&mut self, key: IcKey, preedit: &Preedit) {
        let Some(ic) = self.handler.ics.get_mut(&key) else {
            return;
        };
        if !ic.preedit_callbacks {
            return;
        }
        let (client_win, im, icid) = key;
        let len = preedit.text.chars().count();
        let mut requests = Vec::new();
        if len == 0 {
            if !ic.preedit_started {
                return;
            }
            requests.push(Request::PreeditDraw {
                input_method_id: im,
                input_context_id: icid,
                chg_first: 0,
                chg_length: i32::try_from(ic.preedit_len).unwrap_or(i32::MAX),
                caret: 0,
                preedit_string: Vec::new(),
                feedbacks: Vec::new(),
                status: PreeditDrawStatus::NO_FEEDBACK | PreeditDrawStatus::NO_STRING,
            });
            requests.push(Request::PreeditDone {
                input_method_id: im,
                input_context_id: icid,
            });
            ic.preedit_started = false;
            ic.preedit_len = 0;
        } else {
            if !ic.preedit_started {
                requests.push(Request::PreeditStart {
                    input_method_id: im,
                    input_context_id: icid,
                });
                ic.preedit_started = true;
            }
            let caret = preedit
                .text
                .char_indices()
                .take_while(|(offset, _)| {
                    i64::try_from(*offset).unwrap_or(i64::MAX) < i64::from(preedit.cursor_begin)
                })
                .count();
            requests.push(Request::PreeditDraw {
                input_method_id: im,
                input_context_id: icid,
                chg_first: 0,
                chg_length: i32::try_from(ic.preedit_len).unwrap_or(i32::MAX),
                caret: i32::try_from(caret).unwrap_or(i32::MAX),
                preedit_string: xim_ctext::utf8_to_compound_text(&preedit.text),
                feedbacks: vec![Feedback::UNDERLINE; len],
                status: PreeditDrawStatus::empty(),
            });
            ic.preedit_len = len;
        }
        for request in requests {
            self.send((client_win, im, icid), request, false);
        }
    }
}

type Connected = Result<(RustConnection, usize), x11rb::errors::ConnectError>;

thread_local! {
    /// A connection attempt that did not finish in time. The next attempt waits for it
    /// instead of starting another.
    static PENDING_CONNECT: RefCell<Option<Receiver<Connected>>> = const { RefCell::new(None) };
}

/// Connects to the X display in `$DISPLAY`, waiting at most `wait`. An X server that
/// accepts the connection but never answers would block the whole daemon, keyboard input
/// included; the attempt is left to itself instead, and fails here to be checked again
/// later.
/// Makes the window of the server [`SERVER_NAME`] answer by `name` as well, unless
/// another XIM server does. Returns whether it does now.
fn register_alias(conn: &RustConnection, root: u32, name: &str) -> anyhow::Result<bool> {
    let atom = |name: &str| -> anyhow::Result<u32> {
        Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
    };
    let own = atom(&format!("@server={SERVER_NAME}"))?;
    let window = conn.get_selection_owner(own)?.reply()?.owner;
    anyhow::ensure!(window != x11rb::NONE, "the server has no window");
    let selection = atom(&format!("@server={name}"))?;
    let owner = conn.get_selection_owner(selection)?.reply()?.owner;
    if owner != x11rb::NONE && owner != window {
        return Ok(false);
    }
    conn.set_selection_owner(window, selection, x11rb::CURRENT_TIME)?;
    let servers = atom("XIM_SERVERS")?;
    let listed = conn
        .get_property(false, root, servers, AtomEnum::ATOM, 0, u32::MAX)?
        .reply()?
        .value32()
        .is_some_and(|mut atoms| atoms.any(|atom| atom == selection));
    if !listed {
        conn.change_property32(
            PropMode::APPEND,
            root,
            servers,
            AtomEnum::ATOM,
            &[selection],
        )?;
    }
    conn.flush()?;
    Ok(true)
}

fn connect_display(wait: Duration) -> anyhow::Result<(RustConnection, usize)> {
    let receiver = match PENDING_CONNECT.take() {
        Some(receiver) => receiver,
        None => {
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("x11-connect".into())
                .spawn(move || {
                    let _ = sender.send(RustConnection::connect(None));
                })?;
            receiver
        }
    };
    match receiver.recv_timeout(wait) {
        Ok(connected) => connected.context("cannot connect to the X server"),
        Err(RecvTimeoutError::Timeout) => {
            PENDING_CONNECT.set(Some(receiver));
            anyhow::bail!("the X server does not answer")
        }
        Err(RecvTimeoutError::Disconnected) => anyhow::bail!("cannot connect to the X server"),
    }
}

/// The protocol header and first ID. Long messages travel in a property, which is
/// only peeked at here and left for the `xim` crate to read and delete.
fn protocol_header(conn: &RustConnection, msg: &ClientMessageEvent) -> Option<[u8; 8]> {
    match msg.format {
        8 => msg.data.as_data8()[..8].try_into().ok(),
        32 => {
            let [_length, property, ..] = msg.data.as_data32();
            conn.get_property(false, msg.window, property, AtomEnum::ANY, 0, 2)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .and_then(|reply| reply.value.get(..8)?.try_into().ok())
        }
        _ => None,
    }
}

fn create_ic_im(header: &[u8]) -> Option<NonZeroU16> {
    if header.first() != Some(&XIM_CREATE_IC) {
        return None;
    }
    // Match the native byte order used by the xim crate's protocol reader.
    NonZeroU16::new(u16::from_ne_bytes(header.get(4..6)?.try_into().ok()?))
}

fn sync_reply_ids(header: &[u8]) -> Option<(u16, u16)> {
    // A SyncReply has exactly four body bytes (one protocol length unit). Reject
    // malformed headers before interpreting the inline transport's padding as IDs.
    if header.get(..2)? != [XIM_SYNC_REPLY, 0]
        || u16::from_ne_bytes(header.get(2..4)?.try_into().ok()?) != 1
    {
        return None;
    }
    let im = NonZeroU16::new(u16::from_ne_bytes(header.get(4..6)?.try_into().ok()?))?;
    let ic = NonZeroU16::new(u16::from_ne_bytes(header.get(6..8)?.try_into().ok()?))?;
    Some((im.get(), ic.get()))
}

/// The monitors of the screen of `root`, or none if RandR cannot tell.
fn load_monitors(conn: &RustConnection, root: u32) -> Vec<Monitor> {
    let Ok(reply) = conn
        .randr_get_monitors(root, true)
        .map_err(anyhow::Error::from)
        .and_then(|cookie| cookie.reply().map_err(Into::into))
    else {
        return Vec::new();
    };
    reply
        .monitors
        .iter()
        .filter_map(|m| {
            let name = conn.get_atom_name(m.name).ok()?.reply().ok()?.name;
            Some(Monitor {
                name: String::from_utf8_lossy(&name).into_owned(),
                rect: (
                    i32::from(m.x),
                    i32::from(m.y),
                    i32::from(m.width),
                    i32::from(m.height),
                ),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Replies(Vec<(u32, Request)>);

    impl xim::ServerCore for Replies {
        type XEvent = XEvent;

        fn deserialize_event(&self, event: &XEvent) -> XEvent {
            event.clone()
        }

        fn send_req(&mut self, client: u32, request: Request) -> Result<(), ServerError> {
            self.0.push((client, request));
            Ok(())
        }
    }

    fn handler() -> Handler {
        Handler {
            context_ids: ContextIds::default(),
            root: 0,
            com_win: None,
            create_ic_im: None,
            clients: BTreeMap::new(),
            ic_allocations: BTreeMap::new(),
            ic_limit_warned: BTreeSet::new(),
            stalled: BTreeMap::new(),
            destroyed_contexts: false,
            keymap: Keymap::default(),
            ics: BTreeMap::new(),
            pending: BTreeMap::new(),
            next_key: 0,
            events: VecDeque::new(),
        }
    }

    #[test]
    fn context_allocations_are_bounded_per_connection_without_live_contexts() {
        let mut handler = handler();
        // No contexts remain live, as in repeated CreateIc / DestroyIc churn.
        for allocation in 0..70_000 {
            assert_eq!(
                handler.reserve_ic(10),
                allocation < u32::from(MAX_IC_ALLOCATIONS),
                "allocation {allocation}"
            );
        }
        assert!(handler.ics.is_empty());
        assert!(
            handler.reserve_ic(20),
            "another connection can still allocate"
        );
        assert!(
            !handler.reserve_ic(10),
            "another client cannot reset the limit"
        );
    }

    #[test]
    fn refused_create_ic_error_matches_libx11_reply_filter() {
        let mut handler = handler();
        let mut replies = Replies::default();
        handler.com_win = Some(10);
        handler.clients.insert(100, 10);
        handler.ic_allocations.insert(10, MAX_IC_ALLOCATIONS);
        // Use multiple, nontrivial IMIDs: the refused request need not belong to the
        // first input method opened on the connection.
        for im in [2, 0x1234] {
            let wire = xim::write_to_vec(Request::CreateIc {
                input_method_id: im,
                ic_attributes: Vec::new(),
            });
            handler.create_ic_im = create_ic_im(&wire);
            assert!(handler.allocate_ic(&mut replies).is_err());
            let (client, request) = replies.0.pop().unwrap();
            assert_eq!(client, 100);
            // _XimCreateICCheck ignores errors without IMID_VALID or with a different
            // IMID. Exercise Server::error's real serialization and validity flags.
            let wire = xim::write_to_vec(request);
            let Request::Error {
                input_method_id,
                input_context_id,
                flag,
                code,
                ..
            } = xim::read::<Request>(&wire).unwrap()
            else {
                panic!("CreateIc was not answered with XIM_ERROR");
            };
            assert!(flag.contains(xim::ErrorFlag::INPUT_METHOD_ID_VALID));
            assert_eq!(input_method_id, im);
            assert_eq!(input_context_id, 0);
            assert!(!flag.contains(xim::ErrorFlag::INPUT_CONTEXT_ID_VALID));
            assert_eq!(code, ErrorCode::BadAlloc);
        }
        assert_eq!(create_ic_im(&[]), None);
        assert_eq!(create_ic_im(&[XIM_CREATE_IC; 5]), None);
        assert_eq!(
            create_ic_im(&xim::write_to_vec(Request::Close { input_method_id: 2 })),
            None
        );
    }

    fn active_connections() -> (Handler, VecDeque<Queued>, Option<SyncWait>) {
        let mut handler = handler();
        let mut queue = VecDeque::new();
        let waiting = Some(SyncWait {
            since: Instant::now(),
            ic: (100, 2, 1),
        });
        let event = serialize_event(&KeyPressEvent::default());
        for (client, connection, id) in [(100, 10, 1), (200, 20, 2)] {
            let key = (client, 2, 1);
            handler.clients.insert(client, connection);
            handler.ics.insert(key, Ic::default());
            handler.focus_in(key);
            handler.pending.insert(
                id,
                Pending {
                    ic: key,
                    event: event.clone(),
                },
            );
            queue.push_back(Queued {
                ic: key,
                request: Request::Commit {
                    input_method_id: 2,
                    input_context_id: 1,
                    data: CommitData::Chars {
                        commited: b"queued".to_vec(),
                        syncronous: true,
                    },
                },
                sync: true,
            });
        }
        handler.events.clear();
        handler.com_win = Some(10);
        handler.create_ic_im = NonZeroU16::new(2);
        handler.ic_allocations.insert(10, MAX_IC_ALLOCATIONS);
        (handler, queue, waiting)
    }

    #[test]
    fn refusing_create_ic_preserves_existing_contexts_and_work() {
        let (mut handler, queue, waiting) = active_connections();
        let mut replies = Replies::default();
        let ctx = handler.ics[&(100, 2, 1)].ctx;
        let since = waiting.unwrap().since;
        // Repeated requests from the same connection keep receiving errors without
        // ending existing contexts or discarding their work.
        for _ in 0..3 {
            assert!(matches!(
                handler.allocate_ic(&mut replies),
                Err(ServerError::XimError(ErrorCode::BadAlloc, _))
            ));
            assert_eq!(handler.ics.len(), 2);
            assert!(handler.ics[&(100, 2, 1)].focused);
            assert_eq!(handler.ics[&(100, 2, 1)].ctx, ctx);
            assert_eq!(handler.pending.len(), 2);
            assert_eq!(handler.pending[&1].ic, (100, 2, 1));
            assert_eq!(handler.clients.get(&100), Some(&10));
            assert_eq!(handler.ic_allocations.get(&10), Some(&MAX_IC_ALLOCATIONS));
            assert_eq!(handler.ic_limit_warned, BTreeSet::from([10]));
            assert!(
                handler.events.is_empty(),
                "refusal must not deactivate a context"
            );
            assert_eq!(queue.len(), 2);
            assert_eq!(queue.front().unwrap().ic.0, 100);
            assert_eq!(
                waiting,
                Some(SyncWait {
                    since,
                    ic: (100, 2, 1)
                })
            );
        }
        assert_eq!(replies.0.len(), 3);
        assert!(replies.0.iter().all(|(client, request)| *client == 100
            && matches!(
                request,
                Request::Error {
                    input_method_id: 2,
                    code: ErrorCode::BadAlloc,
                    ..
                }
            )));
        assert!(handler.reserve_ic(20));
    }

    #[test]
    fn forgetting_connection_cleans_live_context_pending_key_and_queued_output() {
        let (mut handler, mut queue, mut waiting) = active_connections();
        let lost_ctx = handler.ics[&(100, 2, 1)].ctx.unwrap();
        let surviving_ctx = handler.ics[&(200, 2, 1)].ctx.unwrap();
        handler.ic_limit_warned.insert(10);
        handler.forget_connection(10, &mut queue, &mut waiting);

        assert!(
            matches!(handler.events.pop_front(), Some(FrontendEvent::Deactivated { ctx }) if ctx == lost_ctx)
        );
        assert!(handler.events.is_empty());
        assert!(!handler.ics.contains_key(&(100, 2, 1)));
        // finish_key can no longer send the departed client's late key reply.
        assert!(handler.pending.remove(&1).is_none());
        assert!(handler.pending.contains_key(&2));
        assert_eq!(handler.ics[&(200, 2, 1)].ctx, Some(surviving_ctx));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.front().unwrap().ic.0, 200);
        assert!(waiting.is_none());
        assert!(!handler.ic_allocations.contains_key(&10));
        assert!(!handler.ic_limit_warned.contains(&10));
        assert!(handler.reserve_ic(20));
        // Repeated cleanup is harmless and preserves another client's sync wait.
        waiting = Some(SyncWait {
            since: Instant::now(),
            ic: (200, 2, 1),
        });
        handler.forget_connection(10, &mut queue, &mut waiting);
        assert!(waiting.is_some());
        assert!(handler.events.is_empty());
    }

    #[test]
    fn timeout_retires_only_its_client_and_matching_reply_starts_a_fresh_context() {
        let (mut handler, mut queue, mut waiting) = active_connections();
        let old_ctx = handler.ics[&(100, 2, 1)].ctx.unwrap();
        let ic = handler.ics.get_mut(&(100, 2, 1)).unwrap();
        ic.sent_preedit = Some(2);
        ic.preedit_started = true;
        ic.preedit_len = 5;
        handler.retire_output(waiting.take().unwrap().ic, &mut queue);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].ic.0, 200);
        assert_eq!(handler.pending.len(), 1);
        assert!(handler.pending.contains_key(&2));
        assert!(handler.ics[&(100, 2, 1)].ctx.is_none());
        assert!(handler.ics[&(100, 2, 1)].preedit_started);
        assert_eq!(handler.ics[&(100, 2, 1)].preedit_len, 2);
        assert!(
            matches!(handler.events.pop_front(), Some(FrontendEvent::Deactivated { ctx }) if ctx == old_ctx)
        );
        handler.focus_in((100, 2, 1));
        assert!(handler.ics[&(100, 2, 1)].ctx.is_none());

        let healthy_wait = Some(SyncWait {
            since: Instant::now(),
            ic: (200, 2, 1),
        });
        waiting = healthy_wait;
        for (connection, ids) in [(20, (2, 2)), (10, (3, 1)), (10, (2, 2))] {
            handler.sync_reply(connection, ids, &mut waiting);
            assert_eq!(waiting, healthy_wait);
            assert!(handler.stalled.contains_key(&100));
        }
        handler.sync_reply(10, (2, 1), &mut waiting);
        assert_eq!(
            waiting, healthy_wait,
            "the recovered client's reply released a different wait"
        );
        assert!(handler.stalled.is_empty());
        handler.focus_in((100, 2, 1));
        assert_ne!(handler.ics[&(100, 2, 1)].ctx, Some(old_ctx));
        assert!(handler.ics[&(100, 2, 1)].ctx.is_some());
        handler.sync_reply(20, (2, 1), &mut waiting);
        assert!(waiting.is_none());
        handler.stalled.insert(100, (2, 1));
        handler.forget_connection(10, &mut queue, &mut waiting);
        assert!(handler.stalled.is_empty());
    }

    #[test]
    fn destroying_awaited_ic_recovers_other_contexts_without_releasing_other_clients() {
        let (mut handler, mut queue, mut waiting) = active_connections();
        let other = (100, 2, 2);
        let unrelated = (100, 3, 1);
        handler.ics.insert(other, Ic::default());
        handler.ics.insert(unrelated, Ic::default());
        handler.focus_in(other);
        let old_ctx = handler.ics[&other].ctx.unwrap();
        handler.retire_output(waiting.take().unwrap().ic, &mut queue);
        let healthy_wait = Some(SyncWait {
            since: Instant::now(),
            ic: (200, 2, 1),
        });
        waiting = healthy_wait;

        // Another IM on this connection cannot recover the awaited context.
        handler.destroy_ic(unrelated);
        handler.discard_destroyed_output(&mut queue, &mut waiting);
        assert_eq!(handler.stalled.get(&100), Some(&(2, 1)));
        handler.focus_in(other);
        assert!(handler.ics[&other].ctx.is_none());
        assert_eq!(waiting, healthy_wait);

        handler.destroy_ic((100, 2, 1));
        handler.discard_destroyed_output(&mut queue, &mut waiting);
        assert!(handler.stalled.is_empty());
        assert_eq!(waiting, healthy_wait);
        handler.focus_in(other);
        assert!(handler.ics[&other].ctx.is_some());
        assert_ne!(handler.ics[&other].ctx, Some(old_ctx));
    }

    #[test]
    fn destroying_awaited_ic_clears_its_live_wait_and_queued_output() {
        let (mut handler, mut queue, mut waiting) = active_connections();
        let wait = waiting;
        handler.ics.insert((100, 2, 2), Ic::default());
        handler.destroy_ic((100, 2, 2));
        handler.discard_destroyed_output(&mut queue, &mut waiting);
        assert_eq!(waiting, wait, "an unrelated IC released the wait");
        assert_eq!(queue.len(), 2);

        handler.destroy_ic((100, 2, 1));
        handler.discard_destroyed_output(&mut queue, &mut waiting);
        assert!(waiting.is_none());
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].ic, (200, 2, 1));
        assert!(!handler.pending.contains_key(&1));
        assert!(handler.pending.contains_key(&2));
    }

    #[test]
    fn only_complete_sync_reply_headers_have_matching_ids() {
        let wire = xim::write_to_vec(Request::SyncReply {
            input_method_id: 0x1234,
            input_context_id: 0x5678,
        });
        assert_eq!(sync_reply_ids(&wire), Some((0x1234, 0x5678)));
        for length in 0..8 {
            assert_eq!(sync_reply_ids(&wire[..length]), None);
        }
        for (offset, value) in [(0, XIM_CREATE_IC), (1, 1)] {
            let mut malformed = wire.clone();
            malformed[offset] = value;
            assert_eq!(sync_reply_ids(&malformed), None);
        }
        for length in [0_u16, 2, u16::MAX] {
            let mut malformed = wire.clone();
            malformed[2..4].copy_from_slice(&length.to_ne_bytes());
            assert_eq!(sync_reply_ids(&malformed), None);
        }
        for ids in [(0, 1), (1, 0)] {
            assert_eq!(
                sync_reply_ids(&xim::write_to_vec(Request::SyncReply {
                    input_method_id: ids.0,
                    input_context_id: ids.1,
                })),
                None
            );
        }
    }

    #[test]
    fn output_admission_bounds_each_clients_count_and_text_with_feedback() {
        let small = |client| Queued {
            ic: (client, 1, 1),
            request: Request::PreeditStart {
                input_method_id: 1,
                input_context_id: 1,
            },
            sync: false,
        };
        let mut queue = VecDeque::new();
        for _ in 0..MAX_QUEUED_REQUESTS_PER_CLIENT {
            assert!(enqueue(&mut queue, small(100)));
        }
        assert!(!enqueue(&mut queue, small(100)));
        assert!(
            enqueue(&mut queue, small(200)),
            "another client has its own budget"
        );
        assert_eq!(queue.len(), MAX_QUEUED_REQUESTS_PER_CLIENT + 1);

        let large = || Queued {
            ic: (300, 1, 1),
            request: Request::PreeditDraw {
                input_method_id: 1,
                input_context_id: 1,
                chg_first: 0,
                chg_length: 0,
                caret: 0,
                preedit_string: vec![b'x'; 150_000],
                feedbacks: vec![Feedback::UNDERLINE; 150_000],
                status: PreeditDrawStatus::empty(),
            },
            sync: false,
        };
        assert!(enqueue(&mut queue, large()));
        assert!(
            !enqueue(&mut queue, large()),
            "feedback must count against the byte budget too"
        );
        assert!(enqueue(&mut queue, small(400)));
    }
}
