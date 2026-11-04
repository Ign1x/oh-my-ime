//! librime's C API, as much of it as the engine uses: `rime_get_api()` and the table of
//! functions it returns, and the table of the `levers` module for the settings, declared
//! by hand after `rime_api.h` and `rime_levers_api.h` of librime 1.17 (no bindgen, and
//! not the GPL `librime` crate). librime grows the table and its structs
//! at the end and says how much of them it has in `data_size`, so every function this
//! engine calls is checked against that before use ([`static_table`]).
//!
//! The safe wrapper is the parent module, [`crate::rime`].

use std::ffi::{c_char, c_int, c_void};

/// `Bool` in librime: an `int`.
pub type Bool = c_int;
pub type SessionId = usize;

/// `RimeTraits`.
#[repr(C)]
#[derive(Debug)]
pub struct Traits {
    pub data_size: c_int,
    pub shared_data_dir: *const c_char,
    pub user_data_dir: *const c_char,
    pub distribution_name: *const c_char,
    pub distribution_code_name: *const c_char,
    pub distribution_version: *const c_char,
    pub app_name: *const c_char,
    pub modules: *const *const c_char,
    pub min_log_level: c_int,
    pub log_dir: *const c_char,
    pub prebuilt_data_dir: *const c_char,
    pub staging_dir: *const c_char,
}

#[repr(C)]
#[derive(Debug)]
pub struct Composition {
    pub length: c_int,
    pub cursor_pos: c_int,
    pub sel_start: c_int,
    pub sel_end: c_int,
    pub preedit: *mut c_char,
}

#[repr(C)]
#[derive(Debug)]
pub struct Candidate {
    pub text: *mut c_char,
    pub comment: *mut c_char,
    pub reserved: *mut c_void,
}

#[repr(C)]
#[derive(Debug)]
pub struct Menu {
    pub page_size: c_int,
    pub page_no: c_int,
    pub is_last_page: Bool,
    pub highlighted_candidate_index: c_int,
    pub num_candidates: c_int,
    pub candidates: *mut Candidate,
    pub select_keys: *mut c_char,
}

#[repr(C)]
#[derive(Debug)]
pub struct Commit {
    pub data_size: c_int,
    pub text: *mut c_char,
}

#[repr(C)]
#[derive(Debug)]
pub struct Context {
    pub data_size: c_int,
    pub composition: Composition,
    pub menu: Menu,
    pub commit_text_preview: *mut c_char,
    pub select_labels: *mut *mut c_char,
}

#[repr(C)]
#[derive(Debug)]
pub struct Status {
    pub data_size: c_int,
    pub schema_id: *mut c_char,
    pub schema_name: *mut c_char,
    pub is_disabled: Bool,
    pub is_composing: Bool,
    pub is_ascii_mode: Bool,
    pub is_full_shape: Bool,
    pub is_simplified: Bool,
    pub is_traditional: Bool,
    pub is_ascii_punct: Bool,
}

#[repr(C)]
#[derive(Debug)]
pub struct SchemaListItem {
    pub schema_id: *mut c_char,
    pub name: *mut c_char,
    pub reserved: *mut c_void,
}

#[repr(C)]
#[derive(Debug)]
pub struct SchemaList {
    pub size: usize,
    pub list: *mut SchemaListItem,
}

/// `RimeConfig`: a configuration librime owns.
#[repr(C)]
#[derive(Debug)]
pub struct Config {
    pub ptr: *mut c_void,
}

/// `RimeModule`, a module of librime such as `levers`.
#[repr(C)]
#[derive(Debug)]
pub struct Module {
    pub data_size: c_int,
    pub module_name: *const c_char,
    pub initialize: Option<unsafe extern "C" fn()>,
    pub finalize: Option<unsafe extern "C" fn()>,
    /// Its own table of functions, which starts with a `data_size`.
    pub get_api: Option<unsafe extern "C" fn() -> *const c_int>,
}

/// `RimeCustomSettings`: a configuration file and the user's customizations of it
/// (`<id>.custom.yaml`). Opaque.
#[repr(C)]
#[derive(Debug)]
pub struct CustomSettings {
    _placeholder: c_char,
}

/// `RimeSwitcherSettings`: the customizations of `default.yaml`, with the schemas the
/// user can choose. A [`CustomSettings`] as well. Opaque.
#[repr(C)]
#[derive(Debug)]
pub struct SwitcherSettings {
    _placeholder: c_char,
}

