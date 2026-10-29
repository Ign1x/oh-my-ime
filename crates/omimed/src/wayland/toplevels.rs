//! Which application has focus, from `zwlr_foreign_toplevel_manager_v1`.
//!
//! The input method protocol does not say which application a text field belongs to.
//! Compositors with the wlr foreign toplevel protocol (Hyprland, sway, ...) tell every
//! client which window is activated and its app id. Window titles are never looked at.

use std::collections::BTreeMap;
use std::rc::Rc;

use wl_proxy::object::ObjectCoreApi;
use wl_proxy::protocols::wlr_foreign_toplevel_management_unstable_v1::zwlr_foreign_toplevel_handle_v1::{
    ZwlrForeignToplevelHandleV1, ZwlrForeignToplevelHandleV1Handler,
    ZwlrForeignToplevelHandleV1State,
};
use wl_proxy::protocols::wlr_foreign_toplevel_management_unstable_v1::zwlr_foreign_toplevel_manager_v1::{
    ZwlrForeignToplevelManagerV1, ZwlrForeignToplevelManagerV1Handler,
};

use super::{Event, EventQueue};

/// The newest protocol version we use.
pub const VERSION: u32 = 3;

/// Identifies a window while it exists.
pub type ToplevelId = u64;

/// Events of the foreign toplevel objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToplevelEvent {
    AppId(ToplevelId, String),
    Activated(ToplevelId, bool),
    /// Applies the window's pending changes.
    Done(ToplevelId),
    Closed(ToplevelId),
    /// The compositor stopped reporting windows.
    Finished,
}

#[derive(Debug, Default)]
struct Toplevel {
    app_id: Option<String>,
    activated: bool,
    pending_app_id: Option<String>,
    pending_activated: Option<bool>,
    /// Orders activations, in case several windows claim to be activated at once.
    activated_at: u64,
}

/// The windows the compositor reported, and which of them is activated.
#[derive(Debug, Default)]
pub struct Toplevels {
    toplevels: BTreeMap<ToplevelId, Toplevel>,
    activations: u64,
}

impl Toplevels {
    pub fn handle(&mut self, event: ToplevelEvent) {
        match event {
            ToplevelEvent::AppId(id, app_id) => {
                self.toplevels.entry(id).or_default().pending_app_id = Some(app_id);
            }
            ToplevelEvent::Activated(id, activated) => {
                self.toplevels.entry(id).or_default().pending_activated = Some(activated);
            }
            ToplevelEvent::Done(id) => {
                let toplevel = self.toplevels.entry(id).or_default();
                if let Some(app_id) = toplevel.pending_app_id.take() {
                    toplevel.app_id = (!app_id.is_empty()).then_some(app_id);
                }
                if let Some(activated) = toplevel.pending_activated.take() {
                    if activated && !toplevel.activated {
                        self.activations += 1;
                        toplevel.activated_at = self.activations;
                    }
                    toplevel.activated = activated;
                }
            }
            ToplevelEvent::Closed(id) => {
                self.toplevels.remove(&id);
            }
            ToplevelEvent::Finished => self.toplevels.clear(),
        }
    }

    /// The app id of the activated window, if there is one and it has an app id.
    pub fn focused_app(&self) -> Option<&str> {
        self.toplevels
            .values()
            .filter(|t| t.activated)
            .max_by_key(|t| t.activated_at)
            .and_then(|t| t.app_id.as_deref())
    }
}

/// Handler of the bound manager: gives every new window its own handler.
pub struct ManagerEvents {
    queue: EventQueue,
    next_id: ToplevelId,
}

impl ManagerEvents {
    pub fn new(queue: EventQueue) -> Self {
        Self { queue, next_id: 0 }
    }
}

impl ZwlrForeignToplevelManagerV1Handler for ManagerEvents {
    fn handle_toplevel(
        &mut self,
        _slf: &Rc<ZwlrForeignToplevelManagerV1>,
        toplevel: &Rc<ZwlrForeignToplevelHandleV1>,
    ) {
        self.next_id += 1;
        toplevel.set_forward_to_client(false);
        toplevel.set_handler(ToplevelEvents {
            id: self.next_id,
            queue: self.queue.clone(),
        });
    }

