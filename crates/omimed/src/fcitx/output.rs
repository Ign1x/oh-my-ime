//! Bounded, nonblocking output admission. Only adjacent preedits may replace each other.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};

use omime_proto::engine::ContextId;
#[cfg(test)]
use tokio::sync::oneshot;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use zbus::message::{Builder, Header, Message};

use super::{IcId, KeyAnswer, Outgoing, Signal};

pub(super) const MAX_ITEMS: usize = 512;
pub(super) const MAX_BYTES: usize = 256 * 1024;
pub(super) const MAX_IC_ITEMS: usize = 128;
pub(super) const MAX_REPLIES: usize = 256;
pub(super) const MAX_IC_REPLIES: usize = 64;
pub(super) const MAX_IC_BYTES: usize = 64 * 1024;

/// Metadata for every accepted key stays owned until its actual method return
/// has been written. In particular, settling a key cannot release admission.
#[derive(Debug)]
pub enum KeyReply {
    #[cfg(test)]
    Local(oneshot::Sender<KeyAnswer>),
    Wire(Box<WireReply>),
}

pub struct WireReply {
    builder: Builder<'static>,
    batch: bool,
    expected: bool,
    ic: IcId,
    queue: Weak<Shared>,
    _bus_slot: OwnedSemaphorePermit,
    _ic_slot: OwnedSemaphorePermit,
}

impl std::fmt::Debug for WireReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireReply")
            .field("ic", &self.ic)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
impl From<oneshot::Sender<KeyAnswer>> for KeyReply {
    fn from(reply: oneshot::Sender<KeyAnswer>) -> Self {
        Self::Local(reply)
    }
}

impl KeyReply {
    pub(super) fn is_wire(&self) -> bool {
        matches!(self, Self::Wire(_))
    }

    /// Immediate actor completion still goes through the sole writer, using the
    /// reserved compact reply slot. This path never creates another writer task.
    pub(super) fn send(self, answer: KeyAnswer) -> Result<(), KeyAnswer> {
        match self {
            #[cfg(test)]
            Self::Local(reply) => reply.send(answer),
            Self::Wire(reply) => {
                if !reply.expected {
                    return Ok(());
                }
                if let Some(shared) = reply.queue.upgrade() {
                    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                    if !state.closed {
                        // Only non-text settlements use this reserved path.
                        debug_assert!(answer.batch.is_empty());
                        state.entries.push_back(Entry {
                            ic: reply.ic,
                            ctx: None,
                            retired: true,
                            item: Outgoing::Reply {
                                reply: Self::Wire(reply),
                                answer,
                            },
                        });
                        shared.ready.notify_one();
                    }
                }
                Ok(())
            }
        }
    }

    pub(super) async fn write(
        self,
        conn: &zbus::Connection,
        answer: KeyAnswer,
    ) -> zbus::Result<()> {
        match self {
            #[cfg(test)]
            Self::Local(reply) => {
                let _ = reply.send(answer);
                Ok(())
            }
            Self::Wire(reply) => {
                if !reply.expected {
                    return Ok(());
                }
                let builder = if let Some(sender) = conn.unique_name() {
                    reply.builder.sender(sender.clone())
                } else {
                    reply.builder
                };
                let message = if reply.batch {
                    let events: Vec<_> = answer
                        .batch
                        .into_iter()
                        .filter_map(super::service::batched)
                        .collect();
                    builder.build(&(events, answer.handled))?
                } else {
                    builder.build(&answer.handled)?
                };
                // Both permits remain alive through serialization and the socket
                // write, including a suspended or partially completed send.
                conn.send(&message).await
            }
        }
    }
}

/// Cloned by ICs; unlike OutputQueue this handle does not close the worker on drop.
#[derive(Clone)]
pub(super) struct ReplyAdmission {
    queue: Weak<Shared>,
    slots: Arc<Semaphore>,
}

impl ReplyAdmission {
    pub(super) async fn reserve(
        &self,
        ic: IcId,
        ic_slots: Arc<Semaphore>,
        header: &Header<'_>,
        batch: bool,
    ) -> Option<KeyReply> {
        // This is polled by the non-spawning object dispatcher: at most one
        // admission waiter per connection, backed by zbus's bounded input queue.
        let bus_slot = self.slots.clone().acquire_owned().await.ok()?;
        let ic_slot = ic_slots.acquire_owned().await.ok()?;
        Some(KeyReply::Wire(Box::new(WireReply {
            builder: Message::method_return(header),
            batch,
            expected: !header
                .primary()
                .flags()
                .contains(zbus::message::Flags::NO_REPLY_EXPECTED),
            ic,
            queue: self.queue.clone(),
            _bus_slot: bus_slot,
            _ic_slot: ic_slot,
        })))
    }
}

