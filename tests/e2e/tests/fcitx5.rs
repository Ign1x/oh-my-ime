//! End-to-end tests with a real Fcitx5, hosted by omimed as an engine.
//!
//! They use the `fcitx5` on `$PATH` with a configuration of their own (the test
//! environment has its own `XDG_CONFIG_HOME`), and pass without testing anything when
//! Fcitx5 is not installed. See `typing.rs` for how to run them.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use omime_e2e::harness::{TIMEOUT, TestEngine, TestEnv};

/// Fcitx5 as a hosted engine, if it is installed.
fn fcitx5() -> Option<TestEngine> {
    let program = std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("fcitx5"))
        .find(|path| path.is_file())?;
    Some(TestEngine {
        id: "fcitx5",
        program,
        args: Vec::new(),
        hosted: true,
    })
}

/// A profile with the US keyboard only, so that typing gives what is typed.
const PROFILE: &str = "\
[Groups/0]
Name=Default
Default Layout=us
DefaultIM=keyboard-us

[Groups/0/Items/0]
Name=keyboard-us
Layout=

[GroupOrder]
0=Default
";

/// Gives Fcitx5 [`PROFILE`], in the environment's configuration directory.
fn write_profile(env: &TestEnv) -> anyhow::Result<()> {
    let config: PathBuf = env.dir().join("config/fcitx5");
    std::fs::create_dir_all(&config)?;
    std::fs::write(config.join("profile"), PROFILE)?;
    Ok(())
}

/// A profile with Pinyin, active from the start (omimed takes Ctrl+Space, Fcitx5's
/// trigger key).
const PINYIN_PROFILE: &str = "\
[Groups/0]
Name=Default
Default Layout=us
DefaultIM=pinyin

[Groups/0/Items/0]
Name=keyboard-us
Layout=

[Groups/0/Items/1]
Name=pinyin
Layout=

[GroupOrder]
0=Default
";

/// Gives Fcitx5 [`PINYIN_PROFILE`] and switches to it.
fn start_pinyin(env: &TestEnv) -> anyhow::Result<()> {
    let config: PathBuf = env.dir().join("config/fcitx5");
    std::fs::create_dir_all(&config)?;
    std::fs::write(config.join("profile"), PINYIN_PROFILE)?;
    std::fs::write(config.join("config"), "[Behavior]\nActiveByDefault=True\n")?;
    env.omime(&["switch", "fcitx5"])?;
    env.wait_for_log("hosted input method ready")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn pinyin_types_into_wayland_applications() -> anyhow::Result<()> {
    let Some(engine) = fcitx5() else {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    };
    let env = TestEnv::start(&[engine], None)?;
    start_pinyin(&env)?;
    let editor = env.editor(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    // Space picks the first candidate.
    env.type_text("nihao ")?;
    editor.wait_for_text("你好")?;
    env.type_text("zhongguo ")?;
    editor.wait_for_text("你好中国")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn pinyin_types_into_xim_applications() -> anyhow::Result<()> {
    let Some(engine) = fcitx5() else {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    };
    let env = TestEnv::start_with_x11(&[engine], None)?;
    start_pinyin(&env)?;
    let client = env.xim_client(&[])?;
    env.type_on_seat("seat0", "nihao ")?;
    client.wait_for_text("你好")?;
    env.type_on_seat("seat0", "zhongguo ")?;
    client.wait_for_text("你好中国")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn pinyin_types_into_fcitx_module_applications() -> anyhow::Result<()> {
    let Some(engine) = fcitx5() else {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    };
    let env = TestEnv::start(&[engine], None)?;
    start_pinyin(&env)?;
    let client = env.fcitx_client(&[])?;
    client.send("focus-in")?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    // n i h a o space, by keyval and X11 key code.
    for (keyval, keycode) in [
        (0x6e, 57),
        (0x69, 31),
        (0x68, 43),
        (0x61, 38),
        (0x6f, 32),
        (0x20, 65),
    ] {
        client.send(&format!("key {keyval:#x} {keycode} 0 press"))?;
        client.send(&format!("key {keyval:#x} {keycode} 0 release"))?;
    }
    client.wait_for("the commit", |l| l == "commit \"你好\"")?;
    // Fcitx5 does not consume releases and sends them back; no press may reach the
    // application, which would type the letter.
    let lines = client.lines();
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("forward ") && l.ends_with(" false")),
        "keys leaked to the application: {lines:?}"
    );
    Ok(())
}

/// Starts the environment and Fcitx5 with [`PROFILE`].
fn start(engine: TestEngine) -> anyhow::Result<TestEnv> {
    let env = TestEnv::start(&[engine], None)?;
    write_profile(&env)?;
    env.omime(&["switch", "fcitx5"])?;
    env.wait_for_log("hosted input method ready")?;
    Ok(env)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_shipped_manifest_installs_fcitx5() -> anyhow::Result<()> {
    if fcitx5().is_none() {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    }
    let env = TestEnv::start(&[], None)?;
    write_profile(&env)?;
    // The manifest users install, as the repository ships it.
    let source = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/engines/fcitx5");
    let output = env.omime(&["install", source])?;
    assert!(output.contains("Installed fcitx5"), "{output}");
    env.omime(&["switch", "fcitx5"])?;
    env.wait_for_log("hosted input method ready")?;
    let editor = env.editor(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    env.type_text("ok")?;
    editor.wait_for_text("ok")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn fcitx5_types_through_omimed() -> anyhow::Result<()> {
    let Some(engine) = fcitx5() else {
        eprintln!("fcitx5 is not installed; nothing to test");
        return Ok(());
    };
    let env = start(engine)?;
    let editor = env.editor(&[])?;
    env.wait_for_log("hosted input method grabbed the keyboard")?;
    env.type_text("hello")?;
    editor.wait_for_text("hello")?;

    // Ctrl+Shift+U types a character by its code point: Fcitx5 composes it in the
    // preedit, shows its window and commits the character.
    env.key(&["ctrl", "shift"], "u")?;
    env.type_text("4e2d")?;
    editor.wait_for("the code point in the preedit", |l| {
        l.starts_with("preedit") && l.contains("4e2d")
    })?;
    env.key(&[], "space")?;
    editor.wait_for_text("hello中")?;

    // Ctrl+Alt+Shift+U searches characters: the candidates appear in Fcitx5's window,
    // and Return picks the first (for "4e00", the Kangxi radical ⼀ comes before 一).
    env.key(&["ctrl", "alt", "shift"], "u")?;
    env.wait_for_log("hosted input method popup created")?;
    env.type_text("4e00")?;
    if std::env::var_os("OMIME_E2E_SCREENSHOT").is_some() {
        std::thread::sleep(Duration::from_millis(300));
        env.screenshot(&env.dir().join("fcitx5.png"))?;
    }
    env.key(&[], "Return")?;
    let deadline = Instant::now() + TIMEOUT;
    while editor.text().chars().count() < "hello中".chars().count() + 1 {
        anyhow::ensure!(
            Instant::now() < deadline,
            "nothing was picked: the editor has {:?}",
            editor.text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = editor.text();
    assert!(
        text.starts_with("hello中") && !text.contains("4e00"),
        "{text:?}"
    );
    Ok(())
}
