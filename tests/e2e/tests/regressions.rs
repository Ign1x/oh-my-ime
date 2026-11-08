//! Regression tests for the input correctness and engine isolation audit.
//! See typing.rs for the private headless environment used to run these tests.

use std::time::Duration;

use omime_e2e::harness::{
    TestEnv, dummy_engine, echo_engine, forwarding_engine, hosted_engine, sparkle_engine,
    wait_until,
};

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn late_reply_from_previous_engine_preserves_new_composition() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[dummy_engine(&["--slow-ms", "1200"]), echo_engine()],
        Some("dummy"),
    )?;
    env.configure("[engine]\nkey_timeout_ms = 3000\n")?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&[])?;
    env.type_text("a")?;
    env.wait_for_log("processing delayed key")?;
    env.omime(&["switch", "echo"])?;
    env.type_text("bc")?;
    editor.wait_for("new engine composition", |l| {
        l.starts_with("preedit \"bc\"")
    })?;
    // The old call is cancelled locally, so no daemon reply log is expected.
    // Observe its completion in the engine before checking the new composition.
    env.wait_for_log("finished delayed key")?;
    env.key(&[], "space")?;
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        editor.text(),
        "aBC ",
        "late reply must not reset the new engine; events: {:?}",
        editor.lines()
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn simultaneous_seats_keep_compositions_separate() -> anyhow::Result<()> {
    independent_compositions(echo_engine(), "ab", "cd ", " ", "AB ", "CD ")
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn sparkle_keeps_each_fields_query() -> anyhow::Result<()> {
    independent_compositions(sparkle_engine(), ":rocket", ":heart ", " ", "🚀", "❤️")
}

fn independent_compositions(
    engine: omime_e2e::harness::TestEngine,
    first: &str,
    second: &str,
    finish: &str,
    expected_a: &str,
    expected_b: &str,
) -> anyhow::Result<()> {
    let id = engine.id;
    let env = TestEnv::start_with_seats(&[engine], Some(id), &["seat1"])?;
    let a = env.editor(&["--app-id", "a"])?;
    let b = env.spawn_editor(&["--app-id", "b", "--seat", "seat1"])?;
    b.wait_for("seat1 editor", |l| l == "ready")?;
    env.wait_for_log("keyboard grabbed seat=\"seat1\"")?;
    let grabs = env.count_in_log("keyboard grabbed seat=\"seat0\"");
    wait_until("seat0 to focus a", || {
        env.click_on_seat("seat0", 100, 50).ok()?;
        std::thread::sleep(Duration::from_millis(100));
        (env.count_in_log("keyboard grabbed seat=\"seat0\"") > grabs).then_some(())
    })?;
    type_composition(&env, "seat0", first)?;
    let preedit = format!("preedit {first:?}");
    a.wait_for("seat0 preedit", |l| l.starts_with(&preedit))?;
    type_composition(&env, "seat1", second)?;
    b.wait_for_text(expected_b)?;
    env.type_on_seat("seat0", finish)?;
    a.wait_for_text(expected_a)?;
    assert_eq!(
        (a.text(), b.text()),
        (expected_a.into(), expected_b.into()),
        "compositions from independent seats must not mix"
    );
    Ok(())
}

fn type_composition(env: &TestEnv, seat: &str, text: &str) -> anyhow::Result<()> {
    // The test keyboard's --type accepts only letters, digits and spaces.
    let text = if let Some(query) = text.strip_prefix(':') {
        env.keys_on_seat(seat, "press shift\ntap 39\nrelease shift\n")?;
        query
    } else {
        text
    };
    env.type_on_seat(seat, text)
}

struct ResumeOnDrop(rustix::process::Pid);

impl Drop for ResumeOnDrop {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.0, rustix::process::Signal::CONT);
    }
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn timeout_recovery_finishes_before_a_new_composition() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    env.configure("engines = [\"echo\"]\n[engine]\nkey_timeout_ms = 500\n")?;
    let editor = env.editor(&[])?;
    env.type_text("x")?;
    editor.wait_for("initial composition", |l| l.starts_with("preedit \"x\""))?;
    let pid = rustix::process::Pid::from_raw(env.engine_pid("echo").expect("echo runs") as i32)
        .expect("valid pid");
    rustix::process::kill_process(pid, rustix::process::Signal::STOP)?;
    let resume = ResumeOnDrop(pid);
    env.type_text("a")?;
    editor.wait_for_text("a")?;
    // These keys bypass the still-busy engine, rather than extend its old word.
    env.type_text("bc")?;
    editor.wait_for_text("abc")?;
    drop(resume);
    env.wait_for_log("discarding a late answer")?;
    env.type_text("de")?;
    editor.wait_for("new composition", |l| l.starts_with("preedit \"de\""))?;
    env.type_text(" ")?;
    editor.wait_for_text("abcDE ")?;
    assert_eq!(env.count_in_log("engine launched engine=\"echo\""), 1);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn focus_out_finishes_in_flight_fcitx_key() -> anyhow::Result<()> {
    fcitx_pending_key_finishes("FocusOut", false)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn destroyed_context_finishes_in_flight_fcitx_key() -> anyhow::Result<()> {
    fcitx_pending_key_finishes("DestroyIC", false)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn sensitive_context_finishes_in_flight_fcitx_key() -> anyhow::Result<()> {
    fcitx_pending_key_finishes("SetCapability", false)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn deactivation_finishes_in_flight_fcitx_batches() -> anyhow::Result<()> {
    for method in ["FocusOut", "DestroyIC", "SetCapability"] {
        fcitx_pending_key_finishes(method, true)?;
    }
    Ok(())
}

fn fcitx_pending_key_finishes(method: &str, batch: bool) -> anyhow::Result<()> {
    let env = TestEnv::start(&[dummy_engine(&["--slow-ms", "450"])], Some("dummy"))?;
    env.wait_for_log("engine ready")?;
    let address = String::from_utf8(
        env.command("printenv")
            .arg("DBUS_SESSION_BUS_ADDRESS")
            .output()?
            .stdout,
    )?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let conn = zbus::connection::Builder::address(address.trim())
                .build()
                .await?;
            let service = zbus::Proxy::new(
                &conn,
                "org.fcitx.Fcitx5",
                "/org/freedesktop/portal/inputmethod",
                "org.fcitx.Fcitx.InputMethod1",
            )
            .await?;
            let (path, _): (zbus::OwnedObjectPath, Vec<u8>) = service
                .call(
                    "CreateInputContext",
                    &vec![("program", "audit"), ("display", "x11:")],
                )
                .await?;
            let ic = zbus::Proxy::new(
                &conn,
                "org.fcitx.Fcitx5",
                path,
                "org.fcitx.Fcitx.InputContext1",
            )
            .await?;
            let _: () = ic.call("FocusIn", &()).await?;
            tokio::time::sleep(Duration::from_millis(100)).await;
            let key = async {
                let args = (0x61_u32, 38_u32, 0_u32, false, 1_u32);
                if batch {
                    let (actions, handled): (Vec<(u32, zbus::OwnedVariant)>, bool) =
                        ic.call("ProcessKeyEventBatch", &args).await?;
                    assert!(
                        actions.is_empty(),
                        "old actions reached a deactivated field"
                    );
                    Ok(handled)
                } else {
                    ic.call::<_, _, bool>("ProcessKeyEvent", &args).await
                }
            };
            let focus_out = async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if method == "SetCapability" {
                    ic.call::<_, _, ()>(method, &(1_u64 << 3)).await
                } else {
                    ic.call::<_, _, ()>(method, &()).await
                }
            };
            let (key, focus_out) =
                tokio::join!(tokio::time::timeout(Duration::from_secs(3), key), focus_out);
            focus_out?;
            assert!(
                key.is_ok(),
                "{method} left the key unanswered for 3 seconds (batch={batch}): {key:?}"
            );
            assert!(!key??, "the abandoned key must be returned to the client");
            anyhow::Ok(())
        })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_repeat_protocol_fault_does_not_terminate_the_daemon() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[dummy_engine(&["--misbehave", "viewport"]), echo_engine()],
        Some("dummy"),
    )?;
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    while std::time::Instant::now() < deadline {
        if env
            .daemon_log()
            .contains("the connection keeps failing; giving up")
        {
            break;
        }
        if env
            .daemon_log()
            .contains("engine keeps crashing; giving up")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let log = env.daemon_log();
    assert!(
        !log.contains("the connection keeps failing; giving up"),
        "one broken engine terminated the whole daemon after {} lost connections",
        log.matches("lost the compositor connection").count()
    );
    assert!(
        env.omime(&["status"]).is_ok(),
        "daemon must remain reachable"
    );
    assert!(
        log.contains("engine keeps crashing; giving up"),
        "the bad engine was not stopped: {log}"
    );
    assert_eq!(env.count_in_log("lost the compositor connection"), 0);
    env.omime(&["switch", "echo"])?;
    let editor = env.editor(&[])?;
    env.type_text("healthy ")?;
    editor.wait_for_text("HEALTHY ")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn delayed_forwarding_preserves_the_keys_original_shift() -> anyhow::Result<()> {
    let env = TestEnv::start(&[dummy_engine(&["--slow-ms", "80"])], Some("dummy"))?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&["--print-modifiers"])?;
    env.keys_on_seat(
        "seat0",
        "press shift\nsleep 20\ntap a\nsleep 20\nrelease shift\nsleep 800\n",
    )?;
    assert_eq!(
        editor.text(),
        "A",
        "forwarding must use the modifiers at keypress time; events: {:?}",
        editor.lines()
    );
    env.type_on_seat("seat0", "z")?;
    editor.wait_for_text("Az")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn hosted_forwarding_preserves_modifiers_and_restores_current_state() -> anyhow::Result<()> {
    let env = TestEnv::start(&[hosted_engine(&["--slow-ms", "80"])], Some("hosted"))?;
    let editor = env.editor(&["--print-modifiers"])?;
    env.wait_for_hosted("hosted activation", |l| l == "activate")?;
    env.keys_on_seat(
        "seat0",
        "press shift\nsleep 20\ntap 1\nsleep 20\nrelease shift\nsleep 800\n",
    )?;
    editor.wait_for_text("!")?;
    env.type_on_seat("seat0", "1")?;
    editor.wait_for_text("!1")?;
    Ok(())
}

/// Exactly one complete stroke, including its order, after all queued replies.
fn forwarded_stroke(mode: &str, script: &str) -> anyhow::Result<()> {
    let env = TestEnv::start_with_wayland_trace(&[forwarding_engine(mode)], Some("forwarding"))?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&[])?;
    env.keys_on_seat("seat0", script)?;
    // The second stroke is a barrier: its reply follows both events of the first.
    env.keys_on_seat("seat0", "tap z\nsleep 300\n")?;
    editor.wait_for("the barrier release", |line| line == "release \"z\"")?;
    let events: Vec<_> = editor
        .lines()
        .into_iter()
        .filter(|line| line.starts_with("key ") || line.starts_with("release "))
        .collect();
    assert_eq!(
        events,
        ["key \"a\"", "release \"a\"", "key \"z\"", "release \"z\""]
    );
    assert_eq!(editor.text(), "az");
    assert_forwarded_wire(&env, &[(30, 1), (30, 0), (44, 1), (44, 0)]);
    Ok(())
}

