//! Hosted input methods: unmodified Wayland input methods, such as Fcitx5, running as
//! engines.
//!
//! A hosted input method connects to its engine socket like the UI of any engine. Its
//! registry has two more globals, a virtual `zwp_input_method_manager_v2` and a virtual
//! `zwp_virtual_keyboard_manager_v1`, and the bridge answers both itself:
//!
//! - The core activates the virtual input method for a text field, sends it the field's
//!   state and passes keys to its keyboard grab, as a compositor would. Keys carry the
//!   modifiers they were typed with, so the input method sees them in order even when
//!   they arrive in one batch.
//! - What the input method commits comes back as engine actions for that field.
//! - Keys it sends through its virtual keyboard come back as `ForwardKey` actions:
//!   omimed's own virtual keyboard stays the only one, and it keeps track of every key
//!   an application holds.
//! - Its popup surfaces become engine windows, which the bridge shows like the windows
//!   of other engines: as input popups, or on a mirror for X11 applications.

use std::collections::BTreeSet;
use std::os::fd::OwnedFd;
use std::rc::{Rc, Weak};

use omime_proto::engine::{Action, ContextId, ContextInfo, Preedit};
use tracing::{debug, warn};
use wl_proxy::object::{ObjectCoreApi, ObjectUtils};
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_keyboard_grab_v2::{
    ZwpInputMethodKeyboardGrabV2, ZwpInputMethodKeyboardGrabV2Handler,
};
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_manager_v2::{
    ZwpInputMethodManagerV2, ZwpInputMethodManagerV2Handler,
};
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_method_v2::{
    ZwpInputMethodV2, ZwpInputMethodV2Handler,
};
use wl_proxy::protocols::input_method_unstable_v2::zwp_input_popup_surface_v2::{
    ZwpInputPopupSurfaceV2, ZwpInputPopupSurfaceV2Handler,
};
use wl_proxy::protocols::text_input_unstable_v3::zwp_text_input_v3::{
    ZwpTextInputV3ChangeCause, ZwpTextInputV3ContentHint, ZwpTextInputV3ContentPurpose,
};
use wl_proxy::protocols::virtual_keyboard_unstable_v1::zwp_virtual_keyboard_manager_v1::{
    ZwpVirtualKeyboardManagerV1, ZwpVirtualKeyboardManagerV1Handler,
};
use wl_proxy::protocols::virtual_keyboard_unstable_v1::zwp_virtual_keyboard_v1::{
    ZwpVirtualKeyboardV1, ZwpVirtualKeyboardV1Handler,
};
use wl_proxy::protocols::wayland::wl_keyboard::{WlKeyboardKeyState, WlKeyboardKeymapFormat};
use wl_proxy::protocols::wayland::wl_seat::WlSeat;
use wl_proxy::protocols::wayland::wl_surface::WlSurface;
use xkbcommon::xkb;

use super::{Bridge, BridgeSeat, BridgeSurface, EngineId, Shared, kill_client};
use crate::frontend::{HostedEvent, KeyDelivery};
use crate::keyboard::{self, ModifierState, RepeatInfo};
use crate::placement::Rect;
use crate::wayland::{Event, SeatId};

/// Registry names of the virtual globals hosted input methods see.
pub const INPUT_METHOD_MANAGER_NAME: u32 = 0xff00_0004;
pub const INPUT_METHOD_MANAGER_VERSION: u32 = 1;
pub const VIRTUAL_KEYBOARD_MANAGER_NAME: u32 = 0xff00_0005;
pub const VIRTUAL_KEYBOARD_MANAGER_VERSION: u32 = 1;

const KEY_STATE_RELEASED: u32 = 0;
const KEY_STATE_PRESSED: u32 = 1;

/// Text longer than this cannot be sent in one Wayland message; such a field gets no
/// surrounding text.
const MAX_SURROUNDING: usize = 4000;

/// What hosted input methods need to know about a seat's keyboard.
#[derive(Debug, Default, Clone)]
pub struct SeatKeyboard {
    /// The keymap of the seat's keyboard grab, once there was one.
    pub keymap: Option<Rc<str>>,
    pub repeat: RepeatInfo,
    /// The current modifiers, for new grabs; keys carry their own.
    pub modifiers: ModifierState,
}

