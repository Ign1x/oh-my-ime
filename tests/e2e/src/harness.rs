//! A private headless compositor session for end-to-end tests.
//!
//! Each [`TestEnv`] gets its own `XDG_RUNTIME_DIR` with a headless sway, a private
//! D-Bus session bus, an empty configuration and an engine directory, so tests never
//! touch the developer's session and can run in parallel.
//!
//! The binaries under test are taken from the cargo target directory; build the whole
//! workspace first (`cargo build --workspace`).

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};

use crate::parse_json_string;

/// How long to wait for anything to happen.
pub const TIMEOUT: Duration = Duration::from_secs(10);

fn wait_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> anyhow::Result<T> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(value) = f() {
            return Ok(value);
        }
        if Instant::now() > deadline {
            bail!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The directory with the workspace's binaries (`target/debug`).
pub fn target_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("tests know their executable");
    let dir = exe.parent().expect("executables live in a directory");
    // Test binaries live in target/<profile>/deps.
    if dir.ends_with("deps") {
        dir.parent().unwrap_or(dir).to_path_buf()
    } else {
        dir.to_path_buf()
    }
}

/// An engine to install into the test environment.
#[derive(Debug, Clone)]
pub struct TestEngine {
    pub id: &'static str,
    /// Binary name in the target directory.
    pub binary: &'static str,
    pub args: Vec<String>,
}

/// The `sparkle` engine from `engines/sparkle`.
pub fn sparkle_engine() -> TestEngine {
    TestEngine {
        id: "sparkle",
        binary: "omime-sparkle",
        args: Vec::new(),
    }
}

/// The `echo` engine from `engines/echo`.
pub fn echo_engine() -> TestEngine {
    TestEngine {
        id: "echo",
        binary: "omime-echo",
        args: Vec::new(),
    }
}

/// A private compositor session with omimed running in it.
pub struct TestEnv {
    dir: PathBuf,
    env: Vec<(String, String)>,
    children: Vec<Child>,
    daemon_log: PathBuf,
}

impl std::fmt::Debug for TestEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestEnv").field("dir", &self.dir).finish()
    }
}

impl TestEnv {
    /// Starts sway, a session bus and omimed with `engines` installed. `start_engine`
    /// is passed as `--engine`.
    pub fn start(engines: &[TestEngine], start_engine: Option<&str>) -> anyhow::Result<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "omime-e2e-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;

        let target = target_dir();
        let engine_dir = dir.join("engines");
        for engine in engines {
            let path = engine_dir.join(engine.id);
            std::fs::create_dir_all(&path)?;
            let mut exec = vec![target.join(engine.binary).display().to_string()];
            exec.extend(engine.args.iter().cloned());
            let exec: Vec<String> = exec.iter().map(|a| format!("{a:?}")).collect();
            std::fs::write(
                path.join("engine.toml"),
                format!(
                    "[engine]\nid = \"{}\"\nname = \"{}\"\nexec = [{}]\napi = \"1.0\"\n",
                    engine.id,
                    engine.id,
                    exec.join(", ")
                ),
            )?;
        }
        // No borders or title bars: windows start at the output's origin, so tests can
        // compute where popups appear.
        std::fs::write(
            dir.join("sway.conf"),
            "output HEADLESS-1 resolution 1280x720\ndefault_border none\n",
        )?;

        let mut env = TestEnv {
            env: vec![
                ("XDG_RUNTIME_DIR".into(), dir.display().to_string()),
                (
                    "XDG_CONFIG_HOME".into(),
                    dir.join("config").display().to_string(),
                ),
                (
                    "XDG_DATA_HOME".into(),
                    dir.join("data").display().to_string(),
                ),
                (
                    "XDG_STATE_HOME".into(),
                    dir.join("state").display().to_string(),
                ),
                (
                    "XDG_DATA_DIRS".into(),
                    dir.join("system").display().to_string(),
                ),
                ("OMIME_ENGINE_PATH".into(), engine_dir.display().to_string()),
                (
                    "DBUS_SESSION_BUS_ADDRESS".into(),
                    format!("unix:path={}", dir.join("bus").display()),
                ),
            ],
            daemon_log: dir.join("omimed.log"),
            dir,
            children: Vec::new(),
        };

