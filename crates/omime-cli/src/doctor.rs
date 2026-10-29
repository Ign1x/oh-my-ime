//! `omime doctor`: says what is wrong with the setup, one line per finding.
//!
//! The checks are pure functions over what they inspect: the reply of `Diagnose` and
//! `ListEngines`, the text of `config.toml`, the environment, the files found for engines
//! and the names in `/proc`. Only the functions at the bottom of this module talk to
//! D-Bus, the file system, `/proc` or the environment.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use omime_proto::config::{CONFIG_FILE, Config, HotkeySpec};
use omime_proto::manifest::discover;
use omime_proto::names::DAEMON_BUS_NAME;
use omime_proto::paths;
use zbus::OwnedVariant;

use crate::DaemonProxy;

/// How severe a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warning,
    Problem,
}

impl Level {
    /// The character every line of this level starts with.
    fn marker(self) -> char {
        match self {
            Level::Ok => '✓',
            Level::Warning => '!',
            Level::Problem => '✗',
        }
    }
}

/// One line of `omime doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub text: String,
}

impl Finding {
    fn ok(text: impl Into<String>) -> Self {
        Self {
            level: Level::Ok,
            text: text.into(),
        }
    }

    fn warning(text: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            text: text.into(),
        }
    }

    fn problem(text: impl Into<String>) -> Self {
        Self {
            level: Level::Problem,
            text: text.into(),
        }
    }

    /// The line as `omime doctor` prints it. Error messages may span lines (a TOML parse
    /// error does); they are joined so every finding stays one line.
    pub fn line(&self) -> String {
        let text = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        format!("{} {text}", self.level.marker())
    }

    /// Whether this finding makes `omime doctor` exit with status 1.
    pub fn is_problem(&self) -> bool {
        self.level == Level::Problem
    }
}

/// The reply of `Diagnose`: the compositor and the state of every seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    /// The Wayland display omimed is connected to.
    pub display: String,
    /// Every seat's name and whether omimed is its input method.
    pub seats: Vec<(String, bool)>,
    /// Whether the compositor reports which window has focus.
    pub focused_app: bool,
}

/// An engine as `ListEngines` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Engine {
    pub id: String,
    pub enabled: bool,
    /// `running`, `stopped` or `failed`.
    pub state: String,
}

/// What a running omimed reports about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Daemon {
    pub diagnosis: Diagnosis,
    pub engines: Vec<Engine>,
}

/// Another input method among the user's processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherIm {
    pub pid: u32,
    pub process: String,
}

/// Programs of other input methods; a Wayland seat has room for one input method only.
///
/// The names are what `/proc/<pid>/comm` holds: the executable's name, cut at 15 bytes.
const OTHER_INPUT_METHODS: [&str; 6] = [
    "fcitx5",
    "fcitx",
    "ibus-daemon",
    "kime",
    "kime-wayland",
    "wlpinyin",
];

/// The `(pid, comm)` pairs in `processes` that belong to another input method, sorted by pid.
pub fn other_input_methods(processes: &[(u32, String)]) -> Vec<OtherIm> {
    let mut found: Vec<OtherIm> = processes
        .iter()
        .filter(|(_, comm)| OTHER_INPUT_METHODS.contains(&comm.as_str()))
        .map(|(pid, comm)| OtherIm {
            pid: *pid,
            process: comm.clone(),
        })
        .collect();
    found.sort_unstable_by_key(|other| other.pid);
    found
}