/// A key for a hosted input method.
#[derive(Debug, Clone, Copy)]
pub struct HostedKey {
    /// The evdev key code.
    pub keycode: u32,
    pub time: u32,
    /// A `wl_keyboard.key_state`: released, pressed or repeated.
    pub state: u32,
    /// The modifiers when the key was typed, as a keyboard grab reports them.
    pub modifiers: ModifierState,
}

impl Bridge {
    /// The keyboard of `seat` got a new keymap; grabs of hosted input methods that use
    /// it get the keymap too.
    pub fn set_keymap(&self, seat: SeatId, keymap: Rc<str>) {
        self.shared
            .borrow_mut()
            .keyboards
            .entry(seat)
            .or_default()
            .keymap = Some(keymap);
        self.refresh_grabs(seat);
    }

    /// The keyboard of `seat` repeats keys at another rate.
    pub fn set_repeat(&self, seat: SeatId, repeat: RepeatInfo) {
        self.shared
            .borrow_mut()
            .keyboards
            .entry(seat)
            .or_default()
            .repeat = repeat;
        self.refresh_grabs(seat);
    }

    /// The modifiers of `seat` changed. Keys bring their own modifiers to hosted input
    /// methods, so this only matters for new grabs and [`Self::flush_modifiers`].
    pub fn set_modifiers(&self, seat: SeatId, modifiers: ModifierState) {
        self.shared
            .borrow_mut()
            .keyboards
            .entry(seat)
            .or_default()
            .modifiers = modifiers;
    }

    /// Tells hosted input methods typing into Wayland fields about modifier changes
    /// that no key brought them, such as releasing Shift at the end of a batch.
    pub fn flush_modifiers(&self) {
        let seats: Vec<SeatId> = self.shared.borrow().keyboards.keys().copied().collect();
        for seat in seats {
            let modifiers = self.keyboard(seat).modifiers;
            for (im, grab) in self.live_grabs(seat) {
                let wayland = im
                    .try_get_handler_ref::<VirtualIm>()
                    .is_ok_and(|im| im.field.is_some_and(|f| !f.x11));
                if wayland && let Ok(mut handler) = grab.try_get_handler_mut::<VirtualGrab>() {
                    handler.send_modifiers(&grab, modifiers);
                }
            }
        }
    }

    /// A seat on which `engine` has an input method, for fields of X11 applications.
    pub fn hosted_seat(&self, engine: &EngineId) -> Option<SeatId> {
        let shared = self.shared.borrow();
        shared
            .hosted
            .iter()
            .find(|((e, _), im)| e == engine && im.strong_count() > 0)
            .map(|((_, seat), _)| *seat)
    }

    /// Activates the input method of `engine` on `seat` for the text field `ctx`.
    /// Returns `false` if the engine has no input method there (yet).
    pub fn hosted_activate(
        &self,
        engine: &EngineId,
        seat: SeatId,
        ctx: ContextId,
        x11: bool,
        info: &ContextInfo,
    ) -> bool {
        let im = self
            .shared
            .borrow()
            .hosted
            .get(&(engine.clone(), seat))
            .and_then(Weak::upgrade);
        let Some(im) = im else {
            return false;
        };
        let Ok(mut handler) = im.try_get_handler_mut::<VirtualIm>() else {
            return false;
        };
        handler.activate(&im, ctx, x11, info);
        true
    }

    /// Tells the input method serving `ctx` that the field changed; `reset` when the
    /// application changed its text on its own.
    pub fn hosted_update(
        &self,
        engine: &EngineId,
        ctx: ContextId,
        info: &ContextInfo,
        reset: bool,
    ) {
        if let Some(im) = self.serving(engine, ctx)
            && let Ok(mut handler) = im.try_get_handler_mut::<VirtualIm>()
        {
            handler.update(&im, info, reset);
        }
    }

