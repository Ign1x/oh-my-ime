//! A safe librime: started once per process ([`Rime::start`]), with sessions that the
//! engine keeps one per text field. Every call goes through the engine's lock, which
//! is what makes sharing [`Rime`] between the engine's and the window's threads sound.
//!
//! This module, with the declarations in [`ffi`], has all of the engine's unsafe code.

#![allow(unsafe_code)]

mod ffi;

use std::ffi::{CString, c_char, c_void};
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::Sender;

use ffi::Api;
pub use ffi::SessionId;

/// What librime tells the engine on its own, from its threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// Deploying started.
    Deploying,
    /// Deploying ended, successfully or not.
    Deployed { ok: bool },
}

/// Where librime's notices go: librime calls a plain function, from any thread.
static NOTICES: Mutex<Option<Sender<Notice>>> = Mutex::new(None);

/// librime's notification handler.
unsafe extern "C" fn notify(
    _context: *mut c_void,
    _session: SessionId,
    kind: *const c_char,
    value: *const c_char,
) {
    // SAFETY: librime passes NUL-terminated strings that live for the call.
    let (kind, value) = unsafe { (ffi::text(kind), ffi::text(value)) };
    let notice = match (kind.as_str(), value.as_str()) {
        ("deploy", "start") => Notice::Deploying,
        ("deploy", "success") => Notice::Deployed { ok: true },
        ("deploy", "failure") => Notice::Deployed { ok: false },
        _ => return,
    };
    if let Ok(guard) = NOTICES.lock()
        && let Some(sender) = &*guard
    {
        let _ = sender.send(notice);
    }
}

/// A candidate of the menu.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    pub comment: String,
}

/// What a session is composing, as librime has it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composing {
    /// The preedit, and the cursor in it as a byte offset.
    pub preedit: String,
    pub cursor: usize,
    /// The candidates of the page shown, their labels (1, 2, …) and the highlighted one.
    pub candidates: Vec<Candidate>,
    pub labels: Vec<String>,
    pub highlighted: usize,
    pub page: usize,
    pub last_page: bool,
}

/// The state of a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub schema_id: String,
    pub schema_name: String,
    pub ascii_mode: bool,
    pub full_shape: bool,
    pub simplified: bool,
    pub ascii_punct: bool,
}

/// A schema, such as `double_pinyin_flypy` (小鹤双拼).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub id: String,
    pub name: String,
}

/// librime, started.
pub struct Rime {
    api: &'static Api,
}

impl std::fmt::Debug for Rime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rime").finish_non_exhaustive()
    }
}

fn c_path(path: &Path) -> Option<CString> {
    CString::new(path.as_os_str().as_encoded_bytes()).ok()
}

fn c_text(text: &str) -> Option<CString> {
    CString::new(text).ok()
}

impl Rime {
    /// Starts librime with the shared data in `shared` and the user's in `user`, and
    /// deploys in the background what is not built yet: [`Notice`]s go to `notices`.
    pub fn start(shared: &Path, user: &Path, notices: Sender<Notice>) -> Result<Self, String> {
        let api = Api::get().ok_or("librime is older than 1.11, or not librime")?;
        let shared = c_path(shared).ok_or("the shared data directory has a NUL")?;
        let user = c_path(user).ok_or("the user data directory has a NUL")?;
        let name = c_text("oh-my-ime").ok_or("a NUL")?;
        let code_name = c_text("oh-my-ime").ok_or("a NUL")?;
        let version = c_text(env!("CARGO_PKG_VERSION")).ok_or("a NUL")?;
        let app = c_text("rime.oh-my-ime").ok_or("a NUL")?;
        // Only to stderr, which omimed logs: no log files in /tmp.
        let log_dir = c_text("").ok_or("a NUL")?;
        let mut traits = ffi::Traits {
            data_size: ffi::data_size::<ffi::Traits>(),
            shared_data_dir: shared.as_ptr(),
            user_data_dir: user.as_ptr(),
            distribution_name: name.as_ptr(),
            distribution_code_name: code_name.as_ptr(),
            distribution_version: version.as_ptr(),
            app_name: app.as_ptr(),
            modules: std::ptr::null(),
            // Errors only: building a dictionary warns about every entry it cannot
            // spell, hundreds of lines in omimed's log.
            min_log_level: 2,
            log_dir: log_dir.as_ptr(),
            prebuilt_data_dir: std::ptr::null(),
            staging_dir: std::ptr::null(),
        };
        if let Ok(mut guard) = NOTICES.lock() {
            *guard = Some(notices);
        }
        // SAFETY: the strings outlive the calls, which copy them; librime is set up once
        // per process, here, before any other call.
        unsafe {
            (api.setup)(&raw mut traits);
            (api.set_notification_handler)(Some(notify), std::ptr::null_mut());
            (api.initialize)(&raw mut traits);
            // Deploys in a thread of its own when the data changed or was never built.
            (api.start_maintenance)(0);
        }
        Ok(Self { api })
    }

