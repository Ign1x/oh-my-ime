//! An X11 application that uses XIM, like xterm, a Java or Wine application, or GTK
//! with `GTK_IM_MODULE=xim`.
//!
//! It opens a window, connects to the XIM server named in `XMODIFIERS`, creates one
//! input context for the window and prints what happens to stdout, one event per line:
//!
//! ```text
//! ready
//! preedit "ab"
//! commit "AB"
//! text "AB "
//! key " "
//! text "AB  "
//! ```
//!
//! `preedit` is the whole preedit after every draw callback, `commit` a string the
//! server committed, `key` a key the server sent back (handled like a key the
//! application got itself), `release` a key release the server sent back (which it
//! should not) and `text` the document after a commit or key.
//!
//! `--spot X,Y` tells the server where the window draws text (0,0 by default).

use std::io::Write as _;

use anyhow::Context as _;
use omime_e2e::json_string;
use x11rb::COPY_DEPTH_FROM_PARENT;
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, KeyPressEvent, PropMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use xim::x11rb::X11rbClient;
use xim::{
    AHashMap, AttributeName, Client, ClientCore, ClientError, ClientHandler, ForwardEventFlag,
    InputStyle, Point, Request,
};
use xkbcommon::xkb;

/// `ShiftMask`.
const SHIFT: u16 = 1;

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("omime-xim-client: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Where the text is drawn in the window (`--spot X,Y`): the start of its baseline.
fn spot() -> anyhow::Result<Point> {
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (None, _) => Ok(Point { x: 0, y: 0 }),
        (Some("--spot"), Some(value)) => {
            let (x, y) = value.split_once(',').context("--spot X,Y")?;
            Ok(Point {
                x: x.parse()?,
                y: y.parse()?,
            })
        }
        _ => anyhow::bail!("usage: omime-xim-client [--spot X,Y]"),
    }
}

fn run() -> anyhow::Result<()> {
    let spot = spot()?;
    let (conn, screen_num) =
        RustConnection::connect(None).context("cannot connect to the X server")?;
    let screen = conn.setup().roots[screen_num].clone();
    let window = conn.generate_id()?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        window,
        screen.root,
        0,
        0,
        320,
        120,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new()
            .background_pixel(screen.white_pixel)
            .event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE | EventMask::FOCUS_CHANGE),
    )?;
    let title = b"omime xim client";
    conn.change_property(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        8,
        title.len() as u32,
        title,
    )?;
    // The instance and the class of the window, as toolkits set them.
    let class = b"omime-xim-client\0omime-xim-client\0";
    conn.change_property(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        8,
        class.len() as u32,
        class,
    )?;
    conn.map_window(window)?;
    conn.flush()?;

    let keymap = Keymap::load(&conn)?;
    let mut client = X11rbClient::init(&conn, screen_num, None)
        .context("cannot connect to the XIM server (XMODIFIERS)")?;
    let mut handler = Handler {
        window,
        spot,
        input_method_id: None,
        input_context_id: None,
        document: String::new(),
        preedit: String::new(),
        keymap,
    };

    loop {
        let event = match conn.wait_for_event() {
            Ok(event) => event,
            // The X server or our connection to it went away.
            Err(_) => return Ok(()),
        };
        match client.filter_event(&event, &mut handler) {
            Ok(true) => {
                conn.flush()?;
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                emit(&format!("error {}", json_string(&e.to_string())));
                return Err(e).context("a broken XIM message");
            }
        }
        match event {
            Event::KeyPress(e) if e.event == window => {
                if let Some((im, ic)) = handler.context() {
                    // Only presses: the server asked for them.
                    client.forward_event(im, ic, ForwardEventFlag::empty(), &e)?;
                }
            }
            Event::FocusIn(e) if e.event == window => {
                if let Some((im, ic)) = handler.context() {
                    client.set_focus(im, ic)?;
                }
            }
            Event::FocusOut(e) if e.event == window => {
                if let Some((im, ic)) = handler.context() {
                    client.unset_focus(im, ic)?;
                }
            }
            _ => {}
        }
        conn.flush()?;
    }
}

/// Prints a line for the tests.
fn emit(line: &str) {
    println!("{line}");
    // Tests wait for lines as they happen.
    let _ = std::io::stdout().flush();
}

/// The keys of the X server's core keyboard mapping.
#[derive(Debug, Default)]
struct Keymap {
    min_keycode: u8,
    keysyms_per_keycode: u8,
    keysyms: Vec<u32>,
}

impl Keymap {
    fn load(conn: &RustConnection) -> anyhow::Result<Self> {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let reply = conn
            .get_keyboard_mapping(min, max.saturating_sub(min).saturating_add(1))?
            .reply()?;
        Ok(Self {
            min_keycode: min,
            keysyms_per_keycode: reply.keysyms_per_keycode,
            keysyms: reply.keysyms,
        })
    }

    /// The character `keycode` types with the modifiers in `state`, or none.
    fn character(&self, keycode: u8, state: u16) -> Option<char> {
        let per = usize::from(self.keysyms_per_keycode);
        let index = usize::from(keycode.checked_sub(self.min_keycode)?).checked_mul(per)?;
        let syms = self.keysyms.get(index..index.checked_add(per)?)?;
        let first = syms.first().copied()?;
        let second = syms.get(1).copied().unwrap_or(0);
        let keysym = if state & SHIFT != 0 && second != 0 {
            second
        } else {
            first
        };
        let text = xkb::keysym_to_utf8(xkb::Keysym::new(keysym));
        let mut chars = text.trim_end_matches('\0').chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    }
}

