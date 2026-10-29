//! End-to-end tests for failure handling: engines that are slow or break the shared
//! Wayland connection must never stop the user from typing.
//!
//! See `typing.rs` for how to run them.

use omime_e2e::harness::{TIMEOUT, TestEngine, TestEnv, target_dir};

fn dummy(args: &[&str]) -> TestEngine {
    TestEngine {
        id: "dummy",
        program: target_dir().join("omime-dummy-ui"),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
    }
}

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
fn omimed_reconnects_when_an_engine_breaks_the_connection() -> anyhow::Result<()> {
    let marker = std::env::temp_dir().join(format!(
        "omime-e2e-misbehaved-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let marker_arg = marker.display().to_string();
    // An invalid viewport size is forwarded to the compositor, which then closes
    // omimed's connection. The restarted engine behaves.
    let env = TestEnv::start(
        &[dummy(&[
            "--misbehave",
            "viewport",
            "--misbehave-once",
            &marker_arg,
        ])],
        Some("dummy"),
    )?;
    env.wait_for_log("lost the compositor connection")?;
    wait_for_count(&env, "connected to the compositor", 2);
    wait_for_count(&env, "engine ready", 2);

    let editor = env.editor(&[])?;
    env.type_text("still typing")?;
    editor.wait_for_text("still typing")?;
    let _ = std::fs::remove_file(marker);
    Ok(())
}
