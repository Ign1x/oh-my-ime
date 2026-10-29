//! A client of the Fcitx5 input method module protocol, like the Fcitx5 GTK or Qt module.
//!
//! It connects to omimed on the session bus, creates an input context and then runs
//! commands from stdin, one per line. Every signal of the context is printed as it
//! arrives, in arrival order, so tests can check that text reaches the application
//! before the reply to the key that produced it.
//!
//! `--batch` sends keys with `ProcessKeyEventBatch` instead of `ProcessKeyEvent`.

use std::io::{BufRead as _, Write};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use omime_e2e::json_string;
use tokio::sync::mpsc;
use zbus::export::futures_core::Stream;
use zbus::export::serde::ser::Serialize;
use zbus::message::Type;
use zbus::{Connection, DynamicType, Message, MessageStream, OwnedObjectPath, OwnedVariant};

const SERVICE: &str = "org.fcitx.Fcitx5";
const INPUT_METHOD_PATH: &str = "/org/freedesktop/portal/inputmethod";
const INPUT_METHOD_INTERFACE: &str = "org.fcitx.Fcitx.InputMethod1";
const INPUT_CONTEXT_INTERFACE: &str = "org.fcitx.Fcitx.InputContext1";

const DBUS_SERVICE: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const DBUS_INTERFACE: &str = "org.freedesktop.DBus";

/// How long to wait for omimed to take the Fcitx5 name.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
struct Options {
    program: Option<String>,
    display: Option<String>,
    batch: bool,
}

impl Options {
    fn parse() -> anyhow::Result<Self> {
        let mut options = Self::default();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--program" => {
                    options.program = Some(args.next().context("--program needs a value")?);
                }
                "--display" => {
                    options.display = Some(args.next().context("--display needs a value")?);
                }
                "--batch" => options.batch = true,
                _ => bail!("unknown argument `{arg}`"),
            }
        }
        Ok(options)
    }
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            emit(format!("error {e:#}"));
            std::process::ExitCode::FAILURE
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn run() -> anyhow::Result<()> {
    let options = Options::parse()?;
    let program = options
        .program
        .unwrap_or_else(|| "omime-fcitx-client".to_owned());
    let display = options.display.unwrap_or_else(|| "x11:".to_owned());
    let started = Instant::now();

    let conn = Connection::session()
        .await
        .context("cannot connect to the session bus")?;
    let mut stream = MessageStream::from(&conn);
    wait_for_service(&conn, &mut stream).await?;

    let version = request(
        &conn,
        &mut stream,
        SERVICE,
        INPUT_METHOD_PATH,
        INPUT_METHOD_INTERFACE,
        "Version",
        &(),
    )
    .await
    .context("Version failed")?;
    let _version: u32 = version.body().deserialize().context("bad Version reply")?;

    let args = vec![
        ("program".to_owned(), program),
        ("display".to_owned(), display),
    ];
    let created = request(
        &conn,
        &mut stream,
        SERVICE,
        INPUT_METHOD_PATH,
        INPUT_METHOD_INTERFACE,
        "CreateInputContext",
        &args,
    )
    .await
    .context("CreateInputContext failed")?;
    let (path, _uuid): (OwnedObjectPath, Vec<u8>) = created
        .body()
        .deserialize()
        .context("bad CreateInputContext reply")?;
    emit(format!("ready {path}"));

    // Commands are run one at a time, but signals must still be printed while waiting
    // for the next one. A thread feeds lines so both can be awaited together.
    let (lines_tx, mut lines_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });

    loop {
        enum Step {
            Line(Option<String>),
            Message(Option<zbus::Result<Message>>),
        }
        let step = tokio::select! {
            line = lines_rx.recv() => Step::Line(line),
            message = next_message(&mut stream) => Step::Message(message),
        };
        match step {
            Step::Line(Some(line)) => {
                if let Err(e) = command(
                    &conn,
                    &mut stream,
                    path.as_str(),
                    options.batch,
                    &line,
                    started,
                )
                .await
                {
                    emit(format!("error {e:#}"));
                }
            }
            Step::Line(None) => break,
            Step::Message(Some(message)) => {
                let message = message?;
                if let Err(e) = print_signal(&message) {
                    emit(format!("error {e:#}"));
                }
            }
            Step::Message(None) => bail!("the bus connection closed"),
        }
    }
    Ok(())
}