    /// The text field `ctx` lost focus or another engine took over.
    pub fn hosted_deactivate(&self, engine: &EngineId, ctx: ContextId) {
        if let Some(im) = self.serving(engine, ctx)
            && let Ok(mut handler) = im.try_get_handler_mut::<VirtualIm>()
        {
            handler.deactivate(&im);
        }
    }

    /// Passes a key typed into `ctx` to the keyboard grab of `engine`.
    pub fn hosted_key(&self, engine: &EngineId, ctx: ContextId, key: HostedKey) -> KeyDelivery {
        let release = key.state == KEY_STATE_RELEASED;
        // A release goes where its press went; a press without a grab may wait for one.
        let not_ready = if release {
            KeyDelivery::Refused
        } else {
            KeyDelivery::NotReady
        };
        let Some(im) = self.serving(engine, ctx) else {
            return not_ready;
        };
        let Some(grab) = im
            .try_get_handler_ref::<VirtualIm>()
            .ok()
            .and_then(|im| im.grab.clone())
        else {
            return not_ready;
        };
        let Ok(mut handler) = grab.try_get_handler_mut::<VirtualGrab>() else {
            return not_ready;
        };
        if handler.keymap.is_none() {
            return not_ready;
        }
        if release {
            if !handler.pressed.remove(&key.keycode) {
                return KeyDelivery::Refused;
            }
        } else {
            handler.pressed.insert(key.keycode);
        }
        handler.send_modifiers(&grab, key.modifiers);
        handler.serial = handler.serial.wrapping_add(1);
        grab.send_key(
            handler.serial,
            key.time,
            key.keycode,
            WlKeyboardKeyState(key.state),
        );
        KeyDelivery::Delivered
    }

    /// Hands the keymap and repeat rate of `seat` to the grabs that use them.
    fn refresh_grabs(&self, seat: SeatId) {
        for (im, grab) in self.live_grabs(seat) {
            let Some(x11) = im
                .try_get_handler_ref::<VirtualIm>()
                .ok()
                .map(|im| im.field.is_some_and(|f| f.x11))
            else {
                continue;
            };
            if let Ok(mut handler) = grab.try_get_handler_mut::<VirtualGrab>() {
                handler.send_keyboard(&grab, &self.shared, seat, x11);
            }
        }
    }

    fn keyboard(&self, seat: SeatId) -> SeatKeyboard {
        self.shared
            .borrow()
            .keyboards
            .get(&seat)
            .cloned()
            .unwrap_or_default()
    }

    /// The input method of `engine` that serves the text field `ctx`.
    fn serving(&self, engine: &EngineId, ctx: ContextId) -> Option<Rc<ZwpInputMethodV2>> {
        let ims: Vec<_> = {
            let shared = self.shared.borrow();
            shared
                .hosted
                .iter()
                .filter(|((e, _), _)| e == engine)
                .filter_map(|(_, im)| im.upgrade())
                .collect()
        };
        ims.into_iter().find(|im| {
            im.try_get_handler_ref::<VirtualIm>()
                .is_ok_and(|handler| handler.field.is_some_and(|f| f.ctx == ctx))
        })
    }

    /// The input methods on `seat` that have a keyboard grab, with the grab.
    fn live_grabs(
        &self,
        seat: SeatId,
    ) -> Vec<(Rc<ZwpInputMethodV2>, Rc<ZwpInputMethodKeyboardGrabV2>)> {
        let ims: Vec<_> = {
            let shared = self.shared.borrow();
            shared
                .hosted
                .iter()
                .filter(|((_, s), _)| *s == seat)
                .filter_map(|(_, im)| im.upgrade())
                .collect()
        };
        ims.into_iter()
            .filter_map(|im| {
                let grab = im.try_get_handler_ref::<VirtualIm>().ok()?.grab.clone()?;
                Some((im, grab))
            })
            .collect()
    }
}

