//! End-to-end tests: a headless sway, omimed, real engines and a test editor.
//!
//! They need sway, dbus-daemon and wtype (all in `nix develop`) and a built workspace:
//!
//! ```sh
//! cargo build --workspace && cargo test -p omime-e2e -- --ignored
//! ```

use omime_e2e::harness::{TestEnv, echo_engine, wait_until};

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
    // The engine sees the release of Ctrl+Space, which xkb types as NUL.
    assert_eq!(env.count_in_log("engine request failed"), 0);
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

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn sparkle_inserts_emoji_with_keys_and_clicks() -> anyhow::Result<()> {
    use omime_e2e::harness::sparkle_engine;

    let env = TestEnv::start(&[sparkle_engine()], Some("sparkle"))?;
    env.wait_for_log("engine window created")?;
    let editor = env.editor(&[])?;

    env.type_text(":rocket")?;
    editor.wait_for("the :rocket preedit", |l| {
        l.starts_with("preedit \":rocket\"")
    })?;
    env.key(&[], "space")?;
    editor.wait_for_text("🚀")?;

    env.type_text(":heart")?;
    editor.wait_for("the :heart preedit", |l| {
        l.starts_with("preedit \":heart\"")
    })?;
    // The popup opens below the text cursor, which the test editor puts at
    // x = 12 + 8 × column, y = 20, height 18. Sparkle's fourth card is centred at
    // (251, 93) inside its window.
    if std::env::var_os("OMIME_E2E_SCREENSHOT").is_some() {
        env.screenshot(&env.dir().join("sparkle.png"))?;
    }
    let column = 1; // after "🚀"
    env.click(12 + 8 * column + 251, 20 + 18 + 93)?;
    editor.wait_for("a click on a card", |l| {
        l.starts_with("commit \"") && !l.contains(':')
    })?;
    let text = editor.text();
    assert!(
        text.starts_with('🚀') && text.chars().count() == 2,
        "{text:?}"
    );
    Ok(())
}

/// How many virtual keyboards sway has.
fn virtual_keyboards(env: &TestEnv) -> anyhow::Result<usize> {
    let output = env.command("swaymsg").args(["-t", "get_inputs"]).output()?;
    anyhow::ensure!(output.status.success(), "swaymsg failed");
    Ok(String::from_utf8(output.stdout)?
        .matches("\"name\": \"wlr_virtual_keyboard_v1\"")
        .count())
}

/// Waits until sway has `count` virtual keyboards.
fn wait_for_virtual_keyboards(env: &TestEnv, count: usize) -> anyhow::Result<()> {
    wait_until(&format!("{count} virtual keyboards"), || {
        virtual_keyboards(env).ok().filter(|&n| n == count)
    })?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn no_virtual_keyboard_outlives_the_grab() -> anyhow::Result<()> {
    // Hyprland merges the state of all keyboards, an input method's virtual keyboard
    // included once the input method holds no grab. A virtual keyboard kept after the
    // grab could hold a modifier, say Ctrl after leaving a field with Ctrl+W, for
    // every application.
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let editor = env.editor(&[])?;
    env.type_text("ab ")?;
    editor.wait_for_text("AB ")?;
    // The test's own keyboard, which plays the physical one, and omimed's.
    wait_for_virtual_keyboards(&env, 2)?;

    let released = env.count_in_log("keyboard released");
    let _other = env.spawn_editor(&["--app-id", "other", "--no-text-field"])?;
    wait_until("omimed to release the keyboard", || {
        (env.count_in_log("keyboard released") > released).then_some(())
    })?;
    wait_for_virtual_keyboards(&env, 1)?;

    // Back in the field, omimed types through a new one.
    env.focus("io.github.ign1x.OhMyIme.TestEditor")?;
    wait_for_virtual_keyboards(&env, 2)?;
    env.type_text("cd ")?;
    editor.wait_for_text("AB CD ")?;
    Ok(())
}
