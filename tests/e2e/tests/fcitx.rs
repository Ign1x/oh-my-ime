//! End-to-end tests for the Fcitx5 module protocol: a test client that behaves like
//! the Fcitx5 GTK or Qt module, omimed and a real engine.
//!
//! They need a built workspace and the tools of `nix develop`:
//!
//! ```sh
//! cargo build --workspace && cargo test -p omime-e2e --test fcitx -- --ignored
//! ```

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use omime_e2e::harness::{FcitxClient, TIMEOUT, TestEnv, dummy_engine, echo_engine};

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
    env.wait_for_status("direct input")?;

    let typed = type_key(&client, A)?;
    assert_lines(&typed, &["reply false", "reply false"]);
    assert!(!typed.iter().any(|l| l.starts_with("preedit")), "{typed:?}");
    Ok(())
}

const MAIN: &str = "org.fcitx.Fcitx5";
const PORTAL: &str = "org.freedesktop.portal.Fcitx";
const DAEMON: &str = "io.github.ign1x.OhMyIme";
const DAEMON_PATH: &str = "/io/github/ign1x/OhMyIme";
const IC_INTERFACE: &str = "org.fcitx.Fcitx.InputContext1";

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

async fn connect(env: &TestEnv) -> zbus::Result<zbus::Connection> {
    zbus::connection::Builder::address(format!("unix:path={}/bus", env.dir().display()).as_str())
        .method_timeout(Duration::from_secs(3))
        .build()
        .await
}

async fn control(conn: &zbus::Connection) -> zbus::Result<zbus::Proxy<'_>> {
    zbus::Proxy::new(conn, DAEMON, DAEMON_PATH, "io.github.ign1x.OhMyIme.Daemon1").await
}

async fn create_ic(conn: &zbus::Connection, name: &str) -> zbus::Result<zbus::OwnedObjectPath> {
    let im = zbus::Proxy::new(
        conn,
        name,
        "/org/freedesktop/portal/inputmethod",
        "org.fcitx.Fcitx.InputMethod1",
    )
    .await?;
    let (path, _): (zbus::OwnedObjectPath, Vec<u8>) = im
        .call("CreateInputContext", &vec![("program", "fcitx-test")])
        .await?;
    Ok(path)
}

async fn context<'a>(
    conn: &'a zbus::Connection,
    name: &'a str,
    path: zbus::OwnedObjectPath,
) -> zbus::Result<zbus::Proxy<'a>> {
    zbus::Proxy::new(conn, name, path, IC_INTERFACE).await
}

async fn daemon_pid(conn: &zbus::Connection) -> anyhow::Result<u32> {
    let bus = zbus::fdo::DBusProxy::new(conn).await?;
    Ok(bus
        .get_connection_unix_process_id(DAEMON.try_into()?)
        .await?)
}

