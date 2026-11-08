//! The D-Bus objects of the Fcitx5 module protocol and the bus connections they live on.
//!
//! Every object only forwards requests to [`super::FcitxFrontend`] through a channel.
//! What the frontend sends back goes through one queue per connection, so signals
//! reach an application in order and before the reply to the key that caused them.

use std::collections::BTreeMap;
use std::hash::{BuildHasher as _, Hasher as _};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use std::pin::pin;

use futures_core::Stream;
use omime_proto::tr;
#[cfg(test)]
use tokio::sync::oneshot;
use tokio::sync::{Semaphore, mpsc};
use tracing::{debug, info, warn};
use zbus::fdo::RequestNameReply;
use zbus::message::Header;
use zbus::object_server::ObjectServer;
use zbus::{OwnedObjectPath, OwnedVariant, Variant};

use super::{
    Batched, Bus, IcId, KeyAnswer, MAX_CONTEXTS, MAX_CONTEXTS_PER_OWNER, Outgoing, Request, Signal,
    output::{self, OutputQueue},
    window::{MAX_QUERIES, WindowQuery},
};

const INPUT_CONTEXT_INTERFACE: &str = "org.fcitx.Fcitx.InputContext1";
/// Where both bus names serve `org.fcitx.Fcitx.InputMethod1`.
const INPUT_METHOD_PATH: &str = "/org/freedesktop/portal/inputmethod";
/// Where the portal name also serves it, for older clients.
const INPUT_METHOD_COMPAT_PATH: &str = "/inputmethod";

/// Settle pending keys after a stalled write without cancelling a partial D-Bus frame.
const OUTPUT_TIMEOUT: Duration = Duration::from_millis(300);

/// The names fcitx5 owns, by [`Bus`].
const BUS_NAMES: [&str; 2] = ["org.fcitx.Fcitx5", "org.freedesktop.portal.Fcitx"];

#[derive(Default)]
struct ContextCounts {
    owners: BTreeMap<String, usize>,
    total: usize,
}

/// Holds a quota slot until the object is removed, including failed exports.
struct ContextSlot {
    counts: Arc<Mutex<ContextCounts>>,
    owner: String,
}

impl ContextSlot {
    fn reserve(counts: &Arc<Mutex<ContextCounts>>, owner: &str) -> zbus::fdo::Result<Self> {
        let mut state = counts.lock().unwrap_or_else(|e| e.into_inner());
        if state.owners.get(owner).copied().unwrap_or_default() >= MAX_CONTEXTS_PER_OWNER
            || state.total >= MAX_CONTEXTS
        {
            return Err(zbus::fdo::Error::LimitsExceeded(tr!(
                "too many Fcitx input contexts"
            )));
        }
        *state.owners.entry(owner.to_owned()).or_default() += 1;
        state.total += 1;
        Ok(Self {
            counts: counts.clone(),
            owner: owner.to_owned(),
        })
    }
}

impl Drop for ContextSlot {
    fn drop(&mut self) {
        let mut state = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = state.owners.get_mut(&self.owner) {
            *count -= 1;
            if *count == 0 {
                state.owners.remove(&self.owner);
            }
            state.total -= 1;
        }
    }
}

/// Takes the Fcitx5 names on the session bus and serves the protocol. Returns the
/// queues of the two connections (by [`Bus`]); a connection that could not be set up
/// has none.
pub async fn start(requests: mpsc::UnboundedSender<Request>) -> [Option<OutputQueue>; 2] {
    let next_ic = Arc::new(AtomicU64::new(0));
    let counts = Arc::new(Mutex::new(ContextCounts::default()));
    let budget = Arc::new(Semaphore::new(MAX_QUERIES));
    let mut queues = [None, None];
    for bus in [Bus::Main, Bus::Portal] {
        match connect(
            bus,
            requests.clone(),
            next_ic.clone(),
            counts.clone(),
            budget.clone(),
        )
        .await
        {
            Ok(queue) => queues[bus as usize] = Some(queue),
            Err(e) => warn!(
                name = BUS_NAMES[bus as usize],
                "cannot serve the Fcitx5 module protocol: {e}"
            ),
        }
    }
    queues
}

