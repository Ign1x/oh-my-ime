//! `omime-keyboard` — virtual keyboards for the end-to-end tests.
//!
//! ```text
//! omime-keyboard [--seat NAME]               # exists until it is killed
//! omime-keyboard [--seat NAME] --type TEXT   # types TEXT and exits
//! ```
//!
//! The test compositor has no physical keyboard. A seat without any keyboard loses its
//! keyboard capability, and wlroots then clears the keyboard focus: that happens
//! whenever omimed stops while its virtual keyboard is the only one, and the focused
//! window never gets keys again. Without `--type`, this keyboard plays the physical
//! one: it never types, it only exists.
//!
//! With `--type`, it types lowercase letters, digits and spaces on the seat given,
//! which wtype cannot do: it always uses the last seat the compositor announces.

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use xkbcommon::xkb;

/// `WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1`
const KEYMAP_FORMAT_XKB_V1: u32 = 1;
const PRESSED: u32 = 1;
const RELEASED: u32 = 0;

#[derive(Default)]
struct App {
    seats: Vec<(wl_seat::WlSeat, String)>,
    manager: Option<ZwpVirtualKeyboardManagerV1>,
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
                "wl_seat" if version >= 2 => {
                    let index = app.seats.len();
                    let seat = registry.bind(name, 2, qh, index);
                    app.seats.push((seat, String::new()));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    app.manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
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
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Name { name } = event
            && let Some(seat) = app.seats.get_mut(*index)
        {
            seat.1 = name;
        }
    }
}

delegate_noop!(App: ZwpVirtualKeyboardManagerV1);
delegate_noop!(App: ZwpVirtualKeyboardV1);

/// The evdev key code that types `c` with the us layout and no modifiers.
fn keycode(c: char) -> Option<u32> {
    const LETTERS: [u32; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17,
        45, 21, 44,
    ];
    match c {
        'a'..='z' => LETTERS.get(c as usize - 'a' as usize).copied(),
        '1'..='9' => Some(c as u32 - '1' as u32 + 2),
        '0' => Some(11),
        ' ' => Some(57),
        _ => None,
    }
}

fn main() -> anyhow::Result<()> {
    let mut seat_name = None;
    let mut text = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seat" => seat_name = Some(args.next().context("--seat NAME")?),
            "--type" => text = Some(args.next().context("--type TEXT")?),
            other => bail!("unknown argument `{other}`"),
        }
    }
    let keys: Vec<u32> = match &text {
        Some(text) => text
            .chars()
            .map(|c| keycode(c).with_context(|| format!("cannot type {c:?}")))
            .collect::<anyhow::Result<_>>()?,
        None => Vec::new(),
    };

    let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut app = App::default();
    queue.roundtrip(&mut app)?;
    // Seat names arrive after the binds.
    queue.roundtrip(&mut app)?;
    let seat = match &seat_name {
        Some(name) => app.seats.iter().find(|(_, n)| n == name),
        None => app.seats.first(),
    }
    .map(|(seat, _)| seat.clone())
    .with_context(|| format!("no seat {}", seat_name.as_deref().unwrap_or("at all")))?;
    let manager = app
        .manager
        .clone()
        .context("the compositor has no zwp_virtual_keyboard_manager_v1")?;

    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_names(
        &context,
        "",
        "pc105",
        "us",
        "",
        None,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .context("cannot compile the us keymap")?
    .get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
    let fd = rustix::fs::memfd_create("omime-keyboard", rustix::fs::MemfdFlags::CLOEXEC)?;
    let mut file = std::fs::File::from(fd);
    file.write_all(keymap.as_bytes())?;
    file.write_all(&[0])?;

    let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
    keyboard.keymap(KEYMAP_FORMAT_XKB_V1, file.as_fd(), keymap.len() as u32 + 1);
    queue.roundtrip(&mut app)?;

    if text.is_none() {
        println!("ready");
        loop {
            queue.blocking_dispatch(&mut app)?;
        }
    }
    let start = Instant::now();
    let now = || u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX);
    for key in keys {
        keyboard.key(now(), key, PRESSED);
        keyboard.key(now(), key, RELEASED);
        queue.roundtrip(&mut app)?;
        std::thread::sleep(Duration::from_millis(5));
    }
    keyboard.destroy();
    queue.roundtrip(&mut app)?;
    Ok(())
}
