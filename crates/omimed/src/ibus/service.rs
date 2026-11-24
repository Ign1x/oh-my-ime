//! D-Bus transport for the native IBus frontend. All state changes are actor
//! requests. A bounded output worker sends signals before completing key replies.
use super::{COMPONENT, ENGINE, Outgoing, Request, Signal, wire};
use anyhow::Context as _;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::sync::{Semaphore, mpsc, oneshot};
use zbus::{OwnedObjectPath, OwnedVariant};
const INTERFACE: &str = "org.freedesktop.IBus.Engine";
const KEY_TIMEOUT: Duration = Duration::from_secs(3);

pub async fn run(requests: mpsc::Sender<Request>) {
    let mut delay = Duration::from_secs(1);
    loop {
        match connect(requests.clone()).await {
            Ok(conn) => {
                delay = Duration::from_secs(1);
                tracing::info!("registered oh-my-ime with the session IBus daemon");
                conn.closed().await;
            }
            Err(error) => tracing::warn!("cannot connect native IBus frontend: {error:#}"),
        }
        if requests.send(Request::Disconnected).await.is_err() {
            return;
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}
async fn address() -> anyhow::Result<String> {
    if let Ok(address) = std::env::var("IBUS_ADDRESS")
        && !address.trim().is_empty()
    {
        return Ok(address);
    }
    let mut command = tokio::process::Command::new("ibus");
    command.arg("address").kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(2), command.output())
        .await
        .context("ibus address timed out")??;
    let address = String::from_utf8(output.stdout)?.trim().to_owned();
    anyhow::ensure!(
        output.status.success() && !address.is_empty(),
        "session IBus address is unavailable"
    );
    Ok(address)
}
async fn connect(requests: mpsc::Sender<Request>) -> anyhow::Result<zbus::Connection> {
    let address = address().await?;
    let owner = Arc::new(std::sync::OnceLock::<String>::new());
    let factory = Factory {
        owner: owner.clone(),
        requests: requests.clone(),
        next: Arc::new(AtomicU64::new(1)),
        keys: Arc::new(Semaphore::new(64)),
        contexts: Arc::new(Semaphore::new(8192)),
    };
    let conn = tokio::time::timeout(
        Duration::from_secs(5),
        zbus::connection::Builder::address(address.as_str())
            .serve_at("/org/freedesktop/IBus/Factory", factory)
            .build(),
    )
    .await??;
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    let ibus_owner = dbus
        .get_name_owner("org.freedesktop.IBus".try_into()?)
        .await?;
    let _ = owner.set(ibus_owner.to_string());
    conn.request_name(COMPONENT).await?;
    let proxy = zbus::Proxy::new(
        &conn,
        "org.freedesktop.IBus",
        "/org/freedesktop/IBus",
        "org.freedesktop.IBus",
    )
    .await?;
    tokio::time::timeout(
        Duration::from_secs(5),
        proxy.call::<_, _, ()>("RegisterComponent", &(wire::component(),)),
    )
    .await??;
    let (output, mut outgoing) = mpsc::channel(256);
    requests.send(Request::Connected(output)).await?;
    let worker_conn = conn.clone();
    tokio::task::spawn_local(async move {
        while let Some(item) = outgoing.recv().await {
            match item {
                Outgoing::Reply(reply, handled, _produced) => {
                    let _ = reply.send(handled);
                }
                Outgoing::Signal {
                    path,
                    live,
                    ctx,
                    signal,
                    produced,
                } => {
                    if live.load(Ordering::Acquire) != ctx {
                        continue;
                    }
                    let send = emit(&worker_conn, &path, signal);
                    // Closing the connection after a stalled frame avoids reusing a
                    // partially written transport. Unsent replies then fail open.
                    match tokio::time::timeout(Duration::from_millis(500), send).await {
                        Ok(Ok(())) => {
                            if let Some(produced) = produced {
                                produced.store(true, Ordering::Release);
                            }
                        }
                        _ => {
                            let _ = worker_conn.close().await;
                            break;
                        }
                    }
                }
            }
        }
    });
    Ok(conn)
}
async fn emit(conn: &zbus::Connection, path: &OwnedObjectPath, signal: Signal) -> zbus::Result<()> {
    match signal {
        Signal::Commit(text) => {
            // Commit replaces composition even when the engine omits explicit
            // clears. IBus caches preedit and lookup independently from CommitText;
            // clear them in this one ordered transaction before sending the commit.
            conn.emit_signal(
                None::<&str>,
                path,
                INTERFACE,
                "UpdatePreeditText",
                &(wire::text(""), 0u32, false, 0u32),
            )
            .await?;
            conn.emit_signal(
                None::<&str>,
                path,
                INTERFACE,
                "UpdateLookupTable",
                &(
                    wire::lookup(&omime_proto::engine::CandidatePage::default()),
                    false,
                ),
            )
            .await?;
            conn.emit_signal(
                None::<&str>,
                path,
                INTERFACE,
                "CommitText",
                &(wire::text(&text),),
            )
            .await
        }
        Signal::Preedit(text, cursor) => {
            conn.emit_signal(
                None::<&str>,
                path,
                INTERFACE,
                "UpdatePreeditText",
                &(wire::text(&text), cursor, !text.is_empty(), 0u32),
            )
            .await
        }
        Signal::Delete(offset, count) => {
            conn.emit_signal(
                None::<&str>,
                path,
                INTERFACE,
                "DeleteSurroundingText",
                &(offset, count),
            )
            .await
        }
        Signal::Forward(keyval, keycode, state) => {
            conn.emit_signal(
                None::<&str>,
                path,
                INTERFACE,
                "ForwardKeyEvent",
                &(keyval, keycode, state),
            )
            .await
        }
        Signal::Candidates(page) => {
            conn.emit_signal(
                None::<&str>,
                path,
                INTERFACE,
                "UpdateLookupTable",
                &(wire::lookup(&page), !page.candidates.is_empty()),
            )
            .await
        }
    }
}
struct Factory {
    contexts: Arc<Semaphore>,
    owner: Arc<std::sync::OnceLock<String>>,
    requests: mpsc::Sender<Request>,
    next: Arc<AtomicU64>,
    keys: Arc<Semaphore>,
}
#[zbus::interface(name = "org.freedesktop.IBus.Factory")]
impl Factory {
    async fn create_engine(
        &self,
        name: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(object_server)] server: &zbus::object_server::ObjectServer,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        authorize(&self.owner, &header)?;
        if name != ENGINE {
            return Err(zbus::fdo::Error::InvalidArgs("unknown engine".into()));
        }
        let ic = self.next.fetch_add(1, Ordering::Relaxed);
        let slot =
            self.contexts.clone().try_acquire_owned().map_err(|_| {
                zbus::fdo::Error::LimitsExceeded("too many live IBus engines".into())
            })?;
        let path = OwnedObjectPath::try_from(format!("/org/freedesktop/IBus/Engine/OhMyIme/{ic}"))
            .expect("numeric IBus engine path is valid");
        let live = Arc::new(AtomicU64::new(0));
        let object = EngineObject {
            _slot: slot,
            owner: self.owner.clone(),
            ic,
            requests: self.requests.clone(),
            next_key: AtomicU64::new(1),
            live: live.clone(),
            keys: self.keys.clone(),
        };
        server.at(&path, object).await?;
        server
            .at(
                &path,
                Service {
                    owner: self.owner.clone(),
                    path: path.clone(),
                    ic,
                    requests: self.requests.clone(),
                    live: live.clone(),
                },
            )
            .await?;
        if self
            .requests
            .send(Request::Created {
                ic,
                path: path.clone(),
                live,
            })
            .await
            .is_err()
        {
            return Err(zbus::fdo::Error::Failed("daemon stopped".into()));
        }
        Ok(path)
    }
}
struct Service {
    owner: Arc<std::sync::OnceLock<String>>,
    path: OwnedObjectPath,
    ic: u64,
    requests: mpsc::Sender<Request>,
    live: Arc<AtomicU64>,
}
#[zbus::interface(name = "org.freedesktop.IBus.Service")]
impl Service {
    async fn destroy(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(&self.owner, &header)?;
        self.live.store(0, Ordering::Release);
        let _ = self.requests.send(Request::Destroy(self.ic)).await;
        let conn = conn.clone();
        let path = self.path.clone();
        tokio::task::spawn_local(async move {
            let _ = conn.object_server().remove::<EngineObject, _>(&path).await;
            let _ = conn.object_server().remove::<Service, _>(&path).await;
        });
        Ok(())
    }
}
struct EngineObject {
    _slot: tokio::sync::OwnedSemaphorePermit,
    owner: Arc<std::sync::OnceLock<String>>,
    ic: u64,
    requests: mpsc::Sender<Request>,
    next_key: AtomicU64,
    live: Arc<AtomicU64>,
    keys: Arc<Semaphore>,
}
impl EngineObject {
    async fn send(&self, request: Request) {
        if self.requests.try_send(request).is_err() {
            self.live.store(0, Ordering::Release);
            let _ = self.requests.try_send(Request::Expired(self.ic));
        }
    }
}
#[zbus::interface(name = "org.freedesktop.IBus.Engine")]
impl EngineObject {
    async fn process_key_event(
        &self,
        keyval: u32,
        keycode: u32,
        state: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> bool {
        if authorize(&self.owner, &header).is_err() {
            return false;
        }
        if self.live.load(Ordering::Acquire) == 0 {
            return false;
        }
        let Ok(_permit) = self.keys.try_acquire() else {
            return false;
        };
        let (reply, receive) = oneshot::channel();
        let produced = Arc::new(AtomicBool::new(false));
        let id = (self.ic << 32) | self.next_key.fetch_add(1, Ordering::Relaxed);
        if self
            .requests
            .try_send(Request::Key {
                ic: self.ic,
                keyval,
                keycode,
                state,
                reply,
                id,
                produced: produced.clone(),
            })
            .is_err()
        {
            return false;
        }
        match tokio::time::timeout(KEY_TIMEOUT, receive).await {
            Ok(Ok(handled)) => handled,
            _ => {
                self.live.store(0, Ordering::Release);
                let _ = self.requests.try_send(Request::Expired(self.ic));
                produced.load(Ordering::Acquire)
            }
        }
    }
    async fn focus_in(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        self.send(Request::Focus(self.ic, true, None)).await;
    }
    async fn focus_in_id(
        &self,
        _object_path: &str,
        client: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        self.send(Request::Focus(
            self.ic,
            true,
            client.split_once(':').map(|(_, app)| app.to_owned()),
        ))
        .await;
    }
    async fn focus_out(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        self.live.store(0, Ordering::Release);
        self.send(Request::Focus(self.ic, false, None)).await;
    }
    async fn focus_out_id(
        &self,
        _object_path: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        self.live.store(0, Ordering::Release);
        self.send(Request::Focus(self.ic, false, None)).await;
    }
    async fn enable(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        self.send(Request::Enable(self.ic, true)).await;
    }
    async fn disable(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        self.live.store(0, Ordering::Release);
        self.send(Request::Enable(self.ic, false)).await;
    }
    async fn reset(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        self.send(Request::Reset(self.ic)).await;
    }
    async fn set_capabilities(&self, caps: u32, #[zbus(header)] header: zbus::message::Header<'_>) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        self.send(Request::Capabilities(self.ic, caps)).await;
    }
    async fn set_cursor_location(
        &self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        self.send(Request::Cursor(self.ic, (x, y, w, h))).await;
    }
    async fn set_surrounding_text(
        &self,
        text: OwnedVariant,
        cursor: u32,
        anchor: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        if let Some(text) = wire::read_text(&text) {
            let (text, cursor, anchor) = wire::surrounding_window(&text, cursor, anchor);
            self.send(Request::Surrounding(self.ic, text, cursor, anchor))
                .await;
        }
    }
    async fn candidate_clicked(
        &self,
        index: u32,
        button: u32,
        state: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        self.send(Request::Candidate(self.ic, index, button, state))
            .await;
    }
    async fn page_up(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        self.send(Request::Page(self.ic, true)).await;
    }
    async fn page_down(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        if authorize(&self.owner, &header).is_err() {
            return;
        }
        self.send(Request::Page(self.ic, false)).await;
    }
    async fn cursor_up(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        let _ = authorize(&self.owner, &header);
    }
    async fn cursor_down(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        let _ = authorize(&self.owner, &header);
    }
    async fn property_activate(
        &self,
        _name: &str,
        _state: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        let _ = authorize(&self.owner, &header);
    }
    async fn property_show(&self, _name: &str, #[zbus(header)] header: zbus::message::Header<'_>) {
        let _ = authorize(&self.owner, &header);
    }
    async fn property_hide(&self, _name: &str, #[zbus(header)] header: zbus::message::Header<'_>) {
        let _ = authorize(&self.owner, &header);
    }
    #[zbus(property)]
    fn focus_id(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn active_surrounding_text(&self) -> bool {
        true
    }
    #[zbus(property)]
    async fn set_content_type(
        &mut self,
        value: (u32, u32),
        #[zbus(header)] header: Option<zbus::message::Header<'_>>,
    ) {
        if header
            .as_ref()
            .is_none_or(|header| authorize(&self.owner, header).is_err())
        {
            return;
        }
        if matches!(value.0, 8 | 9) || value.1 & ((1 << 11) | (1 << 12)) != 0 {
            self.live.store(0, Ordering::Release);
        }
        self.send(Request::Content(self.ic, value.0, value.1)).await;
    }
}

fn authorize(
    owner: &std::sync::OnceLock<String>,
    header: &zbus::message::Header<'_>,
) -> zbus::fdo::Result<()> {
    if owner.get().is_some_and(|owner| {
        header.sender().is_some_and(|sender| {
            sender.as_str() == owner || sender.as_str() == "org.freedesktop.DBus"
        })
    }) {
        Ok(())
    } else {
        tracing::debug!(sender = ?header.sender(), expected = ?owner.get(), "refusing a call outside the session IBus daemon");
        Err(zbus::fdo::Error::AccessDenied(
            "only the session IBus daemon may call an engine".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn commit_clears_cached_preedit_and_lookup_before_text() {
        use futures_util::StreamExt as _;
        let (server_end, client_end) = std::os::unix::net::UnixStream::pair().unwrap();
        server_end.set_nonblocking(true).unwrap();
        client_end.set_nonblocking(true).unwrap();
        let (server, client) = tokio::join!(
            zbus::connection::Builder::unix_stream(server_end)
                .server(zbus::Guid::generate())
                .p2p()
                .build(),
            zbus::connection::Builder::unix_stream(client_end)
                .p2p()
                .build(),
        );
        let server = server.unwrap();
        let client = client.unwrap();
        let mut incoming = zbus::MessageStream::from(&client);
        let path = OwnedObjectPath::try_from("/org/freedesktop/IBus/Engine/test").unwrap();
        emit(&server, &path, Signal::Commit("HELLO".into()))
            .await
            .unwrap();
        for expected in ["UpdatePreeditText", "UpdateLookupTable", "CommitText"] {
            let message = tokio::time::timeout(Duration::from_secs(1), incoming.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(message.header().member().unwrap().as_str(), expected);
            if expected == "UpdatePreeditText" {
                let (text, cursor, visible, mode): (OwnedVariant, u32, bool, u32) =
                    message.body().deserialize().unwrap();
                assert_eq!(wire::read_text(&text).as_deref(), Some(""));
                assert_eq!(cursor, 0);
                assert!(!visible);
                assert_eq!(mode, 0);
            } else if expected == "UpdateLookupTable" {
                let (_, visible): (OwnedVariant, bool) = message.body().deserialize().unwrap();
                assert!(!visible);
            } else {
                let (text,): (OwnedVariant,) = message.body().deserialize().unwrap();
                assert_eq!(wire::read_text(&text).as_deref(), Some("HELLO"));
            }
        }
    }

    #[test]
    fn authorization_accepts_reserved_ibus_bus_sender_and_rejects_other_clients() {
        let owner = std::sync::OnceLock::new();
        owner.set(":1.0".to_owned()).unwrap();
        for sender in [":1.0", "org.freedesktop.DBus"] {
            let message =
                zbus::Message::method_call("/org/freedesktop/IBus/Factory", "CreateEngine")
                    .sender(sender)
                    .build(&(ENGINE,))
                    .unwrap();
            assert!(authorize(&owner, &message.header()).is_ok());
        }
        let message = zbus::Message::method_call("/org/freedesktop/IBus/Factory", "CreateEngine")
            .sender(":1.9")
            .build(&(ENGINE,))
            .unwrap();
        assert!(authorize(&owner, &message.header()).is_err());
    }
}
