//! End-to-end tests of the Rime engine, `omime-rime`, with the Rime data of the dev
//! shell (`$OMIME_RIME_SHARED_DATA`). See `typing.rs` for how to run them.

use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use omime_e2e::harness::{
    TestEngine, TestEnv, program_dir, target_dir, wait_until, wait_until_within,
};

fn rime() -> Option<TestEngine> {
    if std::env::var_os("OMIME_RIME_SHARED_DATA").is_none() {
        eprintln!("no OMIME_RIME_SHARED_DATA (Rime's data); nothing to test");
        return None;
    }
    // The Nix package of the platform comes without it (nix/rime.nix builds it).
    let program = [program_dir(), target_dir()]
        .into_iter()
        .map(|dir| dir.join("omime-rime"))
        .find(|path| path.is_file())?;
    Some(TestEngine {
        id: "rime",
        program,
        args: Vec::new(),
        env: Vec::new(),
        hosted: false,
    })
}

/// Waits until Rime has deployed its schemas, the first time a while.
fn wait_for_rime(env: &TestEnv) -> anyhow::Result<()> {
    wait_until_within(Duration::from_secs(120), "Rime to deploy", || {
        let list = env.omime(&["list", "--all"]).ok()?;
        list.contains("rime/double_pinyin_flypy").then_some(())
    })
    .with_context(|| env.daemon_log())
}

fn type_two_phrases(env: &TestEnv) -> anyhow::Result<omime_e2e::harness::Editor> {
    wait_for_rime(env)?;
    env.omime(&["switch", "rime/luna_pinyin"])?;
    let editor = env.editor(&[])?;
    env.type_text("nihao ")?;
    editor.wait_for_text("你好")?;
    env.type_text("zhongguo ")?;
    editor.wait_for_text("你好中国")?;
    Ok(editor)
}

fn assert_last_phrase_saved(env: &TestEnv) -> anyhow::Result<()> {
    let data = env.dir().join("data/oh-my-ime/rime");
    let exported = data.join("shutdown-export.txt");
    let output = env
        .command("rime_dict_manager")
        .current_dir(&data)
        .args(["--export", "luna_pinyin"])
        .arg(&exported)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "cannot export Rime's dictionary: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved = std::fs::read_to_string(exported)?;
    assert!(
        saved.contains("你好"),
        "the first transaction was not saved: {saved}"
    );
    // The schema stores traditional Chinese, even when it commits simplified text.
    assert!(
        saved.contains("中國") && saved.contains("#@/tick\t2"),
        "the final transaction was not saved: {saved}"
    );
    Ok(())
}

fn wait_for_rime_exit(pid: u32) -> anyhow::Result<()> {
    wait_until("Rime to exit and be reaped", || {
        (!Path::new(&format!("/proc/{pid}")).exists()).then_some(())
    })
}

/// A shell records the real Rime process's status, even if the supervisor kills
/// its direct child. This distinguishes Rime's own cleanup from a supervisor kill.
fn record_rime_exit(mut engine: TestEngine) -> TestEngine {
    let mut args = vec!["-c".into(),
        "( \"$@\" & pid=$!; printf '%s\\n' \"$pid\" > \"$XDG_STATE_HOME/rime-pid\"; wait \"$pid\"; status=$?; printf '%s\\n' \"$status\" > \"$XDG_STATE_HOME/rime-status\"; exit \"$status\" ) & wait".into(),
        "sh".into()];
    if std::env::var_os("OMIME_E2E_RIME_VALGRIND").is_some() {
        args.extend([
            "valgrind".into(),
            "--log-file=/tmp/omime-rime-valgrind.%p.log".into(),
            "--num-callers=40".into(),
        ]);
    }
    args.push(engine.program.display().to_string());
    args.extend(engine.args);
    engine.program = "sh".into();
    engine.args = args;
    engine
}

fn recorded_rime_pid(env: &TestEnv) -> anyhow::Result<u32> {
    Ok(std::fs::read_to_string(env.dir().join("state/rime-pid"))?
        .trim()
        .parse()?)
}

