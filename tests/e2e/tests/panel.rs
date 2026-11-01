//! End-to-end tests of the switcher, `omime-panel`, which omimed starts with
//! `[panel] enabled`. See `typing.rs` for how to run them.

use omime_e2e::harness::{TestEnv, echo_engine, wait_until, wait_until_within};
use std::time::Duration;

/// Just inside the top edge of the switcher with two choices, at the bottom centre of
/// the 1280 × 720 output: 120 pixels above the bottom, 108 high.
const INSIDE: (u32, u32) = (640, 720 - 120 - 108 + 4);

fn start(config: &str) -> anyhow::Result<TestEnv> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    env.configure(&format!("{config}[panel]\nenabled = true\n"))?;
    env.wait_for_log("omime_panel: waiting for switches")?;
    Ok(env)
}

/// Lines of the switcher's log about switches, shown or not.
fn switches_seen(env: &TestEnv) -> usize {
    env.count_in_log("showing the switcher")
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_switcher_shows_after_hotkeys_and_commands() -> anyhow::Result<()> {
    let env = start("")?;
    let _editor = env.editor(&[])?;
    env.wait_for_log("keyboard grabbed")?;
    let (x, y) = INSIDE;
    let before = env.pixel(x, y)?;

    env.key(&["ctrl"], "space")?;
    env.wait_for_log("showing the switcher input_method=\"echo\" reason=\"hotkey\"")?;
    wait_until("the switcher", || {
        (env.pixel(x, y).ok()? != before).then_some(())
    })?;
    wait_until_within(Duration::from_secs(5), "the switcher to go", || {
        (env.pixel(x, y).ok()? == before).then_some(())
    })?;

    env.omime(&["toggle"])?;
    env.wait_for_log("reason=\"command\"")?;
    wait_until("the switcher again", || {
        (env.pixel(x, y).ok()? != before).then_some(())
    })?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn applications_get_their_input_method_back_without_the_switcher() -> anyhow::Result<()> {
    let env = start("[apps]\nremember = true\n")?;
    let _a = env.editor(&["--app-id", "editor-a"])?;
    env.wait_for_log("focused application changed app=\"editor-a\"")?;
    env.omime(&["switch", "echo"])?;
    let _b = env.editor(&["--app-id", "editor-b"])?;
    env.wait_for_log("focused application changed app=\"editor-b\"")?;
    env.omime(&["switch", "direct"])?;
    wait_until("both switches", || (switches_seen(&env) == 2).then_some(()))?;

    // editor-a gets echo back, quietly: omimed does not even announce it.
    env.focus("editor-a")?;
    wait_until("echo for editor-a", || {
        env.omime(&["status"])
            .ok()?
            .starts_with("echo")
            .then_some(())
    })?;
    env.omime(&["switch", "echo"])?;
    let again = "showing the switcher input_method=\"echo\" reason=\"command\"";
    wait_until("the switch", || {
        (env.count_in_log(again) == 2).then_some(())
    })?;
    // Signals come in order: one for the focus change would have come before.
    assert_eq!(switches_seen(&env), 3, "{}", env.daemon_log());
    Ok(())
}