async fn connect(
    bus: Bus,
    requests: mpsc::UnboundedSender<Request>,
    next_ic: Arc<AtomicU64>,
    counts: Arc<Mutex<ContextCounts>>,
    budget: Arc<Semaphore>,
) -> zbus::Result<OutputQueue> {
    let (queue, outgoing) = output::channel();
    let admission = queue.reply_admission();
    let input_method = || InputMethodObject {
        admission: admission.clone(),
        bus,
        next_ic: next_ic.clone(),
        counts: counts.clone(),
        budget: budget.clone(),
        requests: requests.clone(),
    };
    let mut builder =
        zbus::connection::Builder::session().serve_at(INPUT_METHOD_PATH, input_method());
    if bus == Bus::Portal {
        builder = builder.serve_at(INPUT_METHOD_COMPAT_PATH, input_method());
    }
    let conn = builder.build().await?;
    watch_clients(conn.clone(), requests.clone()).await?;

    // Queue for the name rather than giving up: when fcitx5 runs, X11 applications keep
    // using it until it exits, and then this connection takes over.
    let name = BUS_NAMES[bus as usize];
    match conn
        .request_name_with_flags(name, zbus::fdo::RequestNameFlags::empty())
        .await?
    {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {
            info!(name, "serving the Fcitx5 module protocol");
        }
        RequestNameReply::InQueue | RequestNameReply::Exists => warn!(
            name,
            "another input method (fcitx5?) owns this name; applications using the \
             Fcitx5 module talk to it until it exits"
        ),
    }

    tokio::task::spawn_local(send_outgoing(conn.clone(), outgoing, requests));
    Ok(queue)
}

/// Sends what the frontend queued for this connection, in order.
async fn send_outgoing(
    conn: zbus::Connection,
    mut outgoing: output::Receiver,
    requests: mpsc::UnboundedSender<Request>,
) {
    while let Some(entry) = outgoing.recv().await {
        let (ic, ctx) = (entry.ic, entry.ctx);
        let result = match entry.item {
            Outgoing::Signal { dest, path, signal } => {
                send_with_deadline(
                    &outgoing,
                    &requests,
                    ic,
                    ctx,
                    emit(&conn, &dest, &path, signal),
                )
                .await
            }
            // Everything queued before has been sent now.
            Outgoing::Reply { reply, answer } => {
                send_with_deadline(&outgoing, &requests, ic, ctx, reply.write(&conn, answer)).await
            }
            Outgoing::Remove(path) => conn
                .object_server()
                .remove::<InputContextService, _>(&path)
                .await
                .map(|_| ()),
        };
        if let Err(e) = result {
            let _ = requests.send(Request::OutputFailed { ic, ctx });
            debug!("Fcitx5 module protocol: {e}");
        }
    }
}

async fn send_with_deadline(
    outgoing: &output::Receiver,
    requests: &mpsc::UnboundedSender<Request>,
    ic: IcId,
    ctx: Option<omime_proto::engine::ContextId>,
    send: impl Future<Output = zbus::Result<()>>,
) -> zbus::Result<()> {
    let mut send = pin!(send);
    let mut first_deadline = true;
    loop {
        tokio::select! {
            result = &mut send => return result,
            () = tokio::time::sleep(OUTPUT_TIMEOUT) => {
                let mut failed = outgoing.discard_all();
                if std::mem::take(&mut first_deadline) && !failed.contains(&(ic, ctx)) { failed.push((ic, ctx)); }
                for (ic, ctx) in failed {
                    let _ = requests.send(Request::OutputFailed { ic, ctx });
                }
                // The socket may contain half a message. Keep this one write alive;
                // dropping it and sending a new frame could corrupt the connection.
                // Pending replies have been settled and subsequent admission is bounded.
            }
        }
    }
}