pub(super) fn signal_bytes(signal: &Signal) -> usize {
    match signal {
        Signal::Commit(text) => text.capacity(),
        Signal::Preedit(segments, _) => {
            segments.capacity() * std::mem::size_of::<(String, i32)>()
                + segments
                    .iter()
                    .map(|(text, _)| text.capacity())
                    .sum::<usize>()
        }
        Signal::Forward(..) | Signal::Delete(..) => 0,
    }
}

#[derive(Debug)]
pub(super) struct Entry {
    pub ic: IcId,
    pub ctx: Option<ContextId>,
    pub item: Outgoing,
    retired: bool,
}

impl Entry {
    fn cost(&self) -> (usize, usize) {
        let (items, bytes) = match &self.item {
            Outgoing::Signal { dest, path, signal } => (
                1,
                dest.capacity() + path.as_str().len() + signal_bytes(signal),
            ),
            Outgoing::Reply { reply, answer } if reply.is_wire() && answer.batch.is_empty() => {
                return (0, 0);
            }
            Outgoing::Reply { answer, .. } => (
                1 + answer.batch.len(),
                answer.batch.capacity() * std::mem::size_of::<super::Batched>()
                    + answer
                        .batch
                        .iter()
                        .map(|s| signal_bytes(&s.0))
                        .sum::<usize>(),
            ),
            // Removal retains the live object's quota until processed: at most
            // MAX_CONTEXTS exist, including these cleanup entries.
            Outgoing::Remove(_) => return (0, 0),
        };
        (items, bytes + std::mem::size_of::<Self>())
    }

    fn replaces(&self, old: &Self) -> bool {
        self.ic == old.ic
            && self.ctx == old.ctx
            && matches!(
                (&self.item, &old.item),
                (Outgoing::Signal { dest, path, signal: Signal::Preedit(..) },
                 Outgoing::Signal { dest: old_dest, path: old_path, signal: Signal::Preedit(..) })
                    if dest == old_dest && path == old_path
            )
    }

    fn settle(self) {
        if let Outgoing::Reply { reply, .. } = self.item {
            // A prefix may already be on the wire. Do not replay its raw key.
            let _ = reply.send(KeyAnswer {
                handled: true,
                batch: Vec::new(),
            });
        }
    }
}

#[derive(Default)]
struct State {
    entries: VecDeque<Entry>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    ready: Notify,
    replies: Arc<Semaphore>,
}

/// One sender owned by the frontend. Dropping either endpoint settles queued keys.
pub struct OutputQueue(Arc<Shared>);
pub(super) struct Receiver(Arc<Shared>);

pub(super) fn channel() -> (OutputQueue, Receiver) {
    let shared = Arc::new(Shared {
        state: Mutex::default(),
        ready: Notify::new(),
        replies: Arc::new(Semaphore::new(MAX_REPLIES)),
    });
    (OutputQueue(shared.clone()), Receiver(shared))
}

impl OutputQueue {
    pub(super) fn reply_admission(&self) -> ReplyAdmission {
        ReplyAdmission {
            queue: Arc::downgrade(&self.0),
            slots: self.0.replies.clone(),
        }
    }
    pub(super) fn send(&self, ic: IcId, ctx: Option<ContextId>, item: Outgoing) -> bool {
        let entry = Entry {
            ic,
            ctx,
            item,
            retired: false,
        };
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            drop(state);
            entry.settle();
            return false;
        }
        let replace = state.entries.back().is_some_and(|old| entry.replaces(old));
        let (mut items, mut bytes) = entry.cost();
        let (mut ic_items, mut ic_bytes) = (items, bytes);
        let keep = state.entries.len() - usize::from(replace);
        for old in state.entries.iter().take(keep) {
            let cost = old.cost();
            items += cost.0;
            bytes += cost.1;
            if old.ic == ic {
                ic_items += cost.0;
                ic_bytes += cost.1;
            }
        }
        if !matches!(entry.item, Outgoing::Remove(_))
            && (items > MAX_ITEMS
                || bytes > MAX_BYTES
                || ic_items > MAX_IC_ITEMS
                || ic_bytes > MAX_IC_BYTES)
        {
            drop(state);
            entry.settle();
            return false;
        }
        if replace {
            state.entries.pop_back();
        }
        state.entries.push_back(entry);
        self.0.ready.notify_one();
        true
    }

    pub(super) fn discard(&self, ic: IcId) {
        discard(&self.0, Some(ic));
    }
}

