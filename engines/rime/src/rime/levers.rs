//! Rime's settings, changed as librime's own settings programs change them, through its
//! `levers` module: the schemas there are and those in the user's list, and
//! customizations of `default.yaml`, which librime writes into `default.custom.yaml` in
//! the user's data. librime is set up as a deployer only, without sessions, in the
//! settings window's process; the engine deploys what changed when it starts again.

use std::ffi::{CString, c_char, c_int};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use super::ffi::{self, Api, CustomSettings, LeversApi, SchemaList, SwitcherSettings};
use super::{TraitStrings, c_path, c_text};

/// A schema the user can choose, such as `double_pinyin_flypy` (小鶴雙拼).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    pub id: String,
    pub name: String,
}

/// librime's settings of `default.yaml`, loaded.
pub struct Levers {
    api: &'static Api,
    levers: &'static LeversApi,
    settings: NonNull<SwitcherSettings>,
    strings: TraitStrings,
    user: PathBuf,
}

impl std::fmt::Debug for Levers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Levers").finish_non_exhaustive()
    }
}

impl Levers {
    /// Sets librime up for the shared data in `shared` and the user's in `user`, and
    /// loads the settings. `default.custom.yaml` must exist in `user`: librime reads
    /// nothing, the schemas there are included, without it.
    pub fn open(shared: &Path, user: &Path) -> Result<Self, String> {
        let api = Api::get().ok_or("librime is older than 1.11, or not librime")?;
        let strings = TraitStrings::new(shared, user)?;
        let mut traits = strings.traits();
        let name = c_text("levers").ok_or("a NUL")?;
        // SAFETY: the strings outlive the calls, which copy them; this process sets
        // librime up once, here, as a deployer, which loads the levers module. The module
        // and the table it returns are static data of librime.
        let levers = unsafe {
            (api.setup)(&raw mut traits);
            (api.deployer_initialize)(&raw mut traits);
            let module = ffi::static_table::<ffi::Module>((api.find_module)(name.as_ptr()).cast())
                .ok_or("librime has no settings module (levers)")?;
            let get_api = module
                .get_api
                .ok_or("librime's settings module has no table")?;
            ffi::static_table::<LeversApi>(get_api())
                .ok_or("librime's settings module has an incomplete table")?
        };
        // SAFETY: a plain call; the settings are freed in `drop`.
        let settings = NonNull::new(unsafe { (levers.switcher_settings_init)() })
            .ok_or("librime has no settings for default.yaml")?;
        let this = Self {
            api,
            levers,
            settings,
            strings,
            user: user.to_owned(),
        };
        this.load(user)?;
        // librime takes the user's list from default.yaml as deployed, which it is not
        // before the engine first ran: build it, with the user's customizations.
        if this.selected().is_empty() {
            let file = c_text("default.yaml").ok_or("a NUL")?;
            let version = c_text("config_version").ok_or("a NUL")?;
            // SAFETY: the strings outlive the call.
            unsafe { (api.deploy_config_file)(file.as_ptr(), version.as_ptr()) };
            this.load(user)?;
        }
        Ok(this)
    }

    fn load(&self, user: &Path) -> Result<(), String> {
        // SAFETY: the settings are alive.
        if unsafe { (self.levers.load_settings)(self.custom()) } == 0 {
            return Err(format!("cannot read {CUSTOM} in {}", user.display()));
        }
        Ok(())
    }

    /// Discards unsaved edits before applying a new set of choices in the window.
    pub fn reset(&self) -> Result<(), String> {
        self.load(&self.user)
    }

    /// The switcher's settings are custom settings of `default.yaml` as well.
    fn custom(&self) -> *mut CustomSettings {
        self.settings.as_ptr().cast()
    }

    /// The schemas of the shared and the user's data, in librime's order.
    pub fn available(&self) -> Vec<Available> {
        let mut list = empty_list();
        // SAFETY: librime fills `list` with `size` items whose strings belong to the
        // settings; they are copied before the list itself is freed.
        unsafe {
            if (self.levers.get_available_schema_list)(self.settings.as_ptr(), &raw mut list) == 0 {
                return Vec::new();
            }
            let schemas = items(&list)
                .iter()
                .map(|item| Available {
                    id: ffi::text(item.schema_id),
                    name: ffi::text(item.name),
                })
                .collect();
            (self.levers.schema_list_destroy)(&raw mut list);
            schemas
        }
    }

    /// The ids of the schemas in the user's list, in order.
    pub fn selected(&self) -> Vec<String> {
        let mut list = empty_list();
        // SAFETY: as in `available`; these items have only their ids.
        unsafe {
            if (self.levers.get_selected_schema_list)(self.settings.as_ptr(), &raw mut list) == 0 {
                return Vec::new();
            }
            let ids = items(&list)
                .iter()
                .map(|item| ffi::text(item.schema_id))
                .collect();
            (self.levers.schema_list_destroy)(&raw mut list);
            ids
        }
    }