/// The keymap for a field on `seat`: the seat's, or for X11 applications before any
/// Wayland field was focused, XKB's default.
fn keymap_for(shared: &Shared, seat: SeatId, x11: bool) -> Option<Rc<str>> {
    let mut shared = shared.borrow_mut();
    if let Some(keymap) = shared.keyboards.get(&seat).and_then(|k| k.keymap.clone()) {
        return Some(keymap);
    }
    if !x11 {
        return None;
    }
    if shared.default_keymap.is_none() {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        shared.default_keymap = xkb::Keymap::new_from_names(
            &context,
            "",
            "",
            "",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .map(|keymap| Rc::from(keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)));
    }
    shared.default_keymap.clone()
}

/// The repeat rate for a field on `seat`. X11 applications get repeated keys from the
/// X server; the input method must not repeat them once more.
fn repeat_for(shared: &Shared, seat: SeatId, x11: bool) -> RepeatInfo {
    if x11 {
        return RepeatInfo { rate: 0, delay: 0 };
    }
    shared
        .borrow()
        .keyboards
        .get(&seat)
        .map(|k| k.repeat)
        .unwrap_or_default()
}

/// Answers `zwp_input_method_manager_v2` for a hosted input method.
pub(super) struct VirtualImManager {
    pub shared: Shared,
    pub engine: EngineId,
}

impl ZwpInputMethodManagerV2Handler for VirtualImManager {
    fn handle_get_input_method(
        &mut self,
        _slf: &Rc<ZwpInputMethodManagerV2>,
        seat: &Rc<WlSeat>,
        input_method: &Rc<ZwpInputMethodV2>,
    ) {
        input_method.set_forward_to_server(false);
        let Some(seat) = seat
            .try_get_handler_ref::<BridgeSeat>()
            .ok()
            .map(|seat| seat.name)
        else {
            return kill_client(&**input_method, "input method for an unknown seat");
        };
        let key = (self.engine.clone(), seat);
        let taken = self
            .shared
            .borrow()
            .hosted
            .get(&key)
            .is_some_and(|im| im.strong_count() > 0);
        if taken {
            // One input method per seat, as on a compositor.
            input_method.set_handler(InertIm);
            input_method.send_unavailable();
            return;
        }
        input_method.set_handler(VirtualIm {
            shared: self.shared.clone(),
            engine: self.engine.clone(),
            seat,
            field: None,
            dones: 0,
            pending: Pending::default(),
            grab: None,
        });
        let events = {
            let mut shared = self.shared.borrow_mut();
            shared.hosted.insert(key, Rc::downgrade(input_method));
            shared.events.clone()
        };
        debug!(engine = &*self.engine, seat, "hosted input method ready");
        events
            .borrow_mut()
            .push_back(Event::Hosted(HostedEvent::Ready {
                engine: self.engine.clone(),
            }));
    }

    fn handle_destroy(&mut self, slf: &Rc<ZwpInputMethodManagerV2>) {
        slf.delete_id();
    }
}

/// A text field a virtual input method is active for.
#[derive(Debug, Clone, Copy)]
struct Field {
    ctx: ContextId,
    /// Whether the field belongs to an X11 application.
    x11: bool,
    /// The number of `done` events once the input method knew about the field. Commits
    /// with a lower serial were meant for the field before.
    since: u32,
}

/// Changes requested since the last `commit`.
#[derive(Debug, Default)]
struct Pending {
    commit: String,
    preedit: Option<Preedit>,
    delete: Option<(u32, u32)>,
}

impl Pending {
    /// The actions that one `commit` stands for, in the order a compositor applies
    /// them. A commit without `set_preedit_string` removes the preedit.
    fn into_actions(self) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some((before, after)) = self.delete {
            actions.push(Action::DeleteSurrounding { before, after });
        }
        if !self.commit.is_empty() {
            actions.push(Action::Commit(self.commit));
        }
        actions.push(Action::Preedit(self.preedit.unwrap_or_default()));
        actions
    }
}

/// The `zwp_input_method_v2` of a hosted input method on one seat.
pub(super) struct VirtualIm {
    shared: Shared,
    engine: EngineId,
    seat: SeatId,
    field: Option<Field>,
    /// `done` events sent so far, which is the serial the next commit should carry.
    dones: u32,
    pending: Pending,
    grab: Option<Rc<ZwpInputMethodKeyboardGrabV2>>,
}