fn discard(shared: &Shared, ic: Option<IcId>) -> Vec<(IcId, Option<ContextId>)> {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    let mut failed = Vec::new();
    let entries = std::mem::take(&mut state.entries);
    for mut entry in entries {
        if ic.is_none_or(|ic| entry.ic == ic) && !matches!(entry.item, Outgoing::Remove(_)) {
            if !entry.retired && !failed.contains(&(entry.ic, entry.ctx)) {
                failed.push((entry.ic, entry.ctx));
            }
            if let Outgoing::Reply { reply, answer } = &mut entry.item
                && reply.is_wire()
            {
                // Accepted keys already own reply slots. Keep compact settlements
                // in FIFO position until the one writer physically sends them.
                entry.retired = true;
                *answer = KeyAnswer {
                    handled: true,
                    batch: Vec::new(),
                };
                state.entries.push_back(entry);
            } else {
                entry.settle();
            }
        } else {
            state.entries.push_back(entry);
        }
    }
    failed
}

impl Receiver {
    pub(super) fn discard_all(&self) -> Vec<(IcId, Option<ContextId>)> {
        discard(&self.0, None)
    }

    #[cfg(test)]
    pub(super) fn try_recv(&mut self) -> Option<Entry> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .pop_front()
    }

    pub(super) async fn recv(&mut self) -> Option<Entry> {
        loop {
            let ready = self.0.ready.notified();
            {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = state.entries.pop_front() {
                    return Some(entry);
                }
                if state.closed {
                    return None;
                }
            }
            ready.await;
        }
    }
}

fn close(shared: &Shared) {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    state.closed = true;
    shared.replies.close();
    for entry in state.entries.drain(..) {
        // Closing the transport cannot send remote settlements. Drop their
        // permits; local tests still observe deterministic key completion.
        if !matches!(&entry.item, Outgoing::Reply { reply, .. } if reply.is_wire()) {
            entry.settle();
        }
    }
    shared.ready.notify_one();
}