async fn emit(
    conn: &zbus::Connection,
    dest: &str,
    path: &OwnedObjectPath,
    signal: Signal,
) -> zbus::Result<()> {
    let emit = |name: &'static str| (Some(dest), path, INPUT_CONTEXT_INTERFACE, name);
    match signal {
        Signal::Commit(text) => {
            let (d, p, i, m) = emit("CommitString");
            conn.emit_signal(d, p, i, m, &text).await
        }
        Signal::Preedit(segments, cursor) => {
            let (d, p, i, m) = emit("UpdateFormattedPreedit");
            conn.emit_signal(d, p, i, m, &(segments, cursor)).await
        }
        Signal::Forward(keyval, state, released) => {
            let (d, p, i, m) = emit("ForwardKey");
            conn.emit_signal(d, p, i, m, &(keyval, state, released))
                .await
        }
        Signal::Delete(offset, size) => {
            let (d, p, i, m) = emit("DeleteSurroundingText");
            conn.emit_signal(d, p, i, m, &(offset, size)).await
        }
    }
}

/// Tells the frontend when an application leaves the bus, so its contexts go too.
async fn watch_clients(
    conn: zbus::Connection,
    requests: mpsc::UnboundedSender<Request>,
) -> zbus::Result<()> {
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    let changes = dbus.receive_name_owner_changed().await?;
    // Install the subscription before advertising the service, so even the first
    // client cannot disappear before disconnect monitoring is ready.
    tokio::task::spawn_local(async move {
        let mut changes = pin!(changes);
        while let Some(change) = next(changes.as_mut()).await {
            let Ok(args) = change.args() else { continue };
            if args.name().starts_with(':') && args.new_owner().is_none() {
                let _ = requests.send(Request::ClientGone(args.name().to_string()));
            }
        }
    });
    Ok(())
}

/// Waits for the next item of `stream`.
async fn next<S: Stream>(mut stream: std::pin::Pin<&mut S>) -> Option<S::Item> {
    std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await
}

/// `org.fcitx.Fcitx.InputMethod1`: creates input contexts.
struct InputMethodObject {
    admission: output::ReplyAdmission,
    bus: Bus,
    budget: Arc<Semaphore>,
    next_ic: Arc<AtomicU64>,
    counts: Arc<Mutex<ContextCounts>>,
    requests: mpsc::UnboundedSender<Request>,
}

#[zbus::interface(name = "org.fcitx.Fcitx.InputMethod1")]
impl InputMethodObject {
    /// Creates an input context for the caller. `args` are hints such as `program` and
    /// `display` (`x11:` or `wayland:`).
    async fn create_input_context(
        &self,
        args: Vec<(String, String)>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<(OwnedObjectPath, Vec<u8>)> {
        let owner = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::Failed("no sender".into()))?;
        let owner_name = owner.to_owned();
        let owner = owner.to_string();
        let slot = ContextSlot::reserve(&self.counts, &owner)?;
        let ic = self.next_ic.fetch_add(1, Ordering::Relaxed) + 1;
        let path = OwnedObjectPath::try_from(format!("/org/freedesktop/portal/inputcontext/{ic}"))
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let wayland = args
            .iter()
            .any(|(key, value)| key == "display" && value.starts_with("wayland:"));
        let object = InputContextObject {
            _slot: slot,
            ic,
            owner: owner.clone(),
            requests: self.requests.clone(),
            wayland,
            focused: AtomicBool::new(false),
            window_query: WindowQuery::new(ic, self.requests.clone(), self.budget.clone()),
        };
        server
            .at(
                &path,
                InputContextService {
                    inner: object,
                    admission: self.admission.clone(),
                    reply_slots: Arc::new(Semaphore::new(output::MAX_IC_ITEMS)),
                },
            )
            .await?;
        if self
            .requests
            .send(Request::Created {
                ic,
                bus: self.bus,
                owner,
                path: path.clone(),
                args,
            })
            .is_err()
        {
            let _ = server.remove::<InputContextService, _>(&path).await;
            return Err(zbus::fdo::Error::Failed(tr!("the daemon is shutting down")));
        }
        // ClientGone can precede Created while exporting the object yields. Check
        // after queuing Created: a departed owner needs a later Destroy; an owner
        // still present here is covered by the installed disconnect subscription.
        let connected: zbus::fdo::Result<bool> = async {
            zbus::fdo::DBusProxy::new(conn)
                .await?
                .name_has_owner(owner_name.into())
                .await
        }
        .await;
        if !matches!(connected, Ok(true)) {
            let _ = self.requests.send(Request::Destroy(ic));
            connected?;
            return Err(zbus::fdo::Error::NameHasNoOwner(tr!(
                "the Fcitx client disconnected"
            )));
        }
        Ok((path, uuid(ic)))
    }

