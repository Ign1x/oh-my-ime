//! End-to-end tests for managing engines with the `omime` CLI.
//!
//! See `typing.rs` for how to run them.

use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use omime_e2e::harness::{TIMEOUT, TestEnv, echo_engine, program_dir, sparkle_engine};

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
fn engines_can_be_disabled_enabled_and_are_remembered() -> anyhow::Result<()> {
    let mut env = TestEnv::start(&[echo_engine(), sparkle_engine()], None)?;
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