/// Runs one command line.
async fn command(
    conn: &Connection,
    stream: &mut MessageStream,
    path: &str,
    batch: bool,
    line: &str,
    started: Instant,
) -> anyhow::Result<()> {
    let Some((name, rest)) = take_word(line) else {
        return Ok(());
    };
    match name {
        "focus-in" => context_call(conn, stream, path, "FocusIn").await,
        "focus-out" => context_call(conn, stream, path, "FocusOut").await,
        "reset" => context_call(conn, stream, path, "Reset").await,
        "destroy" => context_call(conn, stream, path, "DestroyIC").await,
        "caps" => {
            let capability: u64 = take_word(rest)
                .context("caps needs a capability mask")?
                .0
                .parse()
                .context("bad capability mask")?;
            context_call_with(conn, stream, path, "SetCapability", &capability).await
        }
        "surrounding" => {
            let (cursor, rest) = take_word(rest).context("surrounding needs a cursor")?;
            let cursor: u32 = cursor.parse().context("bad surrounding cursor")?;
            let (anchor, text) = take_word(rest).context("surrounding needs an anchor")?;
            let anchor: u32 = anchor.parse().context("bad surrounding anchor")?;
            let body = (text, cursor, anchor);
            context_call_with(conn, stream, path, "SetSurroundingText", &body).await
        }
        "key" => {
            let (keyval, rest) = take_word(rest).context("key needs a keyval")?;
            let keyval = parse_u32(keyval).context("bad keyval")?;
            let (keycode, rest) = take_word(rest).context("key needs a keycode")?;
            let keycode: u32 = keycode.parse().context("bad keycode")?;
            let (state, release) = take_word(rest).context("key needs a state")?;
            let state: u32 = state.parse().context("bad key state")?;
            let released = match release.trim() {
                "press" => false,
                "release" => true,
                other => bail!("key needs press or release, not `{other}`"),
            };
            let time = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
            let body = (keyval, keycode, state, released, time);
            let (method, interface) = if batch {
                ("ProcessKeyEventBatch", INPUT_CONTEXT_INTERFACE)
            } else {
                ("ProcessKeyEvent", INPUT_CONTEXT_INTERFACE)
            };
            let reply = request(conn, stream, SERVICE, path, interface, method, &body).await?;
            if batch {
                let (events, handled): (Vec<(u32, OwnedVariant)>, bool) = reply
                    .body()
                    .deserialize()
                    .context("bad ProcessKeyEventBatch reply")?;
                for (kind, value) in events {
                    print_event(kind, value)?;
                }
                emit(format!("reply {handled}"));
            } else {
                let handled: bool = reply
                    .body()
                    .deserialize()
                    .context("bad ProcessKeyEvent reply")?;
                emit(format!("reply {handled}"));
            }
            Ok(())
        }
        other => bail!("unknown command `{other}`"),
    }
}

async fn context_call(
    conn: &Connection,
    stream: &mut MessageStream,
    path: &str,
    member: &str,
) -> anyhow::Result<()> {
    context_call_with(conn, stream, path, member, &()).await
}

async fn context_call_with<B>(
    conn: &Connection,
    stream: &mut MessageStream,
    path: &str,
    member: &str,
    body: &B,
) -> anyhow::Result<()>
where
    B: Serialize + DynamicType,
{
    request(
        conn,
        stream,
        SERVICE,
        path,
        INPUT_CONTEXT_INTERFACE,
        member,
        body,
    )
    .await?;
    emit("ok".to_owned());
    Ok(())
}