    /// The protocol version: 1 has `ProcessKeyEventBatch`.
    fn version(&self) -> u32 {
        1
    }
}

/// A 16 byte id for an input context; clients do not depend on its value.
fn uuid(ic: IcId) -> Vec<u8> {
    let state = std::collections::hash_map::RandomState::new();
    [ic, !ic]
        .iter()
        .flat_map(|seed| {
            let mut hasher = state.build_hasher();
            hasher.write_u64(*seed);
            hasher.finish().to_le_bytes()
        })
        .collect()
}

/// `org.fcitx.Fcitx.InputContext1`: one text field of one application.
struct InputContextObject {
    _slot: ContextSlot,
    ic: IcId,
    /// The unique bus name of the application; only it may use this context.
    owner: String,
    requests: mpsc::UnboundedSender<Request>,
    /// Whether the application runs on Wayland, where its text cursor is relative to its
    /// window, which only the compositor knows the position of.
    wayland: bool,
    focused: AtomicBool,
    window_query: WindowQuery,
}

/// Only key methods bypass the generated responder: their reply belongs to the
/// same ordered worker as text signals, through serialization and the real write.
/// The locked zbus Interface API is delegated for everything else (including XML).
struct InputContextService {
    inner: InputContextObject,
    admission: output::ReplyAdmission,
    reply_slots: Arc<Semaphore>,
}

#[zbus::export::async_trait::async_trait]
impl zbus::object_server::Interface for InputContextService {
    fn name() -> zbus::names::InterfaceName<'static> {
        InputContextObject::name()
    }

    fn spawn_tasks_for_methods(&self) -> bool {
        false
    }

    async fn get(
        &self,
        property: &str,
        server: &ObjectServer,
        connection: &zbus::Connection,
        header: Option<&Header<'_>>,
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> Option<Result<OwnedVariant, zbus::BoxDBusError>> {
        self.inner
            .get(property, server, connection, header, emitter)
            .await
    }

    async fn get_all(
        &self,
        server: &ObjectServer,
        connection: &zbus::Connection,
        header: Option<&Header<'_>>,
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<std::collections::HashMap<String, OwnedVariant>> {
        self.inner
            .get_all(server, connection, header, emitter)
            .await
    }

    fn set<'a>(
        &'a self,
        property: &'a str,
        value: &'a Variant<'_>,
        server: &'a ObjectServer,
        connection: &'a zbus::Connection,
        header: Option<&'a Header<'_>>,
        emitter: &'a zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::object_server::DispatchResult<'a> {
        self.inner
            .set(property, value, server, connection, header, emitter)
    }

    async fn set_mut(
        &mut self,
        property: &str,
        value: &Variant<'_>,
        server: &ObjectServer,
        connection: &zbus::Connection,
        header: Option<&Header<'_>>,
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> Option<Result<(), zbus::BoxDBusError>> {
        self.inner
            .set_mut(property, value, server, connection, header, emitter)
            .await
    }

    fn call<'a>(
        &'a self,
        server: &'a ObjectServer,
        connection: &'a zbus::Connection,
        message: &'a zbus::Message,
        name: zbus::names::MemberName<'a>,
    ) -> zbus::object_server::DispatchResult<'a> {
        let batch = match name.as_str() {
            "ProcessKeyEvent" => false,
            "ProcessKeyEventBatch" => true,
            _ => return self.inner.call(server, connection, message, name),
        };
        zbus::object_server::DispatchResult::from_handler(async move {
            let key = message.body().deserialize::<(u32, u32, u32, bool, u32)>()?;
            let header = message.header();
            let Some(reply) = self
                .admission
                .reserve(self.inner.ic, self.reply_slots.clone(), &header, batch)
                .await
            else {
                return Ok(());
            };
            if self.inner.is_owner(&header) {
                let request = Request::Key {
                    ic: self.inner.ic,
                    key,
                    batch,
                    reply,
                };
                if let Err(error) = self.inner.requests.send(request)
                    && let Request::Key { reply, .. } = error.0
                {
                    let _ = reply.send(KeyAnswer::default());
                }
            } else {
                let _ = reply.send(KeyAnswer::default());
            }
            Ok(())
        })
    }

    fn call_mut<'a>(
        &'a mut self,
        server: &'a ObjectServer,
        connection: &'a zbus::Connection,
        message: &'a zbus::Message,
        name: zbus::names::MemberName<'a>,
    ) -> zbus::object_server::DispatchResult<'a> {
        self.inner.call_mut(server, connection, message, name)
    }

    fn introspect_to_writer(&self, writer: &mut dyn std::fmt::Write, level: usize) {
        self.inner.introspect_to_writer(writer, level);
    }
}