impl VirtualIm {
    fn done(&mut self, slf: &Rc<ZwpInputMethodV2>) {
        slf.send_done();
        self.dones = self.dones.wrapping_add(1);
    }

    fn activate(
        &mut self,
        slf: &Rc<ZwpInputMethodV2>,
        ctx: ContextId,
        x11: bool,
        info: &ContextInfo,
    ) {
        if self.field.is_some_and(|f| f.ctx == ctx) {
            return;
        }
        if self.field.is_some() {
            slf.send_deactivate();
            self.done(slf);
        }
        slf.send_activate();
        send_field_state(slf, info, false);
        self.done(slf);
        self.field = Some(Field {
            ctx,
            x11,
            since: self.dones,
        });
        self.pending = Pending::default();
        if let Some(grab) = &self.grab
            && let Ok(mut handler) = grab.try_get_handler_mut::<VirtualGrab>()
        {
            handler.send_keyboard(grab, &self.shared, self.seat, x11);
        }
    }

    fn update(&mut self, slf: &Rc<ZwpInputMethodV2>, info: &ContextInfo, reset: bool) {
        send_field_state(slf, info, reset);
        self.done(slf);
    }

    fn deactivate(&mut self, slf: &Rc<ZwpInputMethodV2>) {
        if self.field.take().is_none() {
            return;
        }
        slf.send_deactivate();
        self.done(slf);
        self.pending = Pending::default();
    }
}

/// Sends the state of a text field, which the next `done` applies.
fn send_field_state(im: &Rc<ZwpInputMethodV2>, info: &ContextInfo, changed_by_other: bool) {
    if let Some((text, cursor, anchor)) = &info.surrounding
        && text.len() <= MAX_SURROUNDING
    {
        im.send_surrounding_text(text, *cursor, *anchor);
    }
    im.send_text_change_cause(if changed_by_other {
        ZwpTextInputV3ChangeCause::OTHER
    } else {
        ZwpTextInputV3ChangeCause::INPUT_METHOD
    });
    im.send_content_type(
        ZwpTextInputV3ContentHint(info.content_type.hints.0),
        ZwpTextInputV3ContentPurpose(info.content_type.purpose.to_raw()),
    );
}

impl ZwpInputMethodV2Handler for VirtualIm {
    fn handle_commit_string(&mut self, _slf: &Rc<ZwpInputMethodV2>, text: &str) {
        self.pending.commit.push_str(text);
    }

    fn handle_set_preedit_string(
        &mut self,
        _slf: &Rc<ZwpInputMethodV2>,
        text: &str,
        cursor_begin: i32,
        cursor_end: i32,
    ) {
        self.pending.preedit = Some(Preedit {
            text: text.to_owned(),
            cursor_begin,
            cursor_end,
        });
    }

    fn handle_delete_surrounding_text(
        &mut self,
        _slf: &Rc<ZwpInputMethodV2>,
        before_length: u32,
        after_length: u32,
    ) {
        self.pending.delete = Some((before_length, after_length));
    }

    fn handle_commit(&mut self, _slf: &Rc<ZwpInputMethodV2>, serial: u32) {
        let pending = std::mem::take(&mut self.pending);
        let Some(field) = self.field else {
            return;
        };
        // Serials count `done` events and wrap around.
        if (serial.wrapping_sub(field.since) as i32) < 0 {
            debug!(
                engine = &*self.engine,
                "dropping a commit meant for another field"
            );
            return;
        }
        let events = self.shared.borrow().events.clone();
        events
            .borrow_mut()
            .push_back(Event::Hosted(HostedEvent::Actions {
                engine: self.engine.clone(),
                ctx: field.ctx,
                actions: pending.into_actions(),
            }));
    }

