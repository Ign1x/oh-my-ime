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
use std::collections::{BTreeMap, VecDeque};
use std::os::fd::AsFd as _;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use omime_proto::engine::{Action, ContextId, ContextInfo, Preedit};
use tokio::io::unix::AsyncFd;
use tracing::{debug, info};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt as _, EventMask,
    KeyPressEvent,
};
use x11rb::rust_connection::RustConnection;
use xim::x11rb::{HasConnection as _, X11rbServer};
use xim::{
    CommitData, Feedback, ForwardEventFlag, InputContext, InputStyle, PreeditDrawStatus, Request,
    Server as _, ServerCore as _, ServerError, ServerHandler, UserInputContext, XEvent,
    XimConnections,
};

use crate::daemon::{PollFd, watch};
use crate::frontend::{self, ContextIds, FrontendEvent, Origin};
use crate::placement::{self, Monitor};
use keymap::Keymap;

/// The name applications ask for with `XMODIFIERS=@im=omime`.
pub const SERVER_NAME: &str = "omime";
/// `KeyPressMask`: applications forward presses. Releases stay with them: every key
/// sent back costs a round trip, and applications need not answer one for a release.
const KEY_EVENTS: u32 = 1;
/// The major opcode of `XIM_SYNC_REPLY`.
const XIM_SYNC_REPLY: u8 = 62;
/// How long to wait for an application to consume a commit or key before sending the
/// next one anyway.
const SYNC_TIMEOUT: Duration = Duration::from_millis(300);
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
    /// Characters of the preedit the application shows.
    preedit_len: usize,
    /// The last key the application forwarded, a template for keys sent to it.
    last_event: Option<XEvent>,
    /// Where the application draws text, in X screen coordinates (the start of the
    /// baseline).
    spot: Option<(i32, i32)>,
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
    /// The communication window of each client window with input contexts.
    clients: BTreeMap<u32, u32>,
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
    fn focus_in(&mut self, key: IcKey) {
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
            info: ContextInfo::default(),
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
}

type Server = X11rbServer<RustConnection>;

impl ServerHandler<Server> for Handler {
    type InputStyleArray = [InputStyle; 3];
    type InputContextData = ();