fn recorded_rime_exit(env: &TestEnv) -> anyhow::Result<i32> {
    wait_until("Rime's exit status", || {
        std::fs::read_to_string(env.dir().join("state/rime-status"))
            .ok()?
            .trim()
            .parse()
            .ok()
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn ordinary_shutdown_preserves_the_last_learned_rime_phrase() -> anyhow::Result<()> {
    let Some(engine) = rime() else { return Ok(()) };
    let mut env = TestEnv::start(&[engine], Some("rime"))?;
    let _editor = type_two_phrases(&env)?;
    let pid = env.engine_pid("rime").expect("Rime process");
    env.signal_daemon(rustix::process::Signal::TERM)?;
    assert!(env.wait_for_daemon_exit(Duration::from_secs(6))?.success());
    wait_for_rime_exit(pid)?;
    assert_last_phrase_saved(&env)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn rime_exits_cleanly_with_a_focused_field() -> anyhow::Result<()> {
    let Some(engine) = rime() else { return Ok(()) };
    for _ in 0..5 {
        let mut env = TestEnv::start(&[record_rime_exit(engine.clone())], Some("rime"))?;
        let _editor = type_two_phrases(&env)?;
        env.signal_daemon(rustix::process::Signal::TERM)?;
        assert_eq!(recorded_rime_exit(&env)?, 0, "{}", env.daemon_log());
        assert!(env.wait_for_daemon_exit(Duration::from_secs(6))?.success());
        assert!(
            !env.daemon_log().contains("double free"),
            "{}",
            env.daemon_log()
        );
        assert_last_phrase_saved(&env)?;
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn rime_sigterm_preserves_the_last_learned_phrase() -> anyhow::Result<()> {
    let Some(engine) = rime() else { return Ok(()) };
    let env = TestEnv::start(&[record_rime_exit(engine)], Some("rime"))?;
    let _editor = type_two_phrases(&env)?;
    let pid =
        rustix::process::Pid::from_raw(recorded_rime_pid(&env)?.try_into()?).context("Rime pid")?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    assert_eq!(recorded_rime_exit(&env)?, 0, "{}", env.daemon_log());
    assert!(
        !env.daemon_log().contains("double free"),
        "{}",
        env.daemon_log()
    );
    assert_last_phrase_saved(&env)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn rime_finalizes_when_its_window_connection_is_lost() -> anyhow::Result<()> {
    let Some(engine) = rime() else { return Ok(()) };
    for _ in 0..5 {
        let env = TestEnv::start(&[record_rime_exit(engine.clone())], Some("rime"))?;
        let _editor = type_two_phrases(&env)?;
        // This is the harness's private compositor. Losing it must finalize Rime
        // even though Slint returns an error before the usual quit path.
        let _ = env.command("swaymsg").arg("exit").output()?;
        let status = recorded_rime_exit(&env)?;
        assert!(
            (0..=1).contains(&status),
            "Rime aborted: status {status}\n{}",
            env.daemon_log()
        );
        assert!(
            !env.daemon_log().contains("double free"),
            "{}",
            env.daemon_log()
        );
        assert_last_phrase_saved(&env)?;
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn leaving_the_field_saves_rimes_last_learned_phrase() -> anyhow::Result<()> {
    let Some(engine) = rime() else { return Ok(()) };
    let mut env = TestEnv::start(&[engine], Some("rime"))?;
    let editor = type_two_phrases(&env)?;
    let before = env.count_in_log("text field left");
    drop(editor);
    wait_until("the field to be left", || {
        (env.count_in_log("text field left") > before).then_some(())
    })?;
    // Let the no-reply Deactivate reach the engine before stopping the daemon.
    std::thread::sleep(Duration::from_millis(500));
    env.signal_daemon(rustix::process::Signal::TERM)?;
    assert!(env.wait_for_daemon_exit(Duration::from_secs(6))?.success());
    assert_last_phrase_saved(&env)
}

fn wait_for_mode(env: &TestEnv, mode: &str) -> anyhow::Result<()> {
    wait_until(&format!("Rime to confirm input mode {mode}"), || {
        env.omime(&["status", "--json"])
            .ok()?
            .contains(&format!("\"input_mode\":\"{mode}\""))
            .then_some(())
    })
    .with_context(|| {
        format!(
            "status: {:?}\n{}",
            env.omime(&["status", "--json"]),
            env.daemon_log()
        )
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn rime_types_xiaohe_and_pinyin() -> anyhow::Result<()> {
    let Some(engine) = rime() else {
        return Ok(());
    };
    let env = TestEnv::start(&[engine], Some("rime"))?;
    wait_for_rime(&env)?;
    // Xiaohe shuangpin comes first: ni hao is n i h c.
    env.wait_for_status_starting("rime/double_pinyin_flypy")?;
    let editor = env.editor(&[])?;
    env.type_text("nihc ")?;
    editor.wait_for_text("你好")?;
    if std::env::var_os("OMIME_E2E_SCREENSHOT").is_some() {
        env.type_text("zhongguo")?;
        std::thread::sleep(Duration::from_millis(500));
        env.screenshot(&env.dir().join("rime.png"))?;
        env.key(&[], "Escape")?;
    }

    // Pinyin, chosen like any input method.
    env.omime(&["switch", "rime/luna_pinyin"])?;
    env.wait_for_status_starting("rime/luna_pinyin")?;
    env.type_text("zhongguo ")?;
    editor.wait_for_text("你好中国")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn rimes_candidates_are_clicked_and_shift_switches_to_letters() -> anyhow::Result<()> {
    let Some(engine) = rime() else {
        return Ok(());
    };
    let env = TestEnv::start(&[engine], Some("rime"))?;
    wait_for_rime(&env)?;
    env.omime(&["switch", "rime/luna_pinyin"])?;
    env.wait_for_status_starting("rime/luna_pinyin")?;
    let editor = env.editor(&[])?;

    wait_for_mode(&env, "native")?;

    // The window opens below the text cursor, which the test editor puts at x = 12,
    // y = 20, height 18; its first candidate is about 40 pixels in.
    env.type_text("zhongguo")?;
    if std::env::var_os("OMIME_E2E_SCREENSHOT").is_some() {
        std::thread::sleep(Duration::from_millis(800));
        env.screenshot(&env.dir().join("rime-click.png"))?;
    }
    let commits = || {
        editor
            .lines()
            .iter()
            .filter(|l| l.starts_with("commit"))
            .count()
    };
    let before = commits();
    wait_until("a click on the first candidate", || {
        env.click(12 + 40, 20 + 18 + 24).ok()?;
        (commits() > before).then_some(())
    })?;
    editor.wait_for_text("中国")?;

    // Shift alone switches to English letters, and back.
    env.key(&[], "Shift_L")?;
    env.wait_for_status_starting("rime/luna_pinyin (A)")?;
    wait_for_mode(&env, "latin")?;
    env.type_text("ok")?;
    editor.wait_for_text("中国ok")?;
    env.key(&[], "Shift_L")?;
    env.wait_for_status_starting("rime/luna_pinyin (中)")?;
    wait_for_mode(&env, "native")?;
    // So does Rime's menu.
    let menu = env.omime(&["properties"])?;
    assert!(
        menu.contains("ascii_mode") && menu.contains("deploy"),
        "{menu}"
    );
    env.omime(&["activate", "ascii_mode"])?;
    env.wait_for_status_starting("rime/luna_pinyin (A)")?;
    wait_for_mode(&env, "latin")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn rime_restores_each_apps_mode_before_typing_and_after_restart() -> anyhow::Result<()> {
    let Some(engine) = rime() else { return Ok(()) };
    let mut env = TestEnv::start(&[engine], Some("rime"))?;
    env.configure("[apps]\nremember = true\nremember_mode = true\n")?;
    wait_for_rime(&env)?;
    env.omime(&["switch", "rime/luna_pinyin"])?;

    let chinese = env.editor(&["--app-id", "mode-chat"])?;
    env.type_text("nihao ")?;
    chinese.wait_for_text("你好")?;
    wait_for_mode(&env, "native")?;

    let latin = env.editor(&["--app-id", "mode-terminal"])?;
    env.key(&[], "Shift_L")?;
    wait_for_mode(&env, "latin")?;
    env.type_text("hello")?;
    latin.wait_for_text("hello")?;

    let mut chinese_text = "你好".to_owned();
    let mut latin_text = "hello".to_owned();
    for _ in 0..4 {
        // Send keys as soon as the focus is routed, without waiting for a mode
        // confirmation: restoration must run ahead of the first key.
        env.focus("mode-chat")?;
        env.type_text("nihao ")?;
        chinese_text.push_str("你好");
        chinese.wait_for_text(&chinese_text)?;
        wait_for_mode(&env, "native")?;
        env.focus("mode-terminal")?;
        env.type_text("x")?;
        latin_text.push('x');
        latin.wait_for_text(&latin_text)?;
        wait_for_mode(&env, "latin")?;
    }

    // The new Rime process defaults to Chinese. Its Activate report must not
    // replace the terminal's saved Latin mode before the restore request runs.
    env.restart_daemon()?;
    wait_for_rime(&env)?;
    env.wait_for_log("keyboard grabbed")?;
    wait_for_mode(&env, "latin")?;
    env.type_text("restart")?;
    latin_text.push_str("restart");
    latin.wait_for_text(&latin_text)?;
    env.focus("mode-chat")?;
    env.type_text("nihao ")?;
    chinese_text.push_str("你好");
    chinese.wait_for_text(&chinese_text)?;
    wait_for_mode(&env, "native")?;

    // Disabling the option restores shared mode behavior even though per-app
    // choices remain saved, and does not change the selected Rime schema.
    env.omime(&["config", "apps.remember_mode", "false"])?;
    env.focus("mode-terminal")?;
    env.type_text("nihao ")?;
    latin_text.push_str("你好");
    latin.wait_for_text(&latin_text)?;
    wait_for_mode(&env, "native")?;
    env.key(&[], "Shift_L")?;
    wait_for_mode(&env, "latin")?;
    env.focus("mode-chat")?;
    env.type_text("plain")?;
    chinese_text.push_str("plain");
    chinese.wait_for_text(&chinese_text)?;
    wait_for_mode(&env, "latin")?;
    env.wait_for_status_starting("rime/luna_pinyin")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn rime_restores_a_saved_mode_while_deploying_for_the_first_time() -> anyhow::Result<()> {
    let Some(engine) = rime() else { return Ok(()) };
    let mut env = TestEnv::prepare(&[engine])?;
    std::fs::create_dir_all(env.dir().join("config/oh-my-ime"))?;
    std::fs::create_dir_all(env.dir().join("state/oh-my-ime"))?;
    std::fs::write(
        env.dir().join("config/oh-my-ime/config.toml"),
        "engines = ['rime/luna_pinyin']\n[apps]\nremember = true\nremember_mode = true\n",
    )?;
    std::fs::write(
        env.dir().join("state/oh-my-ime/state.toml"),
        "current = 'rime'\n[inner]\nrime = 'luna_pinyin'\n[apps]\nmode-terminal = 'rime/luna_pinyin'\n[app_modes.mode-terminal]\n'rime/luna_pinyin' = 'latin'\n",
    )?;
    env.launch_daemon()?;
    // The editor harness waits for the daemon's focus event, so start the daemon
    // first, then open the field without waiting for Rime's initial deployment.
    let editor = env.editor(&["--app-id", "mode-terminal"])?;
    wait_for_rime(&env)?;
    wait_for_mode(&env, "latin")?;
    env.type_text("plain")?;
    editor.wait_for_text("plain")?;
    let (_, diagnosis) = env.omime_status(&["doctor"])?;
    assert!(
        !diagnosis.contains("input mode restoration failed"),
        "{diagnosis}"
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn rimes_candidates_stand_in_a_column_when_set_so() -> anyhow::Result<()> {
    let Some(engine) = rime() else {
        return Ok(());
    };
    let mut env = TestEnv::prepare(&[engine])?;
    // The settings window writes rime.toml, which the engine reads when it starts.
    std::fs::write(
        env.dir().join("config/oh-my-ime/rime.toml"),
        "layout = \"vertical\"\n",
    )?;
    env.launch_daemon()?;
    env.omime(&["switch", "rime"])?;
    wait_for_rime(&env)?;
    env.omime(&["switch", "rime/luna_pinyin"])?;
    env.wait_for_status_starting("rime/luna_pinyin")?;
    let editor = env.editor(&[])?;
    env.type_text("zhongguo ")?;
    editor.wait_for_text("中国")?;

    // The window opens below the text cursor (x = 12, y = 20, height 18); in a column,
    // the second candidate is a row of about 30 pixels below the first.
    env.type_text("zhongguo")?;
    if std::env::var_os("OMIME_E2E_SCREENSHOT").is_some() {
        std::thread::sleep(Duration::from_millis(800));
        env.screenshot(&env.dir().join("rime-column.png"))?;
    }
    wait_until("a click on the second candidate", || {
        env.click(12 + 30, 20 + 18 + 10 + 30 + 15).ok()?;
        (editor.text() != "中国").then_some(())
    })?;
    let text = editor.text();
    let second = text.strip_prefix("中国").unwrap_or_default();
    assert!(
        !second.is_empty() && second != "中国",
        "the second candidate, not the first: {text:?}"
    );
    Ok(())
}

const SETTINGS_ID: &str = "io.github.ign1x.OhMyIme.Rime";

fn settings_env(engine: TestEngine) -> anyhow::Result<TestEnv> {
    let mut env = TestEnv::prepare(std::slice::from_ref(&engine))?;
    // Use the shipped manifest, including its settings command and restart policy.
    // Keep the harness's library-path wrapper when an installed daemon launches a
    // locally built Rime: installed daemons intentionally clear LD_LIBRARY_PATH.
    let path = env.dir().join("engines/rime/engine.toml");
    let generated = std::fs::read_to_string(&path)?;
    let command = generated
        .lines()
        .find_map(|line| line.strip_prefix("exec = "))
        .context("the harness's engine command")?;
    let manifest = include_str!("../../../engines/rime/engine.toml")
        .replace("[\"omime-rime\"", command.trim_end_matches(']'));
    std::fs::write(path, manifest)?;
    let output = env
        .command("swaymsg")
        .arg(format!(
            "for_window [app_id=\"{SETTINGS_ID}\"] floating enable"
        ))
        .output()?;
    anyhow::ensure!(output.status.success(), "float the settings window");
    env.launch_daemon()?;
    env.omime(&["switch", "rime"])?;
    wait_for_rime(&env)?;
    Ok(env)
}

fn open_settings(env: &TestEnv) -> anyhow::Result<()> {
    env.omime(&["settings", "rime"])?;
    wait_until("Rime's settings window", || {
        env.has_window(SETTINGS_ID).ok()?.then_some(())
    })?;
    let output = env.command("swaymsg").arg(format!(
        "[app_id=\"{SETTINGS_ID}\"] floating enable, resize set 560 660, move absolute position 0 0"
    )).output()?;
    anyhow::ensure!(
        output.status.success(),
        "position the settings window: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    // Let the first frame and keyboard focus reach the client before interacting.
    std::thread::sleep(Duration::from_millis(200));
    if std::env::var_os("OMIME_E2E_SCREENSHOT").is_some() {
        let tree = env
            .command("swaymsg")
            .args(["-t", "get_tree", "--raw"])
            .output()?;
        std::fs::write(env.dir().join("rime-settings-tree.json"), tree.stdout)?;
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn rimes_settings_save_restart_and_recover_from_write_errors() -> anyhow::Result<()> {
    let Some(engine) = rime() else { return Ok(()) };
    let env = settings_env(engine)?;
    open_settings(&env)?;
    // At 560 x 660: increment the page size, select a column, increase the font size.
    env.click(480, 352)?;
    env.click(400, 488)?;
    env.key(&[], "Down")?;
    env.key(&[], "Return")?;
    env.click(480, 572)?;
    if std::env::var_os("OMIME_E2E_SCREENSHOT").is_some() {
        env.screenshot(&env.dir().join("rime-settings.png"))?;
    }
    let style_path = env.dir().join("config/oh-my-ime/rime.toml");
    std::fs::create_dir(&style_path)?;
    env.click(515, 624)?;
    env.wait_for_log("cannot save")?;
    assert!(env.has_window(SETTINGS_ID)?, "a failed save stays open");

    std::fs::remove_dir(&style_path)?;
    env.click(515, 624)?;
    wait_until("the saved settings to close", || {
        (!env.has_window(SETTINGS_ID).ok()?).then_some(())
    })?;
    env.wait_for_log("restarting the engine to read its settings")?;
    let style = std::fs::read_to_string(&style_path)?;
    assert!(style.contains("layout = \"vertical\""), "{style}");
    assert!(style.contains("font_size = 19"), "{style}");
    let custom =
        std::fs::read_to_string(env.dir().join("data/oh-my-ime/rime/default.custom.yaml"))?;
    assert!(custom.contains("\"menu/page_size\": 6"), "{custom}");
    wait_for_rime(&env)?;
    env.wait_for_status_starting("rime/double_pinyin_flypy (中)")?;
    let editor = env.editor(&[])?;
    env.type_text("nihc ")?;
    editor.wait_for_text("你好")?;

    // Reopen and save without editing: the values read back must stay the same.
    open_settings(&env)?;
    env.click(515, 624)?;
    wait_until("the unchanged settings to close", || {
        (!env.has_window(SETTINGS_ID).ok()?).then_some(())
    })?;
    assert_eq!(std::fs::read_to_string(style_path)?, style);
    assert_eq!(
        std::fs::read_to_string(env.dir().join("data/oh-my-ime/rime/default.custom.yaml"))?,
        custom
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn rimes_settings_cancel_and_require_a_schema() -> anyhow::Result<()> {
    let Some(engine) = rime() else { return Ok(()) };
    let env = settings_env(engine)?;
    let custom_path = env.dir().join("data/oh-my-ime/rime/default.custom.yaml");
    let original = std::fs::read_to_string(&custom_path)?;
    let style_path = env.dir().join("config/oh-my-ime/rime.toml");
    open_settings(&env)?;
    env.click(480, 352)?;
    env.click(480, 572)?;
    env.click(445, 624)?;
    wait_until("the canceled settings to close", || {
        (!env.has_window(SETTINGS_ID).ok()?).then_some(())
    })?;
    assert_eq!(std::fs::read_to_string(&custom_path)?, original);
    assert!(!style_path.exists(), "cancel writes no appearance settings");
    env.wait_for_log("restarting the engine to read its settings")?;
    wait_for_rime(&env)?;

    open_settings(&env)?;
    // The first two rows are the selected Xiaohe and pinyin schemas.
    env.click(40, 103)?;
    env.click(40, 127)?;
    env.click(515, 624)?;
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        env.has_window(SETTINGS_ID)?,
        "cannot save without any schema"
    );
    assert_eq!(std::fs::read_to_string(&custom_path)?, original);

    env.click(40, 127)?;
    env.click(515, 624)?;
    wait_until("the selected schema to be saved", || {
        (!env.has_window(SETTINGS_ID).ok()?).then_some(())
    })?;
    let custom = std::fs::read_to_string(custom_path)?;
    assert!(custom.contains("luna_pinyin"), "{custom}");
    assert!(!custom.contains("double_pinyin_flypy"), "{custom}");
    env.wait_for_status_starting("rime/luna_pinyin (中)")?;
    let editor = env.editor(&[])?;
    env.type_text("zhongguo ")?;
    editor.wait_for_text("中国")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn reselecting_the_active_rime_schema_keeps_its_mode_and_composition() -> anyhow::Result<()> {
    let Some(engine) = rime() else { return Ok(()) };
    let env = TestEnv::start(&[engine], Some("rime"))?;
    wait_for_rime(&env)?;
    env.omime(&["switch", "rime/luna_pinyin"])?;
    let editor = env.editor(&[])?;
    env.key(&[], "Shift_L")?;
    wait_for_mode(&env, "latin")?;
    env.omime(&["switch", "rime/luna_pinyin"])?;
    env.type_text("plain")?;
    editor.wait_for_text("plain")?;
    wait_for_mode(&env, "latin")?;
    env.key(&[], "Shift_L")?;
    wait_for_mode(&env, "native")?;
    env.type_text("zhong")?;
    env.omime(&["switch", "rime/luna_pinyin"])?;
    env.type_text("guo ")?;
    editor.wait_for_text("plain中国")?;
    Ok(())
}
