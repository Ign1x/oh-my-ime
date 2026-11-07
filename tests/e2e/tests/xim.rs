//! End-to-end tests for XIM: an X11 test client that uses the protocol like a real
//! application, omimed and a real engine, on the XWayland server of a headless sway.
//!
//! They need a built workspace, the tools of `nix develop` and `xwayland`:
//!
//! ```sh
//! cargo build --workspace && cargo test -p omime-e2e --test xim -- --ignored
//! ```

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use omime_e2e::harness::{TIMEOUT, TestEnv, dummy_engine, echo_engine};
use omime_e2e::json_string;
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt as _, EventMask,
    KeyPressEvent,
};
use x11rb::rust_connection::RustConnection;
use xim::x11rb::X11rbClient;
use xim::{
    Client, ClientCore as _, ClientError, ClientHandler, CommitData, ErrorCode, ErrorFlag,
    ForwardEventFlag, Request,
};

/// The line the client prints for `prefix` and `text`.
fn line(prefix: &str, text: &str) -> String {
    format!("{prefix} {}", json_string(text))
}

/// The first line equal to `wanted`, or a failure that shows every line.
fn position(lines: &[String], wanted: &str) -> usize {
    lines
        .iter()
        .position(|l| l == wanted)
        .unwrap_or_else(|| panic!("no {wanted} in {lines:?}"))
}

/// What omimed logs when an application leaves a synchronous request unanswered.
const SYNC_TIMEOUT: &str = "did not answer in time";

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn xim_clients_type_through_engines() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[echo_engine()], Some("echo"))?;
    let client = env.xim_client(&[])?;

    env.type_on_seat("seat0", "ab ")?;
    client.wait_for_text("AB ")?;

    // The preedit grew letter by letter, the commit came before the space the engine
    // did not consume, and the application handled that space itself.
    let lines = client.lines();
    let a = position(&lines, &line("preedit", "a"));
    let ab = position(&lines, &line("preedit", "ab"));
    let commit = position(&lines, &line("commit", "AB"));
    let space = position(&lines, &line("key", " "));
    assert!(a < ab, "{lines:?}");
    assert!(ab < commit, "{lines:?}");
    assert!(commit < space, "{lines:?}");
    // The client answered every synchronous request, and omimed noticed.
    assert_eq!(env.count_in_log(SYNC_TIMEOUT), 0);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn fast_typing_keeps_the_order() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[echo_engine()], Some("echo"))?;
    let client = env.xim_client(&[])?;

    env.type_on_seat("seat0", "1ab 2")?;
    client.wait_for_text("1AB 2")?;
    assert_eq!(env.count_in_log(SYNC_TIMEOUT), 0);
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn direct_input_sends_keys_back() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[echo_engine()], Some("echo"))?;
    let client = env.xim_client(&[])?;

    env.omime(&["switch", "direct"])?;
    env.wait_for_status("direct input")?;
    let before = client.preedits().len();

    env.type_on_seat("seat0", "cd")?;
    client.wait_for_text("cd")?;
    let preedits = client.preedits();
    assert!(preedits[before..].is_empty(), "{:?}", client.lines());
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn xim_survives_a_client_restart() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[echo_engine()], Some("echo"))?;
    let first = env.xim_client(&[])?;
    env.type_on_seat("seat0", "ab ")?;
    first.wait_for_text("AB ")?;
    // Dropping the client kills it, as when an application crashes.
    drop(first);

    let second = env.xim_client(&[])?;
    env.type_on_seat("seat0", "cd ")?;
    second.wait_for_text("CD ")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn engines_learn_whether_xim_applications_show_the_preedit() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[dummy_engine(&[])], Some("dummy"))?;
    // Like xterm: the input method is to draw the preedit.
    let _xterm = env.xim_client(&["--position"])?;
    env.wait_for_log("activate frontend=xim client-preedit=false")?;
    // The client drawing its own preedit takes the focus.
    let _drawing = env.xim_client(&[])?;
    env.wait_for_log("activate frontend=xim client-preedit=true")?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn applications_find_omimed_by_the_names_of_fcitx_and_ibus() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[echo_engine()], Some("echo"))?;
    // XMODIFIERS=@im=fcitx or @im=ibus, as setups of those input methods leave it.
    for (name, typed, text) in [("fcitx", "ab ", "AB "), ("ibus", "cd ", "CD ")] {
        let client = env.xim_client(&["--im", name])?;
        env.type_on_seat("seat0", typed)?;
        client.wait_for_text(text)?;
    }
    Ok(())
}