    fn handle_get_input_popup_surface(
        &mut self,
        _slf: &Rc<ZwpInputMethodV2>,
        id: &Rc<ZwpInputPopupSurfaceV2>,
        surface: &Rc<WlSurface>,
    ) {
        id.set_forward_to_server(false);
        id.set_handler(HostedPopup {
            surface: Rc::downgrade(surface),
            last: None,
        });
        let assigned = match surface.try_get_handler_mut::<BridgeSurface>() {
            Ok(mut handler) if handler.window.is_none() && handler.input_popup.is_none() => {
                handler.input_popup = Some(Rc::downgrade(id));
                true
            }
            _ => false,
        };
        if !assigned {
            return kill_client(&**id, "input popup for a surface that has a role");
        }
        self.shared
            .borrow_mut()
            .windows
            .push(Rc::downgrade(surface));
        debug!(engine = &*self.engine, "hosted input method popup created");
    }

    fn handle_grab_keyboard(
        &mut self,
        slf: &Rc<ZwpInputMethodV2>,
        keyboard: &Rc<ZwpInputMethodKeyboardGrabV2>,
    ) {
        keyboard.set_forward_to_server(false);
        keyboard.set_handler(VirtualGrab {
            im: Rc::downgrade(slf),
            keymap: None,
            repeat: None,
            modifiers: None,
            pressed: BTreeSet::new(),
            serial: 0,
        });
        self.grab = Some(keyboard.clone());
        let x11 = self.field.is_some_and(|f| f.x11);
        let (modifiers, events) = {
            let shared = self.shared.borrow();
            let modifiers = shared
                .keyboards
                .get(&self.seat)
                .map(|k| k.modifiers)
                .unwrap_or_default();
            (modifiers, shared.events.clone())
        };
        if let Ok(mut grab) = keyboard.try_get_handler_mut::<VirtualGrab>() {
            grab.send_keyboard(keyboard, &self.shared, self.seat, x11);
            grab.send_modifiers(keyboard, modifiers);
        }
        events
            .borrow_mut()
            .push_back(Event::Hosted(HostedEvent::Grabbed {
                engine: self.engine.clone(),
            }));
        debug!(
            engine = &*self.engine,
            "hosted input method grabbed the keyboard"
        );
    }

    fn handle_destroy(&mut self, slf: &Rc<ZwpInputMethodV2>) {
        let mut shared = self.shared.borrow_mut();
        let key = (self.engine.clone(), self.seat);
        if shared
            .hosted
            .get(&key)
            .is_some_and(|im| im.as_ptr() == Rc::as_ptr(slf))
        {
            shared.hosted.remove(&key);
        }
        drop(shared);
        slf.delete_id();
    }
}

/// An input method that was told `unavailable`: it is inert, as on a compositor.
struct InertIm;

impl ZwpInputMethodV2Handler for InertIm {
    fn handle_commit_string(&mut self, _slf: &Rc<ZwpInputMethodV2>, _text: &str) {}

    fn handle_set_preedit_string(
        &mut self,
        _slf: &Rc<ZwpInputMethodV2>,
        _text: &str,
        _cursor_begin: i32,
        _cursor_end: i32,
    ) {
    }

    fn handle_delete_surrounding_text(
        &mut self,
        _slf: &Rc<ZwpInputMethodV2>,
        _before: u32,
        _after: u32,
    ) {
    }

    fn handle_commit(&mut self, _slf: &Rc<ZwpInputMethodV2>, _serial: u32) {}

    fn handle_get_input_popup_surface(
        &mut self,
        _slf: &Rc<ZwpInputMethodV2>,
        id: &Rc<ZwpInputPopupSurfaceV2>,
        _surface: &Rc<WlSurface>,
    ) {
        id.set_forward_to_server(false);
        id.set_handler(InertPopup);
    }

    fn handle_grab_keyboard(
        &mut self,
        _slf: &Rc<ZwpInputMethodV2>,
        keyboard: &Rc<ZwpInputMethodKeyboardGrabV2>,
    ) {
        keyboard.set_forward_to_server(false);
        keyboard.set_handler(InertGrab);
    }

    fn handle_destroy(&mut self, slf: &Rc<ZwpInputMethodV2>) {
        slf.delete_id();
    }
}

struct InertPopup;

