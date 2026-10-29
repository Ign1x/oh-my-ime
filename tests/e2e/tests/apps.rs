//! End-to-end tests for per-application engines (`[apps] remember`).
//!
//! See `typing.rs` for how to run them.

use omime_e2e::harness::{TestEnv, echo_engine};

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn each_application_keeps_its_engine() -> anyhow::Result<()> {
    let mut env = TestEnv::start(&[echo_engine()], None)?;
    env.configure("[apps]\nremember = true\n")?;

    let a = env.editor(&["--app-id", "editor-a"])?;
    env.wait_for_log("focused application changed app=\"editor-a\"")?;
    env.omime(&["switch", "echo"])?;
    env.wait_for_log("engine ready")?;
    env.type_text("ab ")?;
    a.wait_for_text("AB ")?;

    // A new application starts with the current engine...
    let b = env.editor(&["--app-id", "editor-b"])?;
    env.wait_for_log("focused application changed app=\"editor-b\"")?;
    env.type_text("cd ")?;
    b.wait_for_text("CD ")?;
    // ...until the user switches in it.
    env.key(&["ctrl"], "space")?;
    env.type_text("ef ")?;
    b.wait_for_text("CD ef ")?;

    env.focus("editor-a")?;
    env.type_text("gh ")?;
    a.wait_for_text("AB GH ")?;
    assert!(env.omime(&["status"])?.starts_with("echo"));

    env.focus("editor-b")?;
    env.type_text("ij ")?;
    b.wait_for_text("CD ef ij ")?;
    assert_eq!(env.omime(&["status"])?.trim(), "direct input");

    let state = std::fs::read_to_string(env.dir().join("state/oh-my-ime/state.toml"))?;
    assert!(state.contains("editor-a = \"echo\""), "{state}");
    assert!(state.contains("editor-b = \"\""), "{state}");

    // The choices survive a restart.
    env.restart_daemon()?;
    // The restarted daemon first takes the field that still has focus.
    env.wait_for_log("keyboard grabbed")?;
    env.focus("editor-a")?;
    env.type_text("kl ")?;
    a.wait_for_text("AB GH KL ")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn applications_share_the_engine_by_default() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    let a = env.editor(&["--app-id", "editor-a"])?;
    env.omime(&["switch", "echo"])?;
    env.wait_for_log("engine ready")?;

    let b = env.editor(&["--app-id", "editor-b"])?;
    env.key(&["ctrl"], "space")?;
    env.type_text("ab ")?;
    b.wait_for_text("ab ")?;

    env.focus("editor-a")?;
    env.type_text("cd ")?;
    a.wait_for_text("cd ")?;
    Ok(())
}