    /// The configuration as last deployed, before what is customized here.
    fn config(&self) -> Option<ffi::Config> {
        let mut config = ffi::Config {
            ptr: std::ptr::null_mut(),
        };
        // SAFETY: librime points `config` at the settings' own configuration, which
        // lives as long as they do and is not to be closed.
        let found = unsafe { (self.levers.settings_get_config)(self.custom(), &raw mut config) };
        (found != 0 && !config.ptr.is_null()).then_some(config)
    }

    /// An integer of the configuration, such as `menu/page_size`.
    pub fn int(&self, key: &str) -> Option<i32> {
        let mut config = self.config()?;
        let key = c_text(key)?;
        let mut value: c_int = 0;
        // SAFETY: the configuration is alive; the key outlives the call.
        let found =
            unsafe { (self.api.config_get_int)(&raw mut config, key.as_ptr(), &raw mut value) };
        (found != 0).then_some(value)
    }

    /// A string of the configuration, such as `ascii_composer/switch_key/Shift_L`.
    pub fn string(&self, key: &str) -> Option<String> {
        let mut config = self.config()?;
        let key = c_text(key)?;
        // SAFETY: the configuration is alive; the string librime returns belongs to it
        // and is copied.
        unsafe {
            let value = (self.api.config_get_cstring)(&raw mut config, key.as_ptr());
            (!value.is_null()).then(|| ffi::text(value))
        }
    }

    /// Makes `ids` the user's list of schemas, in this order.
    pub fn select(&self, ids: &[String]) -> bool {
        let Ok(ids) = ids
            .iter()
            .map(|id| CString::new(id.as_str()))
            .collect::<Result<Vec<CString>, _>>()
        else {
            return false;
        };
        let pointers: Vec<*const c_char> = ids.iter().map(|id| id.as_ptr()).collect();
        let Ok(count) = c_int::try_from(pointers.len()) else {
            return false;
        };
        // SAFETY: `count` pointers to strings that outlive the call, which copies them.
        unsafe {
            (self.levers.select_schemas)(self.settings.as_ptr(), pointers.as_ptr(), count) != 0
        }
    }

    /// Customizes the integer at `key`, such as `menu/page_size`.
    pub fn customize_int(&self, key: &str, value: i32) -> bool {
        let Some(key) = c_text(key) else { return false };
        // SAFETY: the key outlives the call.
        unsafe { (self.levers.customize_int)(self.custom(), key.as_ptr(), value) != 0 }
    }

    /// Customizes the string at `key`.
    pub fn customize_string(&self, key: &str, value: &str) -> bool {
        let (Some(key), Some(value)) = (c_text(key), c_text(value)) else {
            return false;
        };
        // SAFETY: the strings outlive the call.
        unsafe { (self.levers.customize_string)(self.custom(), key.as_ptr(), value.as_ptr()) != 0 }
    }

    /// Writes the customization into a temporary directory and replaces
    /// `default.custom.yaml` with it at once. librime 1.17 reports success even if its
    /// file write failed, so the file is checked here. Nothing changed is a success;
    /// after an error the choices are to be made again (see [`Self::reset`]).
    pub fn save(&self) -> std::io::Result<()> {
        // SAFETY: the settings are alive, used only on the settings window's thread.
        if unsafe { (self.levers.settings_is_modified)(self.custom()) } == 0 {
            return Ok(());
        }
        let stage = tempfile::Builder::new()
            .prefix(".omime-settings-")
            .tempdir_in(&self.user)?;
        let directory = c_path(stage.path())
            .ok_or_else(|| std::io::Error::other("a NUL in the settings directory"))?;
        let mut traits = self.strings.traits();
        traits.user_data_dir = directory.as_ptr();
        // SAFETY: no sessions or other threads use this process's deployer.
        // The strings outlive the calls; CustomSettings holds this same deployer.
        // Always restore its real directories before returning, including on error.
        let saved = unsafe {
            (self.api.deployer_initialize)(&raw mut traits);
            let saved = (self.levers.save_settings)(self.custom());
            traits = self.strings.traits();
            (self.api.deployer_initialize)(&raw mut traits);
            saved
        };
        if saved == 0 {
            return Err(std::io::Error::other("Rime could not save its settings"));
        }
        let staged = stage.path().join(CUSTOM);
        let file = std::fs::File::open(&staged)?;
        if file.metadata()?.len() == 0 {
            return Err(std::io::Error::other("Rime wrote empty settings"));
        }
        file.sync_all()?;
        replace(&staged, &self.user.join(CUSTOM))
    }
}

/// The user's customization of `default.yaml`, in their Rime data.
const CUSTOM: &str = "default.custom.yaml";

/// Puts `staged` in the place of `destination`, at once. A link stays a link, as
/// dotfiles kept elsewhere are: the file it points to is replaced, which fails where
/// that cannot be written.
fn replace(staged: &Path, destination: &Path) -> std::io::Result<()> {
    if !destination.is_symlink() {
        return std::fs::rename(staged, destination);
    }
    let target = destination.canonicalize()?;
    let directory = target
        .parent()
        .ok_or_else(|| std::io::Error::other("a link to no file"))?;
    // Renaming does not cross file systems: copy beside the file first.
    let beside = tempfile::Builder::new()
        .prefix(".omime-settings-")
        .tempfile_in(directory)?;
    std::fs::copy(staged, beside.path())?;
    beside.as_file().sync_all()?;
    beside.persist(&target).map_err(|e| e.error)?;
    Ok(())
}

