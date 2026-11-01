//! End-to-end tests of omimed's tray item, shown by `omime-tray-host` as a status bar's
//! tray would. See `typing.rs` for how to run them.

use omime_e2e::harness::{TestEnv, dummy_engine, echo_engine, hosted_engine};

/// "Direct input" as omimed names it, in English or Chinese after the locale.
const DIRECT_INPUT: [&str; 2] = ["Direct input", "直接输入"];

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_tray_shows_the_current_engine_and_switches() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    // The tray comes after omimed, as when a status bar restarts: omimed registers its
    // item with the new watcher.
    let tray = env.tray_host()?;
    tray.wait_for("omimed's item", |l| l.starts_with("registered "))?;
    tray.wait_for_latest("the engine in the tooltip", "tooltip ", |t| t == "echo")?;
    let menu = tray.wait_for_latest("the menu", "menu ", |m| m.starts_with("echo*|"))?;
    let direct = DIRECT_INPUT
        .into_iter()
        .find(|d| menu.ends_with(d))
        .ok_or_else(|| anyhow::anyhow!("no direct input in the menu {menu:?}"))?;
    assert_eq!(
        tray.latest("icon "),
        Some("input-keyboard".to_owned()),
        "an engine without an icon shows a keyboard"
    );

    // The menu switches.
    tray.send(&format!("click {direct}"))?;
    env.wait_for_status("direct input")?;
    tray.wait_for_latest("direct input in the tooltip", "tooltip ", |t| t == direct)?;
    tray.wait_for_latest("direct input checked", "menu ", |m| {
        m.starts_with("echo|") && m.ends_with(&format!("|{direct}*"))
    })?;

    // A click goes back to the last engine.
    tray.send("activate")?;
    tray.wait_for_latest("the engine again", "tooltip ", |t| t == "echo")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_tray_shows_what_hosted_input_methods_show() -> anyhow::Result<()> {
    // The input method publishes a tray item on its private bus, as Fcitx 5 does.
    let env = TestEnv::start(
        &[hosted_engine(&["--tray", "omime-test-icon|Test IM|T"])],
        Some("hosted"),
    )?;
    let tray = env.tray_host()?;
    tray.wait_for_latest("the input method's icon", "icon ", |i| {
        i == "omime-test-icon"
    })?;
    tray.wait_for_latest("the input method in the tooltip", "tooltip ", |t| {
        t == "hosted · Test IM"
    })?;
    tray.wait_for_latest("the input method's label", "label ", |l| l == "T")?;
    // Status bars that use `omime status` see the same.
    let status = env.omime(&["status", "--json"])?;
    assert!(status.contains("Test IM"), "{status}");
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_tray_item_comes_and_goes_with_the_configuration() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    let tray = env.tray_host()?;
    let first = tray.wait_for("omimed's item", |l| l.starts_with("registered "))?;
    let name = first.trim_start_matches("registered ").to_owned();

    env.configure("[tray]\nenabled = false\n")?;
    tray.wait_for("the item to go away", |l| {
        l == format!("unregistered {name}")
    })?;
    env.configure("[tray]\nenabled = true\n")?;
    tray.wait_for("a new item", |l| l.starts_with("registered ") && l != first)?;
    tray.wait_for_latest("the engine in the tooltip", "tooltip ", |t| t == "echo")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_tray_menu_has_the_engines_own_menu() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[dummy_engine(&["--hidden", "--properties"])],
        Some("dummy"),
    )?;
    let tray = env.tray_host()?;
    let menu = tray.wait_for_latest("the engine's menu", "menu ", |m| {
        m.contains("Chinese punctuation")
    })?;
    assert!(
        menu.ends_with("|-|Chinese punctuation*|Width(Full width|Half width*)|Say hello"),
        "{menu}"
    );

    tray.send("click Chinese punctuation")?;
    env.wait_for_log("engine: activated punct")?;
    tray.wait_for_latest("the switch to flip", "menu ", |m| {
        m.contains("|Chinese punctuation|")
    })?;
    // An entry in a submenu.
    tray.send("click Full width")?;
    env.wait_for_log("engine: activated full")?;
    tray.wait_for_latest("the other choice", "menu ", |m| {
        m.contains("Width(Full width*|Half width)")
    })?;

    // Direct input has no menu of its own.
    env.omime(&["switch", "direct"])?;
    tray.wait_for_latest("a menu without the engine's", "menu ", |m| {
        !m.contains("Chinese punctuation")
    })?;
    Ok(())
}
