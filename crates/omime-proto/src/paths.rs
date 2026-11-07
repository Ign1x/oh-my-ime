//! Standard locations, following the XDG base directory specification.

use std::env;
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
/// Refuses a symlink at `dir`. Missing parents are created privately as well;
/// existing parent directories keep their permissions.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let metadata = fs::symlink_metadata(dir)?;
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
    fn private_directories_are_created_restricted_and_never_symlinked() {
        under_umask_022(
            "private_directories_are_created_restricted_and_never_symlinked",
            || {
                let dir = TestDir::new();
                let path = dir.0.join("data/engine");
                ensure_private_dir(&path).unwrap();
                assert_eq!(mode(&path), 0o700);
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
                ensure_private_dir(&path).unwrap();
                assert_eq!(mode(&path), 0o700);
                let link = dir.0.join("link");
                std::os::unix::fs::symlink(&path, &link).unwrap();
                assert!(ensure_private_dir(&link).is_err());
                let file = dir.0.join("file");
                fs::write(&file, "keep").unwrap();
                assert!(ensure_private_dir(&file).is_err());
                assert_eq!(fs::read(&file).unwrap(), b"keep");
            },
        );
    }
}
