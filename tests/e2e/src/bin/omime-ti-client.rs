//! `omime-ti-client` — a minimal text editor for end-to-end tests.
//!
//! It opens a window, enables `zwp_text_input_v3` when focused and keeps a text buffer
//! that receives committed text, deletions and keys typed into it. Everything that
//! happens is printed to stdout, one event per line (strings are JSON-quoted):
//!
//! ```text
//! ready
//! preedit "hel" 3 3
//! commit "HELLO"
//! delete 1 0
//! key " "
//! enter
//! text "HELLO "
//! ```
//!
//! `text` follows every change. Options: `--purpose N` and `--hints N` set the content
//! type (8 = password), `--app-id ID` the window's app id and `--seat NAME` the seat
//! whose keyboard and text input it uses (default: the first one).

use std::io::Write as _;
use std::os::fd::AsFd;

use anyhow::{Context as _, bail};
use omime_e2e::{json_string, shm_file};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3, zwp_text_input_v3,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use xkbcommon::xkb;

#[derive(Default)]
struct Pending {
    preedit: Option<(String, i32, i32)>,
    commit: Option<String>,
    delete: Option<(u32, u32)>,
}

/// A seat the compositor offers.
struct Seat {
    seat: wl_seat::WlSeat,
    name: String,
    has_keyboard: bool,
}

struct App {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    seats: Vec<Seat>,
    /// Index of the seat in use, once known.
    seat: Option<usize>,
    ti_manager: Option<zwp_text_input_manager_v3::ZwpTextInputManagerV3>,
    surface: Option<wl_surface::WlSurface>,
    text_input: Option<zwp_text_input_v3::ZwpTextInputV3>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    xkb_state: Option<xkb::State>,
    purpose: u32,
    hints: u32,
    /// The document: text before and after the cursor are one string with the cursor at
    /// the end; enough for the tests.
    text: String,
    pending: Pending,
    configured: bool,
}