impl InputContextObject {
    /// Forwards `request` if the caller owns this context.
    fn send(&self, header: &Header<'_>, request: Request) {
        if self.is_owner(header) {
            let _ = self.requests.send(request);
        }
    }

    fn is_owner(&self, header: &Header<'_>) -> bool {
        header
            .sender()
            .is_some_and(|sender| sender.as_str() == self.owner)
    }

    /// Asks the compositor where the focused window is, for a Wayland application, now
    /// (`always`) or if it was asked long enough ago: windows move.
    fn ask_window(&self, always: bool) {
        if !self.wayland {
            return;
        }
        self.window_query.ask(always);
    }
}

#[zbus::interface(name = "org.fcitx.Fcitx.InputContext1")]
impl InputContextObject {
    fn focus_in(&self, #[zbus(header)] header: Header<'_>) {
        if self.is_owner(&header) {
            self.send(&header, Request::FocusIn(self.ic));
            let was_focused = self.focused.swap(true, Ordering::Relaxed);
            self.ask_window(!was_focused);
        }
    }

    fn focus_out(&self, #[zbus(header)] header: Header<'_>) {
        if self.is_owner(&header) {
            self.focused.store(false, Ordering::Relaxed);
            self.window_query.cancel();
        }
        self.send(&header, Request::FocusOut(self.ic));
    }

    fn reset(&self, #[zbus(header)] header: Header<'_>) {
        self.send(&header, Request::Reset(self.ic));
    }

    fn set_cursor_rect(&self, x: i32, y: i32, w: i32, h: i32, #[zbus(header)] header: Header<'_>) {
        let rect = (x, y, w, h);
        let scale = 1.0;
        self.send(
            &header,
            Request::CursorRect {
                ic: self.ic,
                rect,
                scale,
            },
        );
        if self.is_owner(&header) {
            self.ask_window(false);
        }
    }

    #[zbus(name = "SetCursorRectV2")]
    fn set_cursor_rect_v2(
        &self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        scale: f64,
        #[zbus(header)] header: Header<'_>,
    ) {
        let rect = (x, y, w, h);
        self.send(
            &header,
            Request::CursorRect {
                ic: self.ic,
                rect,
                scale,
            },
        );
        if self.is_owner(&header) {
            self.ask_window(false);
        }
    }

    fn set_capability(&self, capability: u64, #[zbus(header)] header: Header<'_>) {
        let request = Request::Capability {
            ic: self.ic,
            capability,
        };
        self.send(&header, request);
    }

    fn set_supported_capability(&self, _capability: u64) {}

    fn set_surrounding_text(
        &self,
        text: String,
        cursor: u32,
        anchor: u32,
        #[zbus(header)] header: Header<'_>,
    ) {
        let request = Request::Surrounding {
            ic: self.ic,
            text: Some(text),
            cursor,
            anchor,
        };
        self.send(&header, request);
    }

