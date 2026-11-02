//! Focused audit probes against d9899fc. These assert the desired behavior.
//! Copy temporarily to tests/e2e/tests/audit_probe.rs and run inside nix develop .#e2e.

use std::time::Duration;

use omime_e2e::harness::{TestEnv, dummy_engine, echo_engine, wait_until};

#[test]
#[ignore = "audit reproduction; requires the e2e environment"]
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
#[ignore = "audit reproduction; requires the e2e environment"]
fn simultaneous_seats_keep_compositions_separate() -> anyhow::Result<()> {
    let env = TestEnv::start_with_seats(&[echo_engine()], Some("echo"), &["seat1"])?;
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
    env.type_on_seat("seat0", "ab")?;
    a.wait_for("seat0 preedit", |l| l.starts_with("preedit \"ab\""))?;
    env.type_on_seat("seat1", "cd ")?;
    env.type_on_seat("seat0", " ")?;
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        (a.text(), b.text()),
        ("AB ".into(), "CD ".into()),
        "compositions from independent seats must not mix"
    );
    Ok(())
}

#[test]
#[ignore = "audit reproduction; requires the e2e environment"]
fn focus_out_finishes_in_flight_fcitx_key() -> anyhow::Result<()> {
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
            let key =
                ic.call::<_, _, bool>("ProcessKeyEvent", &(0x61_u32, 38_u32, 0_u32, false, 1_u32));
            let focus_out = async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                ic.call::<_, _, ()>("FocusOut", &()).await
            };
            let (key, focus_out) =
                tokio::join!(tokio::time::timeout(Duration::from_secs(3), key), focus_out);
            focus_out?;
            assert!(
                key.is_ok(),
                "FocusOut left ProcessKeyEvent unanswered for 3 seconds: {key:?}"
            );
            anyhow::Ok(())
        })
}

#[test]
#[ignore = "audit reproduction; requires the e2e environment"]
fn a_repeat_protocol_fault_does_not_terminate_the_daemon() -> anyhow::Result<()> {
    let env = TestEnv::start(&[dummy_engine(&["--misbehave", "viewport"])], Some("dummy"))?;
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
    Ok(())
}

#[test]
#[ignore = "audit reproduction; requires the e2e environment"]
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
    Ok(())
}
