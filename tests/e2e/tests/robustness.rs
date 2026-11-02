//! End-to-end tests for failure handling: engines that are slow or break the shared
//! Wayland connection must never stop the user from typing.
//!
//! See `typing.rs` for how to run them.

use omime_e2e::harness::{TIMEOUT, TestEnv, dummy_engine as dummy, echo_engine, wait_until_within};

fn wait_for_count(env: &TestEnv, needle: &str, count: usize) {
    let deadline = std::time::Instant::now() + TIMEOUT;
    while env.count_in_log(needle) < count {
        assert!(
            std::time::Instant::now() < deadline,
            "expected {count} × {needle:?} in the omimed log"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_slow_engine_cannot_hold_up_typing() -> anyhow::Result<()> {
    // The engine answers every key after 450 ms; omimed waits 200 ms.
    let env = TestEnv::start(&[dummy(&["--slow-ms", "450"])], Some("dummy"))?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&[])?;
    env.type_text("abc")?;
    editor.wait_for_text("abc")?;
    env.wait_for_log("did not answer in time")?;
    // Late answers are discarded, so nothing is typed twice.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(editor.text(), "abc");
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn invalid_viewport_only_disconnects_its_engine() -> anyhow::Result<()> {
    let marker = std::env::temp_dir().join(format!(
        "omime-e2e-misbehaved-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let marker_arg = marker.display().to_string();
    // The bridge rejects the invalid viewport before it reaches the compositor.
    // The restarted engine behaves; the daemon keeps its original connection.
    let env = TestEnv::start(
        &[dummy(&[
            "--misbehave",
            "viewport",
            "--misbehave-once",
            &marker_arg,
        ])],
        Some("dummy"),
    )?;
    env.wait_for_log("invalid viewport destination")?;
    wait_for_count(&env, "engine ready", 2);

    let editor = env.editor(&[])?;
    env.type_text("still typing")?;
    editor.wait_for_text("still typing")?;
    assert_eq!(env.count_in_log("lost the compositor connection"), 0);
    let _ = std::fs::remove_file(marker);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn an_upstream_protocol_fault_stops_its_engine_before_the_daemon() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[dummy(&["--misbehave", "subsurface"]), echo_engine()],
        Some("dummy"),
    )?;
    // wl-proxy may observe HUP before the compositor's error message. In that
    // case the process crash budget must still stop the engine before the global
    // reconnect budget can end the daemon.
    wait_until_within(
        std::time::Duration::from_secs(45),
        "the faulty engine to stop",
        || {
            let log = env.daemon_log();
            (log.contains("quarantining an engine")
                || log.contains("engine keeps crashing; giving up"))
            .then_some(())
        },
    )?;
    wait_for_count(&env, "connected to the compositor", 2);
    let editor = env.editor(&[])?;
    env.type_text("plain")?;
    editor.wait_for_text("plain")?;
    env.omime(&["switch", "echo"])?;
    env.type_text("works ")?;
    editor.wait_for_text("plainWORKS ")?;
    assert!(env.count_in_log("engine launched engine=\"dummy\"") <= 6);
    assert!(
        !env.daemon_log()
            .contains("the connection keeps failing; giving up")
    );
    Ok(())
}
