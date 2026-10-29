//! End-to-end tests for engine windows: where they open and when they are shown.
//!
//! See `typing.rs` for how to run them.

use std::time::Duration;

use omime_e2e::harness::{TestEnv, dummy_engine};

/// The dummy engine's window: red, 100 × 40 (with a darker 3-pixel border).
const ARGS: [&str; 4] = ["--color", "ff0000", "--size", "100x40"];
const RED: u32 = 0xff0000;

/// The centre of the window when it opens below the test editor's text cursor, which
/// is at x = 12, y = 20 with a height of 18 in an empty editor.
const CENTRE: (u32, u32) = (12 + 50, 20 + 18 + 20);

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn engine_windows_open_below_the_text_cursor() -> anyhow::Result<()> {
    let env = TestEnv::start(&[dummy_engine(&ARGS)], Some("dummy"))?;
    env.wait_for_log("engine window created")?;
    let _editor = env.editor(&[])?;
    env.wait_for_pixel(CENTRE.0, CENTRE.1, RED)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn engine_windows_stay_hidden_until_the_engine_asks() -> anyhow::Result<()> {
    let mut args = ARGS.to_vec();
    args.push("--hidden");
    let env = TestEnv::start(&[dummy_engine(&args)], Some("dummy"))?;
    env.wait_for_log("engine window created")?;
    let _editor = env.editor(&[])?;
    // Long enough for the window to appear, as in the test above.
    std::thread::sleep(Duration::from_millis(500));
    assert_ne!(env.pixel(CENTRE.0, CENTRE.1)?, RED);
    Ok(())
}