struct Handler {
    window: u32,
    spot: Point,
    input_method_id: Option<u16>,
    input_context_id: Option<u16>,
    /// What the application has typed and committed so far.
    document: String,
    /// The preedit the server drew.
    preedit: String,
    keymap: Keymap,
}

impl Handler {
    fn context(&self) -> Option<(u16, u16)> {
        Some((self.input_method_id?, self.input_context_id?))
    }

    fn commit(&mut self, text: &str) {
        self.document.push_str(text);
        emit(&format!("commit {}", json_string(text)));
        emit(&format!("text {}", json_string(&self.document)));
    }

    /// The character of a key the server sent back, handled like one of our own.
    fn press(&mut self, xev: &KeyPressEvent) {
        let Some(c) = self.keymap.character(xev.detail, u16::from(xev.state)) else {
            return;
        };
        let mut text = String::new();
        text.push(c);
        // A release changes nothing in the document; the server should never send
        // one, since applications get their releases from the X server.
        if xev.response_type & 0x7f != 2 {
            emit(&format!("release {}", json_string(&text)));
            return;
        }
        self.document.push(c);
        emit(&format!("key {}", json_string(&text)));
        emit(&format!("text {}", json_string(&self.document)));
    }

    /// Replaces characters `chg_first..chg_first + chg_len` of the preedit.
    fn draw(&mut self, chg_first: i32, chg_len: i32, text: &str) {
        let chars = self.preedit.chars().count();
        let first = usize::try_from(chg_first).unwrap_or(0).min(chars);
        let end = first
            .saturating_add(usize::try_from(chg_len).unwrap_or(0))
            .min(chars);
        let byte = |index: usize| {
            self.preedit
                .char_indices()
                .nth(index)
                .map_or(self.preedit.len(), |(offset, _)| offset)
        };
        let (start, end) = (byte(first), byte(end));
        self.preedit.replace_range(start..end, text);
        emit(&format!("preedit {}", json_string(&self.preedit)));
    }
}

impl<C: Client<XEvent = KeyPressEvent> + ClientCore> ClientHandler<C> for Handler {
    fn handle_connect(&mut self, client: &mut C) -> Result<(), ClientError> {
        client.open("en_US")
    }

    fn handle_open(&mut self, client: &mut C, input_method_id: u16) -> Result<(), ClientError> {
        self.input_method_id = Some(input_method_id);
        client.get_im_values(input_method_id, &[AttributeName::QueryInputStyle])
    }

    fn handle_get_im_values(
        &mut self,
        client: &mut C,
        input_method_id: u16,
        _attributes: AHashMap<AttributeName, Vec<u8>>,
    ) -> Result<(), ClientError> {
        // The application draws the preedit; it needs no status area.
        let attributes = client
            .build_ic_attributes()
            .push(
                AttributeName::InputStyle,
                InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_NOTHING,
            )
            .push(AttributeName::ClientWindow, self.window)
            .push(AttributeName::FocusWindow, self.window)
            .nested_list(AttributeName::PreeditAttributes, |b| {
                b.push(AttributeName::SpotLocation, self.spot.clone());
            })
            .build();
        client.create_ic(input_method_id, attributes)
    }

    fn handle_create_ic(
        &mut self,
        client: &mut C,
        input_method_id: u16,
        input_context_id: u16,
    ) -> Result<(), ClientError> {
        self.input_context_id = Some(input_context_id);
        client.set_focus(input_method_id, input_context_id)?;
        emit("ready");
        Ok(())
    }

    fn handle_commit(
        &mut self,
        client: &mut C,
        input_method_id: u16,
        input_context_id: u16,
        text: &str,
    ) -> Result<(), ClientError> {
        self.commit(text);
        // xim-parser 0.3 misreads the synchronous flag of string commits (it looks for
        // the keysym flag), so the crate never answers them. omimed commits
        // synchronously; answer like libX11 does, once the commit is handled.
        client.send_req(Request::SyncReply {
            input_method_id,
            input_context_id,
        })
    }

    fn handle_forward_event(
        &mut self,
        _client: &mut C,
        _input_method_id: u16,
        _input_context_id: u16,
        _flag: ForwardEventFlag,
        xev: KeyPressEvent,
    ) -> Result<(), ClientError> {
        self.press(&xev);
        Ok(())
    }

    fn handle_preedit_start(
        &mut self,
        _client: &mut C,
        _input_method_id: u16,
        _input_context_id: u16,
    ) -> Result<(), ClientError> {
        self.preedit.clear();
        Ok(())
    }

    fn handle_preedit_draw(
        &mut self,
        _client: &mut C,
        _input_method_id: u16,
        _input_context_id: u16,
        _caret: i32,
        chg_first: i32,
        chg_len: i32,
        _status: xim::PreeditDrawStatus,
        preedit_string: &str,
        _feedbacks: Vec<xim::Feedback>,
    ) -> Result<(), ClientError> {
        self.draw(chg_first, chg_len, preedit_string);
        Ok(())
    }

    fn handle_preedit_done(
        &mut self,
        _client: &mut C,
        _input_method_id: u16,
        _input_context_id: u16,
    ) -> Result<(), ClientError> {
        self.preedit.clear();
        Ok(())
    }
}
