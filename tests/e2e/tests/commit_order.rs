//! Text commits and forwarded keys while an application reports state slowly.
//! See `typing.rs` for the private headless environment.

use omime_e2e::harness::{Editor, TestEngine, TestEnv, echo_engine, hosted_engine, target_dir};
use omime_e2e::{long_engine_text, parse_json_string};

fn input_engine(mode: &str, bytes: usize) -> TestEngine {
    TestEngine {
        id: "review-input",
        program: target_dir().join("omime-review-input-engine"),
        args: vec![mode.to_owned(), bytes.to_string()],
        env: Vec::new(),
        hosted: false,
    }
}

fn assert_complete_commit(env: &TestEnv, editor: &Editor, expected: &str) -> anyhow::Result<()> {
    editor.wait_for("the following space", |line| line == "key \" \"")?;
    editor.wait_for("the space release", |line| line == "release \"space\"")?;
    assert!(env.omime(&["status"]).is_ok(), "engine reply killed daemon");
    assert_eq!(env.count_in_log("lost the compositor connection"), 0);
    let lines = editor.lines();
    let mut committed = String::new();
    let mut forwarded = false;
    for line in &lines {
        if line == "key \" \"" {
            forwarded = true;
        }
        if let Some(quoted) = line.strip_prefix("commit ") {
            let (text, _) = parse_json_string(quoted).expect("quoted commit");
            assert!(text.len() <= 4000, "commit has {} bytes", text.len());
            assert!(!forwarded, "commit arrived after the forwarded key");
            committed.push_str(&text);
        }
        if let Some(quoted) = line.strip_prefix("preedit ") {
            let (text, offsets) = parse_json_string(quoted).expect("quoted preedit");
            assert!(text.len() <= 4000, "preedit has {} bytes", text.len());
            for offset in offsets.split_whitespace() {
                let offset: i32 = offset.parse()?;
                assert!(offset == -1 || (offset >= 0 && text.is_char_boundary(offset as usize)));
            }
        }
    }
    assert_eq!(committed.len(), expected.len(), "missing or repeated text");
    assert!(committed == expected, "committed text is out of order");
    assert!(editor.text() == format!("{expected} "), "document differs");
    Ok(())
}

