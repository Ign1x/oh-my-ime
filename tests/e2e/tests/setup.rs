//! First-run setup from empty private configuration, using real compositor,
//! D-Bus and input-method processes. See typing.rs for the E2E environment.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};

use anyhow::{Context as _, ensure};
use omime_e2e::harness::{TestEngine, TestEnv, echo_engine, program_dir, wait_until};

fn program(name: &str) -> anyhow::Result<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .with_context(|| format!("{name} must be installed in the E2E environment"))
}

fn bus_pid(env: &TestEnv, name: &str) -> anyhow::Result<Option<u32>> {
    let address = env
        .command("printenv")
        .arg("DBUS_SESSION_BUS_ADDRESS")
        .output()?;
    let address = String::from_utf8(address.stdout)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let conn = zbus::connection::Builder::address(address.trim())
                .build()
                .await?;
            match conn
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "GetConnectionUnixProcessID",
                    &name,
                )
                .await
            {
                Ok(reply) => Ok(Some(reply.body().deserialize()?)),
                Err(zbus::Error::MethodError(name, ..))
                    if name.as_str() == "org.freedesktop.DBus.Error.NameHasNoOwner" =>
                {
                    Ok(None)
                }
                Err(zbus::Error::FDO(error))
                    if matches!(*error, zbus::fdo::Error::NameHasNoOwner(_)) =>
                {
                    Ok(None)
                }
                Err(e) => Err(e.into()),
            }
        })
}

fn start(env: &mut TestEnv, args: &[&str]) -> anyhow::Result<String> {
    start_with_env(env, args, &[])
}

fn start_with_env(
    env: &mut TestEnv,
    args: &[&str],
    variables: &[(&str, &str)],
) -> anyhow::Result<String> {
    let mut command = env.command(program_dir().join("omime"));
    if std::env::var_os("OMIME_E2E_INSTALL_DIR").is_some() {
        // A packaged daemon started by the CLI must also find its libraries
        // without the development shell's help.
        command.env_remove("LD_LIBRARY_PATH");
    }
    let output = command
        .env("OMIME_LOG", "debug")
        .envs(variables.iter().copied())
        .args(args)
        .output()?;
    // Adopt even after a failed startup, so no test leaves a daemon behind.
    if let Some(pid) = bus_pid(env, "io.github.ign1x.OhMyIme")? {
        env.adopt_setup_daemon(pid)?;
    }
    ensure!(
        output.status.success(),
        "setup: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn verify_hyprland(env: &TestEnv, config: &Path) -> anyhow::Result<()> {
    let output = env
        .command("Hyprland")
        .arg("--verify-config")
        .arg("--config")
        .arg(config)
        .output()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        output.status.success() && text.contains("config ok"),
        "invalid generated Hyprland configuration:\n{text}"
    );
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn setup_previews_configures_and_undoes_both_hyprland_formats() -> anyhow::Result<()> {
    for ext in ["lua", "conf"] {
        let env = TestEnv::prepare(&[echo_engine()])?;
        let main = env.dir().join(format!("config/hypr/hyprland.{ext}"));
        let path = main.to_str().context("UTF-8 test path")?;
        let journal = env.dir().join("state/oh-my-ime/setup.toml");
        let original_config = fs::read_to_string(env.dir().join("config/oh-my-ime/config.toml"))?;
        let preview = env.omime(&["setup", "--engine", "echo", "--config", path, "--dry-run"])?;
        assert!(preview.contains("Would Create"), "{preview}");
        assert!(!main.exists());
        assert!(!journal.exists());
        let uwsm = env.dir().join("config/uwsm/env-hyprland");
        let launch_env = env.dir().join("config/environment.d/90-oh-my-ime.conf");
        assert!(!uwsm.exists());
        assert!(!launch_env.exists());
        env.omime(&["setup", "--engine", "echo", "--config", path])?;
        let output = env
            .command("sh")
            .args([
                "-c",
                ". \"$1\"; printf '%s\\n' \"$QT_IM_MODULE\" \"$QT_IM_MODULES\" \"$XMODIFIERS\"",
                "sh",
            ])
            .arg(&uwsm)
            .env("QT_IM_MODULE", "wayland")
            .output()?;
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout)?,
            "fcitx\nwayland;fcitx\n@im=fcitx\n"
        );
        verify_hyprland(&env, &main)?;
        let once = fs::read(&main)?;
        env.omime(&["setup"])?;
        assert_eq!(fs::read(&main)?, once);
        env.omime(&["setup", "--undo", "--dry-run"])?;
        assert!(main.exists());
        env.omime(&["setup", "--undo"])?;
        assert!(!main.exists());
        assert!(!journal.exists());
        assert!(!uwsm.exists());
        assert!(!launch_env.exists());
        assert_eq!(
            fs::read_to_string(env.dir().join("config/oh-my-ime/config.toml"))?,
            original_config
        );
    }
    Ok(())
}

