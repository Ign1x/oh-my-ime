//! An IC owns its compositor query; admission never creates permit-waiting tasks.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{Semaphore, mpsc};
use tokio::task::AbortHandle;

use super::{IcId, Request};
use crate::placement::Rect;

pub(super) const MAX_QUERIES: usize = 8;
const MAX_AGE: Duration = Duration::from_secs(1);

#[derive(Default)]
struct State {
    task: Option<AbortHandle>,
    asked: Option<Instant>,
    refresh: bool,
    generation: u64,
}

pub(super) struct WindowQuery {
    state: Arc<Mutex<State>>,
    budget: Arc<Semaphore>,
    ic: IcId,
    requests: mpsc::UnboundedSender<Request>,
}

impl WindowQuery {
    pub(super) fn new(
        ic: IcId,
        requests: mpsc::UnboundedSender<Request>,
        budget: Arc<Semaphore>,
    ) -> Self {
        Self {
            state: Arc::default(),
            budget,
            ic,
            requests,
        }
    }

    pub(super) fn ask(&self, force: bool) {
        self.ask_with(force, crate::compositor_ipc::focused_window);
    }

    fn ask_with<F, Q>(&self, force: bool, query: F)
    where
        F: Fn() -> Q + Send + 'static,
        Q: Future<Output = Option<Rect>> + Send,
    {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !force && state.asked.is_some_and(|at| at.elapsed() < MAX_AGE) {
            return;
        }
        if state.task.is_some() {
            state.refresh = true;
            return;
        }
        let Ok(permit) = self.budget.clone().try_acquire_owned() else {
            // No task or waiter is created. The next focus/cursor event retries.
            return;
        };
        state.asked = Some(Instant::now());
        let generation = state.generation;
        let (ic, requests, shared) = (self.ic, self.requests.clone(), self.state.clone());
        state.task = Some(
            tokio::spawn(async move {
                let _permit = permit;
                loop {
                    let window = query().await;
                    let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                    if state.generation != generation {
                        return;
                    }
                    let _ = requests.send(Request::Window { ic, window });
                    if !std::mem::take(&mut state.refresh) {
                        state.task = None;
                        return;
                    }
                    state.asked = Some(Instant::now());
                }
            })
            .abort_handle(),
        );
    }

    pub(super) fn cancel(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.generation += 1;
        state.refresh = false;
        state.asked = None;
        if let Some(task) = state.task.take() {
            task.abort();
        }
    }
}

impl Drop for WindowQuery {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Notify;

    #[tokio::test]
    async fn duplicate_queries_coalesce_and_cancellation_releases_shared_capacity() {
        let budget = Arc::new(Semaphore::new(1));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let first = WindowQuery::new(1, tx.clone(), budget.clone());
        let second = WindowQuery::new(2, tx, budget.clone());
        let gate = Arc::new(Notify::new());
        let query = {
            let gate = gate.clone();
            move || {
                let gate = gate.clone();
                async move {
                    gate.notified().await;
                    Some((1, 2, 3, 4))
                }
            }
        };
        first.ask_with(true, query.clone());
        for _ in 0..256 {
            first.ask_with(true, query.clone());
        }
        second.ask_with(true, query.clone());
        assert_eq!(budget.available_permits(), 0);
        assert!(second.state.lock().unwrap().task.is_none());
        gate.notify_one();
        assert!(matches!(
            rx.recv().await,
            Some(Request::Window { ic: 1, .. })
        ));
        gate.notify_one();
        assert!(matches!(
            rx.recv().await,
            Some(Request::Window { ic: 1, .. })
        ));
        tokio::task::yield_now().await;
        assert_eq!(budget.available_permits(), 1);
        assert!(rx.try_recv().is_err());
        first.ask_with(true, query.clone());
        drop(first);
        tokio::task::yield_now().await;
        assert_eq!(budget.available_permits(), 1);
        second.ask_with(true, || async { Some((5, 6, 7, 8)) });
        assert!(matches!(
            rx.recv().await,
            Some(Request::Window {
                ic: 2,
                window: Some((5, 6, 7, 8))
            })
        ));
    }

    #[tokio::test]
    async fn cursor_refresh_and_new_focus_recover_after_a_stalled_query() {
        let budget = Arc::new(Semaphore::new(1));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let query = WindowQuery::new(1, tx, budget.clone());
        query.ask_with(false, std::future::pending);
        for _ in 0..256 {
            query.ask_with(false, std::future::pending);
        }
        assert!(!query.state.lock().unwrap().refresh);
        query.cancel();
        tokio::task::yield_now().await;
        query.ask_with(true, || async { Some((2, 3, 4, 5)) });
        assert!(matches!(
            rx.recv().await,
            Some(Request::Window {
                window: Some((2, 3, 4, 5)),
                ..
            })
        ));
        query.state.lock().unwrap().asked = Some(Instant::now() - MAX_AGE);
        query.ask_with(false, || async { Some((6, 7, 8, 9)) });
        assert!(matches!(
            rx.recv().await,
            Some(Request::Window {
                window: Some((6, 7, 8, 9)),
                ..
            })
        ));
    }
}
