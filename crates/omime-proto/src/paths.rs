//! Standard XDG locations, private file writes and executable lookup.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Replaces a file through an exclusively created sibling. Existing permissions
/// are kept (including through a destination symlink); new files are `0600`.
/// `mode` overrides this for private history such as Sparkle's recent emoji.
///
/// Configuration writers set `sync` to flush the completed file before renaming.
/// Frequent state/cache writers leave it off to avoid stalling input on fsync.
/// A failure leaves the destination intact and removes our temporary file.
/// Renaming replaces a destination symlink, without changing its target.
pub fn atomic_write(path: &Path, contents: &[u8], mode: Option<u32>, sync: bool) -> io::Result<()> {
    atomic_write_with(path, mode, sync, |file| file.write_all(contents))
}

fn atomic_write_with(
    path: &Path,
    mode: Option<u32>,
    sync: bool,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mode = match mode {
        Some(mode) => mode,
        None => match fs::metadata(path) {
            Ok(metadata) => metadata.permissions().mode() & 0o7777,
            Err(e) if e.kind() == io::ErrorKind::NotFound => 0o600,
            Err(e) => return Err(e),
        },
    };
    let (temp, mut file) = loop {
        let temp = parent.join(format!(
            ".omime-{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
        {
            Ok(file) => break (temp, file),
            // A stale file (or symlink) is never opened, truncated or removed.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        write(&mut file)?;
        // Apply the final mode after writing: a partial temporary stays private,
        // and a restrictive umask cannot silently change an existing file's mode.
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        if sync {
            file.sync_all()?;
        }
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Creates a `0700` data directory, or restricts an existing one to that mode.
/// Follows directory symlinks and restricts their targets, leaving links in place.
/// Missing parents are created privately as well; existing parent directories
/// keep their permissions. Non-directories and unresolved links return an error.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let metadata = fs::metadata(dir)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            crate::tr!("private data path is not a directory"),
        ));
    }
    if metadata.permissions().mode() & 0o7777 != 0o700 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Name of the project directory inside XDG base directories.
pub const PROJECT_DIR: &str = "oh-my-ime";

fn home() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    match env::var_os(var) {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => home().map(|h| h.join(fallback)),
    }
}

/// `$XDG_CONFIG_HOME/oh-my-ime`.
pub fn config_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config").map(|d| d.join(PROJECT_DIR))
}

/// `$XDG_STATE_HOME/oh-my-ime`.
pub fn state_dir() -> Option<PathBuf> {
    xdg_dir("XDG_STATE_HOME", ".local/state").map(|d| d.join(PROJECT_DIR))
}

/// `$XDG_CACHE_HOME/oh-my-ime`.
pub fn cache_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CACHE_HOME", ".cache").map(|d| d.join(PROJECT_DIR))
}

/// `$XDG_DATA_HOME/oh-my-ime/engines`, the directory `omime install` writes to.
pub fn user_engine_dir() -> Option<PathBuf> {
    xdg_dir("XDG_DATA_HOME", ".local/share").map(|d| d.join(PROJECT_DIR).join("engines"))
}

/// Directories searched for engines, highest priority first:
///
/// 1. every entry of `$OMIME_ENGINE_PATH` (colon separated; handy for development),
/// 2. [`user_engine_dir`] (user installs),
/// 3. `<dir>/oh-my-ime/engines` for each entry of `$XDG_DATA_DIRS`.
pub fn engine_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(extra) = env::var_os("OMIME_ENGINE_PATH") {
        dirs.extend(env::split_paths(&extra).filter(|p| !p.as_os_str().is_empty()));
    }
    if let Some(user) = user_engine_dir() {
        dirs.push(user);
    }
    let data_dirs = env::var_os("XDG_DATA_DIRS")
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(
        env::split_paths(&data_dirs)
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.join(PROJECT_DIR).join("engines")),
    );
    dirs
}

/// Finds an executable regular file, following symlinks and checking execute access
/// for the current user. Names containing `/` are paths used as supplied; bare names
/// are searched in `$PATH` order. Empty entries mean the current directory, while an
/// absent `$PATH` has no entries. Returned paths retain their symlinks and may be
/// relative to the current directory.
pub fn find_program(program: impl AsRef<Path>) -> Option<PathBuf> {
    find_program_in(program.as_ref(), env::var_os("PATH").as_deref())
}