    /// Whether librime is deploying, when it can do nothing else.
    pub fn deploying(&self) -> bool {
        // SAFETY: a plain query.
        unsafe { (self.api.is_maintenance_mode)() != 0 }
    }

    /// Waits for a deployment to end.
    pub fn join_deployment(&self) {
        // SAFETY: waits for librime's own thread.
        unsafe { (self.api.join_maintenance_thread)() }
    }

    /// Deploys everything again, as after the user changed the configuration.
    pub fn redeploy(&self) {
        // SAFETY: as in `start`.
        unsafe {
            (self.api.start_maintenance)(1);
        }
    }

    pub fn version(&self) -> String {
        // SAFETY: a static string.
        unsafe { ffi::text((self.api.get_version)()) }
    }

    pub fn create_session(&self) -> Option<SessionId> {
        // SAFETY: a plain call; 0 means no session.
        let id = unsafe { (self.api.create_session)() };
        (id != 0).then_some(id)
    }

    pub fn destroy_session(&self, session: SessionId) {
        // SAFETY: librime checks the id.
        unsafe {
            (self.api.destroy_session)(session);
        }
    }

    pub fn session_exists(&self, session: SessionId) -> bool {
        // SAFETY: librime checks the id.
        unsafe { (self.api.find_session)(session) != 0 }
    }

    /// Hands librime a key: an X11 keysym and an X11 modifier mask. Whether it took it.
    pub fn process_key(&self, session: SessionId, keysym: u32, mask: u32) -> bool {
        let (Ok(keysym), Ok(mask)) = (i32::try_from(keysym), i32::try_from(mask)) else {
            return false;
        };
        // SAFETY: librime checks the id.
        unsafe { (self.api.process_key)(session, keysym, mask) != 0 }
    }

    /// The text librime has committed, once.
    pub fn take_commit(&self, session: SessionId) -> Option<String> {
        let mut commit = ffi::zeroed_commit();
        // SAFETY: `commit` has its data size; what librime puts in it is freed by
        // `free_commit`, after the text is copied.
        unsafe {
            if (self.api.get_commit)(session, &raw mut commit) == 0 {
                return None;
            }
            let text = ffi::text(commit.text);
            (self.api.free_commit)(&raw mut commit);
            (!text.is_empty()).then_some(text)
        }
    }