/// Waits until `org.fcitx.Fcitx5` has an owner.
async fn wait_for_service(conn: &Connection, stream: &mut MessageStream) -> anyhow::Result<()> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let reply = request(
            conn,
            stream,
            DBUS_SERVICE,
            DBUS_PATH,
            DBUS_INTERFACE,
            "NameHasOwner",
            &(SERVICE,),
        )
        .await?;
        let owned: bool = reply
            .body()
            .deserialize()
            .context("bad NameHasOwner reply")?;
        if owned {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("timed out waiting for {SERVICE}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Sends a method call and prints every signal that arrives until its reply. Returns
/// the reply.
async fn request<B>(
    conn: &Connection,
    stream: &mut MessageStream,
    destination: &str,
    path: &str,
    interface: &str,
    member: &str,
    body: &B,
) -> anyhow::Result<Message>
where
    B: Serialize + DynamicType,
{
    let message = Message::method_call(path, member)
        .destination(destination)
        .interface(interface)
        .build(body)?;
    let serial = message.primary_header().serial_num();
    conn.send(&message).await?;
    loop {
        let Some(reply) = next_message(stream).await else {
            bail!("the bus connection closed while waiting for {member}");
        };
        let reply = reply?;
        if reply
            .header()
            .reply_serial()
            .is_some_and(|reply_to| reply_to == serial)
        {
            if reply.message_type() == Type::Error {
                let name = reply
                    .header()
                    .error_name()
                    .map_or_else(|| "error".to_owned(), |name| name.as_str().to_owned());
                let detail = reply.body().deserialize::<String>().unwrap_or_default();
                bail!("{name}: {detail}");
            }
            return Ok(reply);
        }
        print_signal(&reply)?;
    }
}

/// The next message of the connection, or `None` when it closed.
async fn next_message(stream: &mut MessageStream) -> Option<zbus::Result<Message>> {
    std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx)).await
}

/// Prints a signal of the input context, if `message` is one.
fn print_signal(message: &Message) -> anyhow::Result<()> {
    let header = message.header();
    if header.message_type() != Type::Signal
        || header
            .interface()
            .is_none_or(|interface| interface.as_str() != INPUT_CONTEXT_INTERFACE)
    {
        return Ok(());
    }
    let Some(member) = header.member() else {
        return Ok(());
    };
    match member.as_str() {
        "CommitString" => {
            let text: String = message.body().deserialize()?;
            emit(format!("commit {}", json_string(&text)));
        }
        "UpdateFormattedPreedit" => {
            let (segments, cursor): (Vec<(String, i32)>, i32) = message.body().deserialize()?;
            emit_preedit(&segments, cursor);
        }
        "ForwardKey" => {
            let (keyval, state, released): (u32, u32, bool) = message.body().deserialize()?;
            emit(format!("forward {keyval} {state} {released}"));
        }
        "DeleteSurroundingText" => {
            let (offset, length): (i32, u32) = message.body().deserialize()?;
            emit(format!("delete {offset} {length}"));
        }
        _ => (),
    }
    Ok(())
}

/// Prints an event of a `ProcessKeyEventBatch` reply.
fn print_event(kind: u32, value: OwnedVariant) -> anyhow::Result<()> {
    match kind {
        0 => {
            let text: String = value.try_into()?;
            emit(format!("commit {}", json_string(&text)));
        }
        1 => {
            let (segments, cursor): (Vec<(String, i32)>, i32) = value.try_into()?;
            emit_preedit(&segments, cursor);
        }
        2 => {
            let (keyval, state, released): (u32, u32, bool) = value.try_into()?;
            emit(format!("forward {keyval} {state} {released}"));
        }
        3 => {
            let (offset, length): (i32, u32) = value.try_into()?;
            emit(format!("delete {offset} {length}"));
        }
        other => bail!("unknown batch event kind {other}"),
    }
    Ok(())
}

fn emit_preedit(segments: &[(String, i32)], cursor: i32) {
    let text: String = segments.iter().map(|(text, _)| text.as_str()).collect();
    emit(format!("preedit {} {cursor}", json_string(&text)));
}

/// Writes one line and flushes it. Every line goes through here, so they never
/// interleave.
fn emit(line: String) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

/// The first word of `line` and the rest without leading whitespace.
fn take_word(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    if line.is_empty() {
        return None;
    }
    match line.split_once(char::is_whitespace) {
        Some((word, rest)) => Some((word, rest.trim_start())),
        None => Some((line, "")),
    }
}

/// A keyval in hexadecimal (`0x61`) or decimal.
fn parse_u32(text: &str) -> anyhow::Result<u32> {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Ok(u32::from_str_radix(hex, 16)?)
    } else {
        Ok(text.parse()?)
    }
}
