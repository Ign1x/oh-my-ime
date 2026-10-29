//! `omime-keyboard` — a virtual keyboard that stays until it is killed.
//!
//! The test compositor has no physical keyboard. A seat without any keyboard loses its
//! keyboard capability, and wlroots then clears the keyboard focus: that happens
//! whenever omimed stops while its virtual keyboard is the only one, and the focused
//! window never gets keys again. This keyboard plays the physical one: it never types,
//! it only exists.

use std::io::Write as _;
use std::os::fd::AsFd as _;

use anyhow::Context as _;
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use xkbcommon::xkb;

/// `WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1`
const KEYMAP_FORMAT_XKB_V1: u32 = 1;

#[derive(Default)]
struct App {
    seat: Option<wl_seat::WlSeat>,
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
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "wl_seat" if app.seat.is_none() => app.seat = Some(registry.bind(name, 1, qh, ())),
                "zwp_virtual_keyboard_manager_v1" => {
                    app.manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

delegate_noop!(App: ignore wl_seat::WlSeat);
delegate_noop!(App: ZwpVirtualKeyboardManagerV1);
delegate_noop!(App: ZwpVirtualKeyboardV1);

fn main() -> anyhow::Result<()> {
    let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut app = App::default();
    queue.roundtrip(&mut app)?;
    let (Some(seat), Some(manager)) = (&app.seat, &app.manager) else {
        anyhow::bail!("the compositor has no seat or no zwp_virtual_keyboard_manager_v1");
    };

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

    let keyboard = manager.create_virtual_keyboard(seat, &qh, ());
    keyboard.keymap(KEYMAP_FORMAT_XKB_V1, file.as_fd(), keymap.len() as u32 + 1);
    queue.roundtrip(&mut app)?;
    println!("ready");

    loop {
        queue.blocking_dispatch(&mut app)?;
    }
}