const CHURN_CYCLES: u32 = 70_000;
// One retained context plus this many create/destroy pairs exhausts the connection.
const LIVE_CONTEXT_BOUNDARY: u32 = 65_533;

#[derive(Clone, Copy, Debug)]
enum ChurnKind {
    Context,
    LiveContext,
    Open,
}

/// One context or input method is churned at a time, optionally alongside a retained
/// context. Count replies so the last DestroyIc or Close is also known to be handled.
struct Churn {
    kind: ChurnKind,
    completed: u32,
    input_method_id: Option<u16>,
    destroying: Option<(u16, u16)>,
    live: Option<(u16, u16)>,
    boundary_started: bool,
    boundary_sent: bool,
}

impl Churn {
    fn new(kind: ChurnKind) -> Self {
        Self {
            kind,
            completed: 0,
            input_method_id: None,
            destroying: None,
            live: None,
            boundary_started: false,
            boundary_sent: false,
        }
    }

    /// xim 0.6.0 never dispatches DestroyIcReply to ClientHandler::handle_destroy_ic.
    /// Read and match it ourselves before letting the client consume the wire message.
    fn destroy_reply(&mut self, request: &Request) -> anyhow::Result<Option<u16>> {
        let Request::DestroyIcReply {
            input_method_id: im,
            input_context_id: ic,
        } = *request
        else {
            return Ok(None);
        };
        anyhow::ensure!(
            self.destroying.take() == Some((im, ic)),
            "unmatched DestroyIcReply"
        );
        self.completed += 1;
        Ok((self.completed < CHURN_CYCLES
            && !(self.live.is_some() && self.completed == LIVE_CONTEXT_BOUNDARY))
            .then_some(im))
    }
}

/// Keep the last key pending until the client resumes or leaves, even with a fast echo
/// engine. Resume it on every exit path so teardown and the second client can proceed.
struct PausedEngine(rustix::process::Pid);

impl PausedEngine {
    fn echo(env: &TestEnv) -> anyhow::Result<Self> {
        let pid = i32::try_from(env.engine_pid("echo").context("echo is not running")?)?;
        let pid = rustix::process::Pid::from_raw(pid).context("invalid echo pid")?;
        rustix::process::kill_process(pid, rustix::process::Signal::STOP)?;
        Ok(Self(pid))
    }
}

impl Drop for PausedEngine {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.0, rustix::process::Signal::CONT);
    }
}

impl<C: Client> ClientHandler<C> for Churn {
    fn handle_connect(&mut self, client: &mut C) -> Result<(), ClientError> {
        client.open("en_US")
    }

    fn handle_open(&mut self, client: &mut C, im: u16) -> Result<(), ClientError> {
        self.input_method_id = Some(im);
        match self.kind {
            ChurnKind::Context | ChurnKind::LiveContext => client.create_ic(im, Vec::new()),
            ChurnKind::Open => client.close(im),
        }
    }

    fn handle_create_ic(&mut self, client: &mut C, im: u16, ic: u16) -> Result<(), ClientError> {
        if matches!(self.kind, ChurnKind::LiveContext) && self.completed == 0 && self.live.is_none()
        {
            self.live = Some((im, ic));
            return client.create_ic(im, Vec::new());
        }
        self.destroying = Some((im, ic));
        client.destroy_ic(im, ic)
    }

    fn handle_close(&mut self, client: &mut C, _im: u16) -> Result<(), ClientError> {
        self.completed += 1;
        if self.completed < CHURN_CYCLES {
            client.open("en_US")?;
        }
        Ok(())
    }
}

/// Peek without deleting property messages: X11rbClient still consumes them afterwards.
fn wire_request(conn: &RustConnection, msg: &ClientMessageEvent) -> anyhow::Result<Request> {
    if msg.format == 32 {
        let [length, atom, ..] = msg.data.as_data32();
        let reply = conn
            .get_property(false, msg.window, atom, AtomEnum::ANY, 0, length)?
            .reply()?;
        Ok(xim::read(&reply.value)?)
    } else {
        Ok(xim::read(&msg.data.as_data8())?)
    }
}

/// libX11's _XimCreateICCheck only accepts an error with a valid matching IMID.
fn create_ic_error_matches(request: &Request, im: u16) -> bool {
    matches!(request, Request::Error { input_method_id, flag, .. }
        if flag.contains(ErrorFlag::INPUT_METHOD_ID_VALID) && *input_method_id == im)
}