fn emit(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

impl App {
    fn changed(&mut self) {
        emit(&format!("text {}", json_string(&self.text)));
        self.send_state();
    }

    /// Tells the input method what the field looks like now.
    fn send_state(&self) {
        let Some(ti) = &self.text_input else { return };
        let cursor = self.text.len() as i32;
        ti.set_surrounding_text(self.text.clone(), cursor, cursor);
        // Pretend to be a monospace editor: 8 px per character on a 20 px line.
        let column = self.text.chars().count() as i32;
        ti.set_cursor_rectangle(12 + 8 * column, 20, 2, 18);
        ti.set_text_change_cause(zwp_text_input_v3::ChangeCause::InputMethod);
        ti.set_content_type(
            zwp_text_input_v3::ContentHint::from_bits_truncate(self.hints),
            zwp_text_input_v3::ContentPurpose::try_from(self.purpose)
                .unwrap_or(zwp_text_input_v3::ContentPurpose::Normal),
        );
        ti.commit();
    }

    fn draw(&self, qh: &QueueHandle<Self>) -> anyhow::Result<()> {
        let (Some(surface), Some(shm)) = (&self.surface, &self.shm) else {
            return Ok(());
        };
        let (w, h) = (320, 120);
        let fd = shm_file((w * h * 4) as usize, |_| 0xff20_3040)?;
        let pool = shm.create_pool(fd.as_fd(), w * h * 4, qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w, h);
        surface.commit();
        Ok(())
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    app.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => app.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => {
                    let index = app.seats.len();
                    app.seats.push(Seat {
                        seat: registry.bind(name, version.min(5), qh, index),
                        name: String::new(),
                        has_keyboard: false,
                    });
                }
                "zwp_text_input_manager_v3" => {
                    app.ti_manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for App {
    fn event(
        app: &mut Self,
        xdg_surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            if !app.configured {
                app.configured = true;
                if let Err(e) = app.draw(qh) {
                    eprintln!("draw failed: {e:#}");
                }
            }
        }
    }
}

impl App {
    /// Gets the keyboard of the seat in use once it has one.
    fn ensure_keyboard(&mut self, qh: &QueueHandle<Self>) {
        if let Some(seat) = self.seat.and_then(|i| self.seats.get(i))
            && seat.has_keyboard
            && self.keyboard.is_none()
        {
            self.keyboard = Some(seat.seat.get_keyboard(qh, ()));
        }
    }
}

impl Dispatch<wl_seat::WlSeat, usize> for App {
    fn event(
        app: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        index: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let Some(seat) = app.seats.get_mut(*index) else {
            return;
        };
        match event {
            wl_seat::Event::Name { name } => seat.name = name,
            wl_seat::Event::Capabilities {
                capabilities: WEnum::Value(caps),
            } => {
                seat.has_keyboard = caps.contains(wl_seat::Capability::Keyboard);
                app.ensure_keyboard(qh);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap {
                format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                fd,
                size,
            } => {
                let mut buf = vec![0u8; size as usize];
                let mut filled = 0;
                while filled < buf.len() {
                    match rustix::io::pread(&fd, &mut buf[filled..], filled as u64) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => filled += n,
                    }
                }
                buf.truncate(filled);
                while buf.last() == Some(&0) {
                    buf.pop();
                }
                let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                let keymap = String::from_utf8(buf).ok().and_then(|text| {
                    xkb::Keymap::new_from_string(
                        &context,
                        text,
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::KEYMAP_COMPILE_NO_FLAGS,
                    )
                });
                app.xkb_state = keymap.map(|k| xkb::State::new(&k));
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(state) = &mut app.xkb_state {
                    state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => {
                let Some(xkb) = &app.xkb_state else { return };
                let code = xkb::Keycode::new(key + 8);
                let keysym = xkb.key_get_one_sym(code);
                let text = xkb.key_get_utf8(code);
                match keysym.raw() {
                    0xff0d => {
                        emit("enter");
                    }
                    0xff08 => {
                        app.text.pop();
                        emit("key backspace");
                        app.changed();
                    }
                    _ if !text.is_empty() && !text.chars().any(char::is_control) => {
                        emit(&format!("key {}", json_string(&text)));
                        app.text.push_str(&text);
                        app.changed();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<zwp_text_input_v3::ZwpTextInputV3, ()> for App {
    fn event(
        app: &mut Self,
        ti: &zwp_text_input_v3::ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_text_input_v3::Event as E;
        match event {
            E::Enter { .. } => {
                ti.enable();
                app.send_state();
                emit("ready");
            }
            E::Leave { .. } => {
                ti.disable();
                ti.commit();
                emit("left");
            }
            E::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                app.pending.preedit = Some((text.unwrap_or_default(), cursor_begin, cursor_end));
            }
            E::CommitString { text } => app.pending.commit = text,
            E::DeleteSurroundingText {
                before_length,
                after_length,
            } => app.pending.delete = Some((before_length, after_length)),
            E::Done { .. } => {
                let pending = std::mem::take(&mut app.pending);
                let mut changed = false;
                if let Some((before, after)) = pending.delete {
                    emit(&format!("delete {before} {after}"));
                    let keep = app.text.len().saturating_sub(before as usize);
                    app.text.truncate(keep);
                    changed = true;
                }
                if let Some(text) = pending.commit {
                    emit(&format!("commit {}", json_string(&text)));
                    app.text.push_str(&text);
                    changed = true;
                }
                let (preedit, begin, end) = pending.preedit.unwrap_or_default();
                emit(&format!("preedit {} {begin} {end}", json_string(&preedit)));
                if changed {
                    app.changed();
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(App: wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wl_buffer::WlBuffer);
delegate_noop!(App: zwp_text_input_manager_v3::ZwpTextInputManagerV3);
delegate_noop!(App: ignore xdg_toplevel::XdgToplevel);

fn main() -> anyhow::Result<()> {
    let mut purpose = 0;
    let mut hints = 0;
    let mut app_id = "io.github.ign1x.OhMyIme.TestEditor".to_owned();
    let mut seat_name = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--purpose" => purpose = args.next().context("--purpose N")?.parse()?,
            "--hints" => hints = args.next().context("--hints N")?.parse()?,
            "--app-id" => app_id = args.next().context("--app-id ID")?,
            "--seat" => seat_name = Some(args.next().context("--seat NAME")?),
            other => bail!("unknown argument `{other}`"),
        }
    }

    let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut app = App {
        compositor: None,
        shm: None,
        wm_base: None,
        seats: Vec::new(),
        seat: None,
        ti_manager: None,
        surface: None,
        text_input: None,
        keyboard: None,
        xkb_state: None,
        purpose,
        hints,
        text: String::new(),
        pending: Pending::default(),
        configured: false,
    };
    queue.roundtrip(&mut app)?;
    // Seat names arrive after the binds.
    queue.roundtrip(&mut app)?;
    app.seat = match &seat_name {
        Some(name) => app.seats.iter().position(|s| s.name == *name),
        None => (!app.seats.is_empty()).then_some(0),
    };
    app.ensure_keyboard(&qh);
    let seat = app
        .seat
        .and_then(|i| app.seats.get(i))
        .map(|s| s.seat.clone())
        .with_context(|| format!("no seat {}", seat_name.as_deref().unwrap_or("at all")))?;
    let (Some(compositor), Some(wm_base), Some(ti_manager)) =
        (&app.compositor, &app.wm_base, &app.ti_manager)
    else {
        bail!("the compositor lacks a global this client needs");
    };
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("omime test editor".into());
    toplevel.set_app_id(app_id);
    surface.commit();
    app.text_input = Some(ti_manager.get_text_input(&seat, &qh, ()));
    app.surface = Some(surface);

    loop {
        queue.blocking_dispatch(&mut app)?;
    }
}
