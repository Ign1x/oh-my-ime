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
    /// Something a hosted input method did.
    Hosted(HostedEvent),
}

/// What hosted input methods (Wayland input methods running as engines) do.
#[derive(Debug)]
pub enum HostedEvent {
    /// The engine created an input method object for a seat: it can serve text fields
    /// there.
    Ready { engine: EngineId },
    /// The engine grabbed the keyboard, so it can take keys.
    Grabbed { engine: EngineId },
    /// What the engine did to the text field `ctx`.
    Actions {
        engine: EngineId,
        ctx: ContextId,
        actions: Vec<Action>,
    },
}

/// Whether a hosted engine took a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyDelivery {
    /// The key is the engine's now; the frontend that reported it was told.
    Delivered,
    /// The engine cannot take keys yet, for example because it has not grabbed the
    /// keyboard.
    NotReady,
    /// The key is not for the engine: a release of a key whose press it did not see.
    Refused,
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

    /// Activates the input method of the hosted engine `engine` for the text field
    /// `ctx`. Returns `false` if the engine has none for the field yet.
    fn hosted_activate(
        &mut self,
        engine: &EngineId,
        origin: Origin,
        ctx: ContextId,
        info: &ContextInfo,
    ) -> bool;

    /// Tells the hosted engine serving `ctx` about changes to the field; `reset` means
    /// the application changed the text on its own.
    fn hosted_update(&mut self, engine: &EngineId, ctx: ContextId, info: &ContextInfo, reset: bool);

    /// The hosted engine `engine` no longer serves the field `ctx`.
    fn hosted_deactivate(&mut self, engine: &EngineId, ctx: ContextId);

    /// Hands a key reported in [`FrontendEvent::Key`] to the hosted engine `engine`.
    /// Unless it is delivered, the key still needs [`Self::finish_key`].
    fn hosted_key(&mut self, engine: &EngineId, key: &KeyRef, event: &KeyEvent) -> KeyDelivery;
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
