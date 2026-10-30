//! `omime-im-client` — a tiny Wayland input method, hosted by omimed in the end-to-end
//! tests as Fcitx5 would be.
//!
//! It speaks `zwp_input_method_v2` and `zwp_virtual_keyboard_v1` like the echo engine
//! speaks the engine protocol: letters are collected in a preedit and shown in a
//! popup of one color; any other key commits the word in UPPERCASE and goes on to the
//! application through the virtual keyboard, its release too unless `--keep-releases`.
//! Backspace edits the preedit and Escape cancels it. Like Fcitx5, it grabs the keyboard and creates a virtual keyboard
//! whenever it is activated, and destroys its popup surface whenever the preedit is
//! empty.
//!
//! It prints what happens to stdout, one event per line, and with `--log FILE` also to
//! FILE (relative to `$XDG_RUNTIME_DIR`), since omimed does not capture its output:
//!
//! ```text
//! ready
//! activate
//! deactivate
//! repeat <rate> <delay>
//! key <keysym name> pressed|released
//! rect <x> <y> <width> <height>
//! ```

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context as _, bail};
use omime_e2e::{parse_color, shm_file};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
    zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use xkbcommon::xkb;

const USAGE: &str =
    "Usage: omime-im-client [--color RRGGBB] [--size WxH] [--log FILE] [--keep-releases]";

/// Where `--log` sends the output.
static LOG: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

/// Reports an event.
fn say(line: &str) {
    println!("{line}");
    if let Some(file) = LOG.get() {
        let mut file = file.lock().unwrap_or_else(|p| p.into_inner());
        let _ = writeln!(file, "{line}");
    }
}

/// `wl_keyboard.key_state`
const RELEASED: u32 = 0;
const PRESSED: u32 = 1;

/// Keysyms used below (see xkbcommon-keysyms.h).
mod keysym {
    pub const BACKSPACE: u32 = 0xff08;
    pub const ESCAPE: u32 = 0xff1b;
}

/// The input method of one seat.
#[derive(Default)]
struct Seat {
    seat: Option<wl_seat::WlSeat>,
    im: Option<ZwpInputMethodV2>,
    grab: Option<ZwpInputMethodKeyboardGrabV2>,
    vk: Option<ZwpVirtualKeyboardV1>,
    /// Whether the virtual keyboard got its keymap.
    vk_ready: bool,
    xkb: Option<xkb::State>,
    pending_activate: bool,
    pending_deactivate: bool,
    /// `done` events received, the serial of commits.
    serial: u32,
    word: String,
    /// Keys forwarded to the application, whose releases go there too.
    forwarded: Vec<u32>,
    popup: Option<(wl_surface::WlSurface, ZwpInputPopupSurfaceV2)>,
}

#[derive(Default)]
struct App {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    im_manager: Option<ZwpInputMethodManagerV2>,
    vk_manager: Option<ZwpVirtualKeyboardManagerV1>,
    seats: Vec<Seat>,
    color: u32,
    size: (i32, i32),
    /// Never forward releases (`--keep-releases`), as input methods do for keys of a
    /// hotkey.
    keep_releases: bool,
}

impl App {
    fn on_done(&mut self, index: usize, qh: &QueueHandle<Self>) {
        let vk_manager = self.vk_manager.clone();
        let seat = &mut self.seats[index];
        seat.serial += 1;
        if std::mem::take(&mut seat.pending_deactivate) {
            say("deactivate");
            if let Some(grab) = seat.grab.take() {
                grab.release();
            }
            if let Some(vk) = seat.vk.take() {
                vk.destroy();
            }
            seat.vk_ready = false;
            seat.word.clear();
            seat.forwarded.clear();
        }
        if std::mem::take(&mut seat.pending_activate) {
            say("activate");
            seat.word.clear();
            if let (Some(im), Some(vk_manager), Some(wl_seat)) = (&seat.im, &vk_manager, &seat.seat)
            {
                if let Some(grab) = seat.grab.take() {
                    grab.release();
                }
                seat.grab = Some(im.grab_keyboard(qh, index));
                if seat.vk.is_none() {
                    seat.vk = Some(vk_manager.create_virtual_keyboard(wl_seat, qh, ()));
                    seat.vk_ready = false;
                }
            }
        }
        // Answer every done with a commit, as Fcitx5 does.
        self.show(index, qh);
    }