impl ZwpInputPopupSurfaceV2Handler for InertPopup {
    fn handle_destroy(&mut self, slf: &Rc<ZwpInputPopupSurfaceV2>) {
        slf.delete_id();
    }
}

struct InertGrab;

impl ZwpInputMethodKeyboardGrabV2Handler for InertGrab {
    fn handle_release(&mut self, slf: &Rc<ZwpInputMethodKeyboardGrabV2>) {
        slf.delete_id();
    }
}

/// The keyboard grab of a hosted input method.
pub(super) struct VirtualGrab {
    im: Weak<ZwpInputMethodV2>,
    /// The keymap sent, once there was one.
    keymap: Option<Rc<str>>,
    repeat: Option<RepeatInfo>,
    modifiers: Option<ModifierState>,
    /// Keys pressed through the grab; their releases go the same way.
    pressed: BTreeSet<u32>,
    serial: u32,
}

impl VirtualGrab {
    /// Sends the keymap and repeat rate for a field on `seat`, if they changed. Keys
    /// wait until the grab has a keymap.
    fn send_keyboard(
        &mut self,
        grab: &Rc<ZwpInputMethodKeyboardGrabV2>,
        shared: &Shared,
        seat: SeatId,
        x11: bool,
    ) {
        if let Some(keymap) = keymap_for(shared, seat, x11)
            && self
                .keymap
                .as_ref()
                .is_none_or(|sent| !Rc::ptr_eq(sent, &keymap) && **sent != *keymap)
        {
            match keyboard::keymap_memfd(&keymap) {
                Ok((fd, size)) => {
                    grab.send_keymap(WlKeyboardKeymapFormat::XKB_V1, &Rc::new(fd), size);
                    self.keymap = Some(keymap);
                }
                Err(e) => warn!("cannot hand the keymap to a hosted input method: {e}"),
            }
        }
        let repeat = repeat_for(shared, seat, x11);
        if self.repeat != Some(repeat) {
            grab.send_repeat_info(repeat.rate, repeat.delay);
            self.repeat = Some(repeat);
        }
    }

    fn send_modifiers(
        &mut self,
        grab: &Rc<ZwpInputMethodKeyboardGrabV2>,
        modifiers: ModifierState,
    ) {
        if self.keymap.is_none() || self.modifiers == Some(modifiers) {
            return;
        }
        self.serial = self.serial.wrapping_add(1);
        grab.send_modifiers(
            self.serial,
            modifiers.depressed,
            modifiers.latched,
            modifiers.locked,
            modifiers.group,
        );
        self.modifiers = Some(modifiers);
    }
}

impl ZwpInputMethodKeyboardGrabV2Handler for VirtualGrab {
    fn handle_release(&mut self, slf: &Rc<ZwpInputMethodKeyboardGrabV2>) {
        if let Some(im) = self.im.upgrade()
            && let Ok(mut handler) = im.try_get_handler_mut::<VirtualIm>()
            && handler
                .grab
                .as_ref()
                .is_some_and(|grab| Rc::ptr_eq(grab, slf))
        {
            handler.grab = None;
        }
        slf.delete_id();
    }
}

/// The popup role a hosted input method gave one of its surfaces.
pub(super) struct HostedPopup {
    surface: Weak<WlSurface>,
    /// The text cursor rectangle last sent.
    last: Option<Rect>,
}

impl HostedPopup {
    /// Tells the input method where the text cursor is relative to its popup.
    pub fn report(popup: &Rc<ZwpInputPopupSurfaceV2>, (x, y, width, height): Rect) {
        if let Ok(mut handler) = popup.try_get_handler_mut::<HostedPopup>()
            && handler.last.replace((x, y, width, height)) != Some((x, y, width, height))
        {
            popup.send_text_input_rectangle(x, y, width, height);
        }
    }
}

impl ZwpInputPopupSurfaceV2Handler for HostedPopup {
    fn handle_destroy(&mut self, slf: &Rc<ZwpInputPopupSurfaceV2>) {
        // Losing the role unmaps the surface. The compositor's popup role is kept, so
        // that the surface can become a popup again.
        if let Some(surface) = self.surface.upgrade()
            && let Ok(mut handler) = surface.try_get_handler_mut::<BridgeSurface>()
        {
            handler.lose_window(&surface);
            handler.input_popup = None;
        }
        slf.delete_id();
    }
}

