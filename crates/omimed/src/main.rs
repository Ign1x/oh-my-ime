//! `omimed` — the oh-my-ime daemon.

mod control;
mod core;
mod daemon;
mod ime_state;
mod keyboard;
mod runtime_dir;
mod state;
mod wayland;

use std::process::ExitCode;

use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
Usage: omimed [OPTIONS]

The oh-my-ime daemon. It becomes the input method of the Wayland compositor and
routes keyboard input to input method engines.

Options:
  --display <NAME>  Connect to this Wayland display instead of $WAYLAND_DISPLAY
  --engine <ID>     Select this engine at startup (default: the last one used)
  -h, --help        Print this help
  -V, --version     Print the version

Environment:
  OMIME_LOG         Log filter, e.g. `debug` or `omimed=trace`
                    (default: info,wl_proxy=error)
  OMIME_ENGINE_PATH Extra directories to look for engines in (colon separated)
  OMIME_WL_DEBUG    Log every Wayland message (may include typed keys!)";

fn main() -> ExitCode {
    let mut options = daemon::Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("omimed {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--display" => match args.next() {
                Some(display) => options.display = Some(display),
                None => return usage_error("--display needs a value"),
            },
            "--engine" => match args.next() {
                Some(engine) => options.engine = Some(engine),
                None => return usage_error("--engine needs an engine id"),
            },
            other => return usage_error(&format!("unknown argument `{other}`")),
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("OMIME_LOG")
                .unwrap_or_else(|_| EnvFilter::new("info,wl_proxy=error")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();

    match daemon::run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("omimed: {message}\n\n{USAGE}");
    ExitCode::from(2)
}
