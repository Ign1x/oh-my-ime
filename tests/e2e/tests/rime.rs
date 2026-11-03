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
