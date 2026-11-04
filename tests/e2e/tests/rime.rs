//! End-to-end tests of the Rime engine, `omime-rime`, with the Rime data of the dev
//! shell (`$OMIME_RIME_SHARED_DATA`). See `typing.rs` for how to run them.

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
        let list = env.omime(&["list"]).ok()?;
        list.contains("rime/double_pinyin_flypy").then_some(())
    })
    .with_context(|| env.daemon_log())
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
    env.type_text("ok")?;
    editor.wait_for_text("中国ok")?;
    env.key(&[], "Shift_L")?;
    env.wait_for_status_starting("rime/luna_pinyin (中)")?;
    // So does Rime's menu.
    let menu = env.omime(&["properties"])?;
    assert!(
        menu.contains("ascii_mode") && menu.contains("deploy"),
        "{menu}"
    );
    env.omime(&["activate", "ascii_mode"])?;
    env.wait_for_status_starting("rime/luna_pinyin (A)")?;
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
    let manifest = include_str!("../../../engines/rime/engine.toml")
        .replace("[\"omime-rime\"", &format!("[{:?}", engine.program));
    std::fs::write(env.dir().join("engines/rime/engine.toml"), manifest)?;
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
