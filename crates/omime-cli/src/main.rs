//! `omime` — manage the oh-my-ime input method platform.

mod doctor;
mod install;
mod watch;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use omime_proto::names::{DAEMON_BUS_NAME, DAEMON_OBJECT_PATH};
use zbus::OwnedVariant;

const USAGE: &str = "\
Usage: omime <COMMAND>

Commands:
  list             List the input methods to choose from (* marks the one in use)
  engines          List installed engines (* marks enabled ones)
  status [--json] [--watch]
                   Show the input method in use (`--watch` keeps streaming changes)
  switch <ID>      Switch to an input method from `list`, such as `sparkle` or
                   `fcitx5/shuangpin` (`direct` for no input method)
  next             Switch to the next input method
  toggle           Switch between direct input and the last engine
  enable <ENGINE>  Enable an engine (adds it to `engines` in config.toml)
  disable <ENGINE> Disable an engine (removes it from `engines` in config.toml)
  install <DIR>    Install an engine from a directory (`--force` to replace it)
  uninstall <ID>   Remove an engine installed in your user directory
  reload           Re-read config.toml and look for installed or removed engines
  doctor           Explain what is wrong with the setup (fails on problems)
  help             Print this help

The other commands talk to a running `omimed` over the session bus; `install`,
`uninstall` and `doctor` also work without it, and `status --watch` keeps watching
for it.";

#[zbus::proxy(
    interface = "io.github.ign1x.OhMyIme.Daemon1",
    default_service = "io.github.ign1x.OhMyIme",
    default_path = "/io/github/ign1x/OhMyIme"
)]
trait Daemon {
    fn list_engines(&self) -> zbus::Result<Vec<(String, String, bool, String)>>;
    fn list_input_methods(&self) -> zbus::Result<Vec<(String, String, String, String, bool)>>;
    fn status(&self) -> zbus::Result<HashMap<String, OwnedVariant>>;
    fn switch(&self, engine: &str) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn toggle(&self) -> zbus::Result<()>;
    fn reload(&self) -> zbus::Result<()>;

    /// The compositor, the seats and whether focus is reported; see `doctor`.
    fn diagnose(&self) -> zbus::Result<HashMap<String, OwnedVariant>>;

    /// Emitted when what `Status` returns changes. Carries the new status.
    #[zbus(signal)]
    fn status_changed(&self, status: HashMap<String, OwnedVariant>) -> zbus::Result<()>;
}

enum Command {
    /// Commands that need a running omimed.
    Daemon(DaemonCommand),
    Install {
        dir: PathBuf,
        force: bool,
    },
    Uninstall(String),
    /// `status --watch`, which keeps running while omimed is away.
    Watch {
        json: bool,
    },
    /// Checks the setup; needs neither omimed nor a session bus.
    Doctor,
}

enum DaemonCommand {
    List,
    Engines,
    Status { json: bool },
    Switch(String),
    Next,
    Toggle,
    Enable(String),
    Disable(String),
    Reload,
}

fn parse(args: &[String]) -> anyhow::Result<Option<Command>> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(Some(match args.as_slice() {
        [] | ["help" | "-h" | "--help"] => return Ok(None),
        ["install", dir] if *dir != "--force" => Command::Install {
            dir: PathBuf::from(*dir),
            force: false,
        },
        ["install", dir, "--force"] | ["install", "--force", dir] => Command::Install {
            dir: PathBuf::from(*dir),
            force: true,
        },
        ["uninstall", engine] => Command::Uninstall((*engine).to_owned()),
        ["doctor"] => Command::Doctor,
        ["status", "--watch"] => Command::Watch { json: false },
        ["status", "--json", "--watch"] | ["status", "--watch", "--json"] => {
            Command::Watch { json: true }
        }
        args => Command::Daemon(match args {
            ["list"] => DaemonCommand::List,
            ["engines"] => DaemonCommand::Engines,
            ["status"] => DaemonCommand::Status { json: false },
            ["status", "--json"] => DaemonCommand::Status { json: true },
            ["switch", engine] => DaemonCommand::Switch((*engine).to_owned()),
            ["next"] => DaemonCommand::Next,
            ["toggle"] => DaemonCommand::Toggle,
            ["enable", engine] => DaemonCommand::Enable((*engine).to_owned()),
            ["disable", engine] => DaemonCommand::Disable((*engine).to_owned()),
            ["reload"] => DaemonCommand::Reload,
            _ => bail!("unknown command line `{}`\n\n{USAGE}", args.join(" ")),
        }),
    }))
}

