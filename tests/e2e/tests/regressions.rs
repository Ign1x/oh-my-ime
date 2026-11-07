//! Regression tests for the input correctness and engine isolation audit.
//! See typing.rs for the private headless environment used to run these tests.

use std::time::Duration;

use omime_e2e::harness::{
    TestEnv, dummy_engine, echo_engine, hosted_engine, sparkle_engine, wait_until,
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
    env.omime(&["switch", "echo"])?;
    env.type_text("bc")?;
    editor.wait_for("new engine composition", |l| {
        l.starts_with("preedit \"bc\"")
    })?;
    env.wait_for_log("discarding a late answer")?;
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
