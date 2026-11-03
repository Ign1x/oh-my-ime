//! The switcher: `omime-panel`, a client of the compositor that shows the input methods
//! for a moment when the user switches with the hotkeys or `omime`. It learns of the
//! switches from the control API's `Switched`. omimed starts it with `[panel] enabled`,
//! starts it again when it stops, and stops it with itself.

use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

const PROGRAM: &str = "omime-panel";
/// A switcher that stops this soon after starting failed to start.
const QUICK_EXIT: Duration = Duration::from_secs(10);
/// Quick exits in a row after which the switcher is given up.
const MAX_FAILURES: u32 = 5;

/// The running switcher. Dropping it stops it.
#[derive(Debug)]
pub struct Panel {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Panel {
    fn drop(&mut self) {
        // The child goes with the task (`kill_on_drop`).
        self.task.abort();
    }
}

/// Starts the switcher on the compositor at `display` (`$WAYLAND_DISPLAY` when `None`)
/// and keeps it running, unless it is not installed.
pub fn start(display: Option<String>) -> Option<Panel> {
    let Some(program) = crate::programs::find(PROGRAM) else {
        warn!("the switcher ({PROGRAM}) is not installed");
        return None;
    };
    let task = tokio::task::spawn_local(async move {
        let mut failures = 0;
        loop {
            let mut command = tokio::process::Command::new(&program);
            command.kill_on_drop(true);
            if let Some(display) = &display {
                command.env("WAYLAND_DISPLAY", display);
            }
            crate::core::engines::exit_with_omimed(&mut command);
            let started = Instant::now();
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(e) => {
                    warn!("cannot start the switcher: {e}");
                    return;
                }
            };
            info!(pid = child.id().unwrap_or(0), "the switcher started");
            let status = child.wait().await;
            debug!(?status, "the switcher stopped");
            if started.elapsed() < QUICK_EXIT {
                failures += 1;
            } else {
                failures = 0;
            }
            if failures >= MAX_FAILURES {
                warn!("the switcher keeps stopping; giving up on it");
                return;
            }
            tokio::time::sleep(Duration::from_secs(1 << failures)).await;
        }
    });
    Some(Panel { task })
}
