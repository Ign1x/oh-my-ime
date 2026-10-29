//! The daemon's event loop.
//!
//! Everything runs on one thread: the wl-proxy state is not `Send`, and keeping a
//! single owner for all input state makes ordering trivial to reason about.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::rc::Rc;

use anyhow::Context as _;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::signal::unix::{SignalKind, signal};
use tracing::info;

use crate::wayland::Frontend;

/// Options for [`run`].
#[derive(Debug, Default)]
pub struct Options {
    /// Compositor to connect to instead of `$WAYLAND_DISPLAY`.
    pub display: Option<String>,
}

/// The wl-proxy poll fd, shared with the state that owns it.
struct PollFd(Rc<OwnedFd>);

impl AsRawFd for PollFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

#[allow(unsafe_code)]
fn watch(fd: PollFd) -> std::io::Result<AsyncFd<PollFd>> {
    // SAFETY: `PollFd` keeps its own reference to the `OwnedFd`, so the descriptor stays
    // open for as long as the `AsyncFd` exists, and `as_raw_fd` always returns it.
    unsafe { AsyncFd::register_with_interest(fd, Interest::READABLE) }.map_err(Into::into)
}

/// Runs the daemon until it is asked to stop.
pub fn run(options: Options) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to start the async runtime")?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, run_async(options))
}

async fn run_async(options: Options) -> anyhow::Result<()> {
    let mut frontend = Frontend::connect(options.display.as_deref())?;
    let poll_fd = watch(PollFd(frontend.state().poll_fd().clone()))
        .context("failed to watch the Wayland connection")?;
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    info!("connected to the compositor");

    let result = loop {
        if let Err(e) = frontend.flush() {
            break Err(e);
        }
        let deadline = frontend.next_deadline();
        tokio::select! {
            ready = poll_fd.readable() => {
                let mut guard = match ready {
                    Ok(guard) => guard,
                    Err(e) => break Err(e.into()),
                };
                guard.clear_ready();
                if let Err(e) = frontend.dispatch() {
                    break Err(e);
                }
            }
            () = sleep_until(deadline) => {
                if let Err(e) = frontend.on_timer(std::time::Instant::now()) {
                    break Err(e);
                }
            }
            _ = sigterm.recv() => break Ok(()),
            _ = sigint.recv() => break Ok(()),
        }
    };

    info!("shutting down");
    frontend.shutdown();
    // Best effort: the connection may already be gone.
    let _ = frontend.flush();
    result
}

async fn sleep_until(deadline: Option<std::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}
