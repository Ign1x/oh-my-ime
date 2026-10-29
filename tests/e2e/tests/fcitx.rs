//! End-to-end tests for the Fcitx5 module protocol: a test client that behaves like
//! the Fcitx5 GTK or Qt module, omimed and a real engine.
//!
//! They need a built workspace and the tools of `nix develop`:
//!
//! ```sh
//! cargo build --workspace && cargo test -p omime-e2e --test fcitx -- --ignored
//! ```

use std::time::{Duration, Instant};

use anyhow::bail;
use omime_e2e::harness::{FcitxClient, TIMEOUT, TestEnv, echo_engine};

#[derive(Clone, Copy, Debug)]
struct Key {
    keyval: u32,
    keycode: u32,
}

const A: Key = Key {
    keyval: 0x61,
    keycode: 38,
};
const B: Key = Key {
    keyval: 0x62,
    keycode: 56,
};
const SPACE: Key = Key {
    keyval: 0x20,
    keycode: 65,
};

/// Sends a command and returns its output up to and including its final line.
fn send(client: &FcitxClient, line: &str) -> anyhow::Result<Vec<String>> {
    let from = client.lines().len();
    client.send(line)?;
    wait_reply(client, from, line)
}

fn wait_reply(client: &FcitxClient, from: usize, what: &str) -> anyhow::Result<Vec<String>> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let lines = client.lines();
        let new = lines.get(from..).unwrap_or_default();
        if let Some(index) = new
            .iter()
            .position(|l| l == "ok" || l.starts_with("reply ") || l.starts_with("error "))
        {
            let output = new[..=index].to_vec();
            if let Some(error) = output.iter().find(|l| l.starts_with("error ")) {
                bail!("{what}: {error}");
            }
            return Ok(output);
        }
        if Instant::now() > deadline {
            bail!("timed out waiting for {what}; the client printed {lines:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Presses or releases `key` with `state`.
fn key_event(
    client: &FcitxClient,
    key: Key,
    state: u32,
    release: bool,
) -> anyhow::Result<Vec<String>> {
    let action = if release { "release" } else { "press" };
    send(
        client,
        &format!("key {:#x} {} {state} {action}", key.keyval, key.keycode),
    )
}

/// Types `key`: press and release. Returns the lines of both.
fn type_key(client: &FcitxClient, key: Key) -> anyhow::Result<Vec<String>> {
    let mut lines = key_event(client, key, 0, false)?;
    lines.extend(key_event(client, key, 0, true)?);
    Ok(lines)
}

fn assert_lines(lines: &[String], expected: &[&str]) {
    let actual: Vec<&str> = lines.iter().map(String::as_str).collect();
    assert_eq!(actual.as_slice(), expected, "{lines:?}");
}

fn wait_for_status(env: &TestEnv, expected: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let status = env.omime(&["status"])?;
        if status.trim() == expected {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("status is {:?}, not {expected:?}", status.trim());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn fcitx_clients_type_through_engines() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let client = env.fcitx_client(&[])?;
    assert_lines(&send(&client, "focus-in")?, &["ok"]);

    assert_lines(
        &type_key(&client, A)?,
        &["preedit \"a\" 1", "reply true", "reply false"],
    );
    assert_lines(
        &type_key(&client, B)?,
        &["preedit \"ab\" 2", "reply true", "reply false"],
    );
    assert_lines(
        &type_key(&client, SPACE)?,
        &[
            "commit \"AB\"",
            "preedit \"\" 0",
            "reply false",
            "reply false",
        ],
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn key_event_order_fix_forwards_the_key_after_the_text() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let client = env.fcitx_client(&[])?;
    assert_lines(&send(&client, "focus-in")?, &["ok"]);
    // 1 << 37: the client handles an unconsumed key after the text it produced.
    assert_lines(&send(&client, "caps 137438953472")?, &["ok"]);

    type_key(&client, A)?;
    type_key(&client, B)?;
    let mut space = key_event(&client, SPACE, 0, false)?;
    space.extend(key_event(&client, SPACE, 0, true)?);
    assert_lines(
        &space,
        &[
            "commit \"AB\"",
            "preedit \"\" 0",
            "forward 32 0 false",
            "reply true",
            "reply false",
        ],
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn batch_replies_carry_the_events() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let client = env.fcitx_client(&["--batch"])?;
    assert_lines(&send(&client, "focus-in")?, &["ok"]);

    assert_lines(
        &type_key(&client, A)?,
        &["preedit \"a\" 1", "reply true", "reply false"],
    );
    assert_lines(
        &type_key(&client, B)?,
        &["preedit \"ab\" 2", "reply true", "reply false"],
    );
    assert_lines(
        &type_key(&client, SPACE)?,
        &[
            "commit \"AB\"",
            "preedit \"\" 0",
            "reply false",
            "reply false",
        ],
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn password_fields_are_not_composed() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let client = env.fcitx_client(&[])?;
    // 1 << 3: the field is a password.
    assert_lines(&send(&client, "caps 8")?, &["ok"]);
    assert_lines(&send(&client, "focus-in")?, &["ok"]);

    let typed = type_key(&client, A)?;
    assert_lines(&typed, &["reply false", "reply false"]);
    assert!(
        !client
            .lines()
            .iter()
            .any(|l| l.starts_with("preedit") || l.starts_with("commit")),
        "{:?}",
        client.lines()
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn signals_go_to_their_client_only() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let first = env.fcitx_client(&["--program", "first"])?;
    let second = env.fcitx_client(&["--program", "second"])?;
    assert_lines(&send(&first, "focus-in")?, &["ok"]);
    assert_lines(&send(&second, "focus-in")?, &["ok"]);
    assert_lines(&send(&first, "focus-out")?, &["ok"]);

    type_key(&second, A)?;
    type_key(&second, B)?;
    let space = type_key(&second, SPACE)?;
    assert!(space.iter().any(|l| l == "commit \"AB\""), "{space:?}");

    assert!(
        !first
            .lines()
            .iter()
            .any(|l| l.starts_with("preedit") || l.starts_with("commit")),
        "{:?}",
        first.lines()
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn contexts_end_with_their_client() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let mut client = env.fcitx_client(&[])?;
    assert_lines(&send(&client, "focus-in")?, &["ok"]);
    // Only kill once the frontend has the context, so its end is what removes it.
    env.wait_for_log("text field focused")?;
    client.kill()?;
    env.wait_for_log("input context destroyed")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn hotkeys_work_in_fcitx_clients() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let client = env.fcitx_client(&[])?;
    assert_lines(&send(&client, "focus-in")?, &["ok"]);

    // Ctrl+Space (state 4 is the X11 Control bit) switches to direct input.
    assert_lines(&key_event(&client, SPACE, 4, false)?, &["reply true"]);
    wait_for_status(&env, "direct input")?;

    let typed = type_key(&client, A)?;
    assert_lines(&typed, &["reply false", "reply false"]);
    assert!(!typed.iter().any(|l| l.starts_with("preedit")), "{typed:?}");
    Ok(())
}