/// The findings of check 1: the daemon, the seats and the engines.
///
/// `daemon` is `None` when omimed does not own its bus name; `others` is what check 5
/// found, which explains a seat that another input method holds.
pub fn daemon_findings(daemon: Option<&Daemon>, others: &[OtherIm]) -> Vec<Finding> {
    let Some(daemon) = daemon else {
        return vec![Finding::problem(
            "omimed is not running; start it with omimed or systemctl --user start omimed",
        )];
    };
    let diagnosis = &daemon.diagnosis;
    let mut findings = vec![Finding::ok(format!(
        "omimed is running, connected to {}",
        diagnosis.display
    ))];

    let mut every_seat_is_ours = true;
    for (seat, ours) in &diagnosis.seats {
        if *ours {
            findings.push(Finding::ok(format!(
                "seat {seat}: oh-my-ime is the input method"
            )));
        } else {
            every_seat_is_ours = false;
            findings.push(Finding::problem(busy_seat(seat, others.first())));
        }
    }
    // A running input method that does not hold the seat now may take it when omimed
    // restarts, so it is still worth knowing about.
    if every_seat_is_ours {
        findings.extend(others.iter().map(|other| {
            Finding::warning(format!(
                "{} (pid {}) is running; it is not using the seat now, but may take it \
                 when omimed restarts",
                other.process, other.pid
            ))
        }));
    }

    findings.push(if diagnosis.focused_app {
        Finding::ok("the compositor reports the focused window")
    } else {
        Finding::warning(
            "the compositor does not report the focused window, so per-application \
             engines (`[apps] remember`) cannot work",
        )
    });

    for engine in daemon.engines.iter().filter(|e| e.state == "failed") {
        findings.push(Finding::problem(format!(
            "engine {} keeps crashing; omimed gave up on it (see its log)",
            engine.id
        )));
    }
    let enabled: Vec<&Engine> = daemon.engines.iter().filter(|e| e.enabled).collect();
    if enabled.is_empty() {
        findings.push(Finding::warning("no engine is enabled"));
    }
    let working: Vec<String> = enabled
        .iter()
        .filter(|e| e.state != "failed")
        .map(|e| format!("{} ({})", e.id, e.state))
        .collect();
    if !working.is_empty() {
        findings.push(Finding::ok(format!("engines: {}", working.join(", "))));
    }
    findings
}

/// The line for the seat `seat` that another input method holds; `busy` is the process
/// that most likely is that input method.
fn busy_seat(seat: &str, busy: Option<&OtherIm>) -> String {
    let mut text = format!("seat {seat}: another input method holds it");
    match busy {
        Some(other) => text.push_str(&format!(
            "; {} (pid {}) is running",
            other.process, other.pid
        )),
        None => text.push_str("; stop the other input method (see docs/hyprland.md)"),
    }
    text
}

/// The findings of check 2.
///
/// `display` is the path to `config.toml` as the user knows it; `content` is `Ok(None)`
/// without the file and `Err` when it cannot be read. omimed ignores the whole file
/// when it cannot read or parse it.
pub fn config_findings(
    display: &str,
    content: Result<Option<&str>, &str>,
    installed: &[String],
) -> Vec<Finding> {
    let text = match content {
        Ok(None) => return vec![Finding::ok("no config.toml, using the defaults")],
        Ok(Some(text)) => text,
        Err(e) => {
            return vec![Finding::problem(format!(
                "config.toml: {display}: {e}; omimed ignores the whole file then"
            ))];
        }
    };
    let config = match Config::parse(text) {
        Ok(config) => config,
        Err(e) => {
            return vec![Finding::problem(format!(
                "config.toml: {display}: {e}; omimed ignores the whole file then"
            ))];
        }
    };

    let mut findings = vec![Finding::ok(format!("config.toml: {display}"))];
    for id in config.engines.iter().flatten() {
        if !installed.iter().any(|known| known == id) {
            findings.push(Finding::warning(format!(
                "config.toml: engine `{id}` is in `engines` but not installed; omimed leaves it out"
            )));
        }
    }
    for (name, value) in [
        ("toggle", &config.hotkeys.toggle),
        ("next", &config.hotkeys.next),
    ] {
        if let Err(e) = HotkeySpec::parse(value) {
            findings.push(Finding::problem(format!(
                "config.toml: hotkey `{name}` = `{value}`: {e}"
            )));
        }
    }
    findings
}

/// The findings of check 3.
///
/// `broken` is what [`discover`] reported for manifests it could not load; `warnings`
/// are the `(id, warning)` pairs of [`crate::install::command_warning`] for the engines
/// it did load. The warnings look up bare programs in *this* command's `$PATH`, so they
/// describe what a daemon started from the same environment would find.
pub fn engine_findings(
    broken: &[(PathBuf, String)],
    warnings: &[(String, String)],
) -> Vec<Finding> {
    let mut findings: Vec<Finding> = broken
        .iter()
        .map(|(path, problem)| Finding::problem(format!("{}: {problem}", path.display())))
        .collect();
    findings.extend(
        warnings
            .iter()
            .map(|(id, warning)| Finding::problem(format!("engine {id}: {warning}"))),
    );
    findings
}

