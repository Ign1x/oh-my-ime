//! Review fixture: deliberately incomplete/slow engine RPCs, or ordinary mode reports.
//! Used only by review_growth; it never connects to the user's session bus.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use omime_sdk::KeyEvent;
use zbus::{OwnedVariant, Variant};

type Dict = HashMap<String, OwnedVariant>;
type Actions = Vec<(u32, OwnedVariant)>;

struct ReviewEngine {
    behavior: String,
    revision: AtomicU64,
    stopped: Arc<tokio::sync::Notify>,
}

impl ReviewEngine {
    fn mode(&self, mode: &str) -> zbus::fdo::Result<Actions> {
        Ok(vec![(
            4,
            OwnedVariant::try_from(Variant::from((
                "",
                mode,
                self.revision.fetch_add(1, Ordering::Relaxed) + 1,
            )))
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?,
        )])
    }
}

async fn flood_preedit(connection: &zbus::Connection, context: u64) -> anyhow::Result<()> {
    let dir = std::path::PathBuf::from(
        std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is unset")?,
    );
    while !dir.join("review-flood-start").exists() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let host = zbus::Proxy::new(
        connection,
        "io.github.ign1x.OhMyIme",
        "/io/github/ign1x/OhMyIme",
        "io.github.ign1x.OhMyIme.EngineHost1",
    )
    .await?;
    // Each update fits in a Wayland message and changes only one
    // field. Total retained work, not message size, is the test.
    let text = "x".repeat(2048);
    for _ in 0..8192 {
        if host
            .call_noreply("SetPreedit", &(context, &text, 2048_i32, 2048_i32))
            .await
            .is_err()
        {
            break;
        }
    }
    std::fs::write(dir.join("review-flood-done"), b"done")?;
    Ok(())
}

#[zbus::interface(name = "io.github.ign1x.OhMyIme.Engine1")]
impl ReviewEngine {
    async fn activate(
        &self,
        context: u64,
        _info: Dict,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<Actions> {
        eprintln!("review activated {context}");
        if self.behavior == "hang" {
            return std::future::pending().await;
        }
        if self.behavior == "modes" {
            self.mode("native")
        } else {
            if self.behavior == "flood-preedit" {
                let connection = connection.clone();
                tokio::spawn(async move {
                    if let Err(error) = flood_preedit(&connection, context).await {
                        eprintln!("review flood failed: {error:#}");
                    }
                });
            }
            Ok(Vec::new())
        }
    }

    fn deactivate(&self, context: u64) {
        eprintln!("review deactivated {context}");
    }

    async fn update_context(&self, _context: u64, _changes: Dict) -> Actions {
        if self.behavior == "slow-updates" {
            // Each request meets the default 200ms deadline. A backlog therefore
            // never triggers the daemon's hung-engine recovery.
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        Vec::new()
    }

    async fn process_key(
        &self,
        _context: u64,
        key: KeyEvent,
        _extras: Dict,
    ) -> zbus::fdo::Result<(bool, Actions)> {
        if self.behavior == "xim-burst" && key.keysym == u32::from(b'a') && !key.released {
            // Give the test time to stop the requesting client before output.
            tokio::time::sleep(Duration::from_millis(500)).await;
            let mut actions = Vec::new();
            for _ in 0..20 {
                actions.push((
                    0,
                    OwnedVariant::try_from(Variant::from("x"))
                        .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?,
                ));
            }
            return Ok((true, actions));
        }
        Ok((false, Vec::new()))
    }

    fn reset(&self, _context: u64) -> Actions {
        Vec::new()
    }

    fn set_input_mode(&self, _context: u64, mode: String) -> zbus::fdo::Result<Actions> {
        self.mode(&mode)
    }

    fn shutdown(&self) {
        self.stopped.notify_one();
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let behavior = std::env::args().nth(1).unwrap_or_default();
    let stopped = Arc::new(tokio::sync::Notify::new());
    let address = std::env::var("OMIME_ENGINE_ADDRESS")?;
    let connection = zbus::connection::Builder::address(address.as_str())
        .p2p()
        .serve_at(
            "/io/github/ign1x/OhMyIme/Engine",
            ReviewEngine {
                behavior: behavior.clone(),
                revision: AtomicU64::new(0),
                stopped: stopped.clone(),
            },
        )
        .build()
        .await?;
    let host = zbus::Proxy::new(
        &connection,
        "io.github.ign1x.OhMyIme",
        "/io/github/ign1x/OhMyIme",
        "io.github.ign1x.OhMyIme.EngineHost1",
    )
    .await?;
    let mut capabilities = Dict::new();
    if behavior == "modes" {
        capabilities.insert(
            "input-modes".into(),
            OwnedVariant::try_from(Variant::from(vec!["native", "latin"]))?,
        );
    }
    let _: Dict = host
        .call("Register", &("review", 1_u16, 2_u16, capabilities))
        .await?;
    tokio::select! {
        () = stopped.notified() => {},
        () = connection.closed() => {},
    }
    Ok(())
}
