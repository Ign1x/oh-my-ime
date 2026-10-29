//! The D-Bus objects of the Fcitx5 module protocol and the bus connections they live on.
//!
//! Every object only forwards requests to [`super::FcitxFrontend`] through a channel.
//! What the frontend sends back goes through one queue per connection, so signals
//! reach an application in order and before the reply to the key that caused them.

use std::hash::{BuildHasher as _, Hasher as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use std::pin::pin;

use futures_core::Stream;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};
use zbus::fdo::RequestNameReply;
use zbus::message::Header;
use zbus::object_server::ObjectServer;
use zbus::{OwnedObjectPath, OwnedVariant, Variant};

use super::{Batched, Bus, IcId, KeyAnswer, Outgoing, Request, Signal};

const INPUT_CONTEXT_INTERFACE: &str = "org.fcitx.Fcitx.InputContext1";
/// Where both bus names serve `org.fcitx.Fcitx.InputMethod1`.
const INPUT_METHOD_PATH: &str = "/org/freedesktop/portal/inputmethod";
/// Where the portal name also serves it, for older clients.
const INPUT_METHOD_COMPAT_PATH: &str = "/inputmethod";

/// The names fcitx5 owns, by [`Bus`].
const BUS_NAMES: [&str; 2] = ["org.fcitx.Fcitx5", "org.freedesktop.portal.Fcitx"];

/// Takes the Fcitx5 names on the session bus and serves the protocol. Returns the
/// queues of the two connections (by [`Bus`]); a connection that could not be set up
/// has none.
pub async fn start(
    requests: mpsc::UnboundedSender<Request>,
) -> [Option<mpsc::UnboundedSender<Outgoing>>; 2] {
    let next_ic = Arc::new(AtomicU64::new(0));
    let mut queues = [None, None];
    for bus in [Bus::Main, Bus::Portal] {
        match connect(bus, requests.clone(), next_ic.clone()).await {
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
) -> zbus::Result<mpsc::UnboundedSender<Outgoing>> {
    let input_method = || InputMethodObject {
        bus,
        next_ic: next_ic.clone(),
        requests: requests.clone(),
    };
    let mut builder =
        zbus::connection::Builder::session().serve_at(INPUT_METHOD_PATH, input_method());
    if bus == Bus::Portal {
        builder = builder.serve_at(INPUT_METHOD_COMPAT_PATH, input_method());
    }
    let conn = builder.build().await?;

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

    let (queue, outgoing) = mpsc::unbounded_channel();
    tokio::task::spawn_local(send_outgoing(conn.clone(), outgoing));
    tokio::task::spawn_local(watch_clients(conn, requests));
    Ok(queue)
}

/// Sends what the frontend queued for this connection, in order.
async fn send_outgoing(conn: zbus::Connection, mut outgoing: mpsc::UnboundedReceiver<Outgoing>) {
    while let Some(item) = outgoing.recv().await {
        let result = match item {
            Outgoing::Signal { dest, path, signal } => emit(&conn, &dest, &path, signal).await,
            // Everything queued before has been sent now.
            Outgoing::Reply { reply, answer } => {
                let _ = reply.send(answer);
                Ok(())
            }
            Outgoing::Remove(path) => conn
                .object_server()
                .remove::<InputContextObject, _>(&path)
                .await
                .map(|_| ()),
        };
        if let Err(e) = result {
            debug!("Fcitx5 module protocol: {e}");
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
async fn watch_clients(conn: zbus::Connection, requests: mpsc::UnboundedSender<Request>) {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else {
        return;
    };
    let Ok(changes) = dbus.receive_name_owner_changed().await else {
        return;
    };
    let mut changes = pin!(changes);
    while let Some(change) = next(changes.as_mut()).await {
        let Ok(args) = change.args() else { continue };
        if args.name().starts_with(':') && args.new_owner().is_none() {
            let _ = requests.send(Request::ClientGone(args.name().to_string()));
        }
    }
}

/// Waits for the next item of `stream`.
async fn next<S: Stream>(mut stream: std::pin::Pin<&mut S>) -> Option<S::Item> {
    std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await
}

/// `org.fcitx.Fcitx.InputMethod1`: creates input contexts.
struct InputMethodObject {
    bus: Bus,
    next_ic: Arc<AtomicU64>,
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
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<(OwnedObjectPath, Vec<u8>)> {
        let owner = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::Failed("no sender".into()))?
            .to_string();
        let ic = self.next_ic.fetch_add(1, Ordering::Relaxed) + 1;
        let path = OwnedObjectPath::try_from(format!("/org/freedesktop/portal/inputcontext/{ic}"))
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let object = InputContextObject {
            ic,
            owner: owner.clone(),
            requests: self.requests.clone(),
        };
        server.at(&path, object).await?;
        let _ = self.requests.send(Request::Created {
            ic,
            bus: self.bus,
            owner,
            path: path.clone(),
            args,
        });
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
    ic: IcId,
    /// The unique bus name of the application; only it may use this context.
    owner: String,
    requests: mpsc::UnboundedSender<Request>,
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

    async fn key(
        &self,
        header: &Header<'_>,
        key: (u32, u32, u32, bool, u32),
        batch: bool,
    ) -> KeyAnswer {
        if !self.is_owner(header) {
            return KeyAnswer::default();
        }
        let (reply, answer) = oneshot::channel();
        let request = Request::Key {
            ic: self.ic,
            key,
            batch,
            reply,
        };
        if self.requests.send(request).is_err() {
            return KeyAnswer::default();
        }
        answer.await.unwrap_or_default()
    }
}

#[zbus::interface(name = "org.fcitx.Fcitx.InputContext1")]
impl InputContextObject {
    fn focus_in(&self, #[zbus(header)] header: Header<'_>) {
        self.send(&header, Request::FocusIn(self.ic));
    }

    fn focus_out(&self, #[zbus(header)] header: Header<'_>) {
        self.send(&header, Request::FocusOut(self.ic));
    }

    fn reset(&self, #[zbus(header)] header: Header<'_>) {
        self.send(&header, Request::Reset(self.ic));
    }

    // Engine windows do not follow the text cursor of these applications yet.
    fn set_cursor_rect(&self, _x: i32, _y: i32, _w: i32, _h: i32) {}

    #[zbus(name = "SetCursorRectV2")]
    fn set_cursor_rect_v2(&self, _x: i32, _y: i32, _w: i32, _h: i32, _scale: f64) {}

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
        let key = (keyval, keycode, state, is_release, time);
        self.key(&header, key, false).await.handled
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
        let key = (keyval, keycode, state, is_release, time);
        let answer = self.key(&header, key, true).await;
        let events = answer.batch.into_iter().filter_map(batched).collect();
        (events, answer.handled)
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
fn batched(event: Batched) -> Option<(u32, OwnedVariant)> {
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