/// The findings of check 4, for the environment variables `vars` as `(name, value)`.
///
/// An input method module in the environment sends every toolkit window to fcitx5 or
/// IBus directly, past the compositor, so omimed never sees those keys.
pub fn environment_findings(vars: &[(String, String)]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (name, value) in vars {
        let toolkit = if name.eq_ignore_ascii_case("GTK_IM_MODULE") {
            "GTK applications"
        } else if name.eq_ignore_ascii_case("QT_IM_MODULE") {
            "Qt applications"
        } else {
            continue;
        };
        let lower = value.to_ascii_lowercase();
        let target = if lower.contains("fcitx") {
            "fcitx5"
        } else if lower.contains("ibus") {
            "IBus"
        } else {
            continue;
        };
        findings.push(Finding::warning(format!(
            "{name}={value}: {toolkit} started from this environment talk to {target}, \
             not to the compositor; unset {name}"
        )));
    }
    findings
}

/// Runs every check, prints one line per finding and returns whether nothing is wrong.
pub async fn run() -> anyhow::Result<bool> {
    let others = other_input_methods(&processes()?);
    let (engines, broken) = discover(&paths::engine_dirs());
    let installed: Vec<String> = engines.iter().map(|e| e.id().to_owned()).collect();
    let warnings: Vec<(String, String)> = engines
        .iter()
        .filter_map(|engine| {
            crate::install::command_warning(engine).map(|warning| (engine.id().to_owned(), warning))
        })
        .collect();

    let mut findings = daemon_check(&others).await;
    findings.extend(config_check(&installed));
    findings.extend(engine_findings(&broken, &warnings));
    findings.extend(environment_findings(&environment()));

    for finding in &findings {
        println!("{}", finding.line());
    }
    Ok(!findings.iter().any(Finding::is_problem))
}

/// The findings of check 1: asks a running omimed for its state.
///
/// Without a session bus or without omimed that is a single problem, and the checks that
/// do not need the daemon still run.
async fn daemon_check(others: &[OtherIm]) -> Vec<Finding> {
    let Ok(conn) = zbus::Connection::session().await else {
        return daemon_findings(None, others);
    };
    match crate::daemon_owns_bus_name(&conn).await {
        Ok(true) => {}
        Ok(false) => return daemon_findings(None, others),
        Err(e) => {
            return vec![Finding::problem(format!(
                "cannot ask the session bus for {DAEMON_BUS_NAME}: {e}"
            ))];
        }
    }
    let proxy = match DaemonProxy::new(&conn).await {
        Ok(proxy) => proxy,
        Err(e) => return vec![Finding::problem(format!("cannot reach omimed: {e}"))],
    };
    let reply = match proxy.diagnose().await {
        Ok(reply) => reply,
        Err(e) => {
            return vec![Finding::problem(format!(
                "omimed did not answer `Diagnose`: {e}"
            ))];
        }
    };
    let diagnosis = match decode_diagnosis(&reply) {
        Ok(diagnosis) => diagnosis,
        Err(e) => return vec![Finding::problem(format!("omimed's `Diagnose` reply: {e}"))],
    };
    let engines = match proxy.list_engines().await {
        Ok(engines) => engines
            .into_iter()
            .map(|(id, _name, enabled, state)| Engine { id, enabled, state })
            .collect(),
        Err(e) => {
            return vec![Finding::problem(format!(
                "omimed did not answer `ListEngines`: {e}"
            ))];
        }
    };
    daemon_findings(Some(&Daemon { diagnosis, engines }), others)
}

/// Turns the reply of `Diagnose` into plain values.
fn decode_diagnosis(reply: &HashMap<String, OwnedVariant>) -> anyhow::Result<Diagnosis> {
    let seats: Vec<(String, bool)> = reply
        .get("seats")
        .context("there is no `seats` entry")?
        .try_clone()?
        .try_into()?;
    let focused_app = bool::try_from(
        reply
            .get("focused-app")
            .context("there is no `focused-app` entry")?
            .try_clone()?,
    )?;
    Ok(Diagnosis {
        display: crate::string(reply, "display").unwrap_or_default(),
        seats,
        focused_app,
    })
}