    fn set_surrounding_text_position(
        &self,
        cursor: u32,
        anchor: u32,
        #[zbus(header)] header: Header<'_>,
    ) {
        let request = Request::Surrounding {
            ic: self.ic,
            text: None,
            cursor,
            anchor,
        };
        self.send(&header, request);
    }

    #[zbus(name = "DestroyIC")]
    fn destroy_ic(&self, #[zbus(header)] header: Header<'_>) {
        if self.is_owner(&header) {
            self.window_query.cancel();
        }
        self.send(&header, Request::Destroy(self.ic));
    }

    /// Returns whether the key was consumed. What it produced was sent as signals
    /// before this reply.
    async fn process_key_event(
        &self,
        keyval: u32,
        keycode: u32,
        state: u32,
        is_release: bool,
        time: u32,
        #[zbus(header)] header: Header<'_>,
    ) -> bool {
        let _ = (keyval, keycode, state, is_release, time, header);
        unreachable!("key replies are dispatched by InputContextService")
    }

    /// Like `ProcessKeyEvent`, but returns what the key produced instead of signalling
    /// it.
    async fn process_key_event_batch(
        &self,
        keyval: u32,
        keycode: u32,
        state: u32,
        is_release: bool,
        time: u32,
        #[zbus(header)] header: Header<'_>,
    ) -> (Vec<(u32, OwnedVariant)>, bool) {
        let _ = (keyval, keycode, state, is_release, time, header);
        unreachable!("key replies are dispatched by InputContextService")
    }

    // Candidate lists and on-screen keyboards are the engines' business; clients that
    // ask get no effect.
    fn prev_page(&self) {}

    fn next_page(&self) {}

    fn select_candidate(&self, _index: i32) {}

    fn invoke_action(&self, _action: u32, _cursor: i32) {}

    fn is_virtual_keyboard_visible(&self) -> bool {
        false
    }

    fn show_virtual_keyboard(&self) {}

    fn hide_virtual_keyboard(&self) {}
}

/// Encodes an event of a `ProcessKeyEventBatch` reply.
pub(super) fn batched(event: Batched) -> Option<(u32, OwnedVariant)> {
    let (kind, value) = match event {
        Batched(Signal::Commit(text)) => (0, Variant::from(text)),
        Batched(Signal::Preedit(segments, cursor)) => (1, Variant::from((segments, cursor))),
        Batched(Signal::Forward(keyval, state, released)) => {
            (2, Variant::from((keyval, state, released)))
        }
        Batched(Signal::Delete(offset, size)) => (3, Variant::from((offset, size))),
    };
    OwnedVariant::try_from(value)
        .ok()
        .map(|value| (kind, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stalled_write_settles_current_and_later_keys_without_cancelling_the_frame() {
        let (queue, receiver) = output::channel();
        let (requests, mut events) = mpsc::unbounded_channel();
        let (reply, mut answer) = oneshot::channel();
        assert!(queue.send(
            1,
            Some(1),
            Outgoing::Reply {
                reply: reply.into(),
                answer: KeyAnswer::default()
            }
        ));
        let (complete, completion) = oneshot::channel();
        let send = send_with_deadline(&receiver, &requests, 1, Some(1), async {
            completion.await.unwrap();
            Ok(())
        });
        let mut send = pin!(send);
        tokio::select! {
            _ = &mut send => panic!("the frame must still be pending"),
            event = events.recv() => assert!(matches!(event, Some(Request::OutputFailed { ic: 1, ctx: Some(1) }))),
        }
        assert!(answer.try_recv().unwrap().handled);
        let (reply, mut answer) = oneshot::channel();
        assert!(queue.send(
            2,
            Some(2),
            Outgoing::Reply {
                reply: reply.into(),
                answer: KeyAnswer::default()
            }
        ));
        tokio::select! {
            _ = &mut send => panic!("the frame must still be pending"),
            event = events.recv() => assert!(matches!(event, Some(Request::OutputFailed { ic: 2, ctx: Some(2) }))),
        }
        assert!(answer.try_recv().unwrap().handled);
        complete.send(()).unwrap();
        send.await.unwrap();
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