impl Drop for Levers {
    fn drop(&mut self) {
        // SAFETY: the settings are not used after this; librime is finalized last.
        unsafe {
            (self.levers.custom_settings_destroy)(self.custom());
            (self.api.finalize)();
        }
    }
}

fn empty_list() -> SchemaList {
    SchemaList {
        size: 0,
        list: std::ptr::null_mut(),
    }
}

/// The items of a list librime filled.
///
/// # Safety
///
/// `list` is a list librime filled and has not freed: `size` items at `list`.
unsafe fn items(list: &SchemaList) -> &[ffi::SchemaListItem] {
    if list.list.is_null() {
        return &[];
    }
    // SAFETY: by the caller's promise.
    unsafe { std::slice::from_raw_parts(list.list, list.size) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against librime and the Rime data of the dev shell, in a user directory of its
    /// own: the only test of this crate that starts librime, which is once per process.
    #[test]
    fn schemas_and_customizations_are_written_as_librime_writes_them() {
        let Some(shared) = std::env::var_os(crate::data::SHARED_DATA_ENV).map(PathBuf::from) else {
            eprintln!("no Rime data; nothing to test");
            return;
        };
        let user = std::env::temp_dir().join(format!("omime-rime-levers-{}", std::process::id()));
        let elsewhere =
            std::env::temp_dir().join(format!("omime-rime-dotfiles-{}", std::process::id()));
        crate::data::customize(&shared, &user).expect("customized");
        {
            let levers = Levers::open(&shared, &user).expect("librime's settings");
            let available = levers.available();
            assert!(
                available.iter().any(|s| s.id == "cangjie5"),
                "{available:?}"
            );
            assert_eq!(
                levers.selected(),
                ["double_pinyin_flypy", "luna_pinyin"],
                "oh-my-ime's first list, though nothing was deployed yet"
            );
            assert_eq!(levers.int("menu/page_size"), Some(5));
            assert!(levers.select(&["luna_pinyin".to_owned(), "cangjie5".to_owned()]));
            assert!(levers.customize_int("menu/page_size", 7));
            assert!(levers.customize_string("ascii_composer/switch_key/Shift_L", "noop"));
            levers.save().expect("save settings");

            // A failed save is reported and leaves the file as it was; the choices
            // made again are saved, as the window does.
            let path = user.join(CUSTOM);
            let backup = user.join("default.custom.backup");
            std::fs::rename(&path, &backup).expect("back up the settings");
            std::fs::create_dir(&path).expect("block the destination");
            assert!(levers.customize_int("menu/page_size", 8));
            assert!(
                levers.save().is_err(),
                "a directory cannot be replaced with settings"
            );
            std::fs::remove_dir(&path).expect("unblock the destination");
            std::fs::rename(&backup, &path).expect("restore the settings");
            let kept = std::fs::read_to_string(&path).expect("kept settings");
            assert!(kept.contains("\"menu/page_size\": 7"), "{kept}");
            levers.reset().expect("read again");
            assert!(levers.customize_int("menu/page_size", 8));
            levers.save().expect("save the choices made again");
            let retried = std::fs::read_to_string(&path).expect("retried settings");
            assert!(retried.contains("\"menu/page_size\": 8"), "{retried}");

            // Dotfiles kept elsewhere: the link stays, the file it points to changes.
            std::fs::create_dir(&elsewhere).expect("a directory elsewhere");
            let target = elsewhere.join("rime-default.yaml");
            std::fs::rename(&path, &target).expect("move the settings elsewhere");
            std::os::unix::fs::symlink(&target, &path).expect("link them");
            levers.reset().expect("read through the link");
            assert!(levers.customize_int("menu/page_size", 9));
            levers.save().expect("save through the link");
            assert!(path.is_symlink(), "the link was replaced with a file");
            assert_eq!(
                std::fs::read_to_string(&path).expect("linked settings"),
                std::fs::read_to_string(&target).expect("settings elsewhere"),
            );
        }
        let written = std::fs::read_to_string(user.join(CUSTOM)).expect("written");
        let _ = std::fs::remove_dir_all(&user);
        let _ = std::fs::remove_dir_all(&elsewhere);
        let luna = written.find("luna_pinyin").expect(&written);
        let cangjie = written.find("cangjie5").expect(&written);
        assert!(luna < cangjie, "{written}");
        assert!(!written.contains("double_pinyin_flypy"), "{written}");
        assert!(written.contains("\"menu/page_size\": 9"), "{written}");
        assert!(
            written.contains("\"ascii_composer/switch_key/Shift_L\": noop"),
            "{written}"
        );
        // What oh-my-ime wrote the first time and nobody changed stays.
        assert!(
            written.contains("\"ascii_composer/switch_key/Shift_R\": commit_code"),
            "{written}"
        );
    }
}
