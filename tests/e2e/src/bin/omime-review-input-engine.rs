//! Engine replies used by the commit ordering and size regression tests.

use anyhow::Context as _;
use omime_e2e::long_engine_text;
use omime_sdk::{Context, Engine, KeyEvent, Preedit};

struct Probe {
    mode: String,
    bytes: usize,
}

impl Engine for Probe {
    fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool {
        if key.released {
            return false;
        }
        match (self.mode.as_str(), key.text.as_str()) {
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