/// `RimeLeversApi` of `rime_levers_api.h`, the table of the `levers` module, which
/// librime's settings programs use, up to `select_schemas`.
#[repr(C)]
#[derive(Debug)]
pub struct LeversApi {
    pub data_size: c_int,
    _custom_settings_init: Unused,
    pub custom_settings_destroy: unsafe extern "C" fn(*mut CustomSettings),
    pub load_settings: unsafe extern "C" fn(*mut CustomSettings) -> Bool,
    pub save_settings: unsafe extern "C" fn(*mut CustomSettings) -> Bool,
    _customize_bool: Unused,
    pub customize_int: unsafe extern "C" fn(*mut CustomSettings, *const c_char, c_int) -> Bool,
    _customize_double: Unused,
    pub customize_string:
        unsafe extern "C" fn(*mut CustomSettings, *const c_char, *const c_char) -> Bool,
    _is_first_run: Unused,
    pub settings_is_modified: unsafe extern "C" fn(*mut CustomSettings) -> Bool,
    pub settings_get_config: unsafe extern "C" fn(*mut CustomSettings, *mut Config) -> Bool,
    pub switcher_settings_init: unsafe extern "C" fn() -> *mut SwitcherSettings,
    pub get_available_schema_list:
        unsafe extern "C" fn(*mut SwitcherSettings, *mut SchemaList) -> Bool,
    pub get_selected_schema_list:
        unsafe extern "C" fn(*mut SwitcherSettings, *mut SchemaList) -> Bool,
    pub schema_list_destroy: unsafe extern "C" fn(*mut SchemaList),
    _get_schema_id: Unused,
    _get_schema_name: Unused,
    _get_schema_version: Unused,
    _get_schema_author: Unused,
    _get_schema_description: Unused,
    _get_schema_file_path: Unused,
    pub select_schemas:
        unsafe extern "C" fn(*mut SwitcherSettings, *const *const c_char, c_int) -> Bool,
}

/// What librime calls with `(context_object, session, message_type, message_value)`.
pub type NotificationHandler =
    unsafe extern "C" fn(*mut c_void, SessionId, *const c_char, *const c_char);

/// An entry of the table this engine does not call: only its size matters.
type Unused = *const c_void;

/// `RimeApi`, up to `change_page` (librime 1.11).
#[repr(C)]
#[derive(Debug)]
pub struct Api {
    pub data_size: c_int,
    pub setup: unsafe extern "C" fn(*mut Traits),
    pub set_notification_handler: unsafe extern "C" fn(Option<NotificationHandler>, *mut c_void),
    pub initialize: unsafe extern "C" fn(*mut Traits),
    pub finalize: unsafe extern "C" fn(),
    pub start_maintenance: unsafe extern "C" fn(Bool) -> Bool,
    pub is_maintenance_mode: unsafe extern "C" fn() -> Bool,
    pub join_maintenance_thread: unsafe extern "C" fn(),
    pub deployer_initialize: unsafe extern "C" fn(*mut Traits),
    _prebuild: Unused,
    _deploy: Unused,
    _deploy_schema: Unused,
    pub deploy_config_file: unsafe extern "C" fn(*const c_char, *const c_char) -> Bool,
    _sync_user_data: Unused,
    pub create_session: unsafe extern "C" fn() -> SessionId,
    pub find_session: unsafe extern "C" fn(SessionId) -> Bool,
    pub destroy_session: unsafe extern "C" fn(SessionId) -> Bool,
    _cleanup_stale_sessions: Unused,
    _cleanup_all_sessions: Unused,
    pub process_key: unsafe extern "C" fn(SessionId, c_int, c_int) -> Bool,
    _commit_composition: Unused,
    pub clear_composition: unsafe extern "C" fn(SessionId),
    pub get_commit: unsafe extern "C" fn(SessionId, *mut Commit) -> Bool,
    pub free_commit: unsafe extern "C" fn(*mut Commit) -> Bool,
    pub get_context: unsafe extern "C" fn(SessionId, *mut Context) -> Bool,
    pub free_context: unsafe extern "C" fn(*mut Context) -> Bool,
    pub get_status: unsafe extern "C" fn(SessionId, *mut Status) -> Bool,
    pub free_status: unsafe extern "C" fn(*mut Status) -> Bool,
    pub set_option: unsafe extern "C" fn(SessionId, *const c_char, Bool),
    pub get_option: unsafe extern "C" fn(SessionId, *const c_char) -> Bool,
    _set_property: Unused,
    _get_property: Unused,
    pub get_schema_list: unsafe extern "C" fn(*mut SchemaList) -> Bool,
    pub free_schema_list: unsafe extern "C" fn(*mut SchemaList),
    _get_current_schema: Unused,
    pub select_schema: unsafe extern "C" fn(SessionId, *const c_char) -> Bool,
    _schema_open: Unused,
    _config_open: Unused,
    _config_close: Unused,
    _config_get_bool: Unused,
    pub config_get_int: unsafe extern "C" fn(*mut Config, *const c_char, *mut c_int) -> Bool,
    _config_get_double: Unused,
    _config_get_string: Unused,
    pub config_get_cstring: unsafe extern "C" fn(*mut Config, *const c_char) -> *const c_char,
    _config_update_signature: Unused,
    _config_begin_map: Unused,
    _config_next: Unused,
    _config_end: Unused,
    _simulate_key_sequence: Unused,
    _register_module: Unused,
    pub find_module: unsafe extern "C" fn(*const c_char) -> *mut Module,
    _run_task: Unused,
    _get_shared_data_dir: Unused,
    _get_user_data_dir: Unused,
    _get_sync_dir: Unused,
    _get_user_id: Unused,
    _get_user_data_sync_dir: Unused,
    _config_init: Unused,
    _config_load_string: Unused,
    _config_set_bool: Unused,
    _config_set_int: Unused,
    _config_set_double: Unused,
    _config_set_string: Unused,
    _config_get_item: Unused,
    _config_set_item: Unused,
    _config_clear: Unused,
    _config_create_list: Unused,
    _config_create_map: Unused,
    _config_list_size: Unused,
    _config_begin_list: Unused,
    _get_input: Unused,
    _get_caret_pos: Unused,
    _select_candidate: Unused,
    pub get_version: unsafe extern "C" fn() -> *const c_char,
    _set_caret_pos: Unused,
    pub select_candidate_on_current_page: unsafe extern "C" fn(SessionId, usize) -> Bool,
    _candidate_list_begin: Unused,
    _candidate_list_next: Unused,
    _candidate_list_end: Unused,
    _user_config_open: Unused,
    _candidate_list_from_index: Unused,
    _get_prebuilt_data_dir: Unused,
    _get_staging_dir: Unused,
    _commit_proto: Unused,
    _context_proto: Unused,
    _status_proto: Unused,
    _get_state_label: Unused,
    _delete_candidate: Unused,
    _delete_candidate_on_current_page: Unused,
    _get_state_label_abbreviated: Unused,
    _set_input: Unused,
    _get_shared_data_dir_s: Unused,
    _get_user_data_dir_s: Unused,
    _get_prebuilt_data_dir_s: Unused,
    _get_staging_dir_s: Unused,
    _get_sync_dir_s: Unused,
    _highlight_candidate: Unused,
    _highlight_candidate_on_current_page: Unused,
    pub change_page: unsafe extern "C" fn(SessionId, Bool) -> Bool,
}