    fn handle_finished(&mut self, _slf: &Rc<ZwlrForeignToplevelManagerV1>) {
        self.queue
            .borrow_mut()
            .push_back(Event::Toplevel(ToplevelEvent::Finished));
    }
}

struct ToplevelEvents {
    id: ToplevelId,
    queue: EventQueue,
}

impl ToplevelEvents {
    fn push(&self, event: ToplevelEvent) {
        self.queue.borrow_mut().push_back(Event::Toplevel(event));
    }
}

impl ZwlrForeignToplevelHandleV1Handler for ToplevelEvents {
    fn handle_app_id(&mut self, _slf: &Rc<ZwlrForeignToplevelHandleV1>, app_id: &str) {
        self.push(ToplevelEvent::AppId(self.id, app_id.to_owned()));
    }

    fn handle_state(&mut self, _slf: &Rc<ZwlrForeignToplevelHandleV1>, state: &[u8]) {
        // A wl_array of u32 values in native byte order.
        let (values, _) = state.as_chunks::<4>();
        let activated = values
            .iter()
            .map(|bytes| u32::from_ne_bytes(*bytes))
            .any(|value| value == ZwlrForeignToplevelHandleV1State::ACTIVATED.0);
        self.push(ToplevelEvent::Activated(self.id, activated));
    }

    fn handle_done(&mut self, _slf: &Rc<ZwlrForeignToplevelHandleV1>) {
        self.push(ToplevelEvent::Done(self.id));
    }

    fn handle_closed(&mut self, slf: &Rc<ZwlrForeignToplevelHandleV1>) {
        self.push(ToplevelEvent::Closed(self.id));
        // The compositor sends nothing more; the handle only waits for us to drop it.
        slf.send_destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ToplevelEvent::*;

    fn feed(toplevels: &mut Toplevels, events: &[ToplevelEvent]) {
        for event in events {
            toplevels.handle(event.clone());
        }
    }

    #[test]
    fn follows_the_activated_window() {
        let mut t = Toplevels::default();
        feed(&mut t, &[AppId(1, "foot".into()), Activated(1, true)]);
        assert_eq!(t.focused_app(), None, "nothing applies before done");
        feed(&mut t, &[Done(1)]);
        assert_eq!(t.focused_app(), Some("foot"));

        // Focus moves to a new window; the old one may be deactivated after or before.
        feed(
            &mut t,
            &[
                AppId(2, "firefox".into()),
                Activated(2, true),
                Done(2),
                Activated(1, false),
                Done(1),
            ],
        );
        assert_eq!(t.focused_app(), Some("firefox"));
        feed(&mut t, &[Activated(2, false), Done(2)]);
        assert_eq!(t.focused_app(), None);
        feed(&mut t, &[Activated(1, true), Done(1)]);
        assert_eq!(t.focused_app(), Some("foot"));
    }

    #[test]
    fn newest_activation_wins_and_closing_forgets() {
        let mut t = Toplevels::default();
        feed(
            &mut t,
            &[
                AppId(1, "a".into()),
                Activated(1, true),
                Done(1),
                AppId(2, "b".into()),
                Activated(2, true),
                Done(2),
            ],
        );
        assert_eq!(t.focused_app(), Some("b"));
        feed(&mut t, &[Closed(2)]);
        assert_eq!(t.focused_app(), Some("a"));
        feed(&mut t, &[Finished]);
        assert_eq!(t.focused_app(), None);
    }

    #[test]
    fn app_ids_can_change_or_be_missing() {
        let mut t = Toplevels::default();
        feed(&mut t, &[Activated(1, true), Done(1)]);
        assert_eq!(t.focused_app(), None);
        feed(&mut t, &[AppId(1, "late".into()), Done(1)]);
        assert_eq!(t.focused_app(), Some("late"));
        feed(&mut t, &[AppId(1, String::new()), Done(1)]);
        assert_eq!(t.focused_app(), None);
    }
}