/// Answers `zwp_virtual_keyboard_manager_v1` for a hosted input method.
pub(super) struct VirtualKeyboardManager {
    pub shared: Shared,
    pub engine: EngineId,
}

impl ZwpVirtualKeyboardManagerV1Handler for VirtualKeyboardManager {
    fn handle_create_virtual_keyboard(
        &mut self,
        _slf: &Rc<ZwpVirtualKeyboardManagerV1>,
        seat: &Rc<WlSeat>,
        id: &Rc<ZwpVirtualKeyboardV1>,
    ) {
        id.set_forward_to_server(false);
        let Some(seat) = seat
            .try_get_handler_ref::<BridgeSeat>()
            .ok()
            .map(|seat| seat.name)
        else {
            return kill_client(&**id, "virtual keyboard for an unknown seat");
        };
        id.set_handler(VirtualKeyboard {
            shared: self.shared.clone(),
            engine: self.engine.clone(),
            seat,
            has_keymap: false,
        });
    }
}

/// The virtual keyboard of a hosted input method. Its keys reach the application
/// through omimed's own virtual keyboard, as keys an engine forwards.
struct VirtualKeyboard {
    shared: Shared,
    engine: EngineId,
    seat: SeatId,
    has_keymap: bool,
}

impl ZwpVirtualKeyboardV1Handler for VirtualKeyboard {
    fn handle_keymap(
        &mut self,
        _slf: &Rc<ZwpVirtualKeyboardV1>,
        _format: u32,
        _fd: &Rc<OwnedFd>,
        _size: u32,
    ) {
        // Input methods hand on the keymap of their grab, which is the seat's.
        self.has_keymap = true;
    }

    fn handle_key(&mut self, _slf: &Rc<ZwpVirtualKeyboardV1>, _time: u32, key: u32, state: u32) {
        if !self.has_keymap {
            debug!(
                engine = &*self.engine,
                "ignoring a virtual key sent before a keymap"
            );
            return;
        }
        let im = self
            .shared
            .borrow()
            .hosted
            .get(&(self.engine.clone(), self.seat))
            .and_then(Weak::upgrade);
        let Some(ctx) = im.as_ref().and_then(|im| {
            im.try_get_handler_ref::<VirtualIm>()
                .ok()
                .and_then(|im| im.field.map(|f| f.ctx))
        }) else {
            return;
        };
        let events = self.shared.borrow().events.clone();
        events
            .borrow_mut()
            .push_back(Event::Hosted(HostedEvent::Actions {
                engine: self.engine.clone(),
                ctx,
                actions: vec![Action::ForwardKey {
                    keycode: key,
                    pressed: state == KEY_STATE_PRESSED,
                }],
            }));
    }

    fn handle_modifiers(
        &mut self,
        _slf: &Rc<ZwpVirtualKeyboardV1>,
        _mods_depressed: u32,
        _mods_latched: u32,
        _mods_locked: u32,
        _group: u32,
    ) {
        // omimed's virtual keyboard already passes the real modifiers on.
    }

    fn handle_destroy(&mut self, slf: &Rc<ZwpVirtualKeyboardV1>) {
        slf.delete_id();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_commit_replaces_the_preedit() {
        let pending = Pending {
            commit: "你好".into(),
            preedit: None,
            delete: Some((3, 0)),
        };
        assert_eq!(
            pending.into_actions(),
            [
                Action::DeleteSurrounding {
                    before: 3,
                    after: 0
                },
                Action::Commit("你好".into()),
                Action::Preedit(Preedit::default()),
            ]
        );
        let pending = Pending {
            preedit: Some(Preedit::with_cursor_at_end("ni")),
            ..Pending::default()
        };
        assert_eq!(
            pending.into_actions(),
            [Action::Preedit(Preedit::with_cursor_at_end("ni"))]
        );
    }
}