fn oversized_commit(mode: &str, bytes: usize, expected: &str) -> anyhow::Result<()> {
    let env = TestEnv::start(&[input_engine(mode, bytes)], Some("review-input"))?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&["--surrounding-delay-ms", "20", "--print-modifiers"])?;
    env.type_on_seat("seat0", "a ")?;
    assert_complete_commit(&env, &editor, expected)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn oversized_commit_is_contained() -> anyhow::Result<()> {
    oversized_commit("large-commit", 65_536, &long_engine_text(65_536))
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn oversized_preedit_is_contained() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[input_engine("large-preedit", 65_536)],
        Some("review-input"),
    )?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&["--print-modifiers"])?;
    env.type_on_seat("seat0", "a")?;
    editor.wait_for("the bounded preedit", |line| {
        line.strip_prefix("preedit ")
            .and_then(parse_json_string)
            .is_some_and(|(text, _)| !text.is_empty())
    })?;
    let expected = long_engine_text(65_536);
    let mut end = 4000;
    while !expected.is_char_boundary(end) {
        end -= 1;
    }
    assert!(
        editor
            .preedits()
            .iter()
            .any(|text| text == &expected[..end])
    );
    env.type_on_seat("seat0", "b ")?;
    assert_complete_commit(&env, &editor, &expected)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn individually_small_commits_do_not_overflow_the_output_batch() -> anyhow::Result<()> {
    let expected: String = ["first:", "second:", "third:"]
        .into_iter()
        .map(|prefix| format!("{prefix}{}", long_engine_text(4000 - prefix.len())))
        .collect();
    oversized_commit("batched-commit", 0, &expected)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_commit_between_the_protocol_limit_and_the_panic_threshold_keeps_the_connection()
-> anyhow::Result<()> {
    for bytes in [5000, 4001, 8180] {
        oversized_commit("large-commit", bytes, &long_engine_text(bytes))?;
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_long_hosted_commit_is_delivered() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[hosted_engine(&["--commit-bytes", "65536"])],
        Some("hosted"),
    )?;
    let editor = env.editor(&["--surrounding-delay-ms", "20", "--print-modifiers"])?;
    env.wait_for_hosted("the hosted keyboard grab", |line| {
        line.starts_with("repeat ")
    })?;
    env.type_on_seat("seat0", "a ")?;
    assert_complete_commit(&env, &editor, &long_engine_text(65_536))
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn forwarded_keys_cannot_overflow_a_split_commit_waiting_for_feedback() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[hosted_engine(&[
            "--commit-bytes",
            "65536",
            "--forward-strokes",
            "400",
        ])],
        Some("hosted"),
    )?;
    let editor = env.editor(&["--surrounding-delay-ms", "80", "--print-modifiers"])?;
    env.wait_for_hosted("the hosted keyboard grab", |line| {
        line.starts_with("repeat ")
    })?;
    // F1 reaches the application before the commit; the forwarded A waits behind
    // it. Both releases must survive a queue saturated by synthetic F2 strokes.
    env.keys_on_seat(
        "seat0",
        "press 59\nsleep 100\npress a\nsleep 50\nrelease 59\nrelease a\n",
    )?;
    env.wait_for_hosted("the forwarded burst", |line| line == "forwarded burst")?;
    env.wait_for_log("dropping a key: output queue limit exceeded")?;
    editor.wait_for("the F1 release", |line| line == "release \"F1\"")?;
    editor.wait_for("the A release", |line| line == "release \"a\"")?;
    let lines = editor.lines();
    let mut committed = String::new();
    let mut key_arrived = false;
    let mut strokes = 0;
    for line in &lines {
        if line == "key \"a\"" || line.contains("F2") {
            key_arrived = true;
        }
        if line == "release \"F2\"" {
            strokes += 1;
        }
        if let Some(quoted) = line.strip_prefix("commit ") {
            assert!(!key_arrived, "forwarded key overtook accepted text");
            let (text, _) = parse_json_string(quoted).expect("quoted commit");
            assert!(text.len() <= 4000);
            committed.push_str(&text);
        }
    }
    assert!(
        committed == long_engine_text(65_536),
        "accepted text was lost or repeated"
    );
    assert!(
        (1..=126).contains(&strokes),
        "queued {strokes} strokes beyond the 256-entry budget"
    );
    assert_eq!(lines.iter().filter(|line| *line == "key \"a\"").count(), 1);
    assert_eq!(
        lines
            .iter()
            .rev()
            .find(|line| line.starts_with("held "))
            .map(String::as_str),
        Some("held ")
    );
    env.type_on_seat("seat0", "ok ")?;
    editor.wait_for_text(&format!("{committed}aOK "))?;
    env.omime(&["status"])?;
    assert_eq!(env.count_in_log("lost the compositor connection"), 0);
    assert!(!env.daemon_log().contains("00000:中文"));
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn completed_keys_cannot_overflow_a_split_commit_waiting_for_feedback() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[input_engine("large-commit", 65_536)],
        Some("review-input"),
    )?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&["--surrounding-delay-ms", "80", "--print-modifiers"])?;
    let script = format!(
        "press 59\nsleep 100\ntap a\n{}release 59\n",
        "tap 60\n".repeat(400)
    );
    env.keys_on_seat("seat0", &script)?;
    env.wait_for_log("dropping a key: output queue limit exceeded")?;
    editor.wait_for("the F1 release", |line| line == "release \"F1\"")?;
    let lines = editor.lines();
    let strokes = lines
        .iter()
        .filter(|line| *line == "release \"F2\"")
        .count();
    assert!(
        (1..400).contains(&strokes),
        "excess completed keys were not rejected"
    );
    assert_eq!(
        lines
            .iter()
            .rev()
            .find(|line| line.starts_with("held "))
            .map(String::as_str),
        Some("held ")
    );
    let last_commit = lines
        .iter()
        .rposition(|line| line.starts_with("commit "))
        .expect("committed text");
    let first_key = lines
        .iter()
        .position(|line| line.contains("F2"))
        .expect("forwarded keys");
    assert!(
        last_commit < first_key,
        "completed keys overtook accepted text"
    );
    env.type_on_seat("seat0", " ")?;
    assert_complete_commit(&env, &editor, &long_engine_text(65_536))
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn rejecting_an_oversized_reply_settles_the_key_without_logging_text() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[input_engine("large-commit", 65_537)],
        Some("review-input"),
    )?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&["--print-modifiers"])?;
    env.type_on_seat("seat0", "a ")?;
    assert_complete_commit(&env, &editor, "")?;
    assert_eq!(env.count_in_log("output queue limit exceeded"), 1);
    assert!(!env.daemon_log().contains("00000:中文"));
    env.type_on_seat("seat0", "b ")?;
    editor.wait_for_text(" ok ")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn rejecting_a_hosted_transaction_releases_following_keys() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[hosted_engine(&["--commit-bytes", "65537"])],
        Some("hosted"),
    )?;
    let editor = env.editor(&["--print-modifiers"])?;
    env.wait_for_hosted("the hosted keyboard grab", |line| {
        line.starts_with("repeat ")
    })?;
    env.type_on_seat("seat0", "a ")?;
    assert_complete_commit(&env, &editor, "")?;
    assert_eq!(env.count_in_log("output queue limit exceeded"), 1);
    assert!(!env.daemon_log().contains("00000:中文"));
    env.type_on_seat("seat0", "ok ")?;
    editor.wait_for_text(" OK ")?;
    Ok(())
}

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