struct UserManager(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

#[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
impl UserManager {
    #[zbus(property)]
    fn environment(&self) -> Vec<String> {
        self.0.lock().expect("test manager lock").clone()
    }

    fn set_environment(&self, values: Vec<String>) {
        let mut environment = self.0.lock().expect("test manager lock");
        for value in values {
            let key = value.split_once('=').expect("assignment").0;
            environment.retain(|s| !s.starts_with(&format!("{key}=")));
            environment.push(value);
        }
    }
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn setup_updates_the_matching_launch_manager_and_leaves_other_sessions_alone() -> anyhow::Result<()>
{
    for same_session in [true, false] {
        let env = TestEnv::start(&[echo_engine()], Some("echo"))?;
        let variable = |name: &str| -> anyhow::Result<String> {
            let out = env.command("printenv").arg(name).output()?;
            Ok(String::from_utf8(out.stdout)?.trim().to_owned())
        };
        let bus = variable("DBUS_SESSION_BUS_ADDRESS")?;
        let display = if same_session {
            variable("WAYLAND_DISPLAY")?
        } else {
            "another-desktop".into()
        };
        let values = std::sync::Arc::new(std::sync::Mutex::new(vec![
            format!("WAYLAND_DISPLAY={display}"),
            "QT_IM_MODULE=wayland".into(),
            "KEEP_ME=unchanged".into(),
        ]));
        let before = values.lock().unwrap().clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let _manager = zbus::connection::Builder::address(bus.as_str())
                .name("org.freedesktop.systemd1")
                .serve_at("/org/freedesktop/systemd1", UserManager(values.clone()))
                .build()
                .await?;
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let out = env
                    .command(program_dir().join("omime"))
                    .args(["setup", "--engine", "echo", "--start"])
                    .output()?;
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                assert_eq!(out.status.success(), same_session, "{text}");
                if same_session {
                    assert!(
                        text.contains("D-Bus and the systemd user manager"),
                        "{text}"
                    );
                } else {
                    assert!(text.contains("another session"), "{text}");
                }
                Ok(())
            })
            .await??;
            Ok::<_, anyhow::Error>(())
        })?;
        let after = values.lock().unwrap();
        if same_session {
            assert!(after.contains(&"QT_IM_MODULE=fcitx".into()));
            assert!(after.contains(&"QT_IM_MODULES=wayland;fcitx".into()));
            assert!(after.contains(&"XMODIFIERS=@im=fcitx".into()));
            assert!(after.contains(&"KEEP_ME=unchanged".into()));
        } else {
            assert_eq!(*after, before);
        }
    }
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn first_login_uses_the_chosen_method_and_later_logins_remember_direct_input() -> anyhow::Result<()>
{
    let mut env = TestEnv::prepare(&[echo_engine()])?;
    env.omime(&["setup", "--engine", "echo"])?;
    env.launch_daemon()?;
    env.wait_for_status_starting("echo")?;
    let editor = env.editor(&[])?;
    env.type_text("hello ")?;
    editor.wait_for_text("HELLO ")?;
    env.omime(&["switch", "direct"])?;
    env.restart_daemon()?;
    env.wait_for_status("direct input")?;
    env.type_text("plain")?;
    editor.wait_for_text("HELLO plain")?;
    Ok(())
}

fn fcitx5() -> anyhow::Result<TestEngine> {
    Ok(TestEngine {
        id: "fcitx5",
        program: program("fcitx5")?,
        args: Vec::new(),
        env: Vec::new(),
        hosted: true,
    })
}

fn offered(env: &TestEnv, id: &str) -> anyhow::Result<()> {
    let path = env.dir().join(format!("engines/{id}/engine.toml"));
    let text = fs::read_to_string(&path)? + "enabled_by_default = false\n";
    fs::write(path, text)?;
    Ok(())
}