    /// What the session is composing; `None` when nothing.
    pub fn composing(&self, session: SessionId) -> Option<Composing> {
        let mut context = ffi::zeroed_context();
        // SAFETY: `context` has its data size. The menu's candidates, labels and the
        // preedit belong to librime until `free_context`; they are copied before. The
        // candidates are `num_candidates` long and the labels, when librime gives them,
        // as long as the page.
        unsafe {
            if (self.api.get_context)(session, &raw mut context) == 0 {
                return None;
            }
            let preedit = ffi::text(context.composition.preedit);
            let cursor = usize::try_from(context.composition.cursor_pos).unwrap_or(0);
            let menu = &context.menu;
            let count = usize::try_from(menu.num_candidates).unwrap_or(0);
            let candidates: Vec<Candidate> = if menu.candidates.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(menu.candidates, count)
                    .iter()
                    .map(|c| Candidate {
                        text: ffi::text(c.text),
                        comment: ffi::text(c.comment),
                    })
                    .collect()
            };
            let select_keys = ffi::text(menu.select_keys);
            let labels: Vec<String> = (0..count)
                .map(|i| {
                    if !context.select_labels.is_null() {
                        let label = *context.select_labels.add(i);
                        if !label.is_null() {
                            return ffi::text(label);
                        }
                    }
                    select_keys
                        .chars()
                        .nth(i)
                        .map_or_else(|| ((i + 1) % 10).to_string(), String::from)
                })
                .collect();
            let composing = Composing {
                cursor: cursor.min(preedit.len()),
                preedit,
                candidates,
                labels,
                highlighted: usize::try_from(menu.highlighted_candidate_index).unwrap_or(0),
                page: usize::try_from(menu.page_no).unwrap_or(0),
                last_page: menu.is_last_page != 0,
            };
            (self.api.free_context)(&raw mut context);
            (!composing.preedit.is_empty() || !composing.candidates.is_empty()).then_some(composing)
        }
    }

    pub fn status(&self, session: SessionId) -> Option<Status> {
        let mut status = ffi::zeroed_status();
        // SAFETY: `status` has its data size; its strings are copied before
        // `free_status`.
        unsafe {
            if (self.api.get_status)(session, &raw mut status) == 0 {
                return None;
            }
            let copy = Status {
                schema_id: ffi::text(status.schema_id),
                schema_name: ffi::text(status.schema_name),
                ascii_mode: status.is_ascii_mode != 0,
                full_shape: status.is_full_shape != 0,
                // Schemas with the zh_hans option (luna_pinyin) report it as an option.
                simplified: status.is_simplified != 0,
                ascii_punct: status.is_ascii_punct != 0,
            };
            (self.api.free_status)(&raw mut status);
            Some(copy)
        }
    }

    pub fn option(&self, session: SessionId, name: &str) -> bool {
        let Some(name) = c_text(name) else {
            return false;
        };
        // SAFETY: the name outlives the call.
        unsafe { (self.api.get_option)(session, name.as_ptr()) != 0 }
    }

    pub fn set_option(&self, session: SessionId, name: &str, value: bool) {
        let Some(name) = c_text(name) else { return };
        // SAFETY: the name outlives the call.
        unsafe { (self.api.set_option)(session, name.as_ptr(), i32::from(value)) }
    }

    /// The schemas of the user's list (`schema_list` of default.yaml).
    pub fn schemas(&self) -> Vec<Schema> {
        let mut list = ffi::SchemaList {
            size: 0,
            list: std::ptr::null_mut(),
        };
        // SAFETY: librime fills `list` with `size` items, freed after they are copied.
        unsafe {
            if (self.api.get_schema_list)(&raw mut list) == 0 {
                return Vec::new();
            }
            let schemas = if list.list.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(list.list, list.size)
                    .iter()
                    .map(|item| Schema {
                        id: ffi::text(item.schema_id),
                        name: ffi::text(item.name),
                    })
                    .collect()
            };
            (self.api.free_schema_list)(&raw mut list);
            schemas
        }
    }

    pub fn select_schema(&self, session: SessionId, schema: &str) -> bool {
        let Some(schema) = c_text(schema) else {
            return false;
        };
        // SAFETY: the id outlives the call.
        unsafe { (self.api.select_schema)(session, schema.as_ptr()) != 0 }
    }

    /// Picks the candidate `index` of the page shown.
    pub fn pick(&self, session: SessionId, index: usize) -> bool {
        // SAFETY: librime checks the index.
        unsafe { (self.api.select_candidate_on_current_page)(session, index) != 0 }
    }

    /// Turns the page, back or on.
    pub fn turn_page(&self, session: SessionId, back: bool) -> bool {
        // SAFETY: a plain call.
        unsafe { (self.api.change_page)(session, i32::from(back)) != 0 }
    }

    /// Ends the composition without committing it.
    pub fn clear(&self, session: SessionId) {
        // SAFETY: librime checks the id.
        unsafe { (self.api.clear_composition)(session) }
    }
}

impl Drop for Rime {
    fn drop(&mut self) {
        // SAFETY: the last call; no session is used after it.
        unsafe {
            (self.api.set_notification_handler)(None, std::ptr::null_mut());
            (self.api.finalize)();
        }
        if let Ok(mut guard) = NOTICES.lock() {
            *guard = None;
        }
    }
}

/// librime's modifier mask for a key event: the X11 modifiers it shares with omimed's
/// (Shift, Lock, Control, Mod1/Alt, Mod4/Super), and its own release bit.
pub fn mask(modifiers: u32, released: bool) -> u32 {
    use omime_sdk::modifiers as m;
    const RELEASE: u32 = 1 << 30;
    let mask = modifiers & (m::SHIFT | m::CAPS_LOCK | m::CONTROL | m::ALT | m::SUPER);
    if released { mask | RELEASE } else { mask }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_carry_x11_modifiers_and_the_release_bit() {
        use omime_sdk::modifiers as m;
        assert_eq!(mask(m::SHIFT | m::CONTROL, false), 0b101);
        assert_eq!(
            mask(m::NUM_LOCK, false),
            0,
            "Num Lock means nothing to Rime"
        );
        assert_eq!(mask(0, true), 1 << 30);
        assert_eq!(mask(m::SUPER, true), (1 << 6) | (1 << 30));
    }
}
