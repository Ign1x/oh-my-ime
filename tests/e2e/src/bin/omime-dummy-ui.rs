//! `omime-dummy-ui` — a minimal engine UI for exercising the UI bridge.
//!
//! It opens one frameless toplevel filled with a color and prints what happens to
//! stdout, one event per line:
//!
//! ```text
//! configured <width> <height>
//! pointer-enter <x> <y>
//! pointer-button <button> <pressed|released>
//! pointer-leave
//! ```
//!
//! With `--misbehave <kind>` it sends an invalid request a moment after mapping, to
//! check that the daemon survives broken engines; `--misbehave-once <file>` only does
//! so if `file` does not exist yet (and creates it).
//!
//! When started by omimed (`OMIME_ENGINE_ADDRESS` is set) it also registers as an
//! engine that passes every key through, after `--slow-ms` milliseconds if given. It
//! asks for its window to be shown when a text field is focused, unless `--hidden`.
//! With `--surrounding`, keys report the surrounding length and offsets as preedit;
//! Backspace also requests deletion of three bytes before the cursor. Activation and
//! updates log the synthetic test context so privacy tests see every delivered buffer.

use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use omime_e2e::{parse_color, shm_file};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_subcompositor, wl_subsurface, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const USAGE: &str = "\
Usage: omime-dummy-ui [--color RRGGBB] [--size WxH] [--misbehave shm|viewport|subsurface]
                      [--misbehave-once FILE] [--slow-ms N] [--slow-activate-ms N] [--hidden]
                      [--input-methods ID,ID...] [--properties] [--surrounding]";

/// An engine that consumes nothing, optionally slowly.
struct PassThrough {
    slow: Duration,
    slow_activate: Duration,
    /// Whether to ask for the window to be shown.
    show: bool,
    /// Input methods of its own, as a Rime engine has schemas.
    input_methods: Vec<omime_sdk::InputMethodInfo>,
    /// The one in use.
    current: String,
    /// A menu (`--properties`): a switch, which clicks flip, a choice in a submenu and
    /// an action.
    properties: Vec<omime_sdk::Property>,
    surrounding: bool,
}

impl omime_sdk::Engine for PassThrough {
    fn registered(&mut self, host: &omime_sdk::Host) {
        if !self.input_methods.is_empty() {
            host.set_input_methods(&self.input_methods, self.current.as_str());
        }
        if !self.properties.is_empty() {
            host.set_properties(&self.properties);
        }
    }

    fn activate_property(&mut self, host: &omime_sdk::Host, id: &str) {
        println!("activated {id}");
        let Some(activated) = self.properties.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        for property in &mut self.properties {
            match activated.kind {
                omime_sdk::PropertyKind::Toggle if property.id == id => {
                    property.checked = !property.checked;
                }
                omime_sdk::PropertyKind::Radio if property.parent == activated.parent => {
                    property.checked = property.id == id;
                }
                _ => {}
            }
        }
        host.set_properties(&self.properties);
    }

    fn select_input_method(&mut self, host: &omime_sdk::Host, id: &str) {
        // Into omimed's log, for tests to read.
        println!("selected {id}");
        self.current = id.to_owned();
        host.set_current_input_method(id);
    }

    fn activate(&mut self, ctx: &mut omime_sdk::Context<'_>) {
        // Into omimed's log, for tests to read.
        let info = ctx.info();
        println!(
            "activate frontend={} client-preedit={}",
            info.frontend.as_deref().unwrap_or("?"),
            info.client_preedit
        );
        self.update(ctx);
        std::thread::sleep(self.slow_activate);
        if self.show {
            ctx.host().set_popup_visible(true);
        }
    }

    fn update(&mut self, ctx: &mut omime_sdk::Context<'_>) {
        if self.surrounding {
            println!("surrounding-context {:?}", ctx.info().surrounding);
        }
    }

