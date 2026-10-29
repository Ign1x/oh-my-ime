//! End-to-end tests for XIM: an X11 test client that uses the protocol like a real
//! application, omimed and a real engine, on the XWayland server of a headless sway.
//!
//! They need a built workspace, the tools of `nix develop` and `xwayland`:
//!
//! ```sh
//! cargo build --workspace && cargo test -p omime-e2e --test xim -- --ignored
//! ```

use omime_e2e::harness::{TestEnv, echo_engine};
use omime_e2e::json_string;

/// The line the client prints for `prefix` and `text`.
fn line(prefix: &str, text: &str) -> String {
    format!("{prefix} {}", json_string(text))
}

/// The first line equal to `wanted`, or a failure that shows every line.
fn position(lines: &[String], wanted: &str) -> usize {
    lines
        .iter()
        .position(|l| l == wanted)
        .unwrap_or_else(|| panic!("no {wanted} in {lines:?}"))
}

/// What omimed logs when an application leaves a synchronous request unanswered.
const SYNC_TIMEOUT: &str = "did not answer in time";

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn xim_clients_type_through_engines() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[echo_engine()], Some("echo"))?;
    let client = env.xim_client(&[])?;

    env.type_on_seat("seat0", "ab ")?;
    client.wait_for_text("AB ")?;

    // The preedit grew letter by letter, the commit came before the space the engine
    // did not consume, and the application handled that space itself.
    let lines = client.lines();
    let a = position(&lines, &line("preedit", "a"));
    let ab = position(&lines, &line("preedit", "ab"));
    let commit = position(&lines, &line("commit", "AB"));
    let space = position(&lines, &line("key", " "));
    assert!(a < ab, "{lines:?}");
    assert!(ab < commit, "{lines:?}");
    assert!(commit < space, "{lines:?}");
    // The client answered every synchronous request, and omimed noticed.
    assert_eq!(env.count_in_log(SYNC_TIMEOUT), 0);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn fast_typing_keeps_the_order() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[echo_engine()], Some("echo"))?;
    let client = env.xim_client(&[])?;

    env.type_on_seat("seat0", "1ab 2")?;
    client.wait_for_text("1AB 2")?;
    assert_eq!(env.count_in_log(SYNC_TIMEOUT), 0);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn direct_input_sends_keys_back() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[echo_engine()], Some("echo"))?;
    let client = env.xim_client(&[])?;

    env.omime(&["switch", "direct"])?;
    env.wait_for_status("direct input")?;
    let before = client.preedits().len();

    env.type_on_seat("seat0", "cd")?;
    client.wait_for_text("cd")?;
    let preedits = client.preedits();
    assert!(preedits[before..].is_empty(), "{:?}", client.lines());
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn xim_survives_a_client_restart() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[echo_engine()], Some("echo"))?;
    let first = env.xim_client(&[])?;
    env.type_on_seat("seat0", "ab ")?;
    first.wait_for_text("AB ")?;
    // Dropping the client kills it, as when an application crashes.
    drop(first);

    let second = env.xim_client(&[])?;
    env.type_on_seat("seat0", "cd ")?;
    second.wait_for_text("CD ")?;
    Ok(())
}
