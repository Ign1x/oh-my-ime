//! Exercise actual zbus method dispatch and serialization against a gated writer.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use tokio::sync::{Notify, Semaphore, mpsc};
use zbus::connection::socket::{Channel, Socket, Split, WriteHalf};
use zbus::export::async_trait::async_trait;
use zbus::message::{Message, Type};

use super::*;
use crate::fcitx::{Batched, KeyAnswer, Outgoing, Request, Signal, output};

#[derive(Debug, Default)]
struct Gate {
    blocked: std::sync::atomic::AtomicBool,
    entered: Notify,
    released: Notify,
}

#[derive(Debug)]
struct Gated<S> {
    inner: S,
    gate: Arc<Gate>,
}

impl<S: Socket> Socket for Gated<S> {
    type ReadHalf = S::ReadHalf;
    type WriteHalf = Gated<S::WriteHalf>;
    fn split(self) -> Split<Self::ReadHalf, Self::WriteHalf> {
        let (read, write) = self.inner.split().take();
        Split::new(
            read,
            Gated {
                inner: write,
                gate: self.gate,
            },
        )
    }
}

#[async_trait]
impl<W: WriteHalf> WriteHalf for Gated<W> {
    async fn send_message(&mut self, message: &Message) -> zbus::Result<()> {
        loop {
            let released = self.gate.released.notified();
            if !self.gate.blocked.load(Ordering::SeqCst) {
                break;
            }
            self.gate.entered.notify_one();
            released.await;
        }
        self.inner.send_message(message).await
    }
    async fn close(&mut self) -> std::io::Result<()> {
        self.inner.close().await
    }
}

#[tokio::test]
async fn actual_key_reply_writes_keep_admission_until_transport_completion() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for batch in [true, false] {
                stalled_replies(batch).await;
            }
        })
        .await;
}

#[tokio::test]
async fn destroyed_ic_key_returns_false_on_the_actual_wire() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (client_socket, server_socket) = Channel::pair();
            let guid = zbus::Guid::generate();
            let server =
                zbus::connection::Builder::authenticated_socket(server_socket, guid.clone())
                    .p2p()
                    .build()
                    .await
                    .unwrap();
            let client = zbus::connection::Builder::authenticated_socket(client_socket, guid)
                .p2p()
                .build()
                .await
                .unwrap();
            let mut messages = zbus::MessageStream::from(&client);
            let (requests, _) = mpsc::unbounded_channel();
            let (queue, outgoing) = output::channel();
            let admission = queue.reply_admission();
            let slots = Arc::new(Semaphore::new(output::MAX_IC_REPLIES));
            let mut frontend = crate::fcitx::FcitxFrontend::new(
                [Some(queue), None],
                crate::frontend::ContextIds::default(),
            );
            frontend.handle(Request::Created {
                ic: 1,
                bus: crate::fcitx::Bus::Main,
                owner: ":1.9".into(),
                path: zbus::OwnedObjectPath::try_from("/ic/1").unwrap(),
                args: Vec::new(),
            });
            frontend.handle(Request::Destroy(1));
            let writer = tokio::task::spawn_local(send_outgoing(server, outgoing, requests));
            for batch in [false, true] {
                let call = Message::method_call(
                    "/ic/1",
                    if batch {
                        "ProcessKeyEventBatch"
                    } else {
                        "ProcessKeyEvent"
                    },
                )
                .sender(":1.9")
                .build(&())
                .unwrap();
                let reply = admission
                    .reserve(1, slots.clone(), &call.header(), batch)
                    .await
                    .unwrap();
                frontend.handle(Request::Key {
                    ic: 1,
                    key: (65, 38, 0, false, 0),
                    batch,
                    reply,
                });
                let response = tokio::time::timeout(Duration::from_secs(1), messages.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    response.header().reply_serial(),
                    Some(call.header().primary().serial_num())
                );
                if batch {
                    let (events, handled): (Vec<(u32, zbus::OwnedVariant)>, bool) =
                        response.body().deserialize().unwrap();
                    assert!(events.is_empty());
                    assert!(!handled);
                } else {
                    assert!(!response.body().deserialize::<bool>().unwrap());
                }
                assert_eq!(slots.available_permits(), output::MAX_IC_REPLIES);
            }
            writer.abort();
        })
        .await;
}