impl Drop for OutputQueue {
    fn drop(&mut self) {
        close(&self.0);
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        close(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;
    use zbus::OwnedObjectPath;

    fn signal(ic: IcId, signal: Signal) -> Outgoing {
        Outgoing::Signal {
            dest: format!(":1.{ic}"),
            path: OwnedObjectPath::try_from(format!("/ic/{ic}")).unwrap(),
            signal,
        }
    }

    fn preedit(text: &str) -> Signal {
        Signal::Preedit(vec![(text.into(), 0)], 0)
    }

    #[test]
    fn replacement_never_crosses_context_destination_or_ordering_barriers() {
        let (queue, mut rx) = channel();
        assert!(queue.send(1, Some(1), signal(1, preedit("old"))));
        assert!(queue.send(1, Some(1), signal(1, preedit("new"))));
        for barrier in [
            Signal::Commit("commit".into()),
            Signal::Delete(-1, 1),
            Signal::Forward(65, 0, false),
        ] {
            assert!(queue.send(1, Some(1), signal(1, barrier)));
            assert!(queue.send(1, Some(1), signal(1, preedit("after"))));
        }
        let (reply, mut answer) = oneshot::channel();
        assert!(queue.send(
            1,
            Some(1),
            Outgoing::Reply {
                reply: reply.into(),
                answer: KeyAnswer::default()
            }
        ));
        assert!(queue.send(1, Some(1), signal(1, preedit("after reply"))));
        assert!(queue.send(1, Some(2), signal(1, preedit("new context"))));
        assert!(queue.send(2, Some(2), signal(2, preedit("other IC"))));
        let mut items = Vec::new();
        while let Some(entry) = rx.try_recv() {
            items.push(entry.item);
        }
        assert_eq!(items.len(), 11);
        assert!(
            matches!(&items[0], Outgoing::Signal { signal: Signal::Preedit(segments, _), .. } if segments[0].0 == "new")
        );
        assert!(
            matches!(&items[1], Outgoing::Signal { signal: Signal::Commit(text), .. } if text == "commit")
        );
        assert!(matches!(
            &items[3],
            Outgoing::Signal {
                signal: Signal::Delete(-1, 1),
                ..
            }
        ));
        assert!(matches!(
            &items[5],
            Outgoing::Signal {
                signal: Signal::Forward(..),
                ..
            }
        ));
        assert!(matches!(&items[7], Outgoing::Reply { .. }));
        assert!(
            answer.try_recv().is_err(),
            "a reply remains behind its text"
        );
        if let Outgoing::Reply { reply, answer } = items.remove(7) {
            reply.send(answer).unwrap();
        }
        assert!(!answer.try_recv().unwrap().handled);
    }

    #[test]
    fn count_and_allocation_budgets_reserve_room_for_other_contexts() {
        let (queue, mut rx) = channel();
        for _ in 0..MAX_IC_ITEMS {
            assert!(queue.send(1, Some(1), signal(1, Signal::Commit(String::new()))));
        }
        assert!(!queue.send(1, Some(1), signal(1, Signal::Delete(0, 1))));
        assert!(queue.send(2, Some(2), signal(2, Signal::Commit("healthy".into()))));
        queue.discard(1);
        assert!(matches!(rx.try_recv(), Some(Entry { ic: 2, .. })));

        let text = String::with_capacity(MAX_IC_BYTES);
        assert!(
            !queue.send(1, Some(1), signal(1, Signal::Commit(text))),
            "capacity is charged even for an empty string"
        );
        for ic in 1..=4 {
            for _ in 0..MAX_IC_ITEMS {
                assert!(queue.send(ic, Some(ic), signal(ic, Signal::Delete(0, 1))));
            }
        }
        assert!(!queue.send(5, Some(5), signal(5, Signal::Delete(0, 1))));
        queue.discard(1);
        assert!(queue.send(5, Some(5), signal(5, Signal::Delete(0, 1))));
    }

    #[test]
    fn overflow_teardown_and_dropped_transport_settle_replies_and_keep_removal() {
        let (queue, mut rx) = channel();
        let (reply, mut answer) = oneshot::channel();
        assert!(queue.send(
            1,
            Some(1),
            Outgoing::Reply {
                reply: reply.into(),
                answer: KeyAnswer::default()
            }
        ));
        assert!(queue.send(
            1,
            None,
            Outgoing::Remove(OwnedObjectPath::try_from("/ic/1").unwrap())
        ));
        queue.discard(1);
        assert!(answer.try_recv().unwrap().handled);
        assert!(matches!(
            rx.try_recv(),
            Some(Entry {
                item: Outgoing::Remove(_),
                ..
            })
        ));
        for _ in 0..MAX_IC_ITEMS {
            assert!(queue.send(1, Some(1), signal(1, Signal::Delete(0, 1))));
        }
        let (reply, mut answer) = oneshot::channel();
        assert!(!queue.send(
            1,
            Some(1),
            Outgoing::Reply {
                reply: reply.into(),
                answer: KeyAnswer::default()
            }
        ));
        assert!(answer.try_recv().unwrap().handled);
        queue.discard(1);
        let (reply, mut answer) = oneshot::channel();
        assert!(queue.send(
            2,
            Some(2),
            Outgoing::Reply {
                reply: reply.into(),
                answer: KeyAnswer::default()
            }
        ));
        drop(rx);
        assert!(answer.try_recv().unwrap().handled);
    }

    #[test]
    fn shared_byte_budget_and_replacement_capacity_are_bounded() {
        let (queue, _rx) = channel();
        for _ in 0..8192 {
            assert!(queue.send(1, Some(1), signal(1, preedit(&"x".repeat(2048)))));
        }
        assert_eq!(queue.0.state.lock().unwrap().entries.len(), 1);
        queue.discard(1);
        for ic in 1..=8 {
            assert!(queue.send(
                ic,
                Some(ic),
                signal(ic, Signal::Commit("x".repeat(30 * 1024)))
            ));
        }
        assert!(!queue.send(9, Some(9), signal(9, Signal::Commit("x".repeat(30 * 1024)))));
        queue.discard(1);
        assert!(queue.send(9, Some(9), signal(9, Signal::Commit("x".repeat(30 * 1024)))));
    }
}