    /// Sends the preedit and shows or hides the popup.
    fn show(&mut self, index: usize, qh: &QueueHandle<Self>) {
        let seat = &mut self.seats[index];
        let Some(im) = &seat.im else {
            return;
        };
        if !seat.word.is_empty() {
            let end = seat.word.len() as i32;
            im.set_preedit_string(seat.word.clone(), end, end);
        }
        im.commit(seat.serial);
        if seat.word.is_empty() {
            if let Some((surface, popup)) = seat.popup.take() {
                // Role first, then the surface, as Fcitx5 does.
                popup.destroy();
                surface.destroy();
            }
        } else if seat.popup.is_none()
            && let Err(e) = self.open_popup(index, qh)
        {
            eprintln!("cannot open the popup: {e:#}");
        }
    }

    fn open_popup(&mut self, index: usize, qh: &QueueHandle<Self>) -> anyhow::Result<()> {
        let (Some(compositor), Some(shm)) = (&self.compositor, &self.shm) else {
            bail!("wl_compositor or wl_shm missing");
        };
        let seat = &mut self.seats[index];
        let Some(im) = &seat.im else {
            return Ok(());
        };
        let surface = compositor.create_surface(qh, ());
        let popup = im.get_input_popup_surface(&surface, qh, ());
        let (w, h) = self.size;
        let stride = w * 4;
        let size = (stride * h) as usize;
        let color = self.color;
        let fd = shm_file(size, |_| color)?;
        let pool = shm.create_pool(fd.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w, h);
        surface.commit();
        seat.popup = Some((surface, popup));
        Ok(())
    }

    fn on_key(&mut self, index: usize, time: u32, key: u32, state: u32, qh: &QueueHandle<Self>) {
        let seat = &mut self.seats[index];
        let Some(xkb) = &seat.xkb else {
            return;
        };
        let code = xkb::Keycode::new(key + 8);
        let sym = xkb.key_get_one_sym(code);
        let pressed = state != RELEASED;
        say(&format!(
            "key {} {}",
            xkb::keysym_get_name(sym),
            if pressed { "pressed" } else { "released" }
        ));
        if !pressed {
            if let Some(i) = seat.forwarded.iter().position(|k| *k == key) {
                seat.forwarded.remove(i);
                if !self.keep_releases {
                    Self::forward(seat, time, key, RELEASED);
                }
            }
            return;
        }
        let shortcut = [xkb::MOD_NAME_CTRL, xkb::MOD_NAME_ALT, xkb::MOD_NAME_LOGO]
            .iter()
            .any(|m| xkb.mod_name_is_active(m, xkb::STATE_MODS_EFFECTIVE));
        let text = xkb.key_get_utf8(code);
        let letter = text
            .chars()
            .next()
            .filter(|c| text.chars().count() == 1 && c.is_alphabetic() && !shortcut);
        match (letter, sym.raw()) {
            (Some(c), _) => seat.word.push(c),
            (_, keysym::BACKSPACE) if !seat.word.is_empty() => {
                seat.word.pop();
            }
            (_, keysym::ESCAPE) if !seat.word.is_empty() => seat.word.clear(),
            _ => {
                if !seat.word.is_empty()
                    && let Some(im) = &seat.im
                {
                    im.commit_string(std::mem::take(&mut seat.word).to_uppercase());
                    im.commit(seat.serial);
                }
                seat.forwarded.push(key);
                Self::forward(seat, time, key, PRESSED);
            }
        }
        self.show(index, qh);
    }