#[link(name = "rime")]
unsafe extern "C" {
    fn rime_get_api() -> *const Api;
}

impl Api {
    /// librime's table of functions, if it has every one this engine calls.
    pub fn get() -> Option<&'static Api> {
        // SAFETY: rime_get_api takes nothing and returns a pointer to a static table
        // (or null), valid for the life of the process.
        unsafe { static_table(rime_get_api().cast()) }
    }
}

/// Checks a table's header before forming a reference that covers its whole layout.
///
/// # Safety
///
/// `pointer` is null or a static librime table aligned for `T`. Its first integer is
/// `data_size`, the available bytes after that integer. When large enough, the table
/// must contain a valid `T` and stay immutable for the lifetime of the process.
pub unsafe fn static_table<T>(pointer: *const c_int) -> Option<&'static T> {
    // SAFETY: only the header is read before checking the table's size.
    let size = unsafe { pointer.as_ref()? };
    let declared = std::mem::size_of::<T>() - std::mem::size_of::<c_int>();
    if usize::try_from(*size).ok()? < declared {
        return None;
    }
    // SAFETY: the header covers the full table, valid and static by the caller.
    unsafe { pointer.cast::<T>().as_ref() }
}

// SAFETY: the table is static data librime never changes after `rime_get_api`, and
// every call through it is made under the engine's lock (see `crate::rime`), so sharing
// the reference between threads shares nothing mutable.
unsafe impl Sync for Api {}

/// `RIME_STRUCT_INIT`: `data_size` is the size of the struct after it.
pub fn data_size<T>() -> c_int {
    c_int::try_from(std::mem::size_of::<T>() - std::mem::size_of::<c_int>()).unwrap_or(c_int::MAX)
}

/// A zeroed struct with its `data_size`, as librime expects it.
pub fn zeroed_commit() -> Commit {
    Commit {
        data_size: data_size::<Commit>(),
        text: std::ptr::null_mut(),
    }
}

pub fn zeroed_context() -> Context {
    Context {
        data_size: data_size::<Context>(),
        composition: Composition {
            length: 0,
            cursor_pos: 0,
            sel_start: 0,
            sel_end: 0,
            preedit: std::ptr::null_mut(),
        },
        menu: Menu {
            page_size: 0,
            page_no: 0,
            is_last_page: 0,
            highlighted_candidate_index: 0,
            num_candidates: 0,
            candidates: std::ptr::null_mut(),
            select_keys: std::ptr::null_mut(),
        },
        commit_text_preview: std::ptr::null_mut(),
        select_labels: std::ptr::null_mut(),
    }
}

pub fn zeroed_status() -> Status {
    Status {
        data_size: data_size::<Status>(),
        schema_id: std::ptr::null_mut(),
        schema_name: std::ptr::null_mut(),
        is_disabled: 0,
        is_composing: 0,
        is_ascii_mode: 0,
        is_full_shape: 0,
        is_simplified: 0,
        is_traditional: 0,
        is_ascii_punct: 0,
    }
}

/// The text of a C string librime owns, copied; empty for null.
///
/// # Safety
///
/// `text` is null or points at a NUL-terminated string that lives for the call.
pub unsafe fn text(text: *const c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: not null, and NUL-terminated by the caller's promise.
    unsafe { std::ffi::CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}