    fn new_ic_data(&mut self, _server: &mut Server, _style: InputStyle) -> Result<(), ServerError> {
        Ok(())
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
        let key = ic_key(&user_ic.ic);
        self.focus_out(key);
        self.ics.remove(&key);
        self.pending.retain(|_, pending| pending.ic != key);
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
            if let Some(ctx) = ic.ctx {
                self.events.push_back(FrontendEvent::Updated {
                    ctx,
                    info: ContextInfo::default(),
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
    /// Since when a synchronous request waits for `XIM_SYNC_REPLY`, and from which
    /// client window.
    waiting: Option<(Instant, u32)>,
}

/// A request waiting for its turn.
#[derive(Debug)]
struct Queued {
    client_win: u32,
    request: Request,
    /// Whether what follows waits until the application consumed this.
    sync: bool,
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
        Ok(Self {
            server,
            connections: XimConnections::new(),
            handler: Handler {
                context_ids,
                root,
                com_win: None,
                clients: BTreeMap::new(),
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
        match event {
            Event::MappingNotify(_) => {
                self.handler.keymap = Keymap::load(self.server.conn())?;
            }
            Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => {
                self.monitors = load_monitors(self.server.conn(), self.handler.root);
            }
            // The `xim` crate ignores this answer; it lets the next request out.
            Event::ClientMessage(msg)
                if msg.type_ == self.xim_protocol && is_sync_reply(self.server.conn(), msg) =>
            {
                self.waiting = None;
            }
            Event::DestroyNotify(notify) => self.client_gone(notify.window),
            _ => {}
        }
        self.handler.com_win = match event {
            Event::ClientMessage(msg) if msg.type_ == self.xim_protocol => Some(msg.window),
            _ => None,
        };
        if let Err(e) = self
            .server
            .filter_event(event, &mut self.connections, &mut self.handler)
        {
            debug!("XIM: {e}");
        }
        Ok(())
    }

    /// Ends the input contexts of an application whose client window is gone.
    fn client_gone(&mut self, client_win: u32) {
        let Some(com_win) = self.handler.clients.remove(&client_win) else {
            return;
        };
        debug!(client_win, "XIM: an application went away");
        if let Some(connection) = self.connections.get_connection(com_win)
            && let Err(e) = connection.disconnect(&mut self.server, &mut self.handler)
        {
            debug!("XIM: {e}");
        }
        self.connections.remove_connection(com_win);
        self.queue.retain(|queued| queued.client_win != client_win);
        if self
            .waiting
            .is_some_and(|(_, waiting_for)| waiting_for == client_win)
        {
            self.waiting = None;
        }
    }

    /// When to give up on an application that did not answer a synchronous request.
    pub fn deadline(&self) -> Option<Instant> {
        self.waiting.map(|(since, _)| since + SYNC_TIMEOUT)
    }

    pub fn on_timer(&mut self, now: Instant) -> anyhow::Result<()> {
        if self.deadline().is_some_and(|deadline| deadline <= now) {
            debug!("XIM: an application did not answer in time");
            self.waiting = None;
        }
        self.process()
    }

    /// Queues `request` for `client_win`, for [`Self::process`] to send. A `sync` request
    /// holds back what follows until the application consumed it.
    fn send(&mut self, client_win: u32, request: Request, sync: bool) {
        self.queue.push_back(Queued {
            client_win,
            request,
            sync,
        });
    }

    /// Sends queued requests up to the next synchronous one. Returns whether it sent any.
    fn send_queued(&mut self) -> bool {
        let mut sent = false;
        while self.waiting.is_none() {
            let Some(queued) = self.queue.pop_front() else {
                break;
            };
            sent = true;
            if let Err(e) = self.server.send_req(queued.client_win, queued.request) {
                debug!("XIM: {e}");
                continue;
            }
            if queued.sync {
                self.waiting = Some((Instant::now(), queued.client_win));
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
            .map(|(ctx, spot)| (ctx, spot.map(|(x, y)| (x, y - ASCENT, 1, LINE_HEIGHT))))
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
        self.send(client_win, request, true);
    }

    /// Applies an engine's actions to the field `ctx`.
    pub fn apply_actions(&mut self, ctx: ContextId, actions: &[Action]) {
        let Some(key) = self.find(ctx) else {
            return;
        };
        for action in actions {
            match action {
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
                    self.send(client_win, request, true);
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
                    let template = self
                        .handler
                        .ics
                        .get(&key)
                        .and_then(|ic| ic.last_event.clone());
                    let detail = u8::try_from(*keycode)
                        .ok()
                        .and_then(|code| code.checked_add(X11_KEYCODE_OFFSET));
                    if let (Some(template), Some(detail)) = (template, detail) {
                        let xev = XEvent {
                            response_type: if *pressed { KEY_PRESS } else { KEY_RELEASE },
                            detail,
                            state: 0,
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
            self.send(client_win, request, false);
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

/// Whether the protocol message `msg` is `XIM_SYNC_REPLY`. Short messages travel in the
/// client message itself (format 8); others in a property it names (format 32), which is
/// only peeked at here and left for the `xim` crate to read.
fn is_sync_reply(conn: &RustConnection, msg: &ClientMessageEvent) -> bool {
    match msg.format {
        8 => msg.data.as_data8()[0] == XIM_SYNC_REPLY,
        32 => {
            let [_length, property, ..] = msg.data.as_data32();
            conn.get_property(false, msg.window, property, AtomEnum::ANY, 0, 1)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .is_some_and(|reply| reply.value.first() == Some(&XIM_SYNC_REPLY))
        }
        _ => false,
    }
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
