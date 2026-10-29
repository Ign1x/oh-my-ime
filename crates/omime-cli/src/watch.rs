//! `omime status --watch`: prints the status once and then one line per change.
//!
//! Status bars read the lines from a pipe, so every line is flushed and a reader that
//! goes away (a broken pipe) or a signal ends the process without an error.

use std::collections::HashMap;
use std::io::Write;
use std::pin::Pin;

use anyhow::{Context as _, bail};
use futures_core::Stream;
use omime_proto::names::DAEMON_BUS_NAME;
use tokio::signal::unix::{SignalKind, signal};
use zbus::OwnedVariant;

use crate::{DaemonProxy, daemon_owns_bus_name, format_status, format_stopped};

/// Prints the status and then every change until killed or stdout closes.
pub async fn run(json: bool) -> anyhow::Result<()> {
    let conn = zbus::Connection::session()
        .await
        .context("cannot connect to the session bus")?;
    let daemon = DaemonProxy::new(&conn).await?;
    // Subscribe before asking for the status, so a change in between cannot slip through.
    let mut changes = std::pin::pin!(
        daemon
            .receive_status_changed()
            .await
            .context("cannot listen for status changes")?
    );
    let bus = zbus::fdo::DBusProxy::new(&conn).await?;
    let mut owners = std::pin::pin!(
        bus.receive_name_owner_changed_with_args(&[(0, DAEMON_BUS_NAME)])
            .await
            .context("cannot follow omimed's bus name")?
    );

    let mut last: Option<String> = None;
    // Unlike the other commands this one keeps running when omimed is absent.
    if daemon_owns_bus_name(&conn).await? {
        if !print_current(&conn, &daemon, &mut last, json).await? {
            return Ok(());
        }
    } else if !print_stopped(&mut last, json)? {
        return Ok(());
    }

    let mut interrupt = signal(SignalKind::interrupt()).context("cannot listen for SIGINT")?;
    let mut terminate = signal(SignalKind::terminate()).context("cannot listen for SIGTERM")?;

    loop {
        tokio::select! {
            message = next(changes.as_mut()) => {
                let Some(message) = message else {
                    bail!("the session bus connection closed");
                };
                let status = message.args().context("cannot read the StatusChanged signal")?;
                if !print_status(&mut last, json, status.status())? {
                    return Ok(());
                }
            }
            message = next(owners.as_mut()) => {
                let Some(message) = message else {
                    bail!("the session bus connection closed");
                };
                let args = message.args().context("cannot read the NameOwnerChanged signal")?;
                let printed = if args.new_owner().is_some() {
                    print_current(&conn, &daemon, &mut last, json).await
                } else {
                    print_stopped(&mut last, json)
                };
                if !printed? {
                    return Ok(());
                }
            }
            _ = interrupt.recv() => return Ok(()),
            _ = terminate.recv() => return Ok(()),
        }
    }
}

/// Asks a daemon that is (or just became) present and prints its status.
async fn print_current(
    conn: &zbus::Connection,
    daemon: &DaemonProxy<'_>,
    last: &mut Option<String>,
    json: bool,
) -> anyhow::Result<bool> {
    match daemon.status().await {
        Ok(status) => print_status(last, json, &status),
        Err(e) => {
            // A daemon that went away again announces that with its own signal.
            if daemon_owns_bus_name(conn).await? {
                return Err(e).context("cannot read the status");
            }
            print_stopped(last, json)
        }
    }
}

fn print_status(
    last: &mut Option<String>,
    json: bool,
    status: &HashMap<String, OwnedVariant>,
) -> anyhow::Result<bool> {
    emit(last, format_status(status, json))
}

fn print_stopped(last: &mut Option<String>, json: bool) -> anyhow::Result<bool> {
    emit(last, format_stopped(json))
}

/// Writes `line` unless it repeats the previous one. Returns `false` when stdout is gone.
fn emit(last: &mut Option<String>, line: String) -> anyhow::Result<bool> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    emit_to(&mut stdout, last, &line)
}

/// [`emit`] with an explicit writer, so tests can see the bytes.
fn emit_to(out: &mut impl Write, last: &mut Option<String>, line: &str) -> anyhow::Result<bool> {
    if last.as_deref() == Some(line) {
        return Ok(true);
    }
    // Status bars read a pipe: finish each line so it reaches them right away.
    match writeln!(out, "{line}").and_then(|()| out.flush()) {
        Ok(()) => {
            *last = Some(line.to_owned());
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
        Err(e) => Err(e).context("cannot write the status"),
    }
}

/// Waits for the next item of `stream` (a `StreamExt::next` without the extra crate).
async fn next<S: Stream>(mut stream: Pin<&mut S>) -> Option<S::Item> {
    std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn repeated_lines_are_skipped() {
        let mut out = Vec::new();
        let mut last = None;
        assert!(emit_to(&mut out, &mut last, "direct input").unwrap());
        assert!(emit_to(&mut out, &mut last, "direct input").unwrap());
        assert!(emit_to(&mut out, &mut last, "echo").unwrap());
        assert_eq!(String::from_utf8(out).unwrap(), "direct input\necho\n");
    }

    #[test]
    fn a_broken_pipe_is_not_an_error() {
        struct Closed;

        impl io::Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut last = None;
        assert!(!emit_to(&mut Closed, &mut last, "direct input").unwrap());
        assert_eq!(last, None);
    }
}