/// The findings of check 2: reads `config.toml` and checks it against `installed`.
fn config_check(installed: &[String]) -> Vec<Finding> {
    let path = paths::config_dir().map(|dir| dir.join(CONFIG_FILE));
    let display = path.as_deref().map_or_else(
        || CONFIG_FILE.to_owned(),
        |path| short_path(path, home().as_deref()),
    );
    let content = match &path {
        None => Ok(None),
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        },
    };
    config_findings(
        &display,
        content
            .as_ref()
            .map(Option::as_deref)
            .map_err(String::as_str),
        installed,
    )
}

/// This user's home directory, if the environment names one.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.as_os_str().is_empty())
        .map(PathBuf::from)
}

/// `path` with `home` shortened to `~`, the way the documentation names it.
fn short_path(path: &Path, home: Option<&Path>) -> String {
    home.and_then(|home| path.strip_prefix(home).ok())
        .filter(|rest| !rest.as_os_str().is_empty())
        .map_or_else(
            || path.display().to_string(),
            |rest| Path::new("~").join(rest).display().to_string(),
        )
}

/// The input method module variables of this process's environment.
fn environment() -> Vec<(String, String)> {
    ["GTK_IM_MODULE", "QT_IM_MODULE"]
        .into_iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| (name.to_owned(), value))
        })
        .collect()
}

