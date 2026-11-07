//! A native engine that saves briefly, or hangs, after the SDK receives Shutdown.

use std::io::Write as _;
use std::time::Duration;

use omime_sdk::{Context, Engine, KeyEvent};

struct Saver {
    slow: bool,
    hold_key: bool,
}

impl Saver {
    fn delay(&self) {
        if self.slow {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Engine for Saver {
    fn update(&mut self, _ctx: &mut Context<'_>) {
        self.delay();
    }

    fn reset(&mut self, _ctx: &mut Context<'_>) {
        self.delay();
    }

    fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool {
        self.delay();
        if key.released || key.text.is_empty() {
            return false;
        }
        if self.hold_key {
            let state = std::path::PathBuf::from(
                std::env::var_os("XDG_STATE_HOME").expect("state directory"),
            );
            if std::fs::remove_file(state.join("hold-next-key")).is_ok() {
                std::fs::write(state.join("key-in-flight"), b"ready").expect("key barrier");
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !state.join("release-key").exists() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "key barrier was not released"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        ctx.commit(key.text.clone());
        true
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let hang = args.iter().any(|arg| arg == "--hang");
    // A hung finalizer must survive SIGTERM, so the test also exercises SIGKILL.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _term = runtime.block_on(async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    })?;
    omime_sdk::run(Saver {
        slow: args.iter().any(|arg| arg == "--slow"),
        hold_key: args.iter().any(|arg| arg == "--hold-key"),
    })?;
    std::thread::sleep(Duration::from_millis(50));
    let mut saved = std::fs::OpenOptions::new().append(true).create(true).open(
        std::path::PathBuf::from(std::env::var_os("XDG_STATE_HOME").expect("state directory"))
            .join("engine-saved"),
    )?;
    writeln!(saved, "{}", std::process::id())?;
    if hang {
        loop {
            std::thread::park();
        }
    }
    Ok(())
}
