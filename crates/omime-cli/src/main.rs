//! `omime` — manage the oh-my-ime input method platform.

use std::collections::HashMap;
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use omime_proto::names::{DAEMON_BUS_NAME, DAEMON_OBJECT_PATH};
use zbus::OwnedVariant;

const USAGE: &str = "\
Usage: omime <COMMAND>

Commands:
  list             List installed engines
  status [--json]  Show the current engine
  switch <ENGINE>  Switch to an engine (`direct` for no engine)
  next             Switch to the next enabled engine
  toggle           Switch between direct input and the last engine
  enable <ENGINE>  Enable an engine (adds it to `engines` in config.toml)
  disable <ENGINE> Disable an engine (removes it from `engines` in config.toml)
  reload           Re-read config.toml and look for installed or removed engines
  help             Print this help

The commands talk to a running `omimed` over the session bus.";

#[zbus::proxy(
    interface = "io.github.ign1x.OhMyIme.Daemon1",
    default_service = "io.github.ign1x.OhMyIme",
    default_path = "/io/github/ign1x/OhMyIme"
)]
trait Daemon {
    fn list_engines(&self) -> zbus::Result<Vec<(String, String, bool, String)>>;
    fn status(&self) -> zbus::Result<HashMap<String, OwnedVariant>>;
    fn switch(&self, engine: &str) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn toggle(&self) -> zbus::Result<()>;
    fn reload(&self) -> zbus::Result<()>;
}

enum Command {
    List,
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
        ["list"] => Command::List,
        ["status"] => Command::Status { json: false },
        ["status", "--json"] => Command::Status { json: true },
        ["switch", engine] => Command::Switch((*engine).to_owned()),
        ["next"] => Command::Next,
        ["toggle"] => Command::Toggle,
        ["enable", engine] => Command::Enable((*engine).to_owned()),
        ["disable", engine] => Command::Disable((*engine).to_owned()),
        ["reload"] => Command::Reload,
        _ => bail!("unknown command line `{}`\n\n{USAGE}", args.join(" ")),
    }))
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

async fn run(command: Command) -> anyhow::Result<()> {
    let conn = zbus::Connection::session()
        .await
        .context("cannot connect to the session bus")?;
    let daemon = DaemonProxy::new(&conn).await?;
    let owner = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &DAEMON_BUS_NAME,
        )
        .await?
        .body()
        .deserialize::<bool>()?;
    if !owner {
        bail!("omimed is not running (nobody owns {DAEMON_BUS_NAME} at {DAEMON_OBJECT_PATH})");
    }

    match command {
        Command::List => {
            for (id, name, enabled, state) in daemon.list_engines().await? {
                let mark = if enabled { '*' } else { ' ' };
                println!("{mark} {id:<16} {name:<24} {state}");
            }
        }
        Command::Status { json } => {
            let status = daemon.status().await?;
            let engine = string(&status, "engine");
            let label = string(&status, "label");
            if json {
                let field = |v: &Option<String>| v.as_deref().map_or("null".into(), json_string);
                let tooltip = string(&status, "tooltip");
                // `text`, `tooltip` and `class` make this usable as a waybar module.
                let text = label
                    .clone()
                    .or_else(|| engine.clone())
                    .unwrap_or_else(|| "⌨".into());
                let class = engine.clone().unwrap_or_else(|| "direct".into());
                println!(
                    "{{\"engine\":{},\"label\":{},\"tooltip\":{},\"text\":{},\"class\":{}}}",
                    field(&engine),
                    field(&label),
                    field(&tooltip),
                    json_string(&text),
                    json_string(&class),
                );
            } else {
                match (engine, label) {
                    (None, _) => println!("direct input"),
                    (Some(engine), Some(label)) => println!("{engine} ({label})"),
                    (Some(engine), None) => println!("{engine}"),
                }
            }
        }
        Command::Switch(engine) => {
            let target = if engine == "direct" {
                ""
            } else {
                engine.as_str()
            };
            daemon.switch(target).await?;
        }
        Command::Next => daemon.next().await?,
        Command::Toggle => daemon.toggle().await?,
        Command::Enable(engine) => set_enabled(&daemon, &engine, true).await?,
        Command::Disable(engine) => set_enabled(&daemon, &engine, false).await?,
        Command::Reload => daemon.reload().await?,
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
        Ok(()) => ExitCode::SUCCESS,
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
        assert!(matches!(parse(&args("list")), Ok(Some(Command::List))));
        assert!(matches!(
            parse(&args("status --json")),
            Ok(Some(Command::Status { json: true }))
        ));
        assert!(matches!(parse(&args("switch echo")), Ok(Some(Command::Switch(e))) if e == "echo"));
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
}
