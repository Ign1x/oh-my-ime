//! Engine replies used by the commit ordering and size regression tests.

use std::io::Write as _;
use std::time::Duration;

use anyhow::Context as _;
use omime_e2e::long_engine_text;
use omime_sdk::{Context, Engine, KeyEvent, Preedit};

struct Probe {
    mode: String,
    bytes: usize,
}

impl Engine for Probe {
    fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool {
        if self.mode.contains("forward") {
            let path = std::path::PathBuf::from(
                std::env::var_os("XDG_RUNTIME_DIR").expect("the test supplies a runtime directory"),
            )
            .join("forward-keys.log");
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .expect("the test's runtime directory is writable");
            writeln!(log, "{} {} {}", key.keycode, key.released, key.repeat)
                .expect("the forwarding probe log is writable");
            match self.mode.as_str() {
                "out-of-band-forward" if key.keycode == 30 && !key.released => {
                    let host = ctx.host().clone();
                    let id = ctx.id();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(80));
                        host.forward_key(id, 30, true);
                        host.forward_key(id, 30, false);
                    });
                }
                "out-of-band-forward" => return false,
                "forward-cross-release" => match (key.keycode, key.released) {
                    (30, false) => ctx.forward_key(30, true),
                    (48, false) => {
                        std::thread::sleep(Duration::from_millis(80));
                        ctx.forward_key(30, false);
                    }
                    (44, released) => ctx.forward_key(44, !released),
                    _ => {}
                },
                "forward"
                | "slow-forward"
                | "forward-press"
                | "forward-stroke"
                | "forward-release-overflow" => {
                    if self.mode == "slow-forward" {
                        // XIM forwards Shift plus both letters as three queued keys.
                        // Keep all three replies inside the 200 ms arrival budget.
                        std::thread::sleep(Duration::from_millis(40));
                    }
                    if self.mode == "forward-release-overflow" && key.released {
                        ctx.commit("x".repeat(65_537));
                    }
                    if !key.released
                        || !matches!(self.mode.as_str(), "forward-press" | "forward-stroke")
                    {
                        ctx.forward_key(key.keycode, !key.released);
                        if self.mode == "forward-stroke" && !key.released {
                            ctx.forward_key(key.keycode, false);
                        }
                    }
                }
                _ => panic!("unknown forwarding probe mode"),
            }
            return true;
        }
        if key.released {
            return false;
        }
        match (self.mode.as_str(), key.text.as_str()) {
            ("host-burst", "a") => {
                // The SDK's fire-and-forget Host API spawns each no-reply call.
                // Exercise one full 64-callback ingress burst on a healthy bus.
                for _ in 0..32 {
                    ctx.host()
                        .set_preedit(ctx.id(), Preedit::with_cursor_at_end("x"));
                    ctx.host().commit(ctx.id(), "x");
                }
            }
            ("host-burst", "b") => ctx.commit("ok"),
            ("large-commit", "a") => ctx.commit(long_engine_text(self.bytes)),
            ("large-commit", "b") => ctx.commit("ok"),
            ("large-preedit", "a") => {
                ctx.set_preedit(Preedit::with_cursor_at_end(long_engine_text(self.bytes)));
            }
            ("large-preedit", "b") => ctx.commit(long_engine_text(self.bytes)),
            ("batched-commit", "a") => {
                // Each action fits, but their concatenation does not.
                for prefix in ["first:", "second:", "third:"] {
                    ctx.commit(format!("{prefix}{}", long_engine_text(4000 - prefix.len())));
                }
            }
            _ => return false,
        }
        true
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().context("probe mode is required")?;
    let bytes = args
        .next()
        .map(|arg| arg.parse())
        .transpose()?
        .unwrap_or(65_536);
    omime_sdk::run(Probe { mode, bytes })?;
    Ok(())
}
