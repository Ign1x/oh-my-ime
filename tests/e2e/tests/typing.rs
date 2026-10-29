//! End-to-end tests: a headless sway, omimed, real engines and a test editor.
//!
//! They need sway, dbus-daemon and wtype (all in `nix develop`) and a built workspace:
//!
//! ```sh
//! cargo build --workspace && cargo test -p omime-e2e -- --ignored
//! ```

use omime_e2e::harness::{TestEnv, echo_engine};

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn keys_pass_through_without_an_engine() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    let editor = env.editor(&[])?;
    env.type_text("Hello, World!")?;
    editor.wait_for_text("Hello, World!")?;
    env.wait_for_log("keyboard grabbed")?;
    assert!(editor.preedits().iter().all(String::is_empty));
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn echo_composes_and_commits_words() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let editor = env.editor(&[])?;
    env.type_text("hello world")?;
    env.key(&[], "Return")?;
    editor.wait_for("the Return key", |l| l == "enter")?;
    editor.wait_for_text("HELLO WORLD")?;
    let preedits = editor.preedits();
    for expected in ["h", "hel", "hello", "wor"] {
        assert!(
            preedits.iter().any(|p| p == expected),
            "{expected:?} in {preedits:?}"
        );
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn backspace_and_escape_edit_the_preedit_first() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let editor = env.editor(&[])?;
    env.type_text("abc")?;
    env.key(&[], "BackSpace")?;
    editor.wait_for("the preedit `ab`", |l| l.starts_with("preedit \"ab\""))?;
    env.key(&[], "Escape")?;
    env.type_text("x ")?;
    editor.wait_for_text("X ")?;
    // With an empty preedit, Backspace belongs to the application.
    env.key(&[], "BackSpace")?;
    editor.wait_for_text("X")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn switching_with_the_cli_and_hotkeys() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    let editor = env.editor(&[])?;
    assert!(env.omime(&["list"])?.contains("echo"));
    assert_eq!(env.omime(&["status"])?.trim(), "direct input");

    env.type_text("ab")?;
    editor.wait_for_text("ab")?;

    env.omime(&["switch", "echo"])?;
    env.wait_for_log("engine ready")?;
    env.type_text("cd ")?;
    editor.wait_for_text("abCD ")?;
    assert!(
        env.omime(&["status", "--json"])?
            .contains("\"engine\":\"echo\"")
    );

    env.key(&["ctrl"], "space")?;
    env.type_text("ef")?;
    editor.wait_for_text("abCD ef")?;
    assert_eq!(env.omime(&["status"])?.trim(), "direct input");

    env.key(&["ctrl"], "space")?;
    env.type_text(" gh ")?;
    editor.wait_for_text("abCD ef GH ")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn password_fields_bypass_engines() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    // content_purpose 8 = password
    let editor = env.editor(&["--purpose", "8"])?;
    env.type_text("secret")?;
    editor.wait_for_text("secret")?;
    assert!(editor.preedits().iter().all(String::is_empty));
    assert!(!env.daemon_log().contains("keyboard grabbed"));
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn a_crashing_engine_is_restarted() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let editor = env.editor(&[])?;
    env.type_text("ab ")?;
    editor.wait_for_text("AB ")?;

    let pid = env.engine_pid("echo").expect("echo was launched");
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(pid as i32).expect("valid pid"),
        rustix::process::Signal::KILL,
    )?;
    env.wait_for_log("engine exited unexpectedly")?;
    // While the engine is gone, typing still works.
    env.type_text("cd")?;
    editor.wait_for_text("AB cd")?;

    let deadline = std::time::Instant::now() + omime_e2e::harness::TIMEOUT;
    while env.count_in_log("engine ready") < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "echo was not restarted"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    env.type_text(" ef ")?;
    editor.wait_for_text("AB cd EF ")?;
    Ok(())
}