fn rss_kib(pid: u32) -> anyhow::Result<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .context("no VmRSS")
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn fcitx_contexts_have_a_per_connection_limit() -> anyhow::Result<()> {
    context_limit(MAIN)
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn portal_contexts_have_a_per_connection_limit() -> anyhow::Result<()> {
    context_limit(PORTAL)
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn extreme_fcitx_cursor_rectangles_do_not_terminate_omimed() -> anyhow::Result<()> {
    let env = TestEnv::start(&[], None)?;
    runtime()?.block_on(async {
        let conn = connect(&env).await?;
        let daemon = control(&conn).await?;
        for name in [MAIN, PORTAL] {
            for display in ["x11:", "wayland:"] {
                let im = zbus::Proxy::new(&conn, name, "/org/freedesktop/portal/inputmethod", "org.fcitx.Fcitx.InputMethod1").await?;
                let (path, _): (zbus::OwnedObjectPath, Vec<u8>) = im.call("CreateInputContext", &vec![("program", "fcitx-test"), ("display", display)]).await?;
                let ic = context(&conn, name, path).await?;
                let _: () = ic.call("FocusIn", &()).await?;
                // Include the original x + width / 2 overflow and each field in isolation.
                for value in [i32::MAX, i32::MIN] {
                    for rect in [(value, 0, 2, 1), (0, value, 1, 2), (0, 0, value, 1), (0, 0, 1, value), (value, value, value, value)] {
                        let rect_result = ic.call::<_, _, ()>("SetCursorRect", &rect).await;
                        let status = daemon.call::<_, _, HashMap<String, zbus::OwnedVariant>>("Status", &()).await;
                        assert!(rect_result.is_ok() && status.is_ok(), "cursor {rect:?} stopped omimed: rect={rect_result:?}, status={status:?}\n{}", env.daemon_log());
                        for scale in [1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0, f64::MIN_POSITIVE] {
                            let rect_result = ic.call::<_, _, ()>("SetCursorRectV2", &(rect.0, rect.1, rect.2, rect.3, scale)).await;
                            let status = daemon.call::<_, _, HashMap<String, zbus::OwnedVariant>>("Status", &()).await;
                            assert!(rect_result.is_ok() && status.is_ok(), "cursor {rect:?}, scale {scale} stopped omimed: rect={rect_result:?}, status={status:?}\n{}", env.daemon_log());
                        }
                    }
                }
                let _: () = ic.call("DestroyIC", &()).await?;
            }
        }
        anyhow::Ok(())
    })
}

fn assert_limit(result: zbus::Result<zbus::OwnedObjectPath>) {
    assert!(
        matches!(&result, Err(zbus::Error::FDO(error)) if matches!(error.as_ref(), zbus::fdo::Error::LimitsExceeded(_))),
        "expected LimitsExceeded, got {result:?}"
    );
}

/// Reads the SDK engine's surrounding state and any deletion from one key reply.
async fn surrounding_key(
    ic: &zbus::Proxy<'_>,
    backspace: bool,
) -> anyhow::Result<(String, Vec<(i32, u32)>)> {
    let key = if backspace {
        (0xff08u32, 22u32)
    } else {
        (0x61, 38)
    };
    let (events, handled): (Vec<(u32, zbus::OwnedVariant)>, bool) = ic
        .call("ProcessKeyEventBatch", &(key.0, key.1, 0u32, false, 1u32))
        .await?;
    assert!(handled);
    let mut report = None;
    let mut deletions = Vec::new();
    for (kind, value) in events {
        match kind {
            1 => {
                let (segments, _): (Vec<(String, i32)>, i32) = value.try_into()?;
                report = Some(segments.into_iter().map(|(text, _)| text).collect());
            }
            3 => deletions.push(value.try_into()?),
            other => bail!("unexpected batch event {other}"),
        }
    }
    Ok((
        report.context("engine did not report its surrounding state")?,
        deletions,
    ))
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn moving_outside_the_surrounding_window_clears_engine_text_and_blocks_deletion()
-> anyhow::Result<()> {
    let env = TestEnv::start(
        &[dummy_engine(&["--hidden", "--surrounding"])],
        Some("dummy"),
    )?;
    env.wait_for_log("engine ready")?;
    runtime()?.block_on(async {
        let text = format!("{}中{}", "a".repeat(10), "a".repeat(10_000));
        for name in [MAIN, PORTAL] {
            let conn = connect(&env).await?;
            let ic = context(&conn, name, create_ic(&conn, name).await?).await?;
            let _: () = ic
                .call("SetSurroundingText", &(text.as_str(), 5_000u32, 5_000u32))
                .await?;
            assert_eq!(
                surrounding_key(&ic, false).await?,
                ("4000:2000:2000".into(), vec![])
            );

            let _: () = ic
                .call("SetSurroundingTextPosition", &(11u32, 10u32))
                .await?;
            assert_eq!(surrounding_key(&ic, true).await?, ("0:0:0".into(), vec![]));
            // Neither another position-only update nor refocusing restores discarded text.
            let _: () = ic
                .call("SetSurroundingTextPosition", &(5_000u32, 5_000u32))
                .await?;
            let _: () = ic.call("FocusOut", &()).await?;
            let _: () = ic.call("FocusIn", &()).await?;
            assert_eq!(surrounding_key(&ic, true).await?, ("0:0:0".into(), vec![]));

            let _: () = ic
                .call("SetSurroundingText", &(text.as_str(), 11u32, 10u32))
                .await?;
            // Three bytes before this cursor are one Chinese character.
            assert_eq!(
                surrounding_key(&ic, true).await?,
                ("4000:13:10".into(), vec![(-1, 1)])
            );
            let _: () = ic.call("DestroyIC", &()).await?;
        }
        anyhow::Ok(())
    })
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn disconnect_during_context_creation_restores_global_capacity() -> anyhow::Result<()> {
    let env = TestEnv::start(&[], None)?;
    runtime()?.block_on(async {
        for _ in 0..40 {
            let conn = connect(&env).await?;
            let main = zbus::Proxy::new(
                &conn,
                MAIN,
                "/org/freedesktop/portal/inputmethod",
                "org.fcitx.Fcitx.InputMethod1",
            )
            .await?;
            let portal = zbus::Proxy::new(
                &conn,
                PORTAL,
                "/org/freedesktop/portal/inputmethod",
                "org.fcitx.Fcitx.InputMethod1",
            )
            .await?;
            for index in 0..300 {
                let im = if index % 2 == 0 { &main } else { &portal };
                im.call_noreply(
                    "CreateInputContext",
                    &vec![("program", "fcitx-disconnect-test")],
                )
                .await?;
            }
            // Only the writes were awaited; method handlers may still be exporting objects.
            conn.close().await?;
        }
        // Let the queued work settle before measuring leaked capacity. The number of
        // exported and removed contexts must stop changing, even on a busy test host.
        tokio::time::timeout(TIMEOUT * 6, async {
            let mut counts = (0, 0);
            let mut changed = Instant::now();
            loop {
                let now = (
                    env.count_in_log("input context created"),
                    env.count_in_log("input context destroyed"),
                );
                if now != counts {
                    counts = now;
                    changed = Instant::now();
                }
                if changed.elapsed() >= Duration::from_millis(500) {
                    assert!(counts.0 > 0, "no queued creates reached the frontend");
                    eprintln!(
                        "after disconnect: {} created, {} destroyed",
                        counts.0, counts.1
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("queued creates and disconnect cleanup did not settle")?;
        let mut owners = Vec::new();
        for owner in 0..8 {
            let conn = connect(&env).await?;
            for index in 0..1_024 {
                let name = if (owner + index) % 2 == 0 {
                    MAIN
                } else {
                    PORTAL
                };
                tokio::time::timeout(TIMEOUT, async {
                    loop {
                        match create_ic(&conn, name).await {
                            Ok(_) => break,
                            result => assert_limit(result),
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .with_context(|| {
                    format!(
                        "only {} of 8192 slots recovered after disconnect",
                        owner * 1_024 + index
                    )
                })?;
            }
            owners.push(conn);
        }
        let next = connect(&env).await?;
        assert_limit(create_ic(&next, MAIN).await);
        assert_limit(create_ic(&next, PORTAL).await);
        for owner in owners {
            owner.close().await?;
        }
        anyhow::Ok(())
    })
}

fn context_limit(name: &str) -> anyhow::Result<()> {
    // The probe ceiling is deliberately above the documented per-owner limit.
    const CEILING: usize = 4_096;
    const LIMIT: usize = 1_024;
    let env = TestEnv::start(&[], None)?;
    runtime()?.block_on(async {
        let conn = connect(&env).await?;
        let pid = daemon_pid(&conn).await?;
        let before = rss_kib(pid)?;
        let text = "x".repeat(16 * 1024);
        let other_name = if name == MAIN { PORTAL } else { MAIN };
        let mut paths = Vec::new();
        let mut rejected = None;
        for _ in 0..CEILING {
            match create_ic(&conn, name).await {
                Ok(path) => {
                    let ic = context(&conn, name, path.clone()).await?;
                    let _: () = ic
                        .call("SetSurroundingText", &(text.as_str(), 8_192u32, 8_192u32))
                        .await?;
                    paths.push((name, path));
                }
                Err(error) => {
                    rejected = Some(error);
                    break;
                }
            }
        }
        let _: HashMap<String, zbus::OwnedVariant> =
            control(&conn).await?.call("Status", &()).await?;
        let after = rss_kib(pid)?;
        eprintln!(
            "{name}: accepted {} contexts; VmRSS {before} -> {after} KiB; rejection={rejected:?}",
            paths.len()
        );
        assert_eq!(paths.len(), LIMIT, "the context quota was not enforced");
        assert_limit(Err(rejected.context("no quota rejection")?));
        assert_limit(create_ic(&conn, other_name).await);
        assert!(
            after.saturating_sub(before) < 32 * 1024,
            "retained context RSS grew by {} KiB",
            after.saturating_sub(before)
        );
        // A rejected client can keep trying without retaining more objects or text.
        for _ in 0..CEILING {
            assert_limit(create_ic(&conn, name).await);
        }
        assert!(rss_kib(pid)?.saturating_sub(after) < 8 * 1024);

        let other = connect(&env).await?;
        let other_path = create_ic(&other, name).await?;
        let _: () = context(&other, name, other_path)
            .await?
            .call("DestroyIC", &())
            .await?;
        // Reuse the entire quota several times, mixing both names after the first round.
        for round in 0..3 {
            for (bus, path) in paths.drain(..) {
                let _: () = context(&conn, bus, path)
                    .await?
                    .call("DestroyIC", &())
                    .await?;
            }
            for index in 0..LIMIT {
                let bus = if (index + round) % 2 == 0 {
                    name
                } else {
                    other_name
                };
                paths.push((bus, create_ic(&conn, bus).await?));
            }
            assert_limit(create_ic(&conn, name).await);
            assert_limit(create_ic(&conn, other_name).await);
        }
        let (bus, path) = paths.last().context("last context")?;
        let observer = context(&other, bus, path.clone()).await?;
        conn.close().await?;
        tokio::time::timeout(TIMEOUT, async {
            while observer.call::<_, _, ()>("FocusIn", &()).await.is_ok() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        Ok(())
    })
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn global_context_limit_is_shared_and_disconnect_releases_it() -> anyhow::Result<()> {
    let env = TestEnv::start(&[], None)?;
    runtime()?.block_on(async {
        let mut owners = Vec::new();
        for owner in 0..8 {
            let conn = connect(&env).await?;
            for index in 0..1_024 {
                create_ic(
                    &conn,
                    if (owner + index) % 2 == 0 {
                        MAIN
                    } else {
                        PORTAL
                    },
                )
                .await?;
            }
            owners.push(conn);
        }
        let next = connect(&env).await?;
        assert_limit(create_ic(&next, MAIN).await);
        assert_limit(create_ic(&next, PORTAL).await);
        owners.pop().context("full owner")?.close().await?;
        let path = tokio::time::timeout(TIMEOUT, async {
            loop {
                match create_ic(&next, PORTAL).await {
                    Ok(path) => break path,
                    result => assert_limit(result),
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        for _ in 1..1_024 {
            create_ic(&next, MAIN).await?;
        }
        assert_limit(create_ic(&next, PORTAL).await);
        // At the global cap, destroying one context makes exactly one slot available.
        let _: () = context(&next, PORTAL, path)
            .await?
            .call("DestroyIC", &())
            .await?;
        create_ic(&next, MAIN).await?;
        assert_limit(create_ic(&next, MAIN).await);
        for owner in owners {
            owner.close().await?;
        }
        next.close().await?;
        anyhow::Ok(())
    })
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn foreign_context_calls_cannot_change_composition_or_destroy_the_owner() -> anyhow::Result<()> {
    use futures_util::StreamExt as _;
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    runtime()?.block_on(async {
        for name in [MAIN, PORTAL] {
            let owner = connect(&env).await?;
            let other = connect(&env).await?;
            let path = create_ic(&owner, name).await?;
            let ic = context(&owner, name, path.clone()).await?;
            let foreign = context(&other, name, path.clone()).await?;
            let mut foreign_signals = foreign.receive_all_signals().await?;
            let mut owner_signals = ic.receive_all_signals().await?;
            let handled: bool = ic
                .call("ProcessKeyEvent", &(0x61u32, 38u32, 0u32, false, 1u32))
                .await?;
            assert!(handled);
            let _: () = foreign.call("Reset", &()).await?;
            let _: () = foreign.call("SetCapability", &(1u64 << 3)).await?;
            let _: () = foreign.call("FocusOut", &()).await?;
            let _: () = foreign.call("DestroyIC", &()).await?;
            let handled: bool = foreign
                .call("ProcessKeyEvent", &(0x62u32, 56u32, 0u32, false, 2u32))
                .await?;
            assert!(!handled);
            let _: bool = ic
                .call("ProcessKeyEvent", &(0x20u32, 65u32, 0u32, false, 3u32))
                .await?;
            let commit = tokio::time::timeout(Duration::from_secs(2), async {
                while let Some(message) = owner_signals.next().await {
                    if message
                        .header()
                        .member()
                        .is_some_and(|m| m.as_str() == "CommitString")
                    {
                        return message.body().deserialize::<String>().ok();
                    }
                }
                None
            })
            .await?;
            assert_eq!(commit.as_deref(), Some("A"));
            assert!(
                tokio::time::timeout(Duration::from_millis(150), foreign_signals.next())
                    .await
                    .is_err(),
                "signals leaked to another connection"
            );
            drop(owner_signals);
            drop(ic);
            owner.close().await?;
            let removed = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if foreign.call::<_, _, ()>("FocusIn", &()).await.is_err() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            assert!(
                removed.is_ok(),
                "disconnected client's context remained exported"
            );
        }
        anyhow::Ok(())
    })
}