/// `text` padded to `width` columns; CJK characters take two.
fn pad(text: &str, width: usize) -> String {
    let columns: usize = text
        .chars()
        .map(|c| if (c as u32) >= 0x1100 { 2 } else { 1 })
        .sum();
    format!("{text}{}", " ".repeat(width.saturating_sub(columns)))
}

fn string(dict: &HashMap<String, OwnedVariant>, key: &str) -> Option<String> {
    dict.get(key)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| String::try_from(v).ok())
        .filter(|s| !s.is_empty())
}

/// Minimal JSON string escaping, enough for ids and labels.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Formats a status dictionary (the reply of `Status` or the argument of
/// `StatusChanged`) as the line `omime status` prints.
fn format_status(status: &HashMap<String, OwnedVariant>, json: bool) -> String {
    let engine = string(status, "engine");
    let method = string(status, "input-method").or_else(|| engine.clone());
    let label = string(status, "label");
    if json {
        let field = |v: &Option<String>| v.as_deref().map_or("null".into(), json_string);
        let tooltip = string(status, "tooltip");
        // `text`, `tooltip` and `class` make this usable as a waybar module.
        let text = label
            .clone()
            .or_else(|| engine.clone())
            .unwrap_or_else(|| "⌨".into());
        let class = engine.clone().unwrap_or_else(|| "direct".into());
        format!(
            "{{\"engine\":{},\"input_method\":{},\"label\":{},\"tooltip\":{},\"text\":{},\"class\":{}}}",
            field(&engine),
            field(&method),
            field(&label),
            field(&tooltip),
            json_string(&text),
            json_string(&class),
        )
    } else {
        match (method, label) {
            (None, _) => "direct input".into(),
            (Some(method), Some(label)) => format!("{method} ({label})"),
            (Some(method), None) => method,
        }
    }
}

/// The line for "omimed is not running", in the same formats as [`format_status`].
fn format_stopped(json: bool) -> String {
    if json {
        // `class` is `stopped`, so status bars can tell it from direct input.
        "{\"engine\":null,\"input_method\":null,\"label\":null,\"tooltip\":\"omimed is not running\",\
         \"text\":\"⌨\",\"class\":\"stopped\"}"
            .into()
    } else {
        "omimed is not running".into()
    }
}

