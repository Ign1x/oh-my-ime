//! The frontends that bring text fields to the core: the Wayland compositor's
//! `input-method-v2`, and the protocols X11 applications speak.
//!
//! Frontends report [`FrontendEvent`]s. The core carries out its decisions through
//! [`Frontends`], which hands every call to the frontend the text field came from.

use std::cell::Cell;
use std::rc::Rc;

use omime_proto::engine::{Action, ContextId, ContextInfo, KeyEvent, Rect};

use crate::wayland::{self, EngineId, SeatId};
use crate::{fcitx, xim};

/// Where a text field comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A seat of the Wayland compositor.
    Wayland(SeatId),
    /// An application using the Fcitx5 module protocol.
    Fcitx,
    /// An X11 application using XIM.
    Xim,
}

/// A key the core has to decide about; it goes back to the frontend that reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRef {
    Wayland(wayland::KeyRef),
    Fcitx(fcitx::KeyRef),
    Xim(xim::KeyRef),
}

impl KeyRef {
    /// The text field the key was typed into (0 if none).
    pub fn ctx(&self) -> ContextId {
        match self {
            Self::Wayland(key) => key.ctx,
            Self::Fcitx(key) => key.ctx,
            Self::Xim(key) => key.ctx,
        }
    }
}

/// What the frontends report to the core.
#[derive(Debug)]
pub enum FrontendEvent {
    /// A (non-sensitive) text field got focus.
    Activated {
        origin: Origin,
        ctx: ContextId,
        info: ContextInfo,
    },
    /// Properties of the focused field changed. `reset` means the application changed
    /// the text on its own.
    Updated {
        ctx: ContextId,
        info: ContextInfo,
        reset: bool,
    },
    /// The text field lost focus.
    Deactivated { ctx: ContextId },
    /// A key needs a decision: call [`Frontends::finish_key`].
    Key { key: KeyRef, event: KeyEvent },
    /// The text cursor relative to an engine's popup changed.
    CursorRect { engine: EngineId, rect: Rect },
    /// Another application got focus (`None` when unknown). Sent after the `Updated`
    /// events of the focused fields, which carry the new app id.
    FocusedApp(Option<String>),
}

/// The frontends, as the core drives them.
pub trait Frontends {
    /// Passes a key reported in [`FrontendEvent::Key`] to the application, unless the
    /// engine `handled` it.
    fn finish_key(&mut self, key: &KeyRef, handled: bool);

    /// Applies an engine's actions to the text field `ctx`.
    fn apply_actions(&mut self, origin: Origin, ctx: ContextId, actions: &[Action]);

    /// Removes any preedit from the text field `ctx`.
    fn clear_preedit(&mut self, origin: Origin, ctx: ContextId);

    /// Selects the engine whose windows may be shown.
    fn set_active_engine(&mut self, engine: Option<EngineId>);

    /// Whether `engine` wants its windows shown while it is active.
    fn set_engine_visible(&mut self, engine: EngineId, visible: bool);
}

/// Hands out context ids, unique among all frontends for the daemon's lifetime.
#[derive(Debug, Clone, Default)]
pub struct ContextIds(Rc<Cell<ContextId>>);

impl ContextIds {
    pub fn next(&self) -> ContextId {
        let id = self.0.get() + 1;
        self.0.set(id);
        id
    }
}
