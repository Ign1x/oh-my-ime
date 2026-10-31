//! A private headless compositor session for end-to-end tests.
//!
//! Each [`TestEnv`] gets its own `XDG_RUNTIME_DIR` with a headless sway, a private
//! D-Bus session bus, an empty configuration and an engine directory, so tests never
//! touch the developer's session and can run in parallel.
//!
//! The binaries under test are taken from the cargo target directory; build the whole
//! workspace first (`cargo build --workspace`). To test an installed build instead, point
//! `OMIME_E2E_INSTALL_DIR` at its `bin` directory (for example `result/bin` of the Nix
//! package); the test tools still come from the target directory.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};

use crate::parse_json_string;

/// How long to wait for anything to happen.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Calls `f` until it returns a value, for at most [`TIMEOUT`].
pub fn wait_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> anyhow::Result<T> {
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

const INSTALL_DIR_ENV: &str = "OMIME_E2E_INSTALL_DIR";

/// The directory with the programs under test: omimed, omime and the bundled engines.
pub fn program_dir() -> PathBuf {
    std::env::var_os(INSTALL_DIR_ENV).map_or_else(target_dir, PathBuf::from)
}

/// An engine to install into the test environment.
#[derive(Debug, Clone)]
pub struct TestEngine {
    pub id: &'static str,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Whether the engine is a Wayland input method that omimed hosts.
    pub hosted: bool,
}

/// The `sparkle` engine from `engines/sparkle`.
pub fn sparkle_engine() -> TestEngine {
    TestEngine {
        id: "sparkle",
        program: program_dir().join("omime-sparkle"),
        args: Vec::new(),
        hosted: false,
    }
}

/// The `echo` engine from `engines/echo`.
pub fn echo_engine() -> TestEngine {
    TestEngine {
        id: "echo",
        program: program_dir().join("omime-echo"),
        args: Vec::new(),
        hosted: false,
    }
}

/// `omime-dummy-ui`, an engine that draws a window of one color and consumes nothing.
pub fn dummy_engine(args: &[&str]) -> TestEngine {
    TestEngine {
        id: "dummy",
        program: target_dir().join("omime-dummy-ui"),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        hosted: false,
    }
}

/// The file `omime-im-client` reports to, in the environment's directory.
pub const HOSTED_LOG: &str = "hosted.log";

/// `omime-im-client`, a Wayland input method that omimed hosts as the engine
/// `hosted`. It composes like `echo` and reports to [`HOSTED_LOG`].
pub fn hosted_engine(args: &[&str]) -> TestEngine {
    let mut all = vec!["--log".to_owned(), HOSTED_LOG.to_owned()];
    all.extend(args.iter().map(|a| (*a).to_owned()));
    TestEngine {
        id: "hosted",
        program: target_dir().join("omime-im-client"),
        args: all,
        hosted: true,
    }
}

/// A private compositor session with omimed running in it.
pub struct TestEnv {
    dir: PathBuf,
    env: Vec<(String, String)>,
    children: Vec<Child>,
    daemon: Option<Child>,
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
        Self::start_with_seats(engines, start_engine, &[])
    }

    /// Like [`Self::start`], with more seats than `seat0`.
    pub fn start_with_seats(
        engines: &[TestEngine],
        start_engine: Option<&str>,
        extra_seats: &[&str],
    ) -> anyhow::Result<Self> {
        Self::start_with(engines, start_engine, extra_seats, false)
    }

    /// Like [`Self::start`], with an X server: sway runs XWayland, and everything
    /// started afterwards sees `$DISPLAY` and `XMODIFIERS=@im=omime`, so omimed serves
    /// XIM and X11 applications find it.
    pub fn start_with_x11(
        engines: &[TestEngine],
        start_engine: Option<&str>,
    ) -> anyhow::Result<Self> {
        Self::start_with(engines, start_engine, &[], true)
    }

    fn start_with(
        engines: &[TestEngine],
        start_engine: Option<&str>,
        extra_seats: &[&str],
        x11: bool,
    ) -> anyhow::Result<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "omime-e2e-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;

        let engine_dir = dir.join("engines");
        for engine in engines {
            let path = engine_dir.join(engine.id);
            std::fs::create_dir_all(&path)?;
            let mut exec = vec![engine.program.display().to_string()];
            exec.extend(engine.args.iter().cloned());
            let exec: Vec<String> = exec.iter().map(|a| format!("{a:?}")).collect();
            let protocol = if engine.hosted {
                "protocol = \"input-method-v2\""
            } else {
                "api = \"1.0\""
            };
            std::fs::write(
                path.join("engine.toml"),
                format!(
                    "[engine]\nid = \"{}\"\nname = \"{}\"\nexec = [{}]\n{protocol}\n",
                    engine.id,
                    engine.id,
                    exec.join(", ")
                ),
            )?;
        }
        // No borders or title bars: windows start at the output's origin, so tests can
        // compute where popups appear.
        let mut config = "output HEADLESS-1 resolution 1280x720\ndefault_border none\n".to_owned();
        for seat in extra_seats {
            config += &format!("seat {seat} fallback false\n");
        }
        if x11 {
            config += "xwayland enable\n";
        }
        std::fs::write(dir.join("sway.conf"), config)?;

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
            daemon: None,
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
        let ipc = wait_until("the sway IPC socket", || {
            std::fs::read_dir(&dir).ok()?.flatten().find_map(|e| {
                let name = e.file_name().into_string().ok()?;
                (name.starts_with("sway-ipc.") && name.ends_with(".sock")).then(|| e.path())
            })
        })?;
        env.env.push(("SWAYSOCK".into(), ipc.display().to_string()));

        if x11 {
            let display = env.wait_for_xwayland()?;
            env.env.push(("DISPLAY".into(), display));
            env.env.push(("XMODIFIERS".into(), "@im=omime".into()));
        }

        // Stand in for physical keyboards: without one, a seat would lose its keyboard
        // capability, and with it the keyboard focus, whenever omimed restarts.
        for seat in std::iter::once("seat0").chain(extra_seats.iter().copied()) {
            let mut keyboard = env
                .command(target_dir().join("omime-keyboard"))
                .args(["--seat", seat])
                .stdout(Stdio::piped())
                .spawn()
                .context("cannot start omime-keyboard (build the workspace first)")?;
            let stdout = keyboard.stdout.take().context("no stdout")?;
            env.children.push(keyboard);
            let mut line = String::new();
            BufReader::new(stdout).read_line(&mut line)?;
            if line.trim() != "ready" {
                bail!("omime-keyboard did not start on {seat}");
            }
        }

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

        env.start_daemon(start_engine)?;
        Ok(env)
    }

    /// Asks sway for the XWayland display it started and waits until it has one.
    ///
    /// sway sets `$DISPLAY` in its own environment when XWayland comes up, so `exec`
    /// expands it; the first attempts may run before the X server is ready and write an
    /// empty line.
    fn wait_for_xwayland(&self) -> anyhow::Result<String> {
        let file = self.dir.join("display");
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let command = format!("echo $DISPLAY > '{}'", file.display());
            let _ = self
                .command("swaymsg")
                .args(["exec", command.as_str()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if let Ok(display) = std::fs::read_to_string(&file) {
                let display = display.trim();
                if !display.is_empty() {
                    return Ok(display.to_owned());
                }
            }
            if Instant::now() > deadline {
                bail!("XWayland did not start (is `xwayland` installed?)");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn start_daemon(&mut self, engine: Option<&str>) -> anyhow::Result<()> {
        let mut omimed = self.command(program_dir().join("omimed"));
        if let Some(engine) = engine {
            omimed.args(["--engine", engine]);
        }
        if std::env::var_os(INSTALL_DIR_ENV).is_some() {
            // An installed build must find its libraries without the dev shell's help.
            omimed.env_remove("LD_LIBRARY_PATH");
        }
        let daemon = omimed
            .env("OMIME_LOG", "debug")
            // Headless sway renders with pixman and has no GPU buffers to offer.
            .env("SLINT_BACKEND", "winit-software")
            .stderr(std::fs::File::create(&self.daemon_log)?)
            .spawn()
            .context("cannot start omimed (build the workspace first)")?;
        self.daemon = Some(daemon);
        let log = self.daemon_log.clone();
        wait_until("omimed to connect", || {
            std::fs::read_to_string(&log)
                .ok()?
                .contains("starting input method session")
                .then_some(())
        })?;
        Ok(())
    }

    /// Stops omimed (SIGTERM, like a session ending) and starts it again without
    /// `--engine`.
    pub fn restart_daemon(&mut self) -> anyhow::Result<()> {
        if let Some(mut daemon) = self.daemon.take() {
            rustix::process::kill_process(
                rustix::process::Pid::from_child(&daemon),
                rustix::process::Signal::TERM,
            )?;
            daemon.wait()?;
        }
        self.start_daemon(None)
    }

    /// The runtime directory of this environment.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A command that runs inside this environment. The programs under test come first
    /// on its `$PATH`, for manifests that name them.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let path = match std::env::var_os("PATH") {
            Some(path) => {
                let mut paths = vec![program_dir()];
                paths.extend(std::env::split_paths(&path));
                std::env::join_paths(paths).unwrap_or(path)
            }
            None => program_dir().into_os_string(),
        };
        let mut command = Command::new(program);
        command
            .env("PATH", path)
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

    /// What `omime-im-client` reported so far (see [`hosted_engine`]).
    pub fn hosted_lines(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join(HOSTED_LOG))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Waits until `omime-im-client` reports a line matching `pred`, and returns it.
    pub fn wait_for_hosted(
        &self,
        what: &str,
        pred: impl Fn(&str) -> bool,
    ) -> anyhow::Result<String> {
        wait_until(what, || {
            self.hosted_lines().into_iter().find(|line| pred(line))
        })
        .with_context(|| format!("the hosted input method said: {:?}", self.hosted_lines()))
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
        let editor = self.spawn_editor(args)?;
        editor.wait_for("the editor to get focus", |l| l == "ready")?;
        // Typing only goes through omimed once it holds the keyboard (or decided to
        // stay out of a sensitive field).
        wait_until("omimed to take the focused field", || {
            let now = (self.count_in_log(GRABBED), self.count_in_log(SENSITIVE));
            (now.0 > before.0 || now.1 > before.1).then_some(())
        })?;
        Ok(editor)
    }

    /// Opens the test editor without waiting for it to get focus.
    pub fn spawn_editor(&self, args: &[&str]) -> anyhow::Result<Editor> {
        self.spawn_capturing("omime-ti-client", args)
    }

    /// Opens the XIM test client, an X11 application, and waits until it has an input
    /// context. Sway gives the new window the keyboard focus.
    pub fn xim_client(&self, args: &[&str]) -> anyhow::Result<Editor> {
        let client = self.spawn_capturing("omime-xim-client", args)?;
        client.wait_for("the XIM client to be ready", |l| l == "ready")?;
        Ok(client)
    }

    /// Runs a test client that prints one line per event and collects its output.
    fn spawn_capturing(&self, program: &str, args: &[&str]) -> anyhow::Result<Editor> {
        let program = target_dir().join(program);
        let mut child = self
            .command(&program)
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot start {}", program.display()))?;
        let stdout = child.stdout.take().context("no stdout")?;
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                sink.lock().unwrap_or_else(|p| p.into_inner()).push(line);
            }
        });
        Ok(Editor { child, lines })
    }

    /// Opens a client of the Fcitx5 module protocol and waits until it has an input
    /// context.
    pub fn fcitx_client(&self, args: &[&str]) -> anyhow::Result<FcitxClient> {
        let mut child = self
            .command(target_dir().join("omime-fcitx-client"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("cannot start omime-fcitx-client")?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                sink.lock().unwrap_or_else(|p| p.into_inner()).push(line);
            }
        });
        let client = FcitxClient {
            child,
            stdin: Mutex::new(Some(stdin)),
            lines,
        };
        client.wait_for("the Fcitx client to be ready", |l| {
            l.strip_prefix("ready ").is_some()
        })?;
        Ok(client)
    }

    /// Clicks with the cursor of `seat` at (`x`, `y`) of the output, which gives that
    /// seat's keyboard focus to the window there.
    pub fn click_on_seat(&self, seat: &str, x: u32, y: u32) -> anyhow::Result<()> {
        for command in [
            format!("seat {seat} cursor set {x} {y}"),
            format!("seat {seat} cursor press button1"),
            format!("seat {seat} cursor release button1"),
        ] {
            self.run(self.command("swaymsg").arg(command).stdout(Stdio::null()))?;
        }
        Ok(())
    }

    /// Types `text` (lowercase letters, digits and spaces) with a virtual keyboard on
    /// `seat`.
    pub fn type_on_seat(&self, seat: &str, text: &str) -> anyhow::Result<()> {
        self.run(
            self.command(target_dir().join("omime-keyboard"))
                .args(["--seat", seat, "--type", text]),
        )
    }

    /// Gives keyboard focus to the window with `app_id` and waits until omimed holds the
    /// keyboard for its text field.
    pub fn focus(&self, app_id: &str) -> anyhow::Result<()> {
        const GRABBED: &str = "keyboard grabbed";
        let before = self.count_in_log(GRABBED);
        self.run(
            self.command("swaymsg")
                .arg(format!("[app_id=\"{app_id}\"] focus"))
                .stdout(Stdio::null()),
        )?;
        wait_until("omimed to take the focused field", || {
            (self.count_in_log(GRABBED) > before).then_some(())
        })
    }

    /// Writes `config.toml` and asks omimed to read it.
    pub fn configure(&self, config: &str) -> anyhow::Result<()> {
        let dir = self.dir.join("config/oh-my-ime");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("config.toml"), config)?;
        self.omime(&["reload"])?;
        Ok(())
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

    /// The color of the pixel at `(x, y)` of the output, as `0xRRGGBB`.
    pub fn pixel(&self, x: u32, y: u32) -> anyhow::Result<u32> {
        let geometry = format!("{x},{y} 1x1");
        let output = self
            .command("grim")
            .args(["-g", &geometry, "-t", "ppm", "-"])
            .output()
            .context("cannot run grim")?;
        if !output.status.success() {
            bail!("grim failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        crate::ppm_pixel(&output.stdout).context("grim wrote no image")
    }

    /// Waits until the pixel at `(x, y)` has the color `rgb` (`0xRRGGBB`).
    pub fn wait_for_pixel(&self, x: u32, y: u32, rgb: u32) -> anyhow::Result<()> {
        let mut last = None;
        let found = wait_until(&format!("the color {rgb:06x} at ({x}, {y})"), || {
            last = self.pixel(x, y).ok();
            (last == Some(rgb)).then_some(())
        });
        found.with_context(|| match last {
            Some(color) => format!("the pixel is {color:06x}"),
            None => "no screenshot".to_owned(),
        })
    }

    /// Runs the `omime` CLI and returns its output.
    pub fn omime(&self, args: &[&str]) -> anyhow::Result<String> {
        let output = self
            .command(program_dir().join("omime"))
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

    /// Waits until `omime status` prints `expected`.
    pub fn wait_for_status(&self, expected: &str) -> anyhow::Result<()> {
        wait_until(&format!("the status {expected:?}"), || {
            let status = self.omime(&["status"]).ok()?;
            (status.trim() == expected).then_some(())
        })
    }

    /// Runs the `omime` CLI and returns its exit status and stdout, also when the status
    /// is not success (for `omime doctor`, which fails when it finds a problem).
    pub fn omime_status(&self, args: &[&str]) -> anyhow::Result<(ExitStatus, String)> {
        let output = self
            .command(program_dir().join("omime"))
            .args(args)
            .output()
            .context("cannot run omime")?;
        Ok((
            output.status,
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
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
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
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

/// A client of the Fcitx5 module protocol (`omime-fcitx-client`).
pub struct FcitxClient {
    child: Child,
    stdin: Mutex<Option<ChildStdin>>,
    lines: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for FcitxClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FcitxClient").finish_non_exhaustive()
    }
}

impl FcitxClient {
    /// All lines printed so far.
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Sends one command line to the client.
    pub fn send(&self, line: &str) -> anyhow::Result<()> {
        let mut stdin = self.stdin.lock().unwrap_or_else(|p| p.into_inner());
        let stdin = stdin.as_mut().context("the client's stdin is closed")?;
        writeln!(stdin, "{line}")?;
        stdin.flush()?;
        Ok(())
    }

    /// Waits for a line matching `pred` and returns it.
    pub fn wait_for(&self, what: &str, pred: impl Fn(&str) -> bool) -> anyhow::Result<String> {
        wait_until(what, || self.lines().into_iter().find(|l| pred(l)))
    }

    /// Kills the client, as when an application crashes.
    pub fn kill(&mut self) -> anyhow::Result<()> {
        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }
}

impl Drop for FcitxClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
