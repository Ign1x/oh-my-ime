//! The compositor's outputs: their names and logical geometry (`zxdg_output_v1`), which
//! place engine windows for X11 applications, and the `wl_output` objects layer surfaces
//! are created on.

use std::collections::BTreeMap;
use std::rc::Rc;

use wl_proxy::object::ObjectCoreApi;
use wl_proxy::protocols::wayland::wl_output::{WlOutput, WlOutputHandler};
use wl_proxy::protocols::xdg_output_unstable_v1::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wl_proxy::protocols::xdg_output_unstable_v1::zxdg_output_v1::{
    ZxdgOutputV1, ZxdgOutputV1Handler,
};

use super::{Event, EventQueue};
use crate::placement::{Monitor, Rect};

/// `wl_output` version 4 has names.
pub const OUTPUT_VERSION: u32 = 4;
/// `zxdg_output_manager_v1` version 2 has names too.
pub const XDG_OUTPUT_VERSION: u32 = 3;

/// What the output objects report.
#[derive(Debug)]
pub enum OutputEvent {
    Name {
        global: u32,
        name: String,
    },
    Position {
        global: u32,
        x: i32,
        y: i32,
    },
    Size {
        global: u32,
        width: i32,
        height: i32,
    },
}

#[derive(Debug)]
struct Output {
    object: Rc<WlOutput>,
    xdg: Option<Rc<ZxdgOutputV1>>,
    name: Option<String>,
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
}

/// The outputs, by registry name.
#[derive(Debug, Default)]
pub struct OutputSet {
    outputs: BTreeMap<u32, Output>,
    manager: Option<Rc<ZxdgOutputManagerV1>>,
}

impl OutputSet {
    pub fn add_output(&mut self, global: u32, object: Rc<WlOutput>, queue: &EventQueue) {
        object.set_forward_to_client(false);
        object.set_handler(OutputEvents {
            global,
            queue: queue.clone(),
        });
        let mut output = Output {
            object,
            xdg: None,
            name: None,
            position: None,
            size: None,
        };
        if let Some(manager) = &self.manager {
            output.xdg = Some(xdg_output(manager, &output.object, global, queue));
        }
        self.outputs.insert(global, output);
    }

    pub fn set_manager(&mut self, manager: Rc<ZxdgOutputManagerV1>, queue: &EventQueue) {
        manager.set_forward_to_client(false);
        for (global, output) in &mut self.outputs {
            if output.xdg.is_none() {
                output.xdg = Some(xdg_output(&manager, &output.object, *global, queue));
            }
        }
        self.manager = Some(manager);
    }

    /// Forgets the output or manager with the registry name `global`.
    pub fn remove(&mut self, global: u32) {
        if let Some(output) = self.outputs.remove(&global) {
            if let Some(xdg) = output.xdg {
                xdg.send_destroy();
            }
            if output.object.version() >= 3 {
                output.object.send_release();
            }
        }
    }

    pub fn handle(&mut self, event: OutputEvent) {
        match event {
            OutputEvent::Name { global, name } => {
                if let Some(output) = self.outputs.get_mut(&global) {
                    output.name = Some(name);
                }
            }
            OutputEvent::Position { global, x, y } => {
                if let Some(output) = self.outputs.get_mut(&global) {
                    output.position = Some((x, y));
                }
            }
            OutputEvent::Size {
                global,
                width,
                height,
            } => {
                if let Some(output) = self.outputs.get_mut(&global) {
                    output.size = Some((width, height));
                }
            }
        }
    }

    /// The outputs whose name and logical geometry are known.
    pub fn monitors(&self) -> Vec<Monitor> {
        self.outputs
            .values()
            .filter_map(|output| {
                let ((x, y), (w, h)) = (output.position?, output.size?);
                Some(Monitor {
                    name: output.name.clone()?,
                    rect: (x, y, w, h),
                })
            })
            .collect()
    }

    /// The output object and logical geometry of the output called `name`.
    pub fn find(&self, name: &str) -> Option<(Rc<WlOutput>, Rect)> {
        self.outputs.values().find_map(|output| {
            let ((x, y), (w, h)) = (output.position?, output.size?);
            (output.name.as_deref() == Some(name)).then(|| (output.object.clone(), (x, y, w, h)))
        })
    }
}

fn xdg_output(
    manager: &Rc<ZxdgOutputManagerV1>,
    output: &Rc<WlOutput>,
    global: u32,
    queue: &EventQueue,
) -> Rc<ZxdgOutputV1> {
    let xdg = manager.new_send_get_xdg_output(output);
    xdg.set_forward_to_client(false);
    xdg.set_handler(XdgOutputEvents {
        global,
        queue: queue.clone(),
    });
    xdg
}

struct OutputEvents {
    global: u32,
    queue: EventQueue,
}

impl WlOutputHandler for OutputEvents {
    fn handle_name(&mut self, _slf: &Rc<WlOutput>, name: &str) {
        let event = OutputEvent::Name {
            global: self.global,
            name: name.to_owned(),
        };
        self.queue.borrow_mut().push_back(Event::Output(event));
    }
}

struct XdgOutputEvents {
    global: u32,
    queue: EventQueue,
}

impl XdgOutputEvents {
    fn push(&self, event: OutputEvent) {
        self.queue.borrow_mut().push_back(Event::Output(event));
    }
}

impl ZxdgOutputV1Handler for XdgOutputEvents {
    fn handle_logical_position(&mut self, _slf: &Rc<ZxdgOutputV1>, x: i32, y: i32) {
        self.push(OutputEvent::Position {
            global: self.global,
            x,
            y,
        });
    }

    fn handle_logical_size(&mut self, _slf: &Rc<ZxdgOutputV1>, width: i32, height: i32) {
        self.push(OutputEvent::Size {
            global: self.global,
            width,
            height,
        });
    }

    fn handle_name(&mut self, _slf: &Rc<ZxdgOutputV1>, name: &str) {
        self.push(OutputEvent::Name {
            global: self.global,
            name: name.to_owned(),
        });
    }
}
