//! Sway onboarding in a private compositor, bus and configuration tree.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context as _, ensure};
use omime_e2e::harness::{TestEnv, echo_engine, program_dir};

fn cli(env: &TestEnv) -> Command {
    let mut command = env.command(program_dir().join("omime"));
    command
        .env("XDG_CURRENT_DESKTOP", "sway")
        .env("XDG_SESSION_DESKTOP", "sway")
        .env_remove("HYPRLAND_INSTANCE_SIGNATURE");
    command
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn setup(env: &TestEnv, args: &[&str]) -> anyhow::Result<String> {
    let output = cli(env).arg("setup").args(args).output()?;
    let text = output_text(&output);
    ensure!(output.status.success(), "{text}");
    Ok(text)
}

fn snapshot(path: &Path) -> anyhow::Result<Vec<(PathBuf, Vec<u8>)>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            entries.extend(snapshot(&entry.path())?);
        } else {
            entries.push((entry.path(), fs::read(entry.path())?));
        }
    }
    entries.sort();
    Ok(entries)
}

#[test]
#[ignore = "end-to-end test; needs headless sway and the E2E shell"]
fn sway_setup_previews_validates_preserves_and_undoes_configuration() -> anyhow::Result<()> {
    let env = TestEnv::prepare(&[echo_engine()])?;
    let config_dir = env.dir().join("config");
    let state_dir = env.dir().join("state");
    let main = config_dir.join("sway/config");
    fs::create_dir_all(main.parent().context("Sway config parent")?)?;
    let original = "# User configuration\nset $mod Mod4\nfocus_follows_mouse no\n";
    fs::write(&main, original)?;
    let original_files = snapshot(&config_dir)?;
    let original_state = snapshot(&state_dir)?;

    // Auto detection must select Sway, and preview must leave all files untouched.
    setup(&env, &["--engine", "echo", "--dry-run"])?;
    assert_eq!(snapshot(&config_dir)?, original_files);
    assert_eq!(snapshot(&state_dir)?, original_state);
    setup(&env, &["--engine", "echo"])?;

    let snippet = config_dir.join("oh-my-ime/sway.conf");
    let uwsm = config_dir.join("uwsm/env-sway");
    let launch_env = config_dir.join("environment.d/90-oh-my-ime.conf");
    assert!(snippet.exists());
    assert!(uwsm.exists());
    assert!(launch_env.exists());
    assert!(!config_dir.join("hypr").exists());
    assert!(fs::read_to_string(&main)?.starts_with(original));

    // Parse through Sway itself, including the generated include. Validation does
    // not execute startup commands or import environment into a user manager.
    let validation = env
        .command("sway")
        .args(["--validate", "--config"])
        .arg(&main)
        .output()?;
    ensure!(
        validation.status.success(),
        "generated Sway configuration is invalid: {}",
        output_text(&validation)
    );
    let environment = env
        .command("sh")
        .args([
            "-c",
            ". \"$1\"; printf '%s\\n' \"$GTK_IM_MODULE\" \"$QT_IM_MODULE\" \"$QT_IM_MODULES\" \"$XMODIFIERS\"",
            "sh",
        ])
        .arg(&uwsm)
        .env("GTK_IM_MODULE", "ibus")
        .env("QT_IM_MODULE", "wayland")
        .output()?;
    ensure!(
        environment.status.success(),
        "{}",
        output_text(&environment)
    );
    assert_eq!(
        String::from_utf8(environment.stdout)?,
        "\nfcitx\nwayland;fcitx\n@im=fcitx\n"
    );

    let applied = snapshot(&config_dir)?;
    setup(&env, &[])?;
    assert_eq!(snapshot(&config_dir)?, applied);
    setup(&env, &["--undo", "--dry-run"])?;
    assert_eq!(snapshot(&config_dir)?, applied);

    // Undo also works outside Sway and preserves edits made after setup.
    let user_edit = "# Added after setup\n";
    let mut current = fs::read_to_string(&main)?;
    current.push_str(user_edit);
    fs::write(&main, current)?;
    let undo = cli(&env)
        .env("XDG_CURRENT_DESKTOP", "GNOME")
        .args(["setup", "--undo"])
        .output()?;
    ensure!(undo.status.success(), "{}", output_text(&undo));
    assert_eq!(fs::read_to_string(main)?, format!("{original}{user_edit}"));
    assert!(!snippet.exists());
    assert!(!uwsm.exists());
    assert!(!launch_env.exists());
    assert!(!state_dir.join("oh-my-ime/setup.toml").exists());
    Ok(())
}

#[test]
#[ignore = "end-to-end test; needs headless sway and the E2E shell"]
fn an_unsupported_desktop_identity_does_not_write_hyprland_configuration() -> anyhow::Result<()> {
    let env = TestEnv::prepare(&[echo_engine()])?;
    let config_dir = env.dir().join("config");
    let state_dir = env.dir().join("state");
    let before_config = snapshot(&config_dir)?;
    let before_state = snapshot(&state_dir)?;
    let output = cli(&env)
        .env("XDG_CURRENT_DESKTOP", "UnknownDesktop")
        .env("XDG_SESSION_DESKTOP", "unknown")
        .args(["setup", "--engine", "echo"])
        .output()?;
    assert!(!output.status.success(), "{}", output_text(&output));
    assert!(
        output_text(&output).contains("--desktop"),
        "{}",
        output_text(&output)
    );
    assert_eq!(snapshot(&config_dir)?, before_config);
    assert_eq!(snapshot(&state_dir)?, before_state);
    Ok(())
}

fn setup_daemon_pid(env: &TestEnv) -> anyhow::Result<Option<u32>> {
    let address = env
        .command("printenv")
        .arg("DBUS_SESSION_BUS_ADDRESS")
        .output()?;
    let address = String::from_utf8(address.stdout)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let connection = zbus::connection::Builder::address(address.trim())
                .build()
                .await?;
            match connection
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "GetConnectionUnixProcessID",
                    &"io.github.ign1x.OhMyIme",
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
                Err(error) => Err(error.into()),
            }
        })
}

#[test]
#[ignore = "end-to-end test; needs headless sway and the E2E shell"]
fn sway_setup_start_composes_from_the_first_key() -> anyhow::Result<()> {
    let mut env = TestEnv::prepare(&[echo_engine()])?;
    let mut command = cli(&env);
    if std::env::var_os("OMIME_E2E_INSTALL_DIR").is_some() {
        command.env_remove("LD_LIBRARY_PATH");
    }
    let output = command
        .env("OMIME_LOG", "debug")
        .args(["setup", "--desktop", "sway", "--engine", "echo", "--start"])
        .output()?;
    // Adopt even when setup failed, so this private daemon is always cleaned up.
    if let Some(pid) = setup_daemon_pid(&env)? {
        env.adopt_setup_daemon(pid)?;
    }
    ensure!(output.status.success(), "{}", output_text(&output));
    env.wait_for_status("echo")?;
    let editor = env.editor(&[])?;
    env.type_text("sway ")?;
    editor.wait_for_text("SWAY ")?;
    assert!(editor.preedits().iter().any(|text| text == "sway"));
    Ok(())
}