fn find_program_in(program: &Path, path: Option<&OsStr>) -> Option<PathBuf> {
    let executable = |path: &Path| {
        path.is_file() && rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
    };
    if program.as_os_str().as_encoded_bytes().contains(&b'/') {
        return executable(program).then(|| program.to_path_buf());
    }
    env::split_paths(path?)
        .map(|dir| {
            // Keep a slash in the result so Command does not search PATH again.
            if dir.as_os_str().is_empty() {
                Path::new(".").join(program)
            } else {
                dir.join(program)
            }
        })
        .find(|path| executable(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = env::temp_dir().join(format!("omime-paths-test-{}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // umask is process-global: each permission probe gets its own test process,
    // even when the outer cargo test runs many cases concurrently.
    fn under_umask_022(name: &str, test: impl FnOnce()) {
        if env::var("OMIME_PATHS_TEST").as_deref() == Ok(name) {
            test();
            return;
        }
        let output = Command::new("sh")
            .args(["-c", "umask 022; exec \"$@\"", "omime-paths-test"])
            .arg(env::current_exe().unwrap())
            .args(["--exact", &format!("paths::tests::{name}"), "--nocapture"])
            .env("OMIME_PATHS_TEST", name)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn entries(dir: &Path) -> usize {
        fs::read_dir(dir).unwrap().count()
    }

    #[test]
    fn atomic_write_keeps_existing_permissions() {
        under_umask_022("atomic_write_keeps_existing_permissions", || {
            let dir = TestDir::new();
            let path = dir.0.join("config.toml");
            for expected in [0o600, 0o640, 0o644] {
                fs::write(&path, "old").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(expected)).unwrap();
                atomic_write(&path, b"new", None, true).unwrap();
                assert_eq!(mode(&path), expected);
                assert_eq!(fs::read(&path).unwrap(), b"new");
                assert_eq!(entries(&dir.0), 1);
            }
        });
    }

    #[test]
    fn atomic_write_creates_private_files_and_can_restrict_history() {
        under_umask_022(
            "atomic_write_creates_private_files_and_can_restrict_history",
            || {
                let dir = TestDir::new();
                let path = dir.0.join("state");
                atomic_write(&path, b"new", None, false).unwrap();
                assert_eq!(mode(&path), 0o600);
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                atomic_write(&path, b"private", Some(0o600), false).unwrap();
                assert_eq!(mode(&path), 0o600);
                assert_eq!(entries(&dir.0), 1);
            },
        );
    }

    #[test]
    fn failed_write_keeps_old_file_and_cleans_temporary() {
        under_umask_022("failed_write_keeps_old_file_and_cleans_temporary", || {
            let dir = TestDir::new();
            let path = dir.0.join("state");
            fs::write(&path, "previous").unwrap();
            let result = atomic_write_with(&path, None, false, |file| {
                file.write_all(b"partial")?;
                assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
                assert_eq!(fs::read(&path)?, b"previous");
                Err(io::Error::other("injected write failure"))
            });
            assert!(result.is_err());
            assert_eq!(fs::read(&path).unwrap(), b"previous");
            assert_eq!(entries(&dir.0), 1);

            // A failed final rename has the same cleanup guarantee.
            let directory = dir.0.join("directory");
            fs::create_dir(&directory).unwrap();
            assert!(atomic_write(&directory, b"new", None, true).is_err());
            assert!(directory.is_dir());
            assert_eq!(entries(&dir.0), 2);
        });
    }

    #[test]
    fn stale_temporary_files_and_symlinks_are_left_alone() {
        under_umask_022("stale_temporary_files_and_symlinks_are_left_alone", || {
            let dir = TestDir::new();
            let path = dir.0.join("config.toml");
            let first = NEXT_TEMP.load(Ordering::Relaxed);
            let temp = |index| {
                dir.0
                    .join(format!(".omime-{}-{index}.tmp", std::process::id()))
            };
            let stale = temp(first);
            fs::write(&stale, "stale").unwrap();
            let target = dir.0.join("target");
            fs::write(&target, "untouched").unwrap();
            let symlink = temp(first + 1);
            std::os::unix::fs::symlink(&target, &symlink).unwrap();
            atomic_write(&path, b"new", None, true).unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"new");
            assert_eq!(fs::read(&stale).unwrap(), b"stale");
            assert_eq!(fs::read(&target).unwrap(), b"untouched");
            assert!(symlink.is_symlink());
            assert_eq!(entries(&dir.0), 4);
        });
    }

    #[test]
    fn atomic_write_replaces_a_destination_symlink() {
        under_umask_022("atomic_write_replaces_a_destination_symlink", || {
            let dir = TestDir::new();
            let target = dir.0.join("target");
            fs::write(&target, "original").unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
            let path = dir.0.join("config.toml");
            std::os::unix::fs::symlink(&target, &path).unwrap();
            atomic_write(&path, b"replacement", None, true).unwrap();
            assert!(!path.is_symlink());
            assert_eq!(mode(&path), 0o640);
            assert_eq!(fs::read(&target).unwrap(), b"original");
            assert_eq!(entries(&dir.0), 2);
        });
    }

    #[test]
    fn private_directories_are_created_restricted_and_follow_links() {
        under_umask_022(
            "private_directories_are_created_restricted_and_follow_links",
            || {
                let dir = TestDir::new();
                let path = dir.0.join("data/engine");
                ensure_private_dir(&path).unwrap();
                assert_eq!(mode(&path), 0o700);
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
                ensure_private_dir(&path).unwrap();
                assert_eq!(mode(&path), 0o700);
                let link = dir.0.join("link");
                std::os::unix::fs::symlink("data/engine", &link).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
                ensure_private_dir(&link).unwrap();
                assert_eq!(mode(&path), 0o700);
                assert_eq!(fs::read_link(&link).unwrap(), Path::new("data/engine"));
                let file = dir.0.join("file");
                fs::write(&file, "keep").unwrap();
                assert!(ensure_private_dir(&file).is_err());
                let file_link = dir.0.join("file-link");
                std::os::unix::fs::symlink(&file, &file_link).unwrap();
                assert!(ensure_private_dir(&file_link).is_err());
                assert_eq!(fs::read(&file).unwrap(), b"keep");
                let dangling = dir.0.join("dangling");
                std::os::unix::fs::symlink("absent", &dangling).unwrap();
                assert!(ensure_private_dir(&dangling).is_err());
                assert!(dangling.is_symlink());
                assert!(!dir.0.join("absent").exists());
            },
        );
    }
}

#[cfg(test)]
mod program_tests {
    use super::*;
    use std::fs;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::symlink;
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = env::temp_dir().join(format!(
                "omime-program-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: impl AsRef<Path>, mode: u32) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn explicit_paths_do_not_need_path() {
        let temp = TempDir::new();
        let program = temp.file("run", 0o755);
        assert_eq!(find_program_in(&program, None), Some(program));
        assert_eq!(find_program_in(&temp.0.join("missing"), None), None);
        assert_eq!(find_program_in(Path::new("run"), None), None);
    }

    #[test]
    fn path_search_skips_directories_and_non_executable_files() {
        let temp = TempDir::new();
        fs::create_dir_all(temp.0.join("first/run")).unwrap();
        temp.file("second/run", 0o644);
        let expected = temp.file("third/run", 0o755);
        temp.file("fourth/run", 0o755);
        let path = env::join_paths(["first", "second", "third", "fourth"].map(|d| temp.0.join(d)))
            .unwrap();
        assert_eq!(
            find_program_in(Path::new("run"), Some(&path)),
            Some(expected)
        );
        assert_eq!(find_program_in(&temp.0.join("first/run"), None), None);
        assert_eq!(find_program_in(&temp.0.join("second/run"), None), None);
        assert_eq!(find_program_in(Path::new(""), Some(&path)), None);
    }

    #[test]
    fn symlinks_keep_their_installation_path() {
        let temp = TempDir::new();
        let target = temp.file("real/run", 0o755);
        let link = temp.0.join("run");
        symlink(&target, &link).unwrap();
        assert_eq!(find_program_in(&link, None), Some(link.clone()));
        fs::remove_file(&target).unwrap();
        assert_eq!(find_program_in(&link, None), None);
        fs::remove_file(&link).unwrap();
        symlink(&temp.0, &link).unwrap();
        assert_eq!(find_program_in(&link, None), None);
    }

    #[test]
    fn execute_permission_must_apply_to_the_current_user() {
        let temp = TempDir::new();
        // The owner cannot execute this file even though another user's bit is
        // set. Root can execute a regular file with any execute bit set.
        let program = temp.file("run", 0o001);
        assert_eq!(
            find_program_in(&program, None).is_some(),
            rustix::process::getuid().is_root()
        );
    }

    #[test]
    fn path_entries_and_programs_need_not_be_utf8() {
        let temp = TempDir::new();
        let name = OsStr::from_bytes(b"run-\xff");
        let dir = temp.0.join(OsStr::from_bytes(b"bin-\xfe"));
        let program = temp.file(dir.join(name), 0o755);
        assert_eq!(
            find_program_in(Path::new(name), Some(dir.as_os_str())),
            Some(program)
        );
    }

    #[test]
    fn relative_paths_and_empty_path_entries_use_the_current_directory() {
        const CHILD: &str = "OMIME_TEST_PATH_LOOKUP_CHILD";
        if env::var_os(CHILD).is_some() {
            // The parent supplies an isolated cwd and PATH; no process-global
            // environment or cwd changes can race the other tests.
            assert_eq!(find_program("run"), Some(PathBuf::from("./bin/run")));
            for path in ["", ":missing", "missing::elsewhere", "missing:"] {
                assert_eq!(
                    find_program_in(Path::new("run"), Some(OsStr::new(path))),
                    Some(PathBuf::from("./run"))
                );
            }
            assert_eq!(find_program_in(Path::new("run"), None), None);
            assert_eq!(
                find_program_in(Path::new("./run"), None),
                Some("./run".into())
            );
            assert_eq!(
                find_program_in(Path::new("bin/run"), None),
                Some("bin/run".into())
            );
            // A relative name containing a slash is never appended to PATH.
            assert_eq!(
                find_program_in(Path::new("other/run"), Some(OsStr::new("bin"))),
                None
            );
            assert_eq!(
                find_program_in(Path::new("run/"), Some(OsStr::new("bin"))),
                None
            );
            return;
        }
        let temp = TempDir::new();
        temp.file("run", 0o755);
        temp.file("bin/run", 0o755);
        temp.file("bin/other/run", 0o755);
        let output = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "paths::program_tests::relative_paths_and_empty_path_entries_use_the_current_directory",
                "--nocapture",
            ])
            .current_dir(&temp.0)
            .env(CHILD, "1")
            .env("PATH", "./bin")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}