struct ChurnPeer {
    client_win: u32,
    com_win: u32,
    protocol: u32,
}

impl ChurnPeer {
    /// These replies are consumed here, without passing them to X11rbClient.
    fn take_request(
        &self,
        conn: &RustConnection,
        event: &Event,
    ) -> anyhow::Result<Option<Request>> {
        if let Event::Error(error) = event {
            anyhow::bail!("X11 error on the churn connection: {error:?}");
        }
        let Event::ClientMessage(msg) = event else {
            return Ok(None);
        };
        if msg.type_ != self.protocol || msg.window != self.client_win {
            return Ok(None);
        }
        let request = wire_request(conn, msg)?;
        if msg.format == 32 {
            conn.delete_property(msg.window, msg.data.as_data32()[1])?;
        }
        Ok(Some(request))
    }

    fn next_request(&self, conn: &RustConnection) -> anyhow::Result<Request> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            anyhow::ensure!(
                Instant::now() < deadline,
                "the exhausted connection stopped answering"
            );
            if let Some(event) = conn.poll_for_event()? {
                anyhow::ensure!(
                    !matches!(&event, Event::DestroyNotify(msg) if msg.window == self.com_win),
                    "CreateIc refusal dropped the connection"
                );
                if let Some(request) = self.take_request(conn, &event)? {
                    return Ok(request);
                }
            } else {
                std::thread::sleep(Duration::from_micros(50));
            }
        }
    }

    fn wait_for_disconnect(&self, conn: &RustConnection) -> anyhow::Result<()> {
        let deadline = Instant::now() + TIMEOUT;
        let mut replied = false;
        loop {
            anyhow::ensure!(
                Instant::now() < deadline,
                "XIM_DISCONNECT was not completed"
            );
            if let Some(event) = conn.poll_for_event()? {
                if matches!(&event, Event::DestroyNotify(msg) if msg.window == self.com_win) {
                    anyhow::ensure!(replied, "transport was destroyed before DisconnectReply");
                    return Ok(());
                }
                if let Some(request) = self.take_request(conn, &event)? {
                    anyhow::ensure!(
                        matches!(request, Request::DisconnectReply { .. }),
                        "unexpected output while disconnecting: {request:?}"
                    );
                    replied = true;
                }
            } else {
                std::thread::sleep(Duration::from_micros(50));
            }
        }
    }

    fn no_stale_output(&self, conn: &RustConnection) -> anyhow::Result<()> {
        // Keep the client's window alive beyond the 300 ms sync timeout, so leaked
        // queued output and late engine replies have somewhere to arrive.
        let deadline = Instant::now() + Duration::from_millis(400);
        loop {
            if let Some(event) = conn.poll_for_event()? {
                if let Some(request) = self.take_request(conn, &event)? {
                    anyhow::ensure!(
                        !matches!(
                            request,
                            Request::Commit { .. } | Request::ForwardEvent { .. }
                        ),
                        "stale output reached the disconnected client"
                    );
                }
            } else if Instant::now() >= deadline {
                return Ok(());
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn expect_commit(request: &Request, im: u16, ic: u16, text: &[u8]) -> anyhow::Result<()> {
    // omimed's Compound Text encoder wraps UTF-8 in ESC % G ... ESC % @.
    anyhow::ensure!(
        matches!(request, Request::Commit {
            input_method_id, input_context_id,
            data: CommitData::Chars { commited, .. }
        } if *input_method_id == im && *input_context_id == ic
            && commited.strip_prefix(b"\x1b%G").and_then(|utf8| utf8.strip_suffix(b"\x1b%@")) == Some(text)),
        "the retained context did not commit the expected text"
    );
    Ok(())
}

/// After refusal, finish the AB commit and both spaces on the retained context.
/// Then prepare another commit, queued space and pending key for client-side cleanup.
fn resume_retained_context(
    conn: &RustConnection,
    client: &mut X11rbClient<&RustConnection>,
    peer: &ChurnPeer,
    env: &TestEnv,
    context: (u16, u16),
    paused: PausedEngine,
    checkpoint: &mut impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<PausedEngine> {
    let (im, ic) = context;
    drop(paused);
    // The AB commit was observed before the refusal. Acknowledge it now so the
    // queued first space and the key held by the paused engine can reach us in order.
    client.send_req(Request::SyncReply {
        input_method_id: im,
        input_context_id: ic,
    })?;
    conn.flush()?;
    for _ in 0..2 {
        let request = peer.next_request(conn)?;
        anyhow::ensure!(
            matches!(&request, Request::ForwardEvent {
                input_method_id, input_context_id, flag, xev, ..
            } if *input_method_id == im && *input_context_id == ic
                && flag.contains(ForwardEventFlag::SYNCHRONOUS) && xev.response_type & 0x7f == 2),
            "the retained context lost a space after CreateIc refusal: {request:?}"
        );
        client.send_req(Request::SyncReply {
            input_method_id: im,
            input_context_id: ic,
        })?;
        conn.flush()?;
    }
    checkpoint()?;
    for character in ['c', 'd', ' '] {
        forward_character(conn, client, im, ic, character)?;
    }
    conn.flush()?;
    expect_commit(&peer.next_request(conn)?, im, ic, b"CD")?;
    let paused = PausedEngine::echo(env)?;
    forward_character(conn, client, im, ic, ' ')?;
    // Leave the CD SyncReply unanswered: its space stays queued while the last key
    // is pending. The caller sends XIM_DISCONNECT after that key on the same stream.
    Ok(paused)
}

fn forward_character(
    conn: &RustConnection,
    client: &mut X11rbClient<&RustConnection>,
    im: u16,
    ic: u16,
    character: char,
) -> anyhow::Result<()> {
    let setup = conn.setup();
    let mapping = conn
        .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)?
        .reply()?;
    let index = mapping
        .keysyms
        .chunks(usize::from(mapping.keysyms_per_keycode))
        .position(|syms| syms.first() == Some(&u32::from(character)))
        .context("the X11 keymap has no unmodified key for the test character")?;
    client.forward_event(
        im,
        ic,
        ForwardEventFlag::empty(),
        &KeyPressEvent {
            response_type: 2,
            detail: setup.min_keycode + u8::try_from(index)?,
            same_screen: true,
            ..KeyPressEvent::default()
        },
    )?;
    Ok(())
}

/// Status must answer promptly; a stuck daemon is as much a failure as an exit.
fn status_answers(env: &TestEnv) -> anyhow::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let conn = zbus::connection::Builder::address(
                format!("unix:path={}/bus", env.dir().display()).as_str(),
            )
            .method_timeout(Duration::from_secs(3))
            .build()
            .await?;
            let daemon = zbus::Proxy::new(
                &conn,
                "io.github.ign1x.OhMyIme",
                "/io/github/ign1x/OhMyIme",
                "io.github.ign1x.OhMyIme.Daemon1",
            )
            .await?;
            daemon
                .call::<_, _, HashMap<String, zbus::OwnedVariant>>("Status", &())
                .await
        })
        .context("omimed must answer Status after XIM churn")?;
    Ok(())
}