fn ibus() -> anyhow::Result<TestEngine> {
    program("ibus-daemon")?;
    Ok(TestEngine {
        id: "ibus",
        program: program_dir().join("omime-ibus"),
        args: Vec::new(),
        env: Vec::new(),
        hosted: true,
    })
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn setup_starts_real_ibus_pinyin_from_empty_configuration() -> anyhow::Result<()> {
    let mut env = TestEnv::prepare(&[ibus()?])?;
    offered(&env, "ibus")?;
    start(
        &mut env,
        &["setup", "--engine", "ibus/libpinyin", "--start"],
    )?;
    let editor = env.editor(&["--app-id", "setup-first"])?;
    env.type_on_seat("seat0", "nihao ")?;
    editor.wait_for_text("你好")?;
    env.type_on_seat("seat0", "zhongguo ")?;
    editor.wait_for_text("你好中国")?;
    // Every new field creates an asynchronous IBus context, not just the first one.
    let other = env.editor(&["--app-id", "setup-second"])?;
    env.type_on_seat("seat0", "nihao ")?;
    other.wait_for_text("你好")?;
    env.focus("setup-first")?;
    env.type_on_seat("seat0", "nihao ")?;
    editor.wait_for_text("你好中国你好")?;
    env.omime(&["setup", "--start"])?;
    env.wait_for_status_starting("ibus/libpinyin")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn setup_starts_real_pinyin_from_empty_configuration() -> anyhow::Result<()> {
    let mut env = TestEnv::prepare(&[fcitx5()?])?;
    offered(&env, "fcitx5")?;
    let result = start(&mut env, &["setup", "--engine", "fcitx5/pinyin", "--start"])?;
    assert!(result.contains("Input is ready"), "{result}");
    let (_, doctor) = env.omime_status(&["doctor"])?;
    let pid = env.engine_pid("fcitx5").context("hosted Fcitx5 pid")?;
    assert!(!doctor.contains(&format!("fcitx5 (pid {pid})")), "{doctor}");
    assert!(
        !doctor.contains("`fcitx5/pinyin` is in `engines` but not installed"),
        "{doctor}"
    );
    let editor = env.editor(&[])?;
    env.type_on_seat("seat0", "nihao ")?;
    editor.wait_for_text("你好")?;
    env.type_on_seat("seat0", "zhongguo ")?;
    editor.wait_for_text("你好中国")?;
    // Running setup again must not mistake the hosted framework for a competitor
    // or override a saved direct-input choice.
    env.omime(&["switch", "direct"])?;
    env.omime(&["setup", "--start"])?;
    env.wait_for_status("direct input")?;
    Ok(())
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn takeover_exits_the_old_fcitx5_and_restores_its_startup_on_undo() -> anyhow::Result<()> {
    let mut env = TestEnv::prepare(&[fcitx5()?])?;
    offered(&env, "fcitx5")?;
    let log = fs::File::create(env.dir().join("old-fcitx5.log"))?;
    let mut old = Process(
        env.command("fcitx5")
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?,
    );
    wait_until("the old Fcitx5 on the session bus", || {
        bus_pid(&env, "org.fcitx.Fcitx5").ok().flatten()
    })?;
    let (status, _) = env.omime_status(&["setup", "--engine", "fcitx5/pinyin", "--start"])?;
    assert!(
        !status.success(),
        "an active framework needs an explicit takeover"
    );
    assert!(old.0.try_wait()?.is_none());
    let journal = env.dir().join("state/oh-my-ime/setup.toml");
    assert!(!journal.exists());
    let main = env.dir().join("config/hypr/hyprland.conf");
    fs::create_dir_all(main.parent().context("config parent")?)?;
    let original = "# desktop\nexec-once = fcitx5 -d\n";
    fs::write(&main, original)?;
    start(
        &mut env,
        &[
            "setup",
            "--engine",
            "fcitx5/pinyin",
            "--start",
            "--take-over",
        ],
    )?;
    wait_until("the old Fcitx5 to exit", || old.0.try_wait().ok().flatten())?;
    assert!(fs::read_to_string(&main)?.contains("disabled startup"));
    verify_hyprland(&env, &main)?;
    let editor = env.editor(&[])?;
    env.type_on_seat("seat0", "nihao ")?;
    editor.wait_for_text("你好")?;
    env.omime(&["setup", "--undo"])?;
    assert_eq!(fs::read_to_string(main)?, original);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn takeover_exits_the_old_ibus_and_starts_a_fresh_hosted_bus() -> anyhow::Result<()> {
    let mut env = TestEnv::prepare(&[ibus()?])?;
    offered(&env, "ibus")?;
    let address = format!("unix:path={}", env.dir().join("old-ibus").display());
    let address_file = env.dir().join("old-ibus-address");
    let log = fs::File::create(env.dir().join("old-ibus.log"))?;
    let mut old = Process(
        env.command("ibus-daemon")
            .args(["--single", "--address", &address])
            .env("IBUS_ADDRESS_FILE", &address_file)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?,
    );
    wait_until("the old IBus to publish its address", || {
        fs::read_to_string(&address_file)
            .ok()
            .filter(|text| text.contains("IBUS_ADDRESS="))
    })?;
    let variables = [
        ("IBUS_ADDRESS", address.as_str()),
        (
            "IBUS_ADDRESS_FILE",
            address_file.to_str().context("UTF-8 test path")?,
        ),
    ];
    let error = start_with_env(
        &mut env,
        &["setup", "--engine", "ibus/libpinyin", "--start"],
        &variables,
    )
    .expect_err("an active IBus needs an explicit takeover");
    assert!(error.to_string().contains("--take-over"), "{error:#}");
    assert!(old.0.try_wait()?.is_none());
    assert!(!env.dir().join("state/oh-my-ime/setup.toml").exists());
    let main = env.dir().join("config/hypr/hyprland.lua");
    fs::create_dir_all(main.parent().context("config parent")?)?;
    let original =
        "hl.on(\"hyprland.start\", function()\n    hl.exec_cmd(\"ibus-daemon -dr\")\nend)\n";
    fs::write(&main, original)?;
    start_with_env(
        &mut env,
        &[
            "setup",
            "--engine",
            "ibus/libpinyin",
            "--take-over",
            "--start",
        ],
        &variables,
    )?;
    wait_until("the old IBus to exit", || old.0.try_wait().ok().flatten())?;
    verify_hyprland(&env, &main)?;
    let editor = env.editor(&[])?;
    env.type_on_seat("seat0", "nihao ")?;
    editor.wait_for_text("你好")?;
    env.omime(&["setup", "--undo"])?;
    assert_eq!(fs::read_to_string(main)?, original);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn a_missing_pinyin_addon_is_reported_instead_of_claiming_input_is_ready() -> anyhow::Result<()> {
    let mut env = TestEnv::prepare(&[fcitx5()?])?;
    offered(&env, "fcitx5")?;
    let error = start(
        &mut env,
        &["setup", "--engine", "fcitx5/missing-addon", "--start"],
    )
    .expect_err("a missing addon cannot be ready");
    assert!(
        error
            .to_string()
            .contains("missing-addon` is not installed"),
        "{error:#}"
    );
    // The failure keeps the adapter available, so selecting an installed method
    // can recover in the same session.
    let editor = env.editor(&[])?;
    env.type_on_seat("seat0", "abc")?;
    editor.wait_for_text("abc")?; // A failed selection must not hold keys forever.
    start(&mut env, &["setup", "--engine", "fcitx5/pinyin", "--start"])?;
    env.type_on_seat("seat0", "nihao ")?;
    editor.wait_for_text("abc你好")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see typing.rs"]
fn invalid_engine_or_missing_wayland_session_changes_no_configuration() -> anyhow::Result<()> {
    let env = TestEnv::prepare(&[echo_engine()])?;
    let config = env.dir().join("config/oh-my-ime/config.toml");
    let before = fs::read(&config)?;
    let (status, _) = env.omime_status(&["setup", "--engine", "missing"])?;
    assert!(!status.success());
    let output = env
        .command(program_dir().join("omime"))
        .env_remove("WAYLAND_DISPLAY")
        .args(["setup", "--engine", "echo", "--start"])
        .output()?;
    assert!(!output.status.success());
    assert_eq!(fs::read(config)?, before);
    assert!(!env.dir().join("config/hypr/hyprland.lua").exists());
    assert!(!env.dir().join("state/oh-my-ime/setup.toml").exists());
    Ok(())
}