        let sway = env
            .command("sway")
            .arg("-c")
            .arg(env.dir.join("sway.conf"))
            .env("WLR_BACKENDS", "headless")
            .env("WLR_RENDERER", "pixman")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env("WLR_HEADLESS_OUTPUTS", "1")
            .stderr(std::fs::File::create(env.dir.join("sway.log"))?)
            .spawn()
            .context("cannot start sway (run the tests inside `nix develop`)")?;
        env.children.push(sway);
        let dir = env.dir.clone();
        let socket = wait_until("the compositor socket", || {
            std::fs::read_dir(&dir).ok()?.flatten().find_map(|e| {
                let name = e.file_name().into_string().ok()?;
                (name.starts_with("wayland-") && !name.ends_with(".lock")).then_some(name)
            })
        })?;
        env.env.push(("WAYLAND_DISPLAY".into(), socket));

        // A private bus without service activation, so that clients asking for portals
        // and the like cannot launch the developer's desktop services.
        let bus_config = env.dir.join("bus.conf");
        std::fs::write(
            &bus_config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                env.dir.join("bus").display()
            ),
        )?;
        let bus = env
            .command("dbus-daemon")
            .args(["--nofork", "--nopidfile"])
            .arg(format!("--config-file={}", bus_config.display()))
            .stdout(Stdio::null())
            .spawn()
            .context("cannot start dbus-daemon")?;
        env.children.push(bus);
        let bus_path = env.dir.join("bus");
        wait_until("the session bus", || bus_path.exists().then_some(()))?;

        let mut omimed = env.command(target.join("omimed"));
        if let Some(engine) = start_engine {
            omimed.args(["--engine", engine]);
        }
        let daemon = omimed
            .env("OMIME_LOG", "debug")
            // Headless sway renders with pixman and has no GPU buffers to offer.
            .env("SLINT_BACKEND", "winit-software")
            .stderr(std::fs::File::create(&env.daemon_log)?)
            .spawn()
            .context("cannot start omimed (build the workspace first)")?;
        env.children.push(daemon);
        let log = env.daemon_log.clone();
        wait_until("omimed to connect", || {
            std::fs::read_to_string(&log)
                .ok()?
                .contains("starting input method session")
                .then_some(())
        })?;
        Ok(env)
    }

    /// The runtime directory of this environment.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A command that runs inside this environment.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("GTK_IM_MODULE")
            .env_remove("QT_IM_MODULE")
            .env_remove("XMODIFIERS")
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null());
        command
    }

    /// Everything omimed logged so far.
    pub fn daemon_log(&self) -> String {
        std::fs::read_to_string(&self.daemon_log).unwrap_or_default()
    }

    /// The pid of the most recently launched process of engine `id`.
    pub fn engine_pid(&self, id: &str) -> Option<u32> {
        let marker = format!("engine=\"{id}\"");
        self.daemon_log()
            .lines()
            .rev()
            .filter(|l| l.contains("engine launched") && l.contains(&marker))
            .find_map(|l| {
                l.split("pid=")
                    .nth(1)?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
    }

    /// How often omimed's log contains `needle`.
    pub fn count_in_log(&self, needle: &str) -> usize {
        self.daemon_log().matches(needle).count()
    }

    /// Waits until omimed's log contains `needle`.
    pub fn wait_for_log(&self, needle: &str) -> anyhow::Result<()> {
        wait_until(&format!("`{needle}` in the omimed log"), || {
            self.daemon_log().contains(needle).then_some(())
        })
    }

    /// Opens the test editor and waits until it is focused.
    pub fn editor(&self, args: &[&str]) -> anyhow::Result<Editor> {
        const GRABBED: &str = "keyboard grabbed";
        const SENSITIVE: &str = "sensitive text field focused";
        let before = (self.count_in_log(GRABBED), self.count_in_log(SENSITIVE));
        let mut child = self
            .command(target_dir().join("omime-ti-client"))
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .context("cannot start omime-ti-client")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                sink.lock().unwrap_or_else(|p| p.into_inner()).push(line);
            }
        });
        let editor = Editor { child, lines };
        editor.wait_for("the editor to get focus", |l| l == "ready")?;
        // Typing only goes through omimed once it holds the keyboard (or decided to
        // stay out of a sensitive field).
        wait_until("omimed to take the focused field", || {
            let now = (self.count_in_log(GRABBED), self.count_in_log(SENSITIVE));
            (now.0 > before.0 || now.1 > before.1).then_some(())
        })?;
        Ok(editor)
    }

    /// Types `text` with a virtual keyboard (wtype).
    pub fn type_text(&self, text: &str) -> anyhow::Result<()> {
        self.run(self.command("wtype").arg("--").arg(text))
    }

    /// Presses and releases the key named `key` (xkb keysym name), holding `modifiers`
    /// (`ctrl`, `shift`, `alt`, `logo`).
    pub fn key(&self, modifiers: &[&str], key: &str) -> anyhow::Result<()> {
        let mut command = self.command("wtype");
        for m in modifiers {
            command.args(["-M", m]);
        }
        command.args(["-k", key]);
        for m in modifiers.iter().rev() {
            command.args(["-m", m]);
        }
        self.run(&mut command)
    }

    /// Clicks at `(x, y)` on the output with a virtual pointer.
    pub fn click(&self, x: u32, y: u32) -> anyhow::Result<()> {
        self.run(self.command(target_dir().join("omime-pointer")).args([
            "HEADLESS-1",
            &x.to_string(),
            &y.to_string(),
            "left",
        ]))
    }

    /// Takes a screenshot of the output.
    pub fn screenshot(&self, path: &Path) -> anyhow::Result<()> {
        self.run(self.command("grim").arg(path))
    }

    /// Runs the `omime` CLI and returns its output.
    pub fn omime(&self, args: &[&str]) -> anyhow::Result<String> {
        let output = self
            .command(target_dir().join("omime"))
            .args(args)
            .output()
            .context("cannot run omime")?;
        if !output.status.success() {
            bail!(
                "omime {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn run(&self, command: &mut Command) -> anyhow::Result<()> {
        let status = command.status()?;
        if !status.success() {
            bail!("{command:?} failed with {status}");
        }
        // Let the keys travel through the compositor, daemon and engine.
        std::thread::sleep(Duration::from_millis(150));
        Ok(())
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // omimed first, so it can release the keyboard; the compositor last.
        for child in self.children.iter_mut().rev() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // The test harness only shows this output for failing tests.
        for log in ["omimed.log", "sway.log"] {
            if let Ok(text) = std::fs::read_to_string(self.dir.join(log)) {
                let tail: Vec<&str> = text.lines().rev().take(80).collect();
                eprintln!("----- {log} (last {} lines)", tail.len());
                for line in tail.iter().rev() {
                    eprintln!("{line}");
                }
            }
        }
        if std::env::var_os("OMIME_E2E_KEEP").is_some() {
            eprintln!("keeping {}", self.dir.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// The test editor (`omime-ti-client`).
pub struct Editor {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Editor").finish_non_exhaustive()
    }
}

impl Editor {
    /// All lines printed so far.
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Waits for a line matching `pred` and returns it.
    pub fn wait_for(&self, what: &str, pred: impl Fn(&str) -> bool) -> anyhow::Result<String> {
        wait_until(what, || self.lines().into_iter().find(|l| pred(l)))
    }

    /// The editor's document.
    pub fn text(&self) -> String {
        self.lines()
            .iter()
            .rev()
            .find_map(|l| l.strip_prefix("text "))
            .and_then(|q| parse_json_string(q).map(|(s, _)| s))
            .unwrap_or_default()
    }

    /// Waits until the document equals `expected`.
    pub fn wait_for_text(&self, expected: &str) -> anyhow::Result<()> {
        wait_until(&format!("the text {expected:?}"), || {
            (self.text() == expected).then_some(())
        })
        .with_context(|| format!("the editor has {:?}", self.text()))
    }

    /// Every preedit the editor was shown.
    pub fn preedits(&self) -> Vec<String> {
        self.lines()
            .iter()
            .filter_map(|l| l.strip_prefix("preedit "))
            .filter_map(|q| parse_json_string(q).map(|(s, _)| s))
            .collect()
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
