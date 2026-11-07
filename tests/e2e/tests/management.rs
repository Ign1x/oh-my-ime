//! End-to-end tests for managing engines with the `omime` CLI.
//!
//! See `typing.rs` for how to run them.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use omime_e2e::harness::{
    TIMEOUT, TestEnv, dummy_engine, echo_engine, program_dir, rime_engine, sparkle_engine,
    wait_until, wait_until_within,
};

/// A running `omime status --watch --json`, with its lines read in the background.
struct StatusWatch {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
    errors: Arc<Mutex<Vec<String>>>,
    /// The index of the next line [`StatusWatch::wait_for`] may return.
    read: usize,
}

impl StatusWatch {
    fn spawn(env: &TestEnv) -> anyhow::Result<Self> {
        let mut child = env
            .command(program_dir().join("omime"))
            .args(["status", "--watch", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot start omime status --watch")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;
        let lines = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                sink.lock().unwrap_or_else(|p| p.into_inner()).push(line);
            }
        });
        let sink = errors.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                sink.lock().unwrap_or_else(|p| p.into_inner()).push(line);
            }
        });
        Ok(Self {
            child,
            lines,
            errors,
            read: 0,
        })
    }

    /// Waits for the next not yet consumed line matching `pred`; earlier lines are dropped.
    fn wait_for(&mut self, what: &str, pred: impl Fn(&str) -> bool) -> anyhow::Result<String> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let found = {
                let lines = self.lines.lock().unwrap_or_else(|p| p.into_inner());
                lines[self.read..]
                    .iter()
                    .position(|line| pred(line))
                    .map(|index| (lines[self.read + index].clone(), index))
            };
            if let Some((line, skipped)) = found {
                self.read += skipped + 1;
                return Ok(line);
            }
            if Instant::now() > deadline {
                let lines = self.lines.lock().unwrap_or_else(|p| p.into_inner());
                let errors = self.errors.lock().unwrap_or_else(|p| p.into_inner());
                anyhow::bail!("timed out waiting for {what}; lines {lines:?}, stderr {errors:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The next line, whatever it is.
    fn next_line(&mut self, what: &str) -> anyhow::Result<String> {
        self.wait_for(what, |_| true)
    }
}

impl Drop for StatusWatch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn status_watch_streams_changes() -> anyhow::Result<()> {
    let mut env = TestEnv::start(&[echo_engine()], None)?;
    env.omime(&["enable", "echo"])?;
    let _editor = env.editor(&[])?;
    let mut watch = StatusWatch::spawn(&env)?;

    let first = watch.next_line("the first status line")?;
    assert!(first.contains("\"class\":\"direct\""), "{first}");

    env.omime(&["switch", "echo"])?;
    watch.wait_for("the echo engine", |l| l.contains("\"engine\":\"echo\""))?;
    watch.wait_for("Echo's label", |l| l.contains("\"label\":\"ECHO\""))?;

    env.restart_daemon()?;
    watch.wait_for("omimed to stop", |l| l.contains("\"class\":\"stopped\""))?;
    watch.wait_for("the restarted omimed", |l| {
        l.contains("\"engine\":\"echo\"")
    })?;
    // Signals of the new daemon arrive too.
    env.omime(&["switch", "direct"])?;
    watch.wait_for("direct input", |l| l.contains("\"class\":\"direct\""))?;
    Ok(())
}

fn enabled(list: &str, id: &str) -> Option<bool> {
    list.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let first = words.next()?;
        match first {
            "*" => (words.next()? == id).then_some(true),
            other => (other == id).then_some(false),
        }
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn demos_are_offered_until_explicitly_enabled() -> anyhow::Result<()> {
    let mut env = TestEnv::start(&[echo_engine(), sparkle_engine()], None)?;
    let config = std::fs::read_to_string(env.dir().join("config/oh-my-ime/config.toml"))?;
    assert!(!config.contains("engines ="), "{config}");
    let list = env.omime(&["engines"])?;
    assert_eq!(enabled(&list, "echo"), Some(false), "{list}");
    assert_eq!(enabled(&list, "sparkle"), Some(false), "{list}");
    let all = env.omime(&["list", "--all"])?;
    assert!(all.contains("echo") && all.contains("sparkle"), "{all}");
    let cycle = env.omime(&["list"])?;
    assert!(
        !cycle.contains("echo") && !cycle.contains("sparkle"),
        "{cycle}"
    );

    let editor = env.editor(&[])?;
    env.key(&["ctrl", "shift"], "space")?;
    env.type_text("plain ")?;
    editor.wait_for_text("plain ")?;
    env.wait_for_status("direct input")?;
    assert!(env.engine_pid("echo").is_none());
    assert!(env.engine_pid("sparkle").is_none());

    // An offered engine can be tried without adding it to the cycle.
    env.omime(&["switch", "sparkle"])?;
    env.wait_for_status_starting("sparkle")?;
    env.type_text(":rocket ")?;
    editor.wait_for_text("plain 🚀")?;
    assert_eq!(enabled(&env.omime(&["engines"])?, "sparkle"), Some(false));
    env.omime(&["switch", "direct"])?;
    env.omime(&["enable", "sparkle"])?;
    let list = env.omime(&["engines"])?;
    assert_eq!(enabled(&list, "sparkle"), Some(true), "{list}");
    assert_eq!(enabled(&list, "echo"), Some(false), "{list}");
    env.key(&["ctrl", "shift"], "space")?;
    env.wait_for_status_starting("sparkle")?;
    env.restart_daemon()?;
    env.wait_for_status_starting("sparkle")?;
    assert_eq!(enabled(&env.omime(&["engines"])?, "echo"), Some(false));
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn engines_can_be_disabled_enabled_and_are_remembered() -> anyhow::Result<()> {
    let mut env = TestEnv::start(&[echo_engine(), sparkle_engine()], None)?;
    env.omime(&["enable", "echo"])?;
    env.omime(&["enable", "sparkle"])?;
    let list = env.omime(&["engines"])?;
    assert_eq!(enabled(&list, "echo"), Some(true), "{list}");
    assert_eq!(enabled(&list, "sparkle"), Some(true), "{list}");

    env.omime(&["switch", "echo"])?;
    // Disabling the engine in use falls back to direct input.
    env.omime(&["disable", "echo"])?;
    let list = env.omime(&["engines"])?;
    assert_eq!(enabled(&list, "echo"), Some(false), "{list}");
    assert_eq!(env.omime(&["status"])?.trim(), "direct input");
    let config = std::fs::read_to_string(env.dir().join("config/oh-my-ime/config.toml"))?;
    assert!(config.contains("engines = [\"sparkle\"]"), "{config}");

    // Only sparkle is left to cycle through.
    env.omime(&["next"])?;
    assert!(env.omime(&["status"])?.starts_with("sparkle"));
    env.omime(&["enable", "echo"])?;
    assert_eq!(enabled(&env.omime(&["engines"])?, "echo"), Some(true));

    // The daemon starts with the engine that was in use when it stopped.
    env.restart_daemon()?;
    assert!(env.omime(&["status"])?.starts_with("sparkle"));
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn engines_can_be_installed_and_uninstalled() -> anyhow::Result<()> {
    let env = TestEnv::start(&[], None)?;
    let program = program_dir().join("omime-echo");
    let source = env.dir().join("echo2");
    std::fs::create_dir_all(&source)?;
    std::fs::write(
        source.join("engine.toml"),
        format!(
            "[engine]\nid = \"echo2\"\nname = \"Echo 2\"\nexec = [{:?}]\napi = \"1.0\"\n",
            program.display().to_string()
        ),
    )?;
    let source = source.to_str().unwrap_or_default();

    let output = env.omime(&["install", source])?;
    assert!(output.contains("Installed echo2"), "{output}");
    let list = env.omime(&["engines"])?;
    assert_eq!(enabled(&list, "echo2"), Some(true), "{list}");
    env.omime(&["switch", "echo2"])?;
    assert!(env.omime(&["status"])?.starts_with("echo2"));

    env.omime(&["uninstall", "echo2"])?;
    let list = env.omime(&["engines"])?;
    assert!(!list.contains("echo2"), "{list}");
    assert_eq!(env.omime(&["status"])?.trim(), "direct input");
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_broken_config_is_reported_and_the_one_in_effect_kept() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine(), sparkle_engine()], None)?;
    env.configure("engines = [\"echo\"]\n")?;
    assert_eq!(enabled(&env.omime(&["engines"])?, "sparkle"), Some(false));

    // A value of the wrong type: the reload fails and says why, and the configuration
    // in effect stays.
    let path = env.dir().join("config/oh-my-ime/config.toml");
    std::fs::write(
        &path,
        "engines = [\"sparkle\"]\n[apps]\nremember = \"yes\"\n",
    )?;
    let error = format!("{:#}", env.omime(&["reload"]).unwrap_err());
    assert!(error.contains("remember"), "{error}");
    assert!(error.contains("keeps the configuration"), "{error}");
    let list = env.omime(&["engines"])?;
    assert_eq!(enabled(&list, "echo"), Some(true), "{list}");
    assert_eq!(enabled(&list, "sparkle"), Some(false), "{list}");

    // What this version does not know is left out, and the rest applies.
    env.configure("engines = [\"sparkle\"]\nfuture = 1\n[hotkeys]\ntogle = \"Super+space\"\n")?;
    let list = env.omime(&["engines"])?;
    assert_eq!(enabled(&list, "sparkle"), Some(true), "{list}");
    assert_eq!(enabled(&list, "echo"), Some(false), "{list}");
    let log = env.daemon_log();
    assert!(log.contains("future, hotkeys.togle"), "{log}");
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn reloading_a_nul_hotkey_keeps_the_daemon_alive() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    env.wait_for_log("engine ready")?;
    std::fs::write(
        env.dir().join("config/oh-my-ime/config.toml"),
        "engines = [\"echo\"]\n[hotkeys]\ntoggle = \"Control+\\u0000\"\n[panel]\nenabled = false\n",
    )?;
    // Rejecting the configuration or disabling this hotkey both keep input working.
    let _ = env.omime_status(&["reload"])?;
    let status = env.omime(&["status"]);
    assert!(
        status.is_ok(),
        "NUL in config terminated omimed: {status:?}\n{}",
        env.daemon_log()
    );
    let editor = env.editor(&[])?;
    env.type_text("ok ")?;
    editor.wait_for_text("OK ")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn starting_with_a_nul_hotkey_keeps_the_daemon_alive() -> anyhow::Result<()> {
    let mut env = TestEnv::prepare(&[echo_engine()])?;
    std::fs::write(
        env.dir().join("config/oh-my-ime/config.toml"),
        "engines = [\"echo\"]\n[hotkeys]\ntoggle = \"Control+\\u0000\"\n[panel]\nenabled = false\n",
    )?;
    env.launch_daemon()
        .with_context(|| format!("NUL in startup config\n{}", env.daemon_log()))?;
    env.omime(&["status"])?;
    env.omime(&["switch", "echo"])?;
    env.wait_for_log("engine ready")?;
    let editor = env.editor(&[])?;
    env.type_text("ok ")?;
    editor.wait_for_text("OK ")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn configuring_a_mistyped_section_keeps_the_daemon_alive() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
    env.wait_for_log("engine ready")?;
    let path = env.dir().join("config/oh-my-ime/config.toml");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let conn = zbus::connection::Builder::address(
            format!("unix:path={}/bus", env.dir().display()).as_str(),
        )
        .method_timeout(TIMEOUT)
        .build()
        .await?;
        let daemon = zbus::Proxy::new(
            &conn,
            "io.github.ign1x.OhMyIme",
            "/io/github/ign1x/OhMyIme",
            "io.github.ign1x.OhMyIme.Daemon1",
        )
        .await?;
        for (malformed, key, value, wire_value) in [
            (
                "apps = false",
                "apps.remember",
                "true",
                zbus::Variant::from(true),
            ),
            (
                "apps = 'oops'",
                "apps.remember",
                "true",
                zbus::Variant::from(true),
            ),
            (
                "apps = []",
                "apps.remember",
                "true",
                zbus::Variant::from(true),
            ),
            (
                "apps = 1",
                "apps.remember",
                "true",
                zbus::Variant::from(true),
            ),
            (
                "[[apps]]",
                "apps.remember",
                "true",
                zbus::Variant::from(true),
            ),
            (
                "tray = false",
                "tray.enabled",
                "true",
                zbus::Variant::from(true),
            ),
            (
                "engine = \"rime\"",
                "engine.key_timeout_ms",
                "300",
                zbus::Variant::from(300u64),
            ),
            (
                "hotkeys = []",
                "hotkeys.toggle",
                "Control+space",
                zbus::Variant::from("Control+space"),
            ),
        ] {
            let text = format!("# my settings\n{malformed}\n[panel]\nenabled = false\n");
            std::fs::write(&path, &text)?;
            let (reload, _) = env.omime_status(&["reload"])?;
            if malformed == "apps = false" {
                assert!(!reload.success(), "the invalid file must first be rejected");
            }
            env.omime(&["status"])?;

            // The settings app uses the same Configure method as the CLI.
            let changes = HashMap::from([(key, wire_value)]);
            let result = daemon.call::<_, _, ()>("Configure", &changes).await;
            let status = daemon
                .call::<_, _, HashMap<String, zbus::OwnedVariant>>("Status", &())
                .await;
            assert!(
                status.is_ok(),
                "Configure terminated omimed on {malformed:?}: {result:?}, {status:?}\n{}",
                env.daemon_log()
            );
            let error = result.expect_err("Configure must reject a malformed section");
            assert!(
                matches!(&error, zbus::Error::FDO(error)
                    if matches!(error.as_ref(), zbus::fdo::Error::Failed(_))),
                "{error}"
            );
            let section = key.split_once('.').context("dotted option")?.0;
            assert!(
                error.to_string().contains(&format!("`{section}`")),
                "{error}"
            );
            assert_eq!(std::fs::read(&path)?, text.as_bytes());

            let error = format!("{:#}", env.omime(&["config", key, value]).unwrap_err());
            assert!(error.contains(&format!("`{section}`")), "{error}");
            assert_eq!(std::fs::read(&path)?, text.as_bytes());
            env.omime(&["status"])?;
        }
        anyhow::Ok(())
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn sighup_reloads_the_configuration() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine(), sparkle_engine()], None)?;
    let path = env.dir().join("config/oh-my-ime/config.toml");
    std::fs::create_dir_all(path.parent().context("no parent")?)?;
    std::fs::write(&path, "engines = [\"echo\"]\n")?;
    env.signal_daemon(rustix::process::Signal::HUP)?;
    omime_e2e::harness::wait_until("omimed to read the configuration", || {
        let list = env.omime(&["engines"]).ok()?;
        (enabled(&list, "echo") == Some(true) && enabled(&list, "sparkle") == Some(false))
            .then_some(())
    })?;
    // It still runs.
    assert_eq!(enabled(&env.omime(&["engines"])?, "echo"), Some(true));
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn frameworks_are_offered_where_installed_and_enabled_when_asked() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    env.omime(&["enable", "echo"])?;
    // Manifests like the ones shipped for Fcitx 5 and IBus: for a program that is
    // installed, and for one that is not.
    let program = program_dir().join("omime-echo").display().to_string();
    for (id, requires) in [
        ("offered", program.as_str()),
        ("absent", "omime-no-such-program"),
    ] {
        let dir = env.dir().join("engines").join(id);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("engine.toml"),
            format!(
                "[engine]\nid = \"{id}\"\nname = \"{id}\"\nexec = [{program:?}]\napi = \"1.0\"\n\
                 requires = [{requires:?}]\nenabled_by_default = false\n"
            ),
        )?;
    }
    env.omime(&["reload"])?;
    let list = env.omime(&["engines"])?;
    assert_eq!(enabled(&list, "offered"), Some(false), "{list}");
    assert!(!list.contains("absent"), "{list}");
    assert_eq!(enabled(&list, "echo"), Some(true), "{list}");
    // Switching to it works without enabling it; enabling puts it in the cycle.
    env.omime(&["switch", "offered"])?;
    env.wait_for_status_starting("offered")?;
    env.omime(&["enable", "offered"])?;
    let list = env.omime(&["engines"])?;
    assert_eq!(enabled(&list, "offered"), Some(true), "{list}");
    assert_eq!(enabled(&list, "echo"), Some(true), "{list}");
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn options_are_set_through_omimed_keeping_the_file() -> anyhow::Result<()> {
    let env = TestEnv::start(&[echo_engine()], None)?;
    env.configure("# my settings\n[hotkeys]\n# mine too\nnext = \"Super+n\"\n")?;
    let config = env.omime(&["config"])?;
    assert!(config.contains("hotkeys.next = \"Super+n\""), "{config}");
    assert!(config.contains("apps.remember = false"), "{config}");

    env.omime(&["config", "apps.remember", "true"])?;
    env.omime(&["config", "engine.key_timeout_ms", "300"])?;
    assert!(env.omime(&["config"])?.contains("apps.remember = true"));
    let file = std::fs::read_to_string(env.dir().join("config/oh-my-ime/config.toml"))?;
    assert!(file.starts_with("# my settings\n"), "{file}");
    assert!(file.contains("# mine too\nnext = \"Super+n\""), "{file}");
    assert!(file.contains("key_timeout_ms = 300"), "{file}");

    // What is not valid is refused, and changes nothing.
    for (key, value) in [
        ("hotkeys.toggle", "Hyper+a"),
        ("apps.remember", "maybe"),
        ("apps.remeber", "true"),
        ("engines", "echo,gone"),
    ] {
        let (status, _) = env.omime_status(&["config", key, value])?;
        assert!(!status.success(), "{key} = {value}");
    }
    let unchanged = std::fs::read_to_string(env.dir().join("config/oh-my-ime/config.toml"))?;
    assert_eq!(unchanged, file);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn engines_with_input_methods_of_their_own_list_and_switch_them() -> anyhow::Result<()> {
    let mut env = TestEnv::start(
        &[dummy_engine(&["--hidden", "--input-methods", "alpha,beta"])],
        Some("dummy"),
    )?;
    // `next` goes round the engine's input methods only.
    env.configure("engines = [\"dummy\"]\n")?;
    let list = omime_e2e::harness::wait_until("the engine's input methods", || {
        let list = env.omime(&["list"]).ok()?;
        list.contains("dummy/beta").then_some(list)
    })?;
    assert!(
        list.contains("dummy/alpha") && list.contains("ALPHA"),
        "{list}"
    );
    env.wait_for_status_starting("dummy/alpha")?;

    env.omime(&["switch", "dummy/beta"])?;
    env.wait_for_log("engine: selected beta")
        .context("the initial native input-method switch")?;
    env.wait_for_status_starting("dummy/beta")?;
    // The next one wraps around to the first.
    env.omime(&["next"])?;
    env.wait_for_log("engine: selected alpha")?;
    env.wait_for_status_starting("dummy/alpha")?;

    // A restarted engine gets back the input method in use.
    env.omime(&["switch", "dummy/beta"])?;
    env.wait_for_status_starting("dummy/beta")?;
    env.restart_daemon()?;
    env.wait_for_log("engine: selected beta")
        .context("restoring the native input method after restart")?;
    env.wait_for_status_starting("dummy/beta")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn engines_have_menus_whose_entries_can_be_activated() -> anyhow::Result<()> {
    let env = TestEnv::start(
        &[dummy_engine(&["--hidden", "--properties"])],
        Some("dummy"),
    )?;
    let menu = omime_e2e::harness::wait_until("the engine's menu", || {
        let menu = env.omime(&["properties"]).ok()?;
        menu.contains("punct").then_some(menu)
    })?;
    assert!(menu.contains("[x] Chinese punctuation  (punct)"), "{menu}");
    assert!(menu.contains("Say hello  (hello)"), "{menu}");

    env.omime(&["activate", "punct"])?;
    env.wait_for_log("engine: activated punct")?;
    omime_e2e::harness::wait_until("the switch to flip", || {
        let menu = env.omime(&["properties"]).ok()?;
        menu.contains("[ ] Chinese punctuation").then_some(())
    })?;
    assert!(env.omime(&["activate", "nothing"]).is_err());
    // Direct input has no menu.
    env.omime(&["switch", "direct"])?;
    assert_eq!(env.omime(&["properties"])?, "");
    Ok(())
}

/// Permission tests must not inherit the developer's (often 077) umask. Set it in
/// a fresh child test process so parallel tests cannot race a process-global mask.
fn under_umask_022(name: &str, test: impl FnOnce() -> anyhow::Result<()>) -> anyhow::Result<()> {
    if std::env::var("OMIME_MANAGEMENT_MODE_TEST").as_deref() == Ok(name) {
        return test();
    }
    let output = Command::new("sh")
        .args(["-c", "umask 022; exec \"$@\"", "omime-mode-test"])
        .arg(std::env::current_exe()?)
        .args(["--exact", name, "--include-ignored", "--nocapture"])
        .env("OMIME_MANAGEMENT_MODE_TEST", name)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn mode(path: &Path) -> anyhow::Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

fn no_write_temporaries(dir: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        anyhow::ensure!(
            path.extension().is_none_or(|ext| ext != "tmp"),
            "temporary file left behind: {}",
            path.display()
        );
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn configure_keeps_private_file_permissions() -> anyhow::Result<()> {
    under_umask_022("configure_keeps_private_file_permissions", || {
        let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
        let path = env.dir().join("config/oh-my-ime/config.toml");
        for permissions in [0o600, 0o640, 0o644] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(permissions))?;
            env.omime(&["config", "apps.remember", "true"])?;
            assert_eq!(
                mode(&path)?,
                permissions,
                "Configure changed config.toml's mode"
            );
        }
        std::fs::remove_file(&path)?;
        env.omime(&["config", "apps.remember", "true"])?;
        assert_eq!(mode(&path)?, 0o600, "new config.toml is not private");
        no_write_temporaries(path.parent().context("config directory")?)
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn state_save_keeps_private_file_permissions() -> anyhow::Result<()> {
    under_umask_022("state_save_keeps_private_file_permissions", || {
        let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
        let path = env.dir().join("state/oh-my-ime/state.toml");
        wait_until("initial saved state", || path.exists().then_some(()))?;
        assert_eq!(mode(&path)?, 0o600, "new state.toml is not private");
        for (permissions, engine) in [(0o600, "direct"), (0o640, "echo"), (0o644, "direct")] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(permissions))?;
            env.omime(&["switch", engine])?;
            assert_eq!(
                mode(&path)?,
                permissions,
                "State::save changed state.toml's mode"
            );
        }
        no_write_temporaries(path.parent().context("state directory")?)
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn catalog_save_keeps_private_file_permissions() -> anyhow::Result<()> {
    under_umask_022("catalog_save_keeps_private_file_permissions", || {
        for previous_mode in [None, Some(0o600), Some(0o640), Some(0o644)] {
            let mut env = TestEnv::prepare(&[])?;
            // A disabled hosted framework advertises its installed methods without
            // launching. This exercises Catalog::save through normal daemon discovery.
            let prefix = env.dir().join("framework");
            let executable = prefix.join("bin/framework");
            std::fs::create_dir_all(executable.parent().context("bin directory")?)?;
            std::fs::write(&executable, "#!/bin/sh\nexit 1\n")?;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
            let methods = prefix.join("share/fcitx5/inputmethod");
            std::fs::create_dir_all(&methods)?;
            std::fs::write(
                methods.join("private-test.conf"),
                "[InputMethod]\nName=Private test\n",
            )?;
            let engine = env.dir().join("engines/framework");
            std::fs::create_dir_all(&engine)?;
            std::fs::write(
                engine.join("engine.toml"),
                format!(
                    "[engine]\nid = \"framework\"\nname = \"Framework\"\nexec = [{:?}]\nprotocol = \"input-method-v2\"\nenabled_by_default = false\n",
                    executable.display().to_string()
                ),
            )?;
            let path = env.dir().join("cache/oh-my-ime/catalog.toml");
            if let Some(permissions) = previous_mode {
                std::fs::create_dir_all(path.parent().context("cache directory")?)?;
                std::fs::write(&path, "")?;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(permissions))?;
            }
            env.launch_daemon()?;
            wait_until("saved installed catalog", || {
                std::fs::read_to_string(&path)
                    .ok()?
                    .contains("private-test")
                    .then_some(())
            })?;
            assert_eq!(mode(&path)?, previous_mode.unwrap_or(0o600));
            no_write_temporaries(path.parent().context("cache directory")?)?;
        }
        Ok(())
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn test_cli_install_keeps_private_config_mode() -> anyhow::Result<()> {
    under_umask_022("test_cli_install_keeps_private_config_mode", || {
        let env = TestEnv::prepare(&[])?;
        let source = env.dir().join("source");
        std::fs::create_dir(&source)?;
        std::fs::write(
            source.join("engine.toml"),
            "[engine]\nid = \"private-example\"\nname = \"Example\"\nexec = [\"false\"]\napi = \"1.0\"\n",
        )?;
        let path = env.dir().join("config/oh-my-ime/config.toml");
        std::fs::write(&path, "engines = []\n")?;
        for permissions in [0o600, 0o640, 0o644] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(permissions))?;
            env.omime(&["install", source.to_str().context("source path")?])?;
            assert_eq!(
                mode(&path)?,
                permissions,
                "install changed config.toml's mode"
            );
            assert!(std::fs::read_to_string(&path)?.contains("private-example"));
            env.omime(&["uninstall", "private-example"])?;
            assert_eq!(
                mode(&path)?,
                permissions,
                "uninstall changed config.toml's mode"
            );
        }
        no_write_temporaries(path.parent().context("config directory")?)
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs; requires OMIME_RIME_SHARED_DATA"]
fn rime_learned_text_is_private() -> anyhow::Result<()> {
    under_umask_022("rime_learned_text_is_private", || {
        std::env::var_os("OMIME_RIME_SHARED_DATA").context("requires OMIME_RIME_SHARED_DATA")?;
        for existing in [false, true] {
            let mut env = TestEnv::prepare(&[rime_engine()?])?;
            let path = env.dir().join("data/oh-my-ime/rime");
            if existing {
                std::fs::create_dir_all(&path)?;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
            }
            env.launch_daemon()?;
            env.omime(&["switch", "rime"])?;
            env.wait_for_log("omime-rime: librime")?;
            assert_eq!(mode(&path)?, 0o700, "Rime's user directory is not private");
        }
        Ok(())
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn sparkle_commit_history_is_private() -> anyhow::Result<()> {
    under_umask_022("sparkle_commit_history_is_private", || {
        for existing in [false, true] {
            let mut env = TestEnv::prepare(&[sparkle_engine()])?;
            let path = env.dir().join("state/oh-my-ime/sparkle");
            let dir = path.parent().context("state directory")?;
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
            if existing {
                std::fs::write(&path, "panel = false\nrecent = 🦀 ✨\n")?;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
            }
            env.launch_daemon()?;
            env.omime(&["switch", "sparkle"])?;
            let editor = env.editor(&[])?;
            env.type_text(":rocket ")?;
            editor.wait_for_text("🚀")?;
            wait_until("committed emoji saved", || {
                std::fs::read_to_string(&path)
                    .ok()?
                    .contains("recent = 🚀")
                    .then_some(())
            })?;
            assert_eq!(mode(&path)?, 0o600, "Sparkle's history is not private");
            assert_eq!(
                mode(dir)?,
                0o700,
                "Sparkle's state directory is not private"
            );
            no_write_temporaries(dir)?;
        }
        Ok(())
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs; requires OMIME_RIME_SHARED_DATA"]
fn rime_uses_linked_private_data_directory() -> anyhow::Result<()> {
    under_umask_022("rime_uses_linked_private_data_directory", || {
        std::env::var_os("OMIME_RIME_SHARED_DATA").context("requires OMIME_RIME_SHARED_DATA")?;
        let mut env = TestEnv::prepare(&[rime_engine()?])?;
        let target = env.dir().join("rime-storage");
        std::fs::create_dir(&target)?;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
        let path = env.dir().join("data/oh-my-ime/rime");
        std::fs::create_dir_all(path.parent().context("data directory")?)?;
        std::os::unix::fs::symlink(&target, &path)?;
        env.launch_daemon()?;
        env.omime(&["switch", "rime"])?;
        env.wait_for_log("engine ready")?;
        assert_eq!(
            mode(&target)?,
            0o700,
            "Rime did not make the linked directory private:\n{}",
            env.daemon_log()
        );
        assert_eq!(std::fs::read_link(&path)?, target);
        wait_until_within(
            Duration::from_secs(120),
            "Rime to deploy through the link",
            || {
                env.omime(&["list", "--all"])
                    .ok()?
                    .contains("rime/luna_pinyin")
                    .then_some(())
            },
        )
        .with_context(|| env.daemon_log())?;
        env.omime(&["switch", "rime/luna_pinyin"])?;
        env.wait_for_status_starting("rime/luna_pinyin")?;
        let editor = env.editor(&[])?;
        env.type_text("nihao ")?;
        editor.wait_for_text("你好")?;
        assert!(target.join("default.custom.yaml").is_file());
        assert_eq!(mode(&target)?, 0o700);
        assert_eq!(std::fs::read_link(&path)?, target);
        Ok(())
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn sparkle_uses_linked_private_state_directory() -> anyhow::Result<()> {
    under_umask_022("sparkle_uses_linked_private_state_directory", || {
        let mut env = TestEnv::prepare(&[sparkle_engine()])?;
        let target = env.dir().join("sparkle-storage");
        std::fs::create_dir(&target)?;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
        let history = target.join("sparkle");
        std::fs::write(&history, "panel = false\nrecent = 🦀 ✨\n")?;
        std::fs::set_permissions(&history, std::fs::Permissions::from_mode(0o644))?;
        let path = env.dir().join("state/oh-my-ime");
        std::fs::create_dir_all(path.parent().context("state directory")?)?;
        std::os::unix::fs::symlink(&target, &path)?;
        env.launch_daemon()?;
        env.omime(&["switch", "sparkle"])?;
        let editor = env.editor(&[])?;
        env.type_text(":rocket ")?;
        editor.wait_for_text("🚀")?;
        assert_eq!(
            mode(&target)?,
            0o700,
            "Sparkle did not make the linked directory private:\n{}",
            env.daemon_log()
        );
        wait_until("history saved through the directory link", || {
            std::fs::read_to_string(&history)
                .ok()?
                .contains("recent = 🚀")
                .then_some(())
        })?;
        assert_eq!(mode(&history)?, 0o600);
        assert_eq!(std::fs::read_link(&path)?, target);
        no_write_temporaries(&target)
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn failed_state_write_preserves_previous_sparkle_state() -> anyhow::Result<()> {
    under_umask_022(
        "failed_state_write_preserves_previous_sparkle_state",
        || {
            let mut engine = sparkle_engine();
            let executable = engine.program.display().to_string();
            engine.program = PathBuf::from("sh");
            // Ignore SIGXFSZ so a zero file-size limit yields a write error. Set the
            // limit after the UI starts so it can first allocate its shared buffers.
            engine.args = vec![
                "-c".into(),
                "trap '' XFSZ; exec \"$1\"".into(),
                "omime-sparkle-test".into(),
                executable,
            ];
            let mut env = TestEnv::prepare(&[engine])?;
            let path = env.dir().join("state/oh-my-ime/sparkle");
            let dir = path.parent().context("state directory")?;
            std::fs::create_dir_all(dir)?;
            let previous = "panel = false\nrecent = 🦀 🚀 ✨\n";
            std::fs::write(&path, previous)?;
            env.launch_daemon()?;
            env.omime(&["switch", "sparkle"])?;
            env.wait_for_log("engine window created")?;
            let editor = env.editor(&[])?;
            env.type_text(":unicorn")?;
            editor.wait_for("unicorn preedit", |line| {
                line.starts_with("preedit \":unicorn\"")
            })?;
            let pid = rustix::process::Pid::from_raw(
                env.engine_pid("sparkle")
                    .context("Sparkle PID")?
                    .try_into()?,
            )
            .context("valid Sparkle PID")?;
            rustix::process::prlimit(
                Some(pid),
                rustix::process::Resource::Fsize,
                rustix::process::Rlimit {
                    current: Some(0),
                    maximum: None,
                },
            )?;
            env.key(&[], "space")?;
            editor.wait_for_text("🦄")?;
            env.wait_for_log("cannot save")?;
            assert_eq!(
                std::fs::read_to_string(&path)?,
                previous,
                "a failed save destroyed the prior state"
            );
            no_write_temporaries(dir)
        },
    )
}
