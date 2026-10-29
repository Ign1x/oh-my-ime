//! End-to-end tests for engine windows: where they open and when they are shown.
//!
//! See `typing.rs` for how to run them.

use std::time::Duration;

use omime_e2e::harness::{FcitxClient, TestEnv, dummy_engine, sparkle_engine};

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

/// The centre of the window when it opens below an X11 text cursor at (`x`, `y`), `height`
/// high (X11 and compositor coordinates are the same in the test session).
fn below(x: u32, y: u32, height: u32) -> (u32, u32) {
    (x + 50, y + height + 20)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn engine_windows_follow_the_text_cursor_of_x11_applications() -> anyhow::Result<()> {
    let env = TestEnv::start(&[dummy_engine(&ARGS)], Some("dummy"))?;
    env.wait_for_log("engine window created")?;
    // No Wayland text field has focus, so the window goes where the X11 application
    // says its text cursor is.
    let client = env.fcitx_client(&["--display", "x11:"])?;
    client.send("focus-in")?;
    client.send("cursor 300 200 2 20")?;
    let (x, y) = below(300, 200, 20);
    env.wait_for_pixel(x, y, RED)?;

    // Near the bottom of the 720 pixels high output, the window opens above the cursor.
    client.send("cursor 300 690 2 20")?;
    env.wait_for_pixel(x, 690 - 20, RED)?;
    assert_ne!(env.pixel(x, y)?, RED, "the window stayed where it was");

    client.send("focus-out")?;
    std::thread::sleep(Duration::from_millis(500));
    assert_ne!(
        env.pixel(x, 690 - 20)?,
        RED,
        "the window stayed after focus-out"
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn engine_windows_follow_xim_applications() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[dummy_engine(&ARGS)], Some("dummy"))?;
    env.wait_for_log("engine window created")?;
    // The client's window fills the output, so its spot is a position on the output too.
    // XIM gives where the text's baseline starts; omimed assumes a line around it.
    let client = env.xim_client(&["--spot", "200,300"])?;
    let (x, y) = below(200, 300 - 16, 22);
    env.wait_for_pixel(x, y, RED)?;

    // An application that goes away without closing the input method takes its text
    // field, and so the window, with it.
    drop(client);
    std::thread::sleep(Duration::from_millis(500));
    assert_ne!(
        env.pixel(x, y)?,
        RED,
        "the window stayed after the client died"
    );
    Ok(())
}

/// Types `keys` (keyval, X11 key code, modifier state) with the Fcitx client.
fn type_keys(client: &FcitxClient, keys: &[(u32, u32, u32)]) -> anyhow::Result<()> {
    for (keyval, keycode, state) in keys {
        client.send(&format!("key {keyval} {keycode} {state} press"))?;
        client.send(&format!("key {keyval} {keycode} {state} release"))?;
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn engine_windows_of_x11_applications_take_clicks() -> anyhow::Result<()> {
    let env = TestEnv::start(&[sparkle_engine()], Some("sparkle"))?;
    env.wait_for_log("engine window created")?;
    let client = env.fcitx_client(&["--display", "x11:"])?;
    client.send("focus-in")?;
    client.send("cursor 100 100 2 20")?;
    // ":heart" (Shift+semicolon, then letters).
    type_keys(
        &client,
        &[
            (0x3a, 47, 1),
            (0x68, 43, 0),
            (0x65, 26, 0),
            (0x61, 38, 0),
            (0x72, 27, 0),
            (0x74, 28, 0),
        ],
    )?;
    client.wait_for("the :heart preedit", |l| {
        l.starts_with("preedit \":heart\"")
    })?;
    // The window opens below the cursor; Sparkle's fourth card is centred at (251, 93)
    // inside it (see typing.rs).
    std::thread::sleep(Duration::from_millis(300));
    env.click(100 + 251, 100 + 20 + 93)?;
    client.wait_for("a click on a card", |l| {
        l.starts_with("commit \"") && !l.contains(':')
    })?;
    Ok(())
}
