//! End-to-end tests for hosted input methods: unmodified Wayland input methods running
//! as engines, played by `omime-im-client`, which composes like the echo engine.
//!
//! See `typing.rs` for how to run them.

use std::time::{Duration, Instant};

use omime_e2e::harness::{
    FcitxClient, TIMEOUT, TestEnv, echo_engine, hosted_engine, target_dir, wait_until,
};

/// The input method's popup with these arguments: green, 100 × 40.
const ARGS: [&str; 4] = ["--color", "00ff00", "--size", "100x40"];
const GREEN: u32 = 0x00ff00;

/// The centre of the popup when it opens below the test editor's text cursor, which is
/// at x = 12, y = 20 with a height of 18 in an empty editor.
const CENTRE: (u32, u32) = (12 + 50, 20 + 18 + 20);

/// Lets a stopped process go on when a test ends, also when it fails: a stopped process
/// would outlive the test and keep its output open.
struct Resume(rustix::process::Pid);

impl Drop for Resume {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.0, rustix::process::Signal::CONT);
    }
}

/// Waits until the input method said `line` `count` times in all.
fn wait_for_count(env: &TestEnv, line: &str, count: usize) -> anyhow::Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    while env.hosted_lines().iter().filter(|l| *l == line).count() < count {
        anyhow::ensure!(
            Instant::now() < deadline,
            "expected {count} × {line:?} from the input method, got {:?}",
            env.hosted_lines()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn hosted_input_methods_compose_and_commit() -> anyhow::Result<()> {
    let env = TestEnv::start(&[hosted_engine(&[])], Some("hosted"))?;
    env.wait_for_hosted("the input method to start", |l| l == "ready")?;
    let editor = env.editor(&[])?;
    env.wait_for_hosted("the input method to be activated", |l| l == "activate")?;
    env.type_text("hello world")?;
    env.key(&[], "Return")?;
    // The input method forwards Return through its virtual keyboard, after the word.
    editor.wait_for("the Return key", |l| l == "enter")?;
    editor.wait_for_text("HELLO WORLD")?;
    let preedits = editor.preedits();
    for expected in ["h", "hel", "hello", "wor"] {
        assert!(
            preedits.iter().any(|p| p == expected),
            "{expected:?} in {preedits:?}"
        );
    }
    // Wayland fields get the compositor's repeat rate: the input method repeats keys.
    assert!(
        env.hosted_lines()
            .iter()
            .any(|l| l.starts_with("repeat ") && l != "repeat 0 0"),
        "{:?}",
        env.hosted_lines()
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn hosted_popups_open_below_the_text_cursor() -> anyhow::Result<()> {
    let env = TestEnv::start(&[hosted_engine(&ARGS)], Some("hosted"))?;
    let editor = env.editor(&[])?;
    env.wait_for_hosted("the input method to be activated", |l| l == "activate")?;
    env.type_text("ab")?;
    env.wait_for_pixel(CENTRE.0, CENTRE.1, GREEN)?;
    // The popup learns where the text cursor is, as from a compositor.
    env.wait_for_hosted("the text cursor", |l| l == "rect 0 -18 2 18")?;

    // The input method destroys its popup once the word is committed.
    env.type_text(" ")?;
    editor.wait_for_text("AB ")?;
    let deadline = Instant::now() + TIMEOUT;
    while env.pixel(CENTRE.0, CENTRE.1)? == GREEN {
        anyhow::ensure!(Instant::now() < deadline, "the popup stayed");
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn hosted_input_methods_switch_with_other_engines() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine(), hosted_engine(&ARGS)], Some("hosted"))?;
    let editor = env.editor(&[])?;
    env.wait_for_hosted("the input method to be activated", |l| l == "activate")?;
    env.type_text("ab")?;
    editor.wait_for("the preedit `ab`", |l| l.starts_with("preedit \"ab\""))?;
    env.wait_for_pixel(CENTRE.0, CENTRE.1, GREEN)?;

    // Leaving the input method takes its preedit and its popup away.
    env.key(&["ctrl"], "space")?;
    env.wait_for_status("direct input")?;
    env.wait_for_hosted("the input method to be deactivated", |l| l == "deactivate")?;
    env.type_text("cd")?;
    editor.wait_for_text("cd")?;
    assert_eq!(editor.preedits().last().map(String::as_str), Some(""));
    assert_ne!(env.pixel(CENTRE.0, CENTRE.1)?, GREEN, "the popup stayed");

    env.omime(&["switch", "echo"])?;
    env.type_text("ef ")?;
    editor.wait_for_text("cdEF ")?;

    env.omime(&["switch", "hosted"])?;
    wait_for_count(&env, "activate", 2)?;
    env.type_text("gh ")?;
    editor.wait_for_text("cdEF GH ")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn keys_the_input_method_forwarded_are_released_with_the_real_key() -> anyhow::Result<()> {
    // The input method forwards presses but keeps every release, as Fcitx 5 does for
    // keys that were part of a hotkey.
    let env = TestEnv::start(&[hosted_engine(&["--keep-releases"])], Some("hosted"))?;
    let editor = env.editor(&[])?;
    env.wait_for_hosted("the input method to be activated", |l| l == "activate")?;
    env.type_text("1")?;
    editor.wait_for("the key", |l| l == "key \"1\"")?;
    // The application must not hold the key until the field is left.
    editor.wait_for("its release", |l| l == "release \"1\"")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn keys_a_slow_input_method_forwarded_late_are_not_held() -> anyhow::Result<()> {
    // The key is up before the input method sends its press back; with or without a
    // release of its own, the application must not hold the key.
    for args in [
        &["--slow-ms", "300"][..],
        &["--slow-ms", "300", "--keep-releases"],
    ] {
        let env = TestEnv::start(&[hosted_engine(args)], Some("hosted"))?;
        let editor = env.editor(&[])?;
        env.wait_for_hosted("the input method to be activated", |l| l == "activate")?;
        env.type_text("1")?;
        editor.wait_for("the key", |l| l == "key \"1\"")?;
        editor.wait_for("its release", |l| l == "release \"1\"")?;
        std::thread::sleep(Duration::from_millis(300));
        let presses = editor.lines().iter().filter(|l| *l == "key \"1\"").count();
        assert_eq!(presses, 1, "{args:?}: {:?}", editor.lines());
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_crashing_hosted_input_method_is_restarted() -> anyhow::Result<()> {
    let env = TestEnv::start(&[hosted_engine(&[])], Some("hosted"))?;
    let editor = env.editor(&[])?;
    env.wait_for_hosted("the input method to be activated", |l| l == "activate")?;
    env.type_text("ab ")?;
    editor.wait_for_text("AB ")?;

    let pid = env
        .engine_pid("hosted")
        .expect("the input method was launched");
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(pid as i32).expect("valid pid"),
        rustix::process::Signal::KILL,
    )?;
    env.wait_for_log("engine exited unexpectedly")?;
    // While the input method is gone, typing still works.
    env.type_text("cd")?;
    editor.wait_for_text("AB cd")?;

    // Once it is back, it serves the focused field again.
    wait_for_count(&env, "activate", 2)?;
    env.type_text(" ef ")?;
    editor.wait_for_text("AB cd EF ")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_hung_hosted_input_method_is_restarted() -> anyhow::Result<()> {
    let env = TestEnv::start(&[hosted_engine(&[])], Some("hosted"))?;
    let editor = env.editor(&[])?;
    env.wait_for_hosted("the input method to be activated", |l| l == "activate")?;
    env.type_text("ab ")?;
    editor.wait_for_text("AB ")?;

    // A stopped process reads nothing from its socket, like a hung one.
    let pid = env
        .engine_pid("hosted")
        .expect("the input method was launched");
    let pid = rustix::process::Pid::from_raw(pid as i32).expect("valid pid");
    rustix::process::kill_process(pid, rustix::process::Signal::STOP)?;
    let _resume = Resume(pid);
    // These keys are lost with the input method; omimed notices it does not read them.
    env.type_text("cd")?;
    env.wait_for_log("does not read what it is sent")?;
    env.wait_for_log("engine exited unexpectedly")?;

    // The new input method serves the field again.
    wait_for_count(&env, "activate", 2)?;
    env.type_text("ef ")?;
    editor.wait_for_text("AB EF ")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn what_engines_print_goes_to_the_log_under_their_id() -> anyhow::Result<()> {
    // The input method prints its events to stdout too.
    let env = TestEnv::start(&[hosted_engine(&[])], Some("hosted"))?;
    env.wait_for_hosted("the input method to start", |l| l == "ready")?;
    omime_e2e::harness::wait_until("the input method's line in omimed's log", || {
        env.daemon_log()
            .lines()
            .any(|l| l.contains(" engine: ready") && l.contains("engine=\"hosted\""))
            .then_some(())
    })?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn engine_settings_open_on_the_desktop_and_the_engine_reads_them() -> anyhow::Result<()> {
    let env = TestEnv::start(&[hosted_engine(&[])], Some("hosted"))?;
    env.wait_for_hosted("the input method to start", |l| l == "ready")?;
    // Settings like Fcitx 5's, which talk to the input method on its bus, and like
    // IBus's, which it reads only when it starts.
    let marker = env.dir().join("settings-saw");
    let manifest = env.dir().join("engines/hosted/engine.toml");
    let mut text = std::fs::read_to_string(&manifest)?;
    text += &format!(
        "\n[engine.settings]\nexec = [\"sh\", \"-c\", \"echo $WAYLAND_DISPLAY $DBUS_SESSION_BUS_ADDRESS > {}\"]\n\
         bus = \"engine\"\nrestart = true\n",
        marker.display()
    );
    std::fs::write(&manifest, text)?;
    env.omime(&["reload"])?;

    env.omime(&["settings", "hosted"])?;
    let saw = omime_e2e::harness::wait_until("the settings program", || {
        std::fs::read_to_string(&marker)
            .ok()
            .filter(|t| t.ends_with('\n'))
    })?;
    let (display, bus) = saw.trim().split_once(' ').unwrap_or_default();
    // The compositor's display, not the engine's bridge.
    assert!(
        !display.is_empty() && !display.contains("oh-my-ime"),
        "{saw}"
    );
    assert!(bus.contains("oh-my-ime/hosted/bus"), "{saw}");
    // It closed: the input method starts again, to read what was changed.
    env.wait_for_log("restarting the engine to read its settings")?;
    wait_for_count(&env, "ready", 2)?;
    assert_eq!(env.count_in_log("engine exited unexpectedly"), 0);
    // An engine without settings says so.
    let without = env.omime_status(&["settings", "nothing"])?;
    assert!(!without.0.success());
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn hosted_input_methods_run_on_a_private_bus() -> anyhow::Result<()> {
    let env = TestEnv::start(&[hosted_engine(&[])], Some("hosted"))?;
    env.wait_for_hosted("the input method to start", |l| l == "ready")?;
    let pid = env
        .engine_pid("hosted")
        .expect("the input method was launched");
    let environ = std::fs::read(format!("/proc/{pid}/environ"))?;
    let vars: Vec<String> = environ
        .split(|b| *b == 0)
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .collect();
    let bus = vars
        .iter()
        .find_map(|v| v.strip_prefix("DBUS_SESSION_BUS_ADDRESS="))
        .unwrap_or_default();
    let private = env.dir().join("oh-my-ime/hosted/bus");
    assert!(
        bus.contains(&*private.to_string_lossy()),
        "the input method talks to {bus:?}"
    );
    // It is not an engine of the engine protocol, and sees no X display.
    assert!(
        !vars.iter().any(|v| v.starts_with("OMIME_ENGINE_ADDRESS=")),
        "{vars:?}"
    );
    assert!(!vars.iter().any(|v| v.starts_with("DISPLAY=")), "{vars:?}");
    Ok(())
}

/// Types the key `keyval` (X11 key code `keycode`) with the Fcitx client: press and
/// release, waiting for both replies.
fn fcitx_key(client: &FcitxClient, keyval: u32, keycode: u32) -> anyhow::Result<()> {
    for action in ["press", "release"] {
        let replies = client
            .lines()
            .iter()
            .filter(|l| l.starts_with("reply "))
            .count();
        client.send(&format!("key {keyval:#x} {keycode} 0 {action}"))?;
        client.wait_for("the reply to a key", |_| {
            client
                .lines()
                .iter()
                .filter(|l| l.starts_with("reply "))
                .count()
                > replies
        })?;
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn fcitx_clients_type_through_hosted_input_methods() -> anyhow::Result<()> {
    let env = TestEnv::start(&[hosted_engine(&[])], Some("hosted"))?;
    env.wait_for_hosted("the input method to start", |l| l == "ready")?;
    let client = env.fcitx_client(&[])?;
    client.send("focus-in")?;
    env.wait_for_hosted("the input method to be activated", |l| l == "activate")?;
    // Applications of the X11 frontends repeat keys themselves.
    env.wait_for_hosted("no repeat", |l| l == "repeat 0 0")?;

    fcitx_key(&client, 0x61, 38)?;
    client.wait_for("the preedit `a`", |l| l.starts_with("preedit \"a\""))?;
    fcitx_key(&client, 0x62, 56)?;
    client.wait_for("the preedit `ab`", |l| l.starts_with("preedit \"ab\""))?;
    // Space commits the word; the input method sends the space itself back to the
    // application, through its virtual keyboard.
    fcitx_key(&client, 0x20, 65)?;
    client.wait_for("the commit", |l| l == "commit \"AB\"")?;
    client.wait_for("the space", |l| l == "forward 32 0 false")?;
    client.wait_for("its release", |l| l == "forward 32 0 true")?;
    let lines = client.lines();
    let commit = lines.iter().position(|l| l == "commit \"AB\"");
    let space = lines.iter().position(|l| l == "forward 32 0 false");
    assert!(commit < space, "{lines:?}");
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn xim_clients_type_through_hosted_input_methods() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[hosted_engine(&[])], Some("hosted"))?;
    env.wait_for_hosted("the input method to start", |l| l == "ready")?;
    let client = env.xim_client(&[])?;
    env.type_on_seat("seat0", "ab ")?;
    client.wait_for_text("AB ")?;
    // XIM passes only presses on; the input method sees every key released, too.
    let lines = env.hosted_lines();
    for line in ["key a pressed", "key a released", "key space released"] {
        assert!(lines.iter().any(|l| l == line), "{line:?} in {lines:?}");
    }
    // The releases the input method sends back must not reach the application: it
    // gets them from the X server, and a release from the input method makes libX11
    // take the next real press for one the input method sent back.
    let lines = client.lines();
    assert!(
        !lines.iter().any(|l| l.starts_with("release ")),
        "a release reached the application: {lines:?}"
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn hosted_popups_follow_the_text_cursor_of_x11_applications() -> anyhow::Result<()> {
    let env = TestEnv::start(&[hosted_engine(&ARGS)], Some("hosted"))?;
    env.wait_for_hosted("the input method to start", |l| l == "ready")?;
    let client = env.fcitx_client(&["--display", "x11:"])?;
    client.send("focus-in")?;
    client.send("cursor 300 200 2 20")?;
    env.wait_for_hosted("the input method to be activated", |l| l == "activate")?;
    fcitx_key(&client, 0x61, 38)?;
    // No Wayland text field has focus: the popup opens below the X11 text cursor, on
    // a layer surface of omimed's, and learns where the cursor is relative to it.
    env.wait_for_pixel(300 + 50, 200 + 20 + 20, GREEN)?;
    env.wait_for_hosted("the text cursor", |l| l == "rect 0 -20 2 20")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn windows_hosted_input_methods_open_show_on_the_desktop() -> anyhow::Result<()> {
    // As Fcitx 5 starts its configuration tool, or Qingjian its settings.
    let window = format!(
        "{} --no-text-field --app-id opened-by-the-input-method",
        target_dir().join("omime-ti-client").display()
    );
    let env = TestEnv::start(&[hosted_engine(&["--spawn", &window])], Some("hosted"))?;
    env.wait_for_log("hosted input method ready")?;
    wait_until("the window on the desktop", || {
        env.has_window("opened-by-the-input-method")
            .ok()?
            .then_some(())
    })?;
    Ok(())
}