async fn stalled_replies(batch: bool) {
    let (client_socket, server_socket) = Channel::pair();
    let gate = Arc::new(Gate::default());
    let guid = zbus::Guid::generate();
    let (requests, mut input) = mpsc::unbounded_channel();
    let (queue, outgoing) = output::channel();
    let counts = Arc::new(Mutex::new(ContextCounts::default()));
    let object = InputContextObject {
        _slot: ContextSlot::reserve(&counts, ":1.9").unwrap(),
        ic: 1,
        owner: ":1.9".into(),
        requests: requests.clone(),
        wayland: false,
        focused: std::sync::atomic::AtomicBool::new(false),
        window_query: crate::fcitx::window::WindowQuery::new(
            1,
            requests.clone(),
            Arc::new(Semaphore::new(8)),
        ),
    };
    let server = zbus::connection::Builder::authenticated_socket(
        Gated {
            inner: server_socket,
            gate: gate.clone(),
        },
        guid.clone(),
    )
    .p2p()
    .serve_at(
        "/ic/1",
        InputContextService {
            inner: object,
            admission: queue.reply_admission(),
            reply_slots: Arc::new(Semaphore::new(output::MAX_IC_REPLIES)),
        },
    )
    .build()
    .await
    .unwrap();
    let client = zbus::connection::Builder::authenticated_socket(client_socket, guid)
        .p2p()
        .build()
        .await
        .unwrap();
    let mut messages = zbus::MessageStream::from(&client);
    let writer = tokio::task::spawn_local(send_outgoing(server.clone(), outgoing, requests));
    let processed = Arc::new(AtomicUsize::new(0));
    let count = processed.clone();
    let actor = tokio::task::spawn_local(async move {
        while let Some(request) = input.recv().await {
            if let Request::Key { reply, .. } = request {
                let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                let marker = |text: &str| Outgoing::Signal {
                    dest: ":1.9".into(),
                    path: zbus::OwnedObjectPath::try_from("/ic/1").unwrap(),
                    signal: Signal::Commit(text.into()),
                };
                if n == 81 {
                    assert!(queue.send(1, Some(1), marker("before")));
                }
                let answer = KeyAnswer {
                    handled: true,
                    batch: if batch {
                        vec![Batched(Signal::Commit("x".repeat(16 * 1024)))]
                    } else {
                        Vec::new()
                    },
                };
                queue.send(1, Some(1), Outgoing::Reply { reply, answer });
                if n == 81 {
                    assert!(queue.send(1, Some(1), marker("after")));
                }
            }
        }
    });
    gate.blocked.store(true, Ordering::SeqCst);
    let member = if batch {
        "ProcessKeyEventBatch"
    } else {
        "ProcessKeyEvent"
    };
    let mut serials = Vec::new();
    for _ in 0..80 {
        let call = Message::method_call("/ic/1", member)
            .interface("org.fcitx.Fcitx.InputContext1")
            .sender(":1.9")
            .build(&(65u32, 38u32, 0u32, false, 0u32))
            .unwrap();
        serials.push(call.header().primary().serial_num());
        client.send(&call).await.unwrap();
    }
    gate.entered.notified().await;
    // The writer is demonstrably stalled. Allow the dispatch/actor queues to run
    // and the transport deadline to settle keys, without an allocator/RSS oracle.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        processed.load(Ordering::SeqCst) <= output::MAX_IC_REPLIES,
        "reply admission escaped into generated zbus writers: {} keys",
        processed.load(Ordering::SeqCst)
    );
    eprintln!(
        "actual zbus stalled {} replies: 80 sent, {} admitted (limit {})",
        if batch { "batch" } else { "ordinary" },
        processed.load(Ordering::SeqCst),
        output::MAX_IC_REPLIES
    );
    gate.blocked.store(false, Ordering::SeqCst);
    gate.released.notify_waiters();
    let mut compact = 0;
    for serial in serials {
        let message = tokio::time::timeout(Duration::from_secs(2), messages.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(message.message_type(), Type::MethodReturn);
        assert_eq!(
            message.header().reply_serial(),
            Some(serial),
            "reply FIFO survived the stall"
        );
        if batch {
            let (events, handled): (Vec<(u32, zbus::OwnedVariant)>, bool) =
                message.body().deserialize().unwrap();
            assert!(handled);
            if events.is_empty() {
                compact += 1;
            }
        } else {
            let _: bool = message.body().deserialize().unwrap();
        }
    }
    if batch {
        assert!(
            compact > 0,
            "overflow/deadline must compact retained batch text"
        );
        eprintln!(
            "batch recovery: {compact} compact settlements, all 80 reply serials retained FIFO"
        );
    }
    assert_eq!(processed.load(Ordering::SeqCst), 80);
    let call = Message::method_call("/ic/1", member)
        .interface("org.fcitx.Fcitx.InputContext1")
        .sender(":1.9")
        .build(&(65u32, 38u32, 0u32, false, 0u32))
        .unwrap();
    client.send(&call).await.unwrap();
    for expected in [Type::Signal, Type::MethodReturn, Type::Signal] {
        let message = tokio::time::timeout(Duration::from_secs(2), messages.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            message.message_type(),
            expected,
            "signals must not overtake the actual method return"
        );
    }
    // A no-reply key keeps the original protocol behavior; it must not produce
    // an unsolicited return before the following ordinary method's reply.
    let no_reply = Message::method_call("/ic/1", member)
        .interface("org.fcitx.Fcitx.InputContext1")
        .sender(":1.9")
        .with_flags(zbus::message::Flags::NO_REPLY_EXPECTED)
        .build(&(65u32, 38u32, 0u32, false, 0u32))
        .unwrap();
    client.send(&no_reply).await.unwrap();
    // Non-key methods retain their generated return signature and keep working.
    let call = Message::method_call("/ic/1", "IsVirtualKeyboardVisible")
        .interface("org.fcitx.Fcitx.InputContext1")
        .sender(":1.9")
        .build(&())
        .unwrap();
    client.send(&call).await.unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), messages.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        response.header().reply_serial(),
        Some(call.header().primary().serial_num())
    );
    assert!(!response.body().deserialize::<bool>().unwrap());
    actor.abort();
    writer.abort();
}
