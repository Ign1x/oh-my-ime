//! Text commits and forwarded keys while an application reports state slowly.
//! See `typing.rs` for the private headless environment.

use omime_e2e::harness::{TestEnv, echo_engine, hosted_engine};

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn text_reaches_the_application_before_a_forwarded_key() -> anyhow::Result<()> {
    for engine in [echo_engine(), hosted_engine(&[])] {
        let id = engine.id;
        let env = TestEnv::start(&[engine], Some(id))?;
        let editor = env.editor(&["--surrounding-delay-ms", "80", "--print-modifiers"])?;
        env.type_on_seat("seat0", "ok next ")?;
        editor.wait_for_text("OK NEXT ")?;
        editor.wait_for("the space release", |line| line == "release \"space\"")?;
        let lines = editor.lines();
        let feedback = lines.iter().position(|line| line == "surrounding \"OK\"");
        let space = lines.iter().position(|line| line == "key \" \"");
        assert!(
            matches!((feedback, space), (Some(feedback), Some(space)) if feedback < space),
            "{id}: the space arrived before the application reported the committed word: {lines:?}"
        );
        assert!(lines.iter().any(|line| line == "held "), "{lines:?}");
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn missing_surrounding_updates_do_not_stall_or_replay_text() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let editor = env.editor(&["--no-surrounding-updates", "--print-modifiers"])?;
    env.type_on_seat("seat0", "one two ")?;
    editor.wait_for_text("ONE TWO ")?;
    editor.wait_for("the space release", |line| line == "release \"space\"")?;
    env.type_on_seat("seat0", "three ")?;
    editor.wait_for_text("ONE TWO THREE ")?;
    assert_eq!(
        editor
            .lines()
            .iter()
            .filter(|line| line.starts_with("commit "))
            .count(),
        3,
        "{:?}",
        editor.lines()
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn leaving_a_field_cancels_its_buffered_keys() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let first = env.editor(&["--no-surrounding-updates", "--app-id", "first"])?;
    env.type_on_seat("seat0", "old ")?;
    first.wait_for("the old word to commit", |line| line == "commit \"OLD\"")?;
    let next = env.editor(&["--app-id", "next", "--print-modifiers"])?;
    first.wait_for("the old field to lose focus", |line| line == "left")?;
    env.type_on_seat("seat0", "new ")?;
    next.wait_for_text("NEW ")?;
    next.wait_for("the space release", |line| line == "release \"space\"")?;
    // On a busy machine the bounded fallback may release the space before the
    // new window gets focus. It must never leak into the new field.
    assert!(matches!(first.text().as_str(), "OLD" | "OLD "));
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn reactivation_releases_keys_delivered_to_the_old_field() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let editor = env.editor(&[
        "--no-surrounding-updates",
        "--reactivate-after-key",
        "--print-modifiers",
    ])?;
    // Shift is forwarded before a word is committed. Keep its physical press
    // down while the client re-enables the text field on the forwarded space.
    env.keys_on_seat("seat0", "press shift\ntap a\ntap space\n")?;
    editor.wait_for("the same-window reactivation", |line| line == "reactivated")?;
    editor.wait_for("the old Shift to be released", |line| {
        line == "release \"Shift_L\""
    })?;
    env.keys_on_seat("seat0", "release shift\n")?;
    env.type_on_seat("seat0", "new ")?;
    editor.wait_for_text("A NEW ")?;
    Ok(())
}
