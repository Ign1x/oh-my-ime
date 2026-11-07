//! `omime-notifications` — a desktop notification server (`org.freedesktop.Notifications`)
//! for the end-to-end tests, as a desktop's notification daemon would be.
//!
//! It prints what it is asked to do, one line each:
//!
//! ```text
//! ready
//! notify <id> <replaces id> <summary>|<body>
//! close <id>
//! closed <id> <reason>
//! clicked <id>
//! ```
//!
//! and reads commands from stdin: `click <id>` invokes the default action of a
//! notification, as a click on it would; `closed <id> <reason>` emits
//! `NotificationClosed`, as when it expired (1) or was dismissed (2). Clicks are
//! deliberately allowed for closed and unknown IDs to test stale actions.
//! `burst notify <count>` or `burst close <count>` arms that many unrelated
//! `NotificationClosed` signals before the next matching method reply; it prints
//! `burst <method> <count> armed` and then `burst <method> <count> sent`.

use std::collections::HashMap;
use std::io::{BufRead as _, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Context as _;
use tokio::sync::mpsc;
use zbus::OwnedVariant;
use zbus::object_server::SignalEmitter;

const PATH: &str = "/org/freedesktop/Notifications";

fn say(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

struct Server {
    last: AtomicU32,
    bursts: Arc<ClosureBursts>,
}

#[derive(Default)]
struct ClosureBursts {
    notify: AtomicU32,
    close: AtomicU32,
}

async fn closure_burst(
    emitter: &SignalEmitter<'_>,
    method: &str,
    pending: &AtomicU32,
) -> zbus::Result<()> {
    let count = pending.swap(0, Ordering::Relaxed);
    for offset in 0..count {
        Server::notification_closed(emitter, 10_000 + offset, 2).await?;
    }
    if count != 0 {
        say(&format!("burst {method} {count} sent"));
    }
    Ok(())
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        _app_name: String,
        replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        _actions: Vec<String>,
        _hints: HashMap<String, OwnedVariant>,
        _expire_timeout: i32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<u32> {
        let id = if replaces_id == 0 {
            self.last.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            replaces_id
        };
        closure_burst(&emitter, "notify", &self.bursts.notify).await?;
        say(&format!(
            "notify {id} {replaces_id} {summary}|{}",
            body.replace('\n', " ")
        ));
        Ok(id)
    }

    async fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        closure_burst(&emitter, "close", &self.bursts.close).await?;
        Self::notification_closed(&emitter, id, 3).await?;
        say(&format!("close {id}"));
        Ok(())
    }

    fn get_capabilities(&self) -> Vec<String> {
        vec!["actions".to_owned(), "body".to_owned()]
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "omime-notifications".to_owned(),
            "oh-my-ime".to_owned(),
            "0".to_owned(),
            "1.2".to_owned(),
        )
    }

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let bursts = Arc::new(ClosureBursts::default());
    let conn = zbus::connection::Builder::session()
        .serve_at(
            PATH,
            Server {
                last: AtomicU32::new(0),
                bursts: bursts.clone(),
            },
        )
        .name("org.freedesktop.Notifications")
        .build()
        .await
        .context("cannot serve notifications on the session bus")?;

    let (commands_tx, mut commands) = mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if commands_tx.send(line).is_err() {
                break;
            }
        }
    });
    say("ready");

    let emitter = SignalEmitter::new(&conn, PATH)?;
    while let Some(command) = commands.recv().await {
        if let Some(id) = command.strip_prefix("click ") {
            let id: u32 = id.trim().parse().context("a notification id")?;
            Server::action_invoked(&emitter, id, "default").await?;
            say(&format!("clicked {id}"));
        } else if let Some(args) = command.strip_prefix("closed ") {
            let (id, reason) = args
                .split_once(' ')
                .context("a notification id and reason")?;
            let id: u32 = id.parse().context("a notification id")?;
            let reason: u32 = reason.trim().parse().context("a close reason")?;
            Server::notification_closed(&emitter, id, reason).await?;
            say(&format!("closed {id} {reason}"));
        } else if let Some(args) = command.strip_prefix("burst ") {
            let (method, count) = args.split_once(' ').context("a method and signal count")?;
            let count: u32 = count.parse().context("a signal count")?;
            let pending = match method {
                "notify" => &bursts.notify,
                "close" => &bursts.close,
                _ => anyhow::bail!("unknown notification method: {method}"),
            };
            pending.store(count, Ordering::Relaxed);
            say(&format!("burst {method} {count} armed"));
        }
    }
    Ok(())
}
