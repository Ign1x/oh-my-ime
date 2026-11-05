//! `omime-settings`: oh-my-ime's settings. It shows and changes what omimed offers
//! through its control API (`io.github.ign1x.OhMyIme.Daemon1`): the input methods and
//! their order, the hotkeys, the input method of each application, the options of
//! config.toml, and what omimed reports about the session. It keeps nothing of its own:
//! while omimed does not run it says so, and it follows omimed as it stops and starts.

mod daemon;
mod hotkey;
mod model;
mod tr;
mod ui;

use std::cell::RefCell;
use std::process::ExitCode;
use std::rc::Rc;

use slint::ComponentHandle as _;
use tracing_subscriber::EnvFilter;

/// The user interface, in ui/.
#[allow(missing_debug_implementations, clippy::unwrap_used, clippy::todo)]
mod window {
    slint::slint! {
        #[style = "fluent"]
        export { AppRow, Backend, CatalogEntry, Check, InputMethodRow, SettingsWindow, Texts, Tr, View } from "../ui/settings.slint";
    }
}

use window::{SettingsWindow, View};

/// The application id: the name of the desktop entry, which compositors match.
const APP_ID: &str = "io.github.ign1x.OhMyIme.Settings";

const USAGE: &str = omime_proto::i18n::message(
    "Usage: omime-settings [PAGE]

Shows oh-my-ime's settings. PAGE opens a page other than the input methods: hotkeys,
applications, general or about.",
);

thread_local! {
    /// The window's controller, for events from the D-Bus thread.
    static CONTROLLER: RefCell<Option<Rc<ui::Controller>>> = const { RefCell::new(None) };
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("OMIME_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
    let page = match std::env::args().nth(1).as_deref() {
        None => 0,
        Some("-h" | "--help") => {
            println!("{}", omime_proto::i18n::text(USAGE));
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("omime-settings {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(name) => match ui::PAGES.iter().position(|p| *p == name) {
            Some(page) => page,
            None => {
                eprintln!(
                    "omime-settings: {}\n\n{}",
                    omime_proto::tr!("there is no page `{name}`", name = name),
                    omime_proto::i18n::text(USAGE)
                );
                return ExitCode::from(2);
            }
        },
    };
    match run(page) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("omime-settings: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(page: usize) -> anyhow::Result<()> {
    let window = SettingsWindow::new()?;
    tr::install(&window, &omime_proto::i18n::preferences().join(":"));
    // Once the window has chosen the platform, before it shows.
    slint::set_xdg_app_id(APP_ID)?;
    window
        .global::<View<'_>>()
        .set_page(i32::try_from(page).unwrap_or_default());
    let (commands, requests) = tokio::sync::mpsc::unbounded_channel();
    let controller = ui::Controller::new(&window, commands);
    CONTROLLER.with(|c| c.replace(Some(controller)));
    std::thread::Builder::new()
        .name("omime-settings-dbus".to_owned())
        .spawn(move || {
            daemon::run(requests, |event| {
                // Fails only when the window is gone, and then nobody needs the event.
                let _ = slint::invoke_from_event_loop(move || {
                    CONTROLLER.with(|c| {
                        if let Some(controller) = &*c.borrow() {
                            controller.apply(event);
                        }
                    });
                });
            });
        })?;
    window.run()?;
    Ok(())
}