async fn run(command: Command) -> anyhow::Result<ExitCode> {
    match command {
        Command::Install { dir, force } => install_engine(&dir, force).await?,
        Command::Uninstall(engine) => uninstall_engine(&engine).await?,
        Command::Watch { json } => watch::run(json).await?,
        Command::Daemon(command) => run_daemon(command).await?,
        Command::Doctor => {
            let clean = doctor::run().await?;
            return Ok(if clean {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Installs the engine directory `dir` for the user and lets a running omimed notice.
async fn install_engine(dir: &Path, force: bool) -> anyhow::Result<()> {
    let engines_dir =
        omime_proto::paths::user_engine_dir().context("cannot find the data directory")?;
    let engine = install::install(dir, &engines_dir, force)?;
    if let Some(warning) = install::command_warning(&engine) {
        eprintln!("warning: {warning}");
    }
    edit_config_engines(engine.id(), true)?;
    let running = reload_if_running().await?;
    println!(
        "Installed {} ({}) into {}",
        engine.id(),
        engine.manifest.engine.name,
        engine.dir.display()
    );
    if !running {
        println!("omimed is not running; it will find the engine when it starts.");
    }
    Ok(())
}

/// Removes the user-installed engine `engine` and lets a running omimed notice.
async fn uninstall_engine(engine: &str) -> anyhow::Result<()> {
    let engines_dir =
        omime_proto::paths::user_engine_dir().context("cannot find the data directory")?;
    install::uninstall(&engines_dir, engine, &omime_proto::paths::engine_dirs())?;
    edit_config_engines(engine, false)?;
    reload_if_running().await?;
    println!("Uninstalled {engine}");
    Ok(())
}

/// Asks a running omimed to reload. Returns `false` when there is none, so installing
/// and uninstalling also work in a session without a bus.
async fn reload_if_running() -> anyhow::Result<bool> {
    let Ok(conn) = zbus::Connection::session().await else {
        return Ok(false);
    };
    if !daemon_owns_bus_name(&conn).await? {
        return Ok(false);
    }
    let daemon = DaemonProxy::new(&conn).await?;
    daemon.reload().await?;
    Ok(true)
}

/// Whether omimed currently owns its bus name.
async fn daemon_owns_bus_name(conn: &zbus::Connection) -> anyhow::Result<bool> {
    Ok(conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &DAEMON_BUS_NAME,
        )
        .await?
        .body()
        .deserialize::<bool>()?)
}

/// Adds `engine` to or removes it from an explicit `engines` list in config.toml.
/// Without a list every installed engine is enabled, so there is nothing to change.
fn edit_config_engines(engine: &str, enable: bool) -> anyhow::Result<()> {
    let dir =
        omime_proto::paths::config_dir().context("cannot find the configuration directory")?;
    let path = dir.join(omime_proto::config::CONFIG_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("cannot parse {}", path.display()))?;
    if doc.get("engines").is_none() {
        return Ok(());
    }
    let updated = edit_engines(&text, &[], engine, enable)
        .with_context(|| format!("cannot update {}", path.display()))?;
    if updated != text {
        std::fs::write(&path, updated)
            .with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(())
}

async fn run_daemon(command: DaemonCommand) -> anyhow::Result<()> {
    let conn = zbus::Connection::session()
        .await
        .context("cannot connect to the session bus")?;
    let daemon = DaemonProxy::new(&conn).await?;
    if !daemon_owns_bus_name(&conn).await? {
        bail!("omimed is not running (nobody owns {DAEMON_BUS_NAME} at {DAEMON_OBJECT_PATH})");
    }

    match command {
        DaemonCommand::List => {
            for (id, name, engine, _label, in_use) in daemon.list_input_methods().await? {
                let mark = if in_use { '*' } else { ' ' };
                let engine = if engine == name {
                    String::new()
                } else {
                    engine
                };
                println!("{mark} {id:<24} {} {engine}", pad(&name, 24));
            }
        }
        DaemonCommand::Engines => {
            for (id, name, enabled, state) in daemon.list_engines().await? {
                let mark = if enabled { '*' } else { ' ' };
                println!("{mark} {id:<16} {name:<24} {state}");
            }
        }
        DaemonCommand::Status { json } => {
            let status = daemon.status().await?;
            println!("{}", format_status(&status, json));
        }
        DaemonCommand::Switch(engine) => {
            let target = if engine == "direct" {
                ""
            } else {
                engine.as_str()
            };
            daemon.switch(target).await?;
        }
        DaemonCommand::Next => daemon.next().await?,
        DaemonCommand::Toggle => daemon.toggle().await?,
        DaemonCommand::Enable(engine) => set_enabled(&daemon, &engine, true).await?,
        DaemonCommand::Disable(engine) => set_enabled(&daemon, &engine, false).await?,
        DaemonCommand::Reload => daemon.reload().await?,
    }
    Ok(())
}

/// Adds `engine` to or removes it from `engines` in config.toml (keeping the rest of
/// the file as it is), then asks the daemon to reload.
async fn set_enabled(daemon: &DaemonProxy<'_>, engine: &str, enable: bool) -> anyhow::Result<()> {
    let installed = daemon.list_engines().await?;
    if !installed.iter().any(|(id, ..)| id == engine) {
        bail!("no engine `{engine}` is installed (see `omime list`)");
    }
    let dir =
        omime_proto::paths::config_dir().context("cannot find the configuration directory")?;
    let path = dir.join(omime_proto::config::CONFIG_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let enabled_now: Vec<String> = installed
        .iter()
        .filter(|(.., enabled, _)| *enabled)
        .map(|(id, ..)| id.clone())
        .collect();
    let updated = edit_engines(&text, &enabled_now, engine, enable)
        .with_context(|| format!("cannot update {}", path.display()))?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, updated).with_context(|| format!("cannot write {}", path.display()))?;
    daemon.reload().await?;
    Ok(())
}

/// Returns `config` with `engine` added to or removed from the `engines` list. Without a
/// list, `enabled_now` (everything installed) is the starting point.
fn edit_engines(
    config: &str,
    enabled_now: &[String],
    engine: &str,
    enable: bool,
) -> anyhow::Result<String> {
    let mut doc: toml_edit::DocumentMut = config.parse()?;
    let mut list: Vec<String> = match doc.get("engines") {
        Some(item) => item
            .as_array()
            .context("`engines` must be a list of engine ids")?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        None => enabled_now.to_vec(),
    };
    if enable {
        if !list.iter().any(|e| e == engine) {
            list.push(engine.to_owned());
        }
    } else {
        list.retain(|e| e != engine);
    }
    let array: toml_edit::Array = list.iter().map(String::as_str).collect();
    doc["engines"] = toml_edit::value(array);
    Ok(doc.to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match parse(&args) {
        Ok(Some(command)) => command,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("omime: {e}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("omime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(command)) {
        Ok(code) => code,
        Err(e) => {
            let message = match e.downcast_ref::<zbus::Error>() {
                Some(zbus::Error::MethodError(_, Some(detail), _)) => detail.clone(),
                _ => format!("{e:#}"),
            };
            eprintln!("omime: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_commands() {
        assert!(matches!(parse(&args("")), Ok(None)));
        assert!(matches!(
            parse(&args("list")),
            Ok(Some(Command::Daemon(DaemonCommand::List)))
        ));
        assert!(matches!(
            parse(&args("engines")),
            Ok(Some(Command::Daemon(DaemonCommand::Engines)))
        ));
        assert!(matches!(
            parse(&args("status --json")),
            Ok(Some(Command::Daemon(DaemonCommand::Status { json: true })))
        ));
        assert!(matches!(
            parse(&args("status --watch")),
            Ok(Some(Command::Watch { json: false }))
        ));
        assert!(matches!(
            parse(&args("status --json --watch")),
            Ok(Some(Command::Watch { json: true }))
        ));
        assert!(matches!(
            parse(&args("status --watch --json")),
            Ok(Some(Command::Watch { json: true }))
        ));
        assert!(
            matches!(parse(&args("switch echo")), Ok(Some(Command::Daemon(DaemonCommand::Switch(e)))) if e == "echo")
        );
        assert!(matches!(
            parse(&args("install /tmp/echo")),
            Ok(Some(Command::Install { dir, force: false })) if dir == Path::new("/tmp/echo")
        ));
        assert!(matches!(
            parse(&args("install /tmp/echo --force")),
            Ok(Some(Command::Install { force: true, .. }))
        ));
        assert!(
            matches!(parse(&args("uninstall echo")), Ok(Some(Command::Uninstall(e))) if e == "echo")
        );
        assert!(matches!(parse(&args("doctor")), Ok(Some(Command::Doctor))));
        assert!(parse(&args("doctor --json")).is_err());
        assert!(parse(&args("install")).is_err());
        assert!(parse(&args("install --force")).is_err());
        assert!(matches!(
            parse(&args("install --force /tmp/echo")),
            Ok(Some(Command::Install { force: true, .. }))
        ));
        assert!(parse(&args("uninstall")).is_err());
        assert!(parse(&args("switch")).is_err());
        assert!(parse(&args("frobnicate")).is_err());
    }

    #[test]
    fn editing_engines_keeps_the_rest_of_the_file() {
        let config = "# my settings\nengines = [\"echo\"]\n\n[hotkeys]\nnext = \"Super+space\"\n";
        let enabled = edit_engines(config, &[], "sparkle", true).unwrap();
        assert!(enabled.starts_with("# my settings\nengines = [\"echo\", \"sparkle\"]"));
        assert!(enabled.contains("next = \"Super+space\""));
        let disabled = edit_engines(&enabled, &[], "echo", false).unwrap();
        assert!(disabled.contains("engines = [\"sparkle\"]"));
    }

    #[test]
    fn disabling_without_a_list_starts_from_everything_enabled() {
        let all = ["echo".to_owned(), "sparkle".to_owned()];
        let config = edit_engines("", &all, "echo", false).unwrap();
        assert_eq!(config.trim(), "engines = [\"sparkle\"]");
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
        assert_eq!(json_string("中"), "\"中\"");
    }

    #[test]
    fn status_lines_match_waybar() {
        let status = HashMap::from([(
            "engine".to_owned(),
            OwnedVariant::try_from(zbus::Variant::from("echo")).unwrap(),
        )]);
        assert_eq!(format_status(&status, false), "echo");
        assert_eq!(
            format_status(&status, true),
            "{\"engine\":\"echo\",\"input_method\":\"echo\",\"label\":null,\"tooltip\":null,\
             \"text\":\"echo\",\"class\":\"echo\"}"
        );
        assert_eq!(
            format_status(&HashMap::new(), true),
            "{\"engine\":null,\"input_method\":null,\"label\":null,\"tooltip\":null,\
             \"text\":\"⌨\",\"class\":\"direct\"}"
        );
        assert_eq!(format_stopped(false), "omimed is not running");
        assert_eq!(
            format_stopped(true),
            "{\"engine\":null,\"input_method\":null,\"label\":null,\"tooltip\":\"omimed is not running\",\
             \"text\":\"⌨\",\"class\":\"stopped\"}"
        );
    }
}
