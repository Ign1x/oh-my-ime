//! Where Rime's data is, and what oh-my-ime changes in it the first time.
//!
//! The shared data (schemas and dictionaries, such as the `rime-data` package) comes
//! from `$OMIME_RIME_SHARED_DATA`, else from `rime-data` in `$XDG_DATA_DIRS`. The user's
//! data, where Rime builds the schemas and learns, is
//! `$XDG_DATA_HOME/oh-my-ime/rime`: apart from other Rime front ends', which build for
//! their own versions of librime.
//!
//! The first time, oh-my-ime writes `default.custom.yaml` there: Xiaohe shuangpin
//! (`double_pinyin_flypy`) first, then the pinyin of the shared data, and Shift
//! switching between Chinese and English letters. It also turns simplified characters
//! on for those schemas, which rime-data has in traditional ones. Files that exist are
//! the user's, and stay.

use std::path::{Path, PathBuf};

/// Overrides where the shared data is.
pub const SHARED_DATA_ENV: &str = "OMIME_RIME_SHARED_DATA";

/// The schemas oh-my-ime puts in the list the first time, in order, where the shared
/// data has them.
const SCHEMAS: [&str; 2] = ["double_pinyin_flypy", "luna_pinyin"];

/// Rime's shared data: the first directory with a `default.yaml`.
pub fn shared_dir() -> Option<PathBuf> {
    let has_defaults = |dir: &Path| dir.join("default.yaml").is_file();
    if let Some(dir) = std::env::var_os(SHARED_DATA_ENV).map(PathBuf::from) {
        return has_defaults(&dir).then_some(dir);
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    std::env::split_paths(&data_dirs)
        .map(|dir| dir.join("rime-data"))
        .find(|dir| has_defaults(dir))
}

/// The user's Rime data of oh-my-ime.
pub fn user_dir() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/share")))?;
    Some(data.join("oh-my-ime/rime"))
}

/// Writes oh-my-ime's first-time customizations into `user` (see the module docs),
/// for the schemas `shared` has. Leaves files that exist alone.
pub fn customize(shared: &Path, user: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(user)?;
    let schemas: Vec<&str> = SCHEMAS
        .into_iter()
        .filter(|id| shared.join(format!("{id}.schema.yaml")).is_file())
        .collect();
    write_new(&user.join("default.custom.yaml"), &defaults(&schemas))?;
    for id in schemas {
        write_new(&user.join(format!("{id}.custom.yaml")), &simplified())?;
    }
    Ok(())
}

/// `default.custom.yaml`.
fn defaults(schemas: &[&str]) -> String {
    let mut text = String::from(
        "# oh-my-ime's defaults, written once; yours to change. Run `omime activate deploy`\n\
         # (or \"Deploy again\" in Rime's menu) after changing it.\n\
         patch:\n",
    );
    if !schemas.is_empty() {
        text += "  schema_list:\n";
        for id in schemas {
            text += &format!("    - schema: {id}\n");
        }
    }
    // Shift alone switches between Chinese and English letters, and gives what was
    // typed so far as letters.
    text += "  \"ascii_composer/switch_key/Shift_L\": commit_code\n";
    text += "  \"ascii_composer/switch_key/Shift_R\": commit_code\n";
    text
}

/// A schema's patch for simplified characters. Both schemas of [`SCHEMAS`] have the
/// switch at the third place: `simplification` (traditional, simplified) in
/// double_pinyin_flypy, the options (zh_hant, zh_hans, zh_hant_hk) in luna_pinyin.
fn simplified() -> String {
    "# oh-my-ime: simplified characters first. Yours to change.\npatch:\n  \"switches/@2/reset\": 1\n"
        .to_owned()
}

fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => std::io::Write::write_all(&mut file, text.as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_time_puts_xiaohe_first_and_keeps_the_users_files() {
        let dir = std::env::temp_dir().join(format!("omime-rime-data-{}", std::process::id()));
        let (shared, user) = (dir.join("shared"), dir.join("user"));
        std::fs::create_dir_all(&shared).expect("a directory");
        for id in ["double_pinyin_flypy", "luna_pinyin", "cangjie5"] {
            std::fs::write(shared.join(format!("{id}.schema.yaml")), "").expect("a file");
        }
        customize(&shared, &user).expect("customized");
        let defaults = std::fs::read_to_string(user.join("default.custom.yaml")).expect("read");
        let flypy = defaults.find("double_pinyin_flypy").expect("Xiaohe");
        let luna = defaults.find("luna_pinyin").expect("pinyin");
        assert!(flypy < luna, "{defaults}");
        assert!(!defaults.contains("cangjie5"), "{defaults}");
        assert!(user.join("luna_pinyin.custom.yaml").is_file());

        std::fs::write(user.join("default.custom.yaml"), "mine").expect("written");
        customize(&shared, &user).expect("customized");
        let kept = std::fs::read_to_string(user.join("default.custom.yaml")).expect("read");
        assert_eq!(kept, "mine");
        let _ = std::fs::remove_dir_all(dir);
    }
}
