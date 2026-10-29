//! `omime-pointer` — move a virtual pointer to a point on an output and click.
//!
//! ```text
//! omime-pointer <OUTPUT> <X> <Y> [left|right|middle]
//! ```
//!
//! Unlike one-shot tools, the virtual pointer lives for the whole sequence, so the
//! seat keeps a pointer between the motion and the click and the client under the
//! cursor receives `enter` before the button.

use std::time::Duration;

use anyhow::{Context as _, bail};
use wayland_client::protocol::{wl_output, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
};

#[derive(Default)]
struct App {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1>,
    outputs: Vec<Output>,
}

struct Output {
    output: wl_output::WlOutput,
    name: String,
    size: (u32, u32),
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
                "wl_seat" if app.seat.is_none() => app.seat = Some(registry.bind(name, 1, qh, ())),
                "zwlr_virtual_pointer_manager_v1" => {
                    app.manager = Some(registry.bind(name, version.min(2), qh, ()));
                }
                "wl_output" if version >= 4 => {
                    let index = app.outputs.len();
                    app.outputs.push(Output {
                        output: registry.bind(name, 4, qh, index),
                        name: String::new(),
                        size: (0, 0),
                    });
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, usize> for App {
    fn event(
        app: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let output = &mut app.outputs[*index];
        match event {
            wl_output::Event::Name { name } => output.name = name,
            wl_output::Event::Mode { width, height, .. } => {
                output.size = (width.max(0) as u32, height.max(0) as u32);
            }
            _ => {}
        }
    }
}

delegate_noop!(App: ignore wl_seat::WlSeat);
delegate_noop!(App: zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1);
delegate_noop!(App: zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1);

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (output_name, x, y, button) = match args.as_slice() {
        [o, x, y] => (o, x.parse::<u32>()?, y.parse::<u32>()?, None),
        [o, x, y, b] => (o, x.parse()?, y.parse()?, Some(b.as_str())),
        _ => bail!("usage: omime-pointer <OUTPUT> <X> <Y> [left|right|middle]"),
    };
    let button = match button {
        None => None,
        Some("left") => Some(BTN_LEFT),
        Some("right") => Some(BTN_RIGHT),
        Some("middle") => Some(BTN_MIDDLE),
        Some(other) => bail!("unknown button `{other}`"),
    };

    let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut app = App::default();
    queue.roundtrip(&mut app)?;
    queue.roundtrip(&mut app)?;
    let manager = app
        .manager
        .clone()
        .context("no zwlr_virtual_pointer_manager_v1")?;
    let output = app
        .outputs
        .iter()
        .find(|o| o.name == *output_name)
        .with_context(|| format!("no output named {output_name}"))?;
    let (width, height) = output.size;
    let pointer = manager.create_virtual_pointer_with_output(
        app.seat.as_ref(),
        Some(&output.output),
        &qh,
        (),
    );

    let step = |app: &mut App, queue: &mut wayland_client::EventQueue<App>, ms: u64| {
        let _ = queue.roundtrip(app);
        std::thread::sleep(Duration::from_millis(ms));
    };
    // Wiggle so that the compositor computes focus at the final position.
    pointer.motion_absolute(0, x.saturating_sub(1), y, width, height);
    pointer.frame();
    step(&mut app, &mut queue, 50);
    pointer.motion_absolute(1, x, y, width, height);
    pointer.frame();
    step(&mut app, &mut queue, 200);
    if let Some(button) = button {
        pointer.button(
            2,
            button,
            wayland_client::protocol::wl_pointer::ButtonState::Pressed,
        );
        pointer.frame();
        step(&mut app, &mut queue, 60);
        pointer.button(
            3,
            button,
            wayland_client::protocol::wl_pointer::ButtonState::Released,
        );
        pointer.frame();
        step(&mut app, &mut queue, 300);
    }
    pointer.destroy();
    queue.roundtrip(&mut app)?;
    Ok(())
}