    fn process_key(&mut self, ctx: &mut omime_sdk::Context<'_>, key: &omime_sdk::KeyEvent) -> bool {
        if self.surrounding && !key.released {
            let report = ctx.info().surrounding.as_ref().map_or_else(
                || "none".to_owned(),
                |(text, cursor, anchor)| format!("{}:{cursor}:{anchor}", text.len()),
            );
            ctx.set_preedit(omime_sdk::Preedit::with_cursor_at_end(report));
            if key.keysym == 0xff08 {
                ctx.delete_surrounding(3, 0);
            }
            return true;
        }
        if !self.slow_activate.is_zero() {
            // Count requests without logging their contents.
            println!("processed key");
        }
        std::thread::sleep(self.slow);
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Misbehave {
    /// wl_shm.create_pool with size 0 (the bridge is expected to catch this).
    Shm,
    /// wp_viewport.set_destination(-5, -5) (the bridge must reject it).
    Viewport,
    /// A surface cannot be its own subsurface parent; exercises upstream errors.
    Subsurface,
}

#[derive(Default)]
struct Globals {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
}

struct App {
    globals: Globals,
    color: u32,
    size: (i32, i32),
    surface: Option<wl_surface::WlSurface>,
    pointer: Option<wl_pointer::WlPointer>,
    configured: bool,
    mapped_at: Option<Instant>,
    closed: bool,
}

impl App {
    fn draw(&mut self, qh: &QueueHandle<Self>) -> anyhow::Result<()> {
        let (Some(surface), Some(shm)) = (&self.surface, &self.globals.shm) else {
            return Ok(());
        };
        let (w, h) = self.size;
        let stride = w * 4;
        let size = (stride * h) as usize;
        let color = self.color;
        let border = color & 0xff7f_7f7f;
        let fd = shm_file(size, |i| {
            let (x, y) = ((i as i32) % w, (i as i32) / w);
            if x < 3 || y < 3 || x >= w - 3 || y >= h - 3 {
                border
            } else {
                color
            }
        })?;
        let pool = shm.create_pool(std::os::fd::AsFd::as_fd(&fd), size as i32, qh, ());
        let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, qh, ());
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
            let g = &mut app.globals;
            match interface.as_str() {
                "wl_compositor" => g.compositor = Some(registry.bind(name, version.min(4), qh, ())),
                "wl_shm" => g.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_subcompositor" => g.subcompositor = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => g.wm_base = Some(registry.bind(name, version.min(5), qh, ())),
                "wl_seat" => g.seat = Some(registry.bind(name, version.min(5), qh, ())),
                "wp_viewporter" => g.viewporter = Some(registry.bind(name, 1, qh, ())),
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
            println!("configured {} {}", app.size.0, app.size.1);
            if !app.configured {
                app.configured = true;
                app.mapped_at = Some(Instant::now());
            }
            if let Err(e) = app.draw(qh) {
                eprintln!("draw failed: {e:#}");
            }
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for App {
    fn event(
        app: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = event {
            app.closed = true;
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        app: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Keyboard) {
                println!("warning: seat advertises a keyboard");
            }
            if caps.contains(wl_seat::Capability::Pointer) && app.pointer.is_none() {
                app.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => println!("pointer-enter {surface_x:.0} {surface_y:.0}"),
            wl_pointer::Event::Leave { .. } => println!("pointer-leave"),
            wl_pointer::Event::Button { button, state, .. } => {
                let state = match state {
                    WEnum::Value(wl_pointer::ButtonState::Pressed) => "pressed",
                    _ => "released",
                };
                println!("pointer-button {button} {state}");
            }
            _ => {}
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
delegate_noop!(App: ignore wl_callback::WlCallback);
delegate_noop!(App: wp_viewporter::WpViewporter);
delegate_noop!(App: wp_viewport::WpViewport);
delegate_noop!(App: wl_subcompositor::WlSubcompositor);
delegate_noop!(App: wl_subsurface::WlSubsurface);

fn main() -> anyhow::Result<()> {
    let mut color = 0xffff_cc00;
    let mut size = (240, 80);
    let mut misbehave = None;
    let mut once: Option<std::path::PathBuf> = None;
    let mut slow = Duration::ZERO;
    let mut slow_activate = Duration::ZERO;
    let mut show = true;
    // Input methods of its own, as a Rime engine has schemas; the first is in use.
    let mut input_methods: Vec<omime_sdk::InputMethodInfo> = Vec::new();
    let mut properties = Vec::new();
    let mut surrounding = false;
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
            "--misbehave" => {
                misbehave = Some(match value()?.as_str() {
                    "shm" => Misbehave::Shm,
                    "viewport" => Misbehave::Viewport,
                    "subsurface" => Misbehave::Subsurface,
                    other => bail!("unknown misbehaviour `{other}`"),
                });
            }
            "--misbehave-once" => once = Some(value()?.into()),
            "--slow-ms" => slow = Duration::from_millis(value()?.parse()?),
            "--slow-activate-ms" => {
                slow_activate = Duration::from_millis(value()?.parse()?);
            }
            "--hidden" => show = false,
            "--surrounding" => surrounding = true,
            "--properties" => {
                properties = vec![
                    omime_sdk::Property {
                        id: "punct".into(),
                        kind: omime_sdk::PropertyKind::Toggle,
                        label: "Chinese punctuation".into(),
                        checked: true,
                        ..omime_sdk::Property::default()
                    },
                    omime_sdk::Property {
                        id: "width".into(),
                        kind: omime_sdk::PropertyKind::Menu,
                        label: "Width".into(),
                        ..omime_sdk::Property::default()
                    },
                    omime_sdk::Property {
                        id: "full".into(),
                        parent: "width".into(),
                        kind: omime_sdk::PropertyKind::Radio,
                        label: "Full width".into(),
                        ..omime_sdk::Property::default()
                    },
                    omime_sdk::Property {
                        id: "half".into(),
                        parent: "width".into(),
                        kind: omime_sdk::PropertyKind::Radio,
                        label: "Half width".into(),
                        checked: true,
                        ..omime_sdk::Property::default()
                    },
                    omime_sdk::Property {
                        id: "hello".into(),
                        label: "Say hello".into(),
                        ..omime_sdk::Property::default()
                    },
                ];
            }
            "--input-methods" => {
                input_methods = value()?
                    .split(',')
                    .map(|id| omime_sdk::InputMethodInfo {
                        id: id.to_owned(),
                        name: id.to_uppercase(),
                        ..omime_sdk::InputMethodInfo::default()
                    })
                    .collect();
            }
            _ => bail!("{USAGE}"),
        }
    }
    if let Some(marker) = &once {
        if marker.exists() {
            misbehave = None;
        } else {
            std::fs::write(marker, b"")?;
        }
    }
    // Keep the engine connection alive for as long as the window exists.
    let _engine = if std::env::var_os(omime_sdk::ENGINE_ADDRESS_ENV).is_some() {
        let current = input_methods
            .first()
            .map(|m| m.id.clone())
            .unwrap_or_default();
        Some(omime_sdk::spawn(PassThrough {
            slow,
            slow_activate,
            show,
            input_methods,
            current,
            properties,
            surrounding,
        })?)
    } else {
        None
    };

    let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut app = App {
        globals: Globals::default(),
        color,
        size,
        surface: None,
        pointer: None,
        configured: false,
        mapped_at: None,
        closed: false,
    };
    queue.roundtrip(&mut app)?;

    let (Some(compositor), Some(wm_base)) = (&app.globals.compositor, &app.globals.wm_base) else {
        bail!("wl_compositor or xdg_wm_base missing");
    };
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("omime dummy ui".into());
    surface.commit();
    app.surface = Some(surface);

    let mut misbehaved = false;
    while !app.closed {
        if let (Some(kind), Some(at), false) = (misbehave, app.mapped_at, misbehaved)
            && at.elapsed() > Duration::from_millis(500)
        {
            misbehaved = true;
            println!("misbehaving: {kind:?}");
            match kind {
                Misbehave::Shm => {
                    if let Some(shm) = &app.globals.shm {
                        let fd = shm_file(4096, |_| 0)?;
                        shm.create_pool(std::os::fd::AsFd::as_fd(&fd), 0, &qh, ());
                    }
                }
                Misbehave::Viewport => {
                    if let (Some(viewporter), Some(surface)) =
                        (&app.globals.viewporter, &app.surface)
                    {
                        viewporter
                            .get_viewport(surface, &qh, ())
                            .set_destination(-5, -5);
                    }
                }
                Misbehave::Subsurface => {
                    if let (Some(subcompositor), Some(surface)) =
                        (&app.globals.subcompositor, &app.surface)
                    {
                        subcompositor.get_subsurface(surface, surface, &qh, ());
                    }
                }
            }
        }
        conn.flush()?;
        let timeout_ms: i64 = if misbehave.is_some() && !misbehaved {
            100
        } else {
            1000
        };
        if let Some(guard) = queue.prepare_read() {
            let fd = guard.connection_fd();
            let mut fds = [rustix::event::PollFd::new(
                &fd,
                rustix::event::PollFlags::IN,
            )];
            let ts = rustix::time::Timespec {
                tv_sec: timeout_ms / 1000,
                tv_nsec: (timeout_ms % 1000) * 1_000_000,
            };
            if rustix::event::poll(&mut fds, Some(&ts))? > 0 {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e))
                        if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        queue.dispatch_pending(&mut app)?;
    }
    drop(toplevel);
    Ok(())
}
