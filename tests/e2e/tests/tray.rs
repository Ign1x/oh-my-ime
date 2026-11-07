//! End-to-end tests of omimed's tray item, shown by `omime-tray-host` as a status bar's
//! tray would. See `typing.rs` for how to run them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use omime_e2e::harness::{
    TestEngine, TestEnv, dummy_engine, echo_engine, hosted_engine, sparkle_engine, target_dir,
};

type Properties = HashMap<String, zbus::OwnedVariant>;
type Layout = (i32, Properties, Vec<zbus::OwnedVariant>);

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

async fn connect(env: &TestEnv) -> zbus::Result<zbus::Connection> {
    zbus::connection::Builder::address(format!("unix:path={}/bus", env.dir().display()).as_str())
        .method_timeout(Duration::from_secs(3))
        .build()
        .await
}

async fn control(conn: &zbus::Connection) -> zbus::Result<zbus::Proxy<'_>> {
    zbus::Proxy::new(
        conn,
        "io.github.ign1x.OhMyIme",
        "/io/github/ign1x/OhMyIme",
        "io.github.ign1x.OhMyIme.Daemon1",
    )
    .await
}

async fn menu(conn: &zbus::Connection) -> anyhow::Result<zbus::Proxy<'_>> {
    let names = zbus::fdo::DBusProxy::new(conn).await?.list_names().await?;
    let name = names
        .into_iter()
        .find(|name| name.starts_with("org.kde.StatusNotifierItem-"))
        .context("no tray item on the bus")?;
    Ok(zbus::Proxy::new(conn, name, "/MenuBar", "com.canonical.dbusmenu").await?)
}

async fn menu_entry(menu: &zbus::Proxy<'_>, label: &str) -> anyhow::Result<i32> {
    let entries: Vec<(i32, Properties)> = menu
        .call(
            "GetGroupProperties",
            &(Vec::<i32>::new(), Vec::<String>::new()),
        )
        .await?;
    entries
        .iter()
        .find(|(_, properties)| {
            properties
                .get("label")
                .and_then(|v| <&str>::try_from(v).ok())
                == Some(label)
        })
        .map(|(id, _)| *id)
        .with_context(|| format!("no {label:?} entry in the engine's menu"))
}

