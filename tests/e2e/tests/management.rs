//! End-to-end tests for managing engines with the `omime` CLI.
//!
//! See `typing.rs` for how to run them.

use omime_e2e::harness::{TestEnv, echo_engine, program_dir, sparkle_engine};

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
fn engines_can_be_disabled_enabled_and_are_remembered() -> anyhow::Result<()> {
    let mut env = TestEnv::start(&[echo_engine(), sparkle_engine()], None)?;
    let list = env.omime(&["list"])?;
    assert_eq!(enabled(&list, "echo"), Some(true), "{list}");
    assert_eq!(enabled(&list, "sparkle"), Some(true), "{list}");

    env.omime(&["switch", "echo"])?;
    // Disabling the engine in use falls back to direct input.
    env.omime(&["disable", "echo"])?;
    let list = env.omime(&["list"])?;
    assert_eq!(enabled(&list, "echo"), Some(false), "{list}");
    assert_eq!(env.omime(&["status"])?.trim(), "direct input");
    let config = std::fs::read_to_string(env.dir().join("config/oh-my-ime/config.toml"))?;
    assert!(config.contains("engines = [\"sparkle\"]"), "{config}");

    // Only sparkle is left to cycle through.
    env.omime(&["next"])?;
    assert!(env.omime(&["status"])?.starts_with("sparkle"));
    env.omime(&["enable", "echo"])?;
    assert_eq!(enabled(&env.omime(&["list"])?, "echo"), Some(true));

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
    let list = env.omime(&["list"])?;
    assert_eq!(enabled(&list, "echo2"), Some(true), "{list}");
    env.omime(&["switch", "echo2"])?;
    assert!(env.omime(&["status"])?.starts_with("echo2"));

    env.omime(&["uninstall", "echo2"])?;
    let list = env.omime(&["list"])?;
    assert!(!list.contains("echo2"), "{list}");
    assert_eq!(env.omime(&["status"])?.trim(), "direct input");
    Ok(())
}