/// Every process of this user with its `/proc/<pid>/comm`, sorted by pid.
///
/// The owner of `/proc/self` is this process's user id, the same value `getuid` returns;
/// the CLI has no libc wrapper of its own.
fn processes() -> anyhow::Result<Vec<(u32, String)>> {
    let uid = std::fs::metadata("/proc/self")
        .context("cannot read /proc/self")?
        .uid();
    let mut processes = Vec::new();
    for entry in std::fs::read_dir("/proc").context("cannot read /proc")? {
        let Ok(entry) = entry else {
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let path = entry.path();
        // Processes of other users, and the ones that ended while we looked, are skipped.
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.uid() != uid {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(path.join("comm")) {
            processes.push((pid, comm.trim_end().to_owned()));
        }
    }
    processes.sort_unstable_by_key(|(pid, _)| *pid);
    Ok(processes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(findings: &[Finding]) -> Vec<Level> {
        findings.iter().map(|finding| finding.level).collect()
    }

    fn daemon(seats: &[(&str, bool)], focused_app: bool, engines: &[(&str, bool, &str)]) -> Daemon {
        Daemon {
            diagnosis: Diagnosis {
                display: "wayland-1".into(),
                seats: seats
                    .iter()
                    .map(|(name, ours)| ((*name).to_owned(), *ours))
                    .collect(),
                focused_app,
            },
            engines: engines
                .iter()
                .map(|(id, enabled, state)| Engine {
                    id: (*id).to_owned(),
                    enabled: *enabled,
                    state: (*state).to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn every_line_starts_with_its_marker() {
        assert_eq!(Finding::ok("fine").line(), "✓ fine");
        assert_eq!(Finding::warning("careful").line(), "! careful");
        assert_eq!(Finding::problem("broken").line(), "✗ broken");
        assert!(Finding::problem("broken").is_problem());
        assert!(!Finding::warning("careful").is_problem());
        assert!(!Finding::ok("fine").is_problem());
    }

    #[test]
    fn multi_line_errors_are_printed_on_one_line() {
        assert_eq!(
            Finding::problem("syntax error\n  |\n1 | [engine]\n  | ^\nmissing field").line(),
            "✗ syntax error | 1 | [engine] | ^ missing field"
        );
    }

    #[test]
    fn a_daemon_that_does_not_run_is_a_problem() {
        let findings = daemon_findings(None, &[]);
        assert_eq!(levels(&findings), [Level::Problem]);
        assert!(findings[0].text.contains("systemctl --user start omimed"));
    }

    #[test]
    fn a_healthy_daemon_reports_seats_focus_and_engines() {
        let daemon = daemon(
            &[("seat0", true)],
            true,
            &[("sparkle", true, "running"), ("echo", true, "stopped")],
        );
        let findings = daemon_findings(Some(&daemon), &[]);
        assert_eq!(levels(&findings), [Level::Ok; 4]);
        assert_eq!(
            findings[0].text,
            "omimed is running, connected to wayland-1"
        );
        assert_eq!(
            findings[1].text,
            "seat seat0: oh-my-ime is the input method"
        );
        assert_eq!(
            findings[2].text,
            "the compositor reports the focused window"
        );
        assert_eq!(
            findings[3].text,
            "engines: sparkle (running), echo (stopped)"
        );
    }

    #[test]
    fn a_busy_seat_names_the_process_that_holds_it() {
        let daemon = daemon(&[("seat0", true), ("seat1", false)], true, &[]);
        let others = other_input_methods(&[(1234, "fcitx5".into())]);
        let findings = daemon_findings(Some(&daemon), &others);
        assert_eq!(findings[2].level, Level::Problem);
        assert_eq!(
            findings[2].text,
            "seat seat1: another input method holds it; fcitx5 (pid 1234) is running"
        );
        // Only seat1 is busy, so the process is not also reported as idle.
        assert!(!findings.iter().any(|f| f.text.contains("may take it")));
    }

    #[test]
    fn a_busy_seat_without_a_known_process_points_at_the_docs() {
        let daemon = daemon(&[("seat0", false)], true, &[]);
        let findings = daemon_findings(Some(&daemon), &[]);
        assert_eq!(findings[1].level, Level::Problem);
        assert!(findings[1].text.contains("docs/hyprland.md"));
    }

    #[test]
    fn an_idle_other_input_method_is_worth_a_warning() {
        let daemon = daemon(&[("seat0", true)], true, &[]);
        let others = other_input_methods(&[(42, "ibus-daemon".into())]);
        let findings = daemon_findings(Some(&daemon), &others);
        assert_eq!(findings[2].level, Level::Warning);
        assert!(
            findings[2]
                .text
                .starts_with("ibus-daemon (pid 42) is running")
        );
        assert!(
            findings[2]
                .text
                .contains("may take it when omimed restarts")
        );
        assert!(!findings.iter().any(|f| f.text.contains("holds it")));
    }

    #[test]
    fn a_compositor_without_focus_information_is_a_warning() {
        let daemon = daemon(&[("seat0", true)], false, &[]);
        let findings = daemon_findings(Some(&daemon), &[]);
        assert_eq!(findings[2].level, Level::Warning);
        assert!(findings[2].text.contains("`[apps] remember`"));
    }

    #[test]
    fn failed_engines_are_problems_and_not_listed_as_working() {
        let daemon = daemon(
            &[("seat0", true)],
            true,
            &[("echo", true, "failed"), ("sparkle", true, "running")],
        );
        let findings = daemon_findings(Some(&daemon), &[]);
        assert!(findings.iter().any(|f| f.level == Level::Problem
            && f.text == "engine echo keeps crashing; omimed gave up on it (see its log)"));
        assert!(
            findings
                .iter()
                .any(|f| f.text == "engines: sparkle (running)")
        );
    }

    #[test]
    fn no_enabled_engine_is_a_warning() {
        let daemon = daemon(&[("seat0", true)], true, &[("echo", false, "stopped")]);
        let findings = daemon_findings(Some(&daemon), &[]);
        assert!(
            findings
                .iter()
                .any(|f| f.level == Level::Warning && f.text == "no engine is enabled")
        );
        assert!(!findings.iter().any(|f| f.text.starts_with("engines:")));
    }

    #[test]
    fn a_missing_config_is_fine() {
        let findings = config_findings("~/.config/oh-my-ime/config.toml", Ok(None), &[]);
        assert_eq!(levels(&findings), [Level::Ok]);
        assert_eq!(findings[0].text, "no config.toml, using the defaults");
    }

    #[test]
    fn an_unreadable_config_is_a_problem() {
        let findings = config_findings("/cfg/config.toml", Err("Permission denied"), &[]);
        assert_eq!(levels(&findings), [Level::Problem]);
        assert!(
            findings[0]
                .text
                .contains("/cfg/config.toml: Permission denied")
        );
        assert!(findings[0].text.contains("ignores the whole file"));
    }

    #[test]
    fn a_config_with_a_syntax_error_is_a_problem() {
        let findings = config_findings("/cfg/config.toml", Ok(Some("bogus = 1")), &[]);
        assert_eq!(levels(&findings), [Level::Problem]);
        assert!(findings[0].text.contains("ignores the whole file"));
    }

    #[test]
    fn a_good_config_is_fine() {
        let installed = ["echo".to_owned()];
        let config = "engines = [\"echo\"]\n[hotkeys]\ntoggle = \"Super+space\"\n";
        let findings = config_findings(
            "~/.config/oh-my-ime/config.toml",
            Ok(Some(config)),
            &installed,
        );
        assert_eq!(levels(&findings), [Level::Ok]);
        assert_eq!(
            findings[0].text,
            "config.toml: ~/.config/oh-my-ime/config.toml"
        );
    }

    #[test]
    fn unknown_engines_and_bad_hotkeys_are_found() {
        let config = "engines = [\"echo\", \"gone\"]\n\n[hotkeys]\ntoggle = \"Hyper+a\"\n";
        let installed = ["echo".to_owned()];
        let findings = config_findings("config.toml", Ok(Some(config)), &installed);
        assert_eq!(
            levels(&findings),
            [Level::Ok, Level::Warning, Level::Problem]
        );
        assert!(findings[1].text.contains("`gone`"));
        assert!(findings[2].text.contains("hotkey `toggle` = `Hyper+a`"));
    }

    #[test]
    fn broken_manifests_and_unstartable_engines_are_problems() {
        let broken = [(
            PathBuf::from("/engines/broken/engine.toml"),
            "`name` must not be empty".to_owned(),
        )];
        let warnings = [(
            "echo".to_owned(),
            "`omimed-echo` is not in PATH, so omimed cannot start this engine".to_owned(),
        )];
        let findings = engine_findings(&broken, &warnings);
        assert_eq!(levels(&findings), [Level::Problem; 2]);
        assert_eq!(
            findings[0].text,
            "/engines/broken/engine.toml: `name` must not be empty"
        );
        assert!(findings[1].text.starts_with("engine echo: "));
    }

    #[test]
    fn input_method_modules_in_the_environment_are_warnings() {
        let vars = [
            ("GTK_IM_MODULE".to_owned(), "fcitx".to_owned()),
            ("QT_IM_MODULE".to_owned(), "IBUS".to_owned()),
            ("GTK_IM_MODULE".to_owned(), "wayland".to_owned()),
            ("QT_IM_MODULE".to_owned(), String::new()),
            ("XMODIFIERS".to_owned(), "@im=fcitx".to_owned()),
        ];
        let findings = environment_findings(&vars);
        assert_eq!(levels(&findings), [Level::Warning; 2]);
        assert_eq!(
            findings[0].text,
            "GTK_IM_MODULE=fcitx: GTK applications started from this environment talk to \
             fcitx5, not to the compositor; unset GTK_IM_MODULE"
        );
        assert!(findings[1].text.contains("Qt applications"));
        assert!(findings[1].text.contains("IBus"));
        assert!(findings[1].text.contains("unset QT_IM_MODULE"));
    }

    #[test]
    fn other_input_methods_are_recognized_by_their_program_names() {
        let processes = [
            (300, "ibus-daemon".to_owned()),
            (100, "bash".to_owned()),
            (200, "fcitx5".to_owned()),
            (400, "fcitx5-wrapper".to_owned()),
        ];
        let found = other_input_methods(&processes);
        assert_eq!(
            found,
            [
                OtherIm {
                    pid: 200,
                    process: "fcitx5".into()
                },
                OtherIm {
                    pid: 300,
                    process: "ibus-daemon".into()
                },
            ]
        );
    }

    #[test]
    fn paths_in_the_home_directory_are_shortened() {
        let home = Path::new("/home/u");
        assert_eq!(
            short_path(
                Path::new("/home/u/.config/oh-my-ime/config.toml"),
                Some(home)
            ),
            "~/.config/oh-my-ime/config.toml"
        );
        assert_eq!(short_path(home, Some(home)), "/home/u");
        assert_eq!(
            short_path(Path::new("/etc/config.toml"), Some(home)),
            "/etc/config.toml"
        );
        assert_eq!(
            short_path(Path::new("/etc/config.toml"), None),
            "/etc/config.toml"
        );
    }

    #[test]
    fn problems_are_what_makes_the_command_fail() {
        let findings = config_findings("config.toml", Ok(Some("bogus = 1")), &[]);
        assert!(findings.iter().any(Finding::is_problem));
        let findings = config_findings("config.toml", Ok(None), &[]);
        assert!(!findings.iter().any(Finding::is_problem));
    }
}