fn assert_forwarded_wire(env: &TestEnv, expected: &[(u32, u32)]) {
    let log = env.daemon_log();
    let events: Vec<_> = log
        .lines()
        .filter(|line| line.contains("<= zwp_virtual_keyboard_v1#") && line.contains(".key("))
        .map(|line| {
            let (_, key) = line
                .split_once(", key: ")
                .expect("key event contains a code");
            let (key, state) = key
                .split_once(", state: ")
                .expect("key event contains a state");
            (
                key.parse::<u32>().expect("numeric key code"),
                state
                    .trim_end_matches(')')
                    .parse::<u32>()
                    .expect("numeric key state"),
            )
        })
        .collect();
    assert_eq!(
        events, expected,
        "outgoing virtual-keyboard events must match exactly"
    );
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_reply_releasing_another_forwarded_key_sends_one_wire_release() -> anyhow::Result<()> {
    let env = TestEnv::start_with_wayland_trace(
        &[forwarding_engine("forward-cross-release")],
        Some("forwarding"),
    )?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&[])?;
    // A is forwarded first. While B's reply is pending, A-up arrives at the
    // grab; B then forwards A-up before A's own release decision is processed.
    env.keys_on_seat(
        "seat0",
        "press a\nsleep 150\npress b\nsleep 20\nrelease a\nrelease b\nsleep 350\ntap z\nsleep 300\n",
    )?;
    editor.wait_for("the barrier release", |line| line == "release \"z\"")?;
    let log = env.daemon_log();
    let physical_release = log
        .lines()
        .position(|line| {
            line.contains("-> zwp_input_method_keyboard_grab_v2#")
                && line.ends_with("key: 30, state: RELEASED)")
        })
        .expect("A-up arrived at the keyboard grab");
    let forwarded_release = log
        .lines()
        .position(|line| {
            line.contains("<= zwp_virtual_keyboard_v1#") && line.ends_with("key: 30, state: 0)")
        })
        .expect("B's reply forwarded A-up");
    assert!(
        physical_release < forwarded_release,
        "the physical release must precede B's reply"
    );
    // Sway discards duplicate releases before delivering them to the editor.
    // Check the daemon's outgoing requests so that filtering cannot hide a bug.
    assert_forwarded_wire(&env, &[(30, 1), (30, 0), (44, 1), (44, 0)]);
    assert_eq!(editor.text(), "az");
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn native_forward_actions_preserve_release() -> anyhow::Result<()> {
    forwarded_stroke("forward", "press a\nsleep 150\nrelease a\nsleep 250\n")
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn an_engine_forwarding_press_and_release_gives_one_release() -> anyhow::Result<()> {
    // Press-only forwarding, both events in the press reply, and a physical
    // release before either reply all leave exactly one complete stroke. A rejected
    // text transaction must still complete the earlier accepted press.
    for (mode, script) in [
        (
            "forward-press",
            "press a\nsleep 150\nrelease a\nsleep 250\n",
        ),
        (
            "forward-stroke",
            "press a\nsleep 150\nrelease a\nsleep 250\n",
        ),
        ("slow-forward", "tap a\nsleep 400\n"),
        (
            "forward-release-overflow",
            "press a\nsleep 150\nrelease a\nsleep 250\n",
        ),
    ] {
        forwarded_stroke(mode, script)?;
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn native_forward_actions_preserve_original_shift() -> anyhow::Result<()> {
    let env = TestEnv::start(&[forwarding_engine("slow-forward")], Some("forwarding"))?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&["--print-modifiers"])?;
    env.keys_on_seat(
        "seat0",
        "press shift\nsleep 20\ntap a\nsleep 20\nrelease shift\nsleep 800\n",
    )?;
    assert_eq!(editor.text(), "A", "events: {:?}", editor.lines());
    env.keys_on_seat("seat0", "tap z\nsleep 400\n")?;
    editor.wait_for_text("Az")?;
    let lines = editor.lines();
    let strokes: Vec<_> = lines
        .iter()
        .filter(|line| {
            line.starts_with("key ")
                || matches!(
                    line.as_str(),
                    "release \"A\"" | "release \"a\"" | "release \"z\""
                )
        })
        .map(String::as_str)
        .collect();
    assert_eq!(
        strokes,
        ["key \"A\"", "release \"A\"", "key \"z\"", "release \"z\""]
    );
    assert_eq!(
        lines
            .iter()
            .rev()
            .find(|line| line.starts_with("active "))
            .map(String::as_str),
        Some("active ")
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_forwarded_handled_press_starts_no_daemon_repeat() -> anyhow::Result<()> {
    for mode in ["forward", "forward-stroke"] {
        let env = TestEnv::start(&[forwarding_engine(mode)], Some("forwarding"))?;
        env.wait_for_log("engine ready")?;
        assert!(
            env.command("swaymsg")
                .args(["input", "type:keyboard", "repeat_delay", "50"])
                .stdout(std::process::Stdio::null())
                .status()?
                .success()
        );
        let editor = env.editor(&[])?;
        env.keys_on_seat(
            "seat0",
            "press a\nsleep 350\nrelease a\nsleep 150\ntap z\nsleep 150\n",
        )?;
        editor.wait_for("the barrier release", |line| line == "release \"z\"")?;
        let keys = std::fs::read_to_string(env.dir().join("forward-keys.log"))?;
        assert_eq!(
            keys.lines()
                .filter(|line| line.starts_with("30 "))
                .collect::<Vec<_>>(),
            ["30 false false", "30 true false"],
            "forwarded key was repeated by the daemon: {keys}"
        );
        let strokes: Vec<_> = editor
            .lines()
            .into_iter()
            .filter(|line| line.starts_with("key ") || line.starts_with("release "))
            .collect();
        assert_eq!(
            strokes,
            ["key \"a\"", "release \"a\"", "key \"z\"", "release \"z\""]
        );
        assert_eq!(editor.text(), "az");
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn out_of_band_forward_key_keeps_current_modifiers() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[forwarding_engine("out-of-band-forward")],
        Some("forwarding"),
    )?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&["--print-modifiers"])?;
    env.keys_on_seat(
        "seat0",
        "press shift\ntap a\nrelease shift\nsleep 300\ntap z\nsleep 150\n",
    )?;
    editor.wait_for_text("az")?;
    let lines = editor.lines();
    let strokes: Vec<_> = lines
        .iter()
        .filter(|line| {
            line.starts_with("key ") || matches!(line.as_str(), "release \"a\"" | "release \"z\"")
        })
        .map(String::as_str)
        .collect();
    assert_eq!(
        strokes,
        ["key \"a\"", "release \"a\"", "key \"z\"", "release \"z\""]
    );
    assert_eq!(
        lines
            .iter()
            .rev()
            .find(|line| line.starts_with("active "))
            .map(String::as_str),
        Some("active ")
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn native_fcitx_forward_uses_the_in_flight_keys_modifiers() -> anyhow::Result<()> {
    let env = TestEnv::start(&[forwarding_engine("slow-forward")], Some("forwarding"))?;
    env.wait_for_log("engine ready")?;
    let address = String::from_utf8(
        env.command("printenv")
            .arg("DBUS_SESSION_BUS_ADDRESS")
            .output()?
            .stdout,
    )?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            // No X server is needed; this is the same portal API available to
            // sandboxed applications loading the Fcitx module.
            let conn = zbus::connection::Builder::address(address.trim())
                .build()
                .await?;
            let service = zbus::Proxy::new(
                &conn,
                "org.freedesktop.portal.Fcitx",
                "/org/freedesktop/portal/inputmethod",
                "org.fcitx.Fcitx.InputMethod1",
            )
            .await?;
            let (path, _): (zbus::OwnedObjectPath, Vec<u8>) = service
                .call(
                    "CreateInputContext",
                    &vec![("program", "review-input"), ("display", "x11:")],
                )
                .await?;
            let ic = zbus::Proxy::new(
                &conn,
                "org.freedesktop.portal.Fcitx",
                path,
                "org.fcitx.Fcitx.InputContext1",
            )
            .await?;
            let _: () = ic.call("FocusIn", &()).await?;
            tokio::time::sleep(Duration::from_millis(100)).await;
            type Batch = (Vec<(u32, zbus::OwnedVariant)>, bool);
            let first_args = (0x41_u32, 38_u32, 1_u32, false, 1_u32);
            let first = ic.call::<_, _, Batch>("ProcessKeyEventBatch", &first_args);
            let later = async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                ic.call::<_, _, Batch>(
                    "ProcessKeyEventBatch",
                    &(0x62_u32, 56_u32, 0_u32, false, 2_u32),
                )
                .await
            };
            let (first, later) =
                tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(first, later) })
                    .await?;
            later?;
            let (actions, handled) = first?;
            assert!(handled);
            let forwarded: Vec<(u32, u32, bool)> = actions
                .into_iter()
                .filter(|(kind, _)| *kind == 2)
                .map(|(_, value)| value.try_into())
                .collect::<Result<_, _>>()?;
            assert_eq!(
                forwarded,
                [(0x41, 1, false)],
                "the later unshifted key must not change the forwarded Shift+A"
            );
            anyhow::Ok(())
        })
}