    fn forward(seat: &Seat, time: u32, key: u32, state: u32) {
        if let (Some(vk), true) = (&seat.vk, seat.vk_ready) {
            vk.key(time, key, state);
        }
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
                "wl_seat" => {
                    let seat = registry.bind(name, version.min(5), qh, ());
                    app.seats.push(Seat {
                        seat: Some(seat),
                        ..Seat::default()
                    });
                }
                "zwp_input_method_manager_v2" => {
                    app.im_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    app.vk_manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwpInputMethodV2, usize> for App {
    fn event(
        app: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        index: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let seat = &mut app.seats[*index];
        match event {
            zwp_input_method_v2::Event::Activate => {
                seat.pending_activate = true;
                seat.pending_deactivate = false;
            }
            zwp_input_method_v2::Event::Deactivate => seat.pending_deactivate = true,
            zwp_input_method_v2::Event::Done => app.on_done(*index, qh),
            zwp_input_method_v2::Event::Unavailable => {
                say("unavailable");
                seat.im = None;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, usize> for App {
    fn event(
        app: &mut Self,
        _: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        index: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_keyboard_grab_v2::Event::Keymap { format, fd, size } => {
                let seat = &mut app.seats[*index];
                let mut bytes = vec![0u8; size as usize];
                let mut read = 0;
                while read < bytes.len() {
                    match rustix::io::pread(&fd, &mut bytes[read..], read as u64) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => read += n,
                    }
                }
                bytes.truncate(read);
                while bytes.last() == Some(&0) {
                    bytes.pop();
                }
                let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                let keymap = String::from_utf8(bytes).ok().and_then(|text| {
                    xkb::Keymap::new_from_string(
                        &context,
                        text,
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::KEYMAP_COMPILE_NO_FLAGS,
                    )
                });
                seat.xkb = keymap.as_ref().map(xkb::State::new);
                // Hand the same keymap on, as Fcitx5 does.
                if let (Some(vk), WEnum::Value(format)) = (&seat.vk, format) {
                    vk.keymap(format as u32, fd.as_fd(), size);
                    seat.vk_ready = true;
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::Key {
                time, key, state, ..
            } => {
                let state = match state {
                    WEnum::Value(state) => state as u32,
                    WEnum::Unknown(state) => state,
                };
                app.on_key(*index, time, key, state, qh);
            }
            zwp_input_method_keyboard_grab_v2::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                let seat = &mut app.seats[*index];
                if let Some(xkb) = &mut seat.xkb {
                    xkb.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
                if let (Some(vk), true) = (&seat.vk, seat.vk_ready) {
                    vk.modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::RepeatInfo { rate, delay } => {
                say(&format!("repeat {rate} {delay}"));
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for App {
    fn event(
        _: &mut Self,
        _: &ZwpInputPopupSurfaceV2,
        event: zwp_input_popup_surface_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_popup_surface_v2::Event::TextInputRectangle {
            x,
            y,
            width,
            height,
        } = event
        {
            say(&format!("rect {x} {y} {width} {height}"));
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for App {
    fn event(
        _: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}

delegate_noop!(App: wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wl_seat::WlSeat);
delegate_noop!(App: ZwpInputMethodManagerV2);
delegate_noop!(App: ZwpVirtualKeyboardManagerV1);
delegate_noop!(App: ZwpVirtualKeyboardV1);

fn main() -> anyhow::Result<()> {
    let mut color = 0xffff_00ff;
    let mut size = (120, 40);
    let mut keep_releases = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().context(USAGE);
        match arg.as_str() {
            "--color" => color = parse_color(&value()?)?,
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').context(USAGE)?;
                size = (w.parse()?, h.parse()?);
            }
            "--keep-releases" => keep_releases = true,
            "--log" => {
                let dir =
                    std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is unset")?;
                let path = std::path::Path::new(&dir).join(value()?);
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .with_context(|| format!("cannot open {}", path.display()))?;
                let _ = LOG.set(Mutex::new(file));
            }
            _ => bail!("{USAGE}"),
        }
    }

    let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut app = App {
        color,
        size,
        keep_releases,
        ..App::default()
    };
    queue.roundtrip(&mut app)?;
    let Some(im_manager) = app.im_manager.clone() else {
        bail!("no zwp_input_method_manager_v2: not started by omimed as a hosted input method");
    };
    if app.vk_manager.is_none() {
        bail!("no zwp_virtual_keyboard_manager_v1");
    }
    for index in 0..app.seats.len() {
        if let Some(seat) = &app.seats[index].seat {
            app.seats[index].im = Some(im_manager.get_input_method(seat, &qh, index));
        }
    }
    queue.roundtrip(&mut app)?;
    say("ready");
    loop {
        queue.blocking_dispatch(&mut app)?;
    }
}