/// A refused CreateIc leaves the connection working; the client then disconnects.
/// Open exhaustion is contained by dropping its connection. Reconnect to finish all
/// 70,000 cycles, checking the daemon at every disconnect.
fn churn(
    env: &TestEnv,
    kind: ChurnKind,
    mut checkpoint: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    env.wait_for_log("serving XIM")?;
    let display = env.command("printenv").arg("DISPLAY").output()?;
    let display = String::from_utf8(display.stdout)?;
    anyhow::ensure!(
        !display.trim().is_empty(),
        "no DISPLAY in the test environment"
    );
    let (conn, screen) = RustConnection::connect(Some(display.trim()))?;
    let xconnect = conn.intern_atom(false, b"_XIM_XCONNECT")?.reply()?.atom;
    let protocol = conn.intern_atom(false, b"_XIM_PROTOCOL")?.reply()?.atom;
    let mut handler = Churn::new(kind);
    let started = Instant::now();
    let mut next_checkpoint = 20_000;
    let mut disconnects = 0;
    let mut errors = 0;
    let mut deactivations = 0;
    let mut paused_engine = None;
    while handler.completed < CHURN_CYCLES {
        let before = handler.completed;
        let mut client = X11rbClient::init(&conn, screen, Some("omime"))?;
        let mut windows = None;
        let mut last_event = Instant::now();
        while handler.completed < CHURN_CYCLES {
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(400),
                "{kind:?} churn timed out after {} cycles",
                handler.completed
            );
            let Some(event) = conn.poll_for_event()? else {
                anyhow::ensure!(
                    last_event.elapsed() < TIMEOUT,
                    "{kind:?} churn stalled after {} cycles; daemon log:\n{}",
                    handler.completed,
                    env.daemon_log()
                );
                std::thread::sleep(Duration::from_micros(50));
                continue;
            };
            last_event = Instant::now();
            match &event {
                Event::ClientMessage(msg) if msg.type_ == xconnect => {
                    let com_win = msg.data.as_data32()[0];
                    windows = Some((msg.window, com_win));
                    conn.change_window_attributes(
                        com_win,
                        &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
                    )?
                    .check()?;
                }
                Event::DestroyNotify(msg) if windows.is_some_and(|(_, win)| win == msg.window) => {
                    anyhow::ensure!(
                        matches!(kind, ChurnKind::Open),
                        "CreateIc refusal dropped the connection"
                    );
                    disconnects += 1;
                    status_answers(env)?;
                    checkpoint()?;
                    break;
                }
                Event::ClientMessage(msg) if msg.type_ == protocol => {
                    let request = wire_request(&conn, msg)?;
                    if let Some(im) = handler.destroy_reply(&request)? {
                        client.create_ic(im, Vec::new())?;
                    }
                    match &request {
                        Request::Error { code, .. } => {
                            let im = handler.input_method_id.context("no pending CreateIc")?;
                            anyhow::ensure!(
                                create_ic_error_matches(&request, im),
                                "libX11 would ignore this unmatched XIM_ERROR"
                            );
                            anyhow::ensure!(*code == ErrorCode::BadAlloc, "unexpected XIM error");
                            // A second matching error proves the connection still answers
                            // and the lifetime allocation limit remains in force.
                            if msg.format == 32 {
                                conn.delete_property(msg.window, msg.data.as_data32()[1])?;
                            }
                            let (client_win, com_win) = windows.context("no XIM transport")?;
                            let peer = ChurnPeer {
                                client_win,
                                com_win,
                                protocol,
                            };
                            client.create_ic(im, Vec::new())?;
                            conn.flush()?;
                            let repeated = peer.next_request(&conn)?;
                            anyhow::ensure!(
                                create_ic_error_matches(&repeated, im)
                                    && matches!(
                                        repeated,
                                        Request::Error {
                                            code: ErrorCode::BadAlloc,
                                            ..
                                        }
                                    ),
                                "the exhausted connection did not answer another CreateIc"
                            );
                            errors += 2;
                            if let Some(context) = handler.live {
                                anyhow::ensure!(
                                    handler.boundary_sent,
                                    "the live context did not reach exhaustion"
                                );
                                anyhow::ensure!(
                                    env.count_in_log("XIM text field left") == deactivations,
                                    "refusal deactivated the retained context"
                                );
                                paused_engine = Some(resume_retained_context(
                                    &conn,
                                    &mut client,
                                    &peer,
                                    env,
                                    context,
                                    paused_engine
                                        .take()
                                        .context("echo was not paused at refusal")?,
                                    &mut checkpoint,
                                )?);
                            }
                            client.disconnect()?;
                            conn.flush()?;
                            peer.wait_for_disconnect(&conn)?;
                            drop(paused_engine.take());
                            disconnects += 1;
                            if handler.live.take().is_some() {
                                anyhow::ensure!(
                                    env.count_in_log("XIM text field left") == deactivations + 1,
                                    "the departing client's context was not deactivated"
                                );
                                peer.no_stale_output(&conn)?;
                            }
                            status_answers(env)?;
                            checkpoint()?;
                            break;
                        }
                        Request::Commit { .. }
                            if handler.boundary_started && !handler.boundary_sent =>
                        {
                            // Echo commits AB before returning the space. Withhold the
                            // SyncReply so that space stays queued; send another key and
                            // the over-limit CreateIc together, leaving pending key work.
                            if msg.format == 32 {
                                conn.delete_property(msg.window, msg.data.as_data32()[1])?;
                            }
                            let (im, ic) = handler.live.context("no retained context")?;
                            expect_commit(&request, im, ic, b"AB")?;
                            paused_engine = Some(PausedEngine::echo(env)?);
                            forward_character(&conn, &mut client, im, ic, ' ')?;
                            client.create_ic(im, Vec::new())?;
                            conn.flush()?;
                            handler.boundary_sent = true;
                            continue;
                        }
                        Request::Commit { .. } | Request::ForwardEvent { .. }
                            if handler.boundary_sent =>
                        {
                            anyhow::bail!("queued output escaped while SyncReply was withheld");
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            client
                .filter_event(&event, &mut handler)
                .context("unexpected XIM error during churn")?;
            if let Some((im, ic)) = handler.live
                && handler.completed == LIVE_CONTEXT_BOUNDARY
                && !handler.boundary_started
            {
                deactivations = env.count_in_log("XIM text field left");
                client.set_focus(im, ic)?;
                for character in ['a', 'b', ' '] {
                    forward_character(&conn, &mut client, im, ic, character)?;
                }
                handler.boundary_started = true;
            }
            conn.flush()?;
            if handler.completed >= next_checkpoint {
                status_answers(env)?;
                checkpoint()?;
                next_checkpoint += 20_000;
                last_event = Instant::now();
            }
        }
        anyhow::ensure!(
            handler.completed > before,
            "reconnected XIM client made no progress"
        );
        if let Some((client_win, _)) = windows {
            conn.destroy_window(client_win)?;
            conn.flush()?;
        }
    }
    assert_eq!(handler.completed, CHURN_CYCLES);
    assert!(
        disconnects > 0,
        "the churn never reached the connection's allocation limit"
    );
    if matches!(kind, ChurnKind::Context | ChurnKind::LiveContext) {
        assert_eq!(
            errors,
            2 * disconnects,
            "each connection must answer both over-limit CreateIc requests"
        );
        assert_eq!(
            env.count_in_log("input context allocation limit reached; refusing CreateIc"),
            disconnects,
            "warn only once per exhausted connection"
        );
        assert!(
            !env.daemon_log().contains("panicked at"),
            "CreateIc reached the allocator panic"
        );
    }
    status_answers(env)?;
    checkpoint()?;
    Ok(())
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn xim_context_churn_over_the_wire_keeps_omimed_alive() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[], None)?;
    churn(&env, ChurnKind::Context, || Ok(()))
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn open_close_churn_keeps_omimed_alive() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[echo_engine()], Some("echo"))?;
    let client = env.xim_client(&[])?;
    let mut expected = String::new();
    churn(&env, ChurnKind::Open, || {
        env.type_on_seat("seat0", "ab ")?;
        expected.push_str("AB ");
        client.wait_for_text(&expected)
    })
}

#[test]
#[ignore = "end-to-end test; see the module docs"]
fn a_second_client_keeps_typing_during_context_churn() -> anyhow::Result<()> {
    let env = TestEnv::start_with_x11(&[echo_engine()], Some("echo"))?;
    let client = env.xim_client(&[])?;
    let mut expected = String::new();
    churn(&env, ChurnKind::LiveContext, || {
        env.type_on_seat("seat0", "ab ")?;
        expected.push_str("AB ");
        client.wait_for_text(&expected)
    })
}

#[test]
fn create_ic_reply_matching_rejects_missing_or_different_imids() {
    let error = |im, flag| Request::Error {
        input_method_id: im,
        input_context_id: 0,
        flag,
        code: ErrorCode::BadAlloc,
        detail: String::new(),
    };
    assert!(!create_ic_error_matches(&error(0, ErrorFlag::empty()), 2));
    assert!(!create_ic_error_matches(&error(2, ErrorFlag::empty()), 2));
    assert!(!create_ic_error_matches(
        &error(1, ErrorFlag::INPUT_METHOD_ID_VALID),
        2
    ));
    assert!(create_ic_error_matches(
        &error(2, ErrorFlag::INPUT_METHOD_ID_VALID),
        2
    ));
}

#[test]
fn context_churn_counts_only_matching_destroy_replies() -> anyhow::Result<()> {
    let mut handler = Churn::new(ChurnKind::Context);
    for ic in 1..=3 {
        handler.destroying = Some((2, ic));
        let wire = xim::write_to_vec(Request::DestroyIcReply {
            input_method_id: 2,
            input_context_id: ic,
        });
        assert_eq!(handler.destroy_reply(&xim::read(&wire)?)?, Some(2));
    }
    assert_eq!(handler.completed, 3);
    assert!(
        handler
            .destroy_reply(&Request::DestroyIcReply {
                input_method_id: 2,
                input_context_id: 3
            })
            .is_err(),
        "a duplicate reply must not advance churn"
    );
    Ok(())
}

#[test]
fn retained_context_commits_use_compound_text() -> anyhow::Result<()> {
    let wire = xim::write_to_vec(Request::Commit {
        input_method_id: 2,
        input_context_id: 3,
        data: CommitData::Chars {
            commited: b"\x1b%GAB\x1b%@".to_vec(),
            syncronous: true,
        },
    });
    let request = xim::read(&wire)?;
    expect_commit(&request, 2, 3, b"AB")?;
    assert!(expect_commit(&request, 2, 3, b"CD").is_err());
    assert!(expect_commit(&request, 2, 4, b"AB").is_err());
    Ok(())
}