async fn wait_for_property(daemon: &zbus::Proxy<'_>, id: &str) -> anyhow::Result<Properties> {
    Ok(tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let properties: Vec<Properties> = daemon.call("Properties", &()).await?;
            if let Some(property) = properties.into_iter().find(|property| {
                property.get("id").and_then(|v| <&str>::try_from(v).ok()) == Some(id)
            }) {
                break zbus::Result::Ok(property);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??)
}

/// Counts every descendant and the descendants supplied by the fixture engine.
fn nodes(layout: &Layout) -> anyhow::Result<(usize, usize)> {
    let mut count = (0, 0);
    for child in &layout.2 {
        let child: Layout = child.try_clone()?.try_into()?;
        let descendants = nodes(&child)?;
        let engine_entry = child
            .1
            .get("label")
            .and_then(|v| <&str>::try_from(v).ok())
            .is_some_and(|label| matches!(label, "MenuLoop" | "MenuSentinel"));
        count.0 += 1 + descendants.0;
        count.1 += usize::from(engine_entry) + descendants.1;
    }
    Ok(count)
}

fn menu_engine(args: &[&str]) -> TestEngine {
    TestEngine {
        id: "menu",
        program: target_dir().join("omime-menu-engine"),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        env: Vec::new(),
        hosted: false,
    }
}

/// "Direct input" as omimed names it, in English or Chinese after the locale.
const DIRECT_INPUT: [&str; 2] = ["Direct input", "直接输入"];

/// The menu without its last entries, the settings of oh-my-ime, which it has when
/// `omime-settings` is installed next to omimed (see settings.rs).
fn without_settings(menu: &str) -> &str {
    ["|-|Settings…", "|-|设置…"]
        .iter()
        .find_map(|entry| menu.strip_suffix(entry))
        .unwrap_or(menu)
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_tray_shows_the_current_engine_and_switches() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    env.omime(&["enable", "echo"])?;
    // The tray comes after omimed, as when a status bar restarts: omimed registers its
    // item with the new watcher.
    let tray = env.tray_host()?;
    tray.wait_for("omimed's item", |l| l.starts_with("registered "))?;
    tray.wait_for_latest("the engine in the tooltip", "tooltip ", |t| t == "echo")?;
    let menu = tray.wait_for_latest("the menu", "menu ", |m| m.starts_with("echo*|"))?;
    let direct = DIRECT_INPUT
        .into_iter()
        .find(|d| without_settings(&menu).ends_with(d))
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
        m.starts_with("echo|") && without_settings(m).ends_with(&format!("|{direct}*"))
    })?;

    // A click goes back to the last engine.
    tray.send("activate")?;
    tray.wait_for_latest("the engine again", "tooltip ", |t| t == "echo")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn the_tray_lists_demo_engines_once_enabled() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine(), sparkle_engine()], None)?;
    let config = std::fs::read_to_string(env.dir().join("config/oh-my-ime/config.toml"))?;
    assert!(!config.contains("engines ="), "{config}");
    let tray = env.tray_host()?;
    tray.wait_for_latest("settings without disabled demos", "menu ", |menu| {
        menu.split('|').any(|entry| entry == "Settings…")
            && !menu
                .split('|')
                .any(|entry| entry == "echo" || entry == "sparkle")
    })?;
    env.omime(&["enable", "sparkle"])?;
    tray.wait_for_latest("the enabled Sparkle demo", "menu ", |menu| {
        menu.split('|').any(|entry| entry == "sparkle")
    })?;
    tray.send("click sparkle")?;
    env.wait_for_status_starting("sparkle")?;
    let list = env.omime(&["list"])?;
    assert!(!list.contains("echo"), "{list}");
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

    env.configure("engines = [\"echo\"]\n[tray]\nenabled = false\n")?;
    tray.wait_for("the item to go away", |l| {
        l == format!("unregistered {name}")
    })?;
    env.configure("engines = [\"echo\"]\n[tray]\nenabled = true\n")?;
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
        without_settings(&menu)
            .ends_with("|-|Chinese punctuation*|Width(Full width|Half width*)|Say hello"),
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

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn cyclic_engine_properties_do_not_expand_into_hundreds_of_menu_entries() -> anyhow::Result<()> {
    // Three submenus reuse one ID; the fourth wire entry is an ordinary sentinel.
    let env = TestEnv::start(&[menu_engine(&["duplicate-submenus", "2"])], Some("menu"))?;
    let tray = env.tray_host()?;
    let menu = tray.wait_for_latest("the engine's property list", "menu ", |menu| {
        menu.contains("MenuSentinel")
    })?;
    let copies = menu.matches("MenuLoop").count();
    assert!(
        copies <= 3,
        "three engine entries expanded into {copies} menu nodes"
    );
    assert!(copies + menu.matches("MenuSentinel").count() <= 4);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn duplicate_submenus_do_not_block_the_daemon() -> anyhow::Result<()> {
    const VERIFY_FANOUT: usize = 5;
    let env = TestEnv::start(
        &[menu_engine(&[
            "duplicate-submenus",
            &VERIFY_FANOUT.to_string(),
        ])],
        Some("menu"),
    )?;
    runtime()?.block_on(async {
        let conn = connect(&env).await?;
        let daemon = control(&conn).await?;
        wait_for_property(&daemon, "sentinel").await?;
        let menu = menu(&conn).await?;
        let baseline_start = Instant::now();
        env.omime(&["status"])?;
        let baseline = baseline_start.elapsed();

        // Poll GetLayout first and start the CLI while the request is in flight.
        let layout_request = async {
            let start = Instant::now();
            let reply: zbus::Result<(u32, Layout)> = menu
                .call("GetLayout", &(0i32, -1i32, Vec::<String>::new()))
                .await;
            (start.elapsed(), reply)
        };
        let status_request = async {
            tokio::task::yield_now().await;
            let start = Instant::now();
            // Keep the blocking process wait off the D-Bus client's runtime.
            let mut command = env.command("omime");
            command.arg("status");
            let output = tokio::task::spawn_blocking(move || command.output()).await?;
            anyhow::Ok((start.elapsed(), output?))
        };
        let ((layout_time, layout), status) = tokio::join!(biased; layout_request, status_request);
        let (status_time, output) = status?;
        let (_, layout) = layout.context("GetLayout failed for duplicate submenu IDs")?;
        let (total, own) = nodes(&layout)?;
        assert!(
            own <= VERIFY_FANOUT + 2,
            "{own} engine nodes from 7 wire entries"
        );
        assert_eq!(own, 2, "the sentinel and first submenu must survive");
        // The other entries are the engine, direct input, a separator and optional settings.
        assert!(total <= VERIFY_FANOUT + 7, "{total} total menu nodes");
        assert!(
            layout_time < Duration::from_secs(1),
            "GetLayout took {layout_time:?}"
        );
        assert!(output.status.success(), "omime status failed: {output:?}");
        assert!(
            status_time < baseline + Duration::from_millis(500),
            "omime status took {status_time:?}, baseline {baseline:?}"
        );
        assert_eq!(String::from_utf8(output.stdout)?.trim(), "menu");
        let names = zbus::fdo::DBusProxy::new(&conn).await?.list_names().await?;
        assert!(
            names
                .iter()
                .any(|name| name.as_str() == menu.destination().as_str())
        );
        assert!(
            names
                .iter()
                .any(|name| name.as_str() == "io.github.ign1x.OhMyIme")
        );
        env.omime(&["status"])?;
        anyhow::Ok(())
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn stale_tray_property_cannot_activate_another_engine() -> anyhow::Result<()> {
    let first = dummy_engine(&["--hidden", "--properties"]);
    let mut second = first.clone();
    second.id = "second";
    let env = TestEnv::start(&[first, second], Some("dummy"))?;
    runtime()?.block_on(async {
        let conn = connect(&env).await?;
        let daemon = control(&conn).await?;
        wait_for_property(&daemon, "punct").await?;
        let menu = menu(&conn).await?;
        let id = menu_entry(&menu, "Chinese punctuation").await?;
        let _: () = daemon.call("Switch", &"second").await?;
        wait_for_property(&daemon, "punct").await?;
        let current_id = menu_entry(&menu, "Chinese punctuation").await?;
        // A status bar may send a click from a layout it saved before the switch.
        let _: () = menu
            .call("Event", &(id, "clicked", zbus::Variant::from(0i32), 0u32))
            .await?;
        env.wait_for_log("ignored stale engine menu click")?;
        let punct = wait_for_property(&daemon, "punct").await?;
        assert_eq!(
            punct.get("checked").and_then(|v| bool::try_from(v).ok()),
            Some(true),
            "clicking the old menu changed the newly selected engine\n{}",
            env.daemon_log()
        );
        assert_ne!(
            id, current_id,
            "menu refresh must not reuse the old engine's ID"
        );
        // The control API deliberately activates the current engine's property.
        env.omime(&["activate", "punct"])?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let punct = wait_for_property(&daemon, "punct").await?;
                if punct.get("checked").and_then(|v| bool::try_from(v).ok()) == Some(false) {
                    break anyhow::Ok(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        anyhow::Ok(())
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn an_oversized_property_list_is_capped_with_one_warning() -> anyhow::Result<()> {
    let env = TestEnv::start(&[menu_engine(&["large"])], Some("menu"))?;
    runtime()?.block_on(async {
        let conn = connect(&env).await?;
        let daemon = control(&conn).await?;
        wait_for_property(&daemon, "entry-0").await?;
        let properties: Vec<Properties> = daemon.call("Properties", &()).await?;
        assert_eq!(properties.len(), 256);
        let menu = menu(&conn).await?;
        for _ in 0..3 {
            let _: (u32, Layout) = menu
                .call("GetLayout", &(0i32, -1i32, Vec::<String>::new()))
                .await?;
        }
        assert_eq!(
            env.count_in_log("engine property list exceeds the limit"),
            1
        );
        anyhow::Ok(())
    })
}
