//! Installing and uninstalling engine directories.
//!
//! The functions only touch the directories they are given, so the commands can be
//! exercised without a real home directory and without a running daemon.

use std::env;
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use omime_proto::manifest::{
    InstalledEngine, MANIFEST_FILE, Manifest, discover, is_valid_engine_id,
};

/// Copies the engine directory `source` to `engines_dir/<id>` and returns the installed
/// engine. `source` itself stays as it is.
///
/// The copy goes into a temporary directory next to the target first, so a failed copy
/// never leaves a half-installed engine and replacing an existing one is one rename.
pub fn install(source: &Path, engines_dir: &Path, force: bool) -> anyhow::Result<InstalledEngine> {
    let source = source
        .canonicalize()
        .with_context(|| format!("cannot find {}", source.display()))?;
    let manifest = read_manifest(&source)?;
    let id = manifest.engine.id.clone();

    fs::create_dir_all(engines_dir)
        .with_context(|| format!("cannot create {}", engines_dir.display()))?;
    let engines_dir = engines_dir
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", engines_dir.display()))?;
    let target = engines_dir.join(&id);
    let target_resolved = target.canonicalize().unwrap_or_else(|_| target.clone());
    if source == target_resolved || source.starts_with(&target_resolved) {
        bail!(
            "cannot install {} into itself ({})",
            source.display(),
            target.display()
        );
    }
    if fs::symlink_metadata(&target).is_ok() && !force {
        bail!(
            "engine `{id}` is already installed in {} (use --force to replace it)",
            target.display()
        );
    }

    // A dot name: engine ids never start with one, so nothing looks inside it.
    let temp = engines_dir.join(format!(".{id}.installing"));
    // An interrupted run may have left the temporary directory behind.
    remove_path(&temp).with_context(|| format!("cannot remove {}", temp.display()))?;
    if let Err(e) = copy_tree(&source, &temp) {
        let _ = remove_path(&temp);
        return Err(e)
            .with_context(|| format!("cannot copy {} to {}", source.display(), temp.display()));
    }
    if force && let Err(e) = remove_path(&target) {
        let _ = remove_path(&temp);
        return Err(e).with_context(|| format!("cannot replace {}", target.display()));
    }
    if let Err(e) = fs::rename(&temp, &target) {
        let _ = remove_path(&temp);
        return Err(e).with_context(|| format!("cannot move the copy to {}", target.display()));
    }

    Ok(InstalledEngine {
        manifest,
        dir: target,
    })
}

/// Removes the engine `id` installed in `engines_dir`.
///
/// When the engine only exists in one of `search` instead, the error says where it is;
/// nothing outside `engines_dir` is ever removed.
pub fn uninstall(engines_dir: &Path, id: &str, search: &[PathBuf]) -> anyhow::Result<()> {
    let target = engines_dir.join(id);
    // The id becomes a path component, so only valid ids may even be looked up.
    if !is_valid_engine_id(id) || !target.join(MANIFEST_FILE).exists() {
        if let Some(engine) = discover(search).0.iter().find(|e| e.id() == id) {
            bail!(
                "engine `{id}` is installed in {}, outside your user directory; \
                 remove it with whatever installed it",
                engine.dir.display()
            );
        }
        bail!("no engine `{id}` is installed");
    }
    fs::remove_dir_all(&target).with_context(|| format!("cannot remove {}", target.display()))
}

/// Returns a warning when omimed will not be able to start `engine`: its program is not
/// in `$PATH` (for a bare name) or the file it resolves to is missing or not executable.
pub fn command_warning(engine: &InstalledEngine) -> Option<String> {
    let program = engine.command().into_iter().next()?;
    if !program.contains('/') {
        return find_in_path(&program)
            .is_none()
            .then(|| format!("`{program}` is not in PATH, so omimed cannot start this engine"));
    }
    let path = Path::new(&program);
    match fs::metadata(path) {
        Ok(metadata) if is_executable(&metadata) => None,
        Ok(_) => Some(format!(
            "{program} is not executable, so omimed cannot start this engine"
        )),
        Err(_) => Some(format!(
            "{program} does not exist, so omimed cannot start this engine"
        )),
    }
}

/// Reads and validates `<dir>/engine.toml`.
fn read_manifest(dir: &Path) -> anyhow::Result<Manifest> {
    let path = dir.join(MANIFEST_FILE);
    let text =
        fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
    Manifest::parse(&text).with_context(|| format!("invalid {}", path.display()))
}

/// Copies `source` to the new directory `target`: files with their permission bits,
/// directories, and symlinks as symlinks rather than what they point at.
fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)?;
        let kind = metadata.file_type();
        if kind.is_symlink() {
            symlink(fs::read_link(&from)?, &to)?;
        } else if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else if kind.is_file() {
            // `fs::copy` also copies the permission bits.
            fs::copy(&from, &to)?;
        } else {
            return Err(io::Error::other(format!(
                "{} is not a regular file, a directory or a symlink",
                from.display()
            )));
        }
    }
    Ok(())
}

/// Removes `path`, whatever it is. A path that is not there is not an error.
///
/// Directories are removed recursively; a symlink is removed itself, not its target.
fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Finds the executable `name` in `$PATH`.
fn find_in_path(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| fs::metadata(path).is_ok_and(|m| is_executable(&m)))
}

fn is_executable(metadata: &fs::Metadata) -> bool {
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    const MANIFEST: &str = r#"
[engine]
id = "echo"
name = "Echo"
exec = ["bin/engine"]
api = "1.0"
"#;

    /// A scratch directory that removes itself.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "omime-install-{name}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// An engine directory with a manifest, an executable and a symlink to it.
    fn engine_source(dir: &Path) -> PathBuf {
        let source = dir.join("source");
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::write(source.join(MANIFEST_FILE), MANIFEST).unwrap();
        fs::write(source.join("bin/engine"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(source.join("bin/engine"), fs::Permissions::from_mode(0o755)).unwrap();
        symlink("bin/engine", source.join("engine")).unwrap();
        source
    }

    #[test]
    fn installs_a_copy_with_modes_and_symlinks() {
        let dir = TempDir::new("copy");
        let source = engine_source(dir.path());
        let engines = dir.path().join("engines");

        let installed = install(&source, &engines, false).unwrap();
        let target = engines.join("echo");
        assert_eq!(installed.dir, target);
        assert_eq!(installed.id(), "echo");

        assert_eq!(
            fs::read_to_string(target.join(MANIFEST_FILE)).unwrap(),
            MANIFEST
        );
        let bin = target.join("bin/engine");
        assert_eq!(
            fs::metadata(&bin).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let link = fs::symlink_metadata(target.join("engine")).unwrap();
        assert!(link.file_type().is_symlink());
        assert_eq!(
            fs::read_link(target.join("engine")).unwrap(),
            Path::new("bin/engine")
        );
        assert!(source.join("bin/engine").exists(), "the source stays");
        assert_eq!(fs::read_dir(&engines).unwrap().count(), 1);
    }

    #[test]
    fn refuses_an_existing_install_unless_forced() {
        let dir = TempDir::new("replace");
        let source = engine_source(dir.path());
        let engines = dir.path().join("engines");
        install(&source, &engines, false).unwrap();

        let error = install(&source, &engines, false).unwrap_err().to_string();
        assert!(error.contains("already installed"), "{error}");
        assert!(error.contains("--force"), "{error}");

        fs::write(engines.join("echo/stale"), "old").unwrap();
        let leftover = engines.join(".echo.installing/leftover");
        fs::create_dir_all(leftover.parent().unwrap_or(&engines)).unwrap();
        fs::write(&leftover, "leftover").unwrap();
        install(&source, &engines, true).unwrap();
        assert!(!engines.join("echo/stale").exists());
        assert!(engines.join("echo/bin/engine").exists());
        assert_eq!(
            fs::read_dir(&engines).unwrap().count(),
            1,
            "the temporary directory is gone"
        );
    }

    #[test]
    fn refuses_to_install_a_directory_into_itself() {
        let dir = TempDir::new("self");
        let source = engine_source(dir.path());
        let engines = dir.path().join("engines");
        install(&source, &engines, false).unwrap();
        let target = engines.join("echo");

        let error = install(&target, &engines, true).unwrap_err().to_string();
        assert!(error.contains("into itself"), "{error}");

        let nested = target.join("sub");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join(MANIFEST_FILE), MANIFEST).unwrap();
        let error = install(&nested, &engines, true).unwrap_err().to_string();
        assert!(error.contains("into itself"), "{error}");
        assert!(target.join(MANIFEST_FILE).exists(), "nothing was removed");
    }

    #[test]
    fn refuses_a_broken_manifest() {
        let dir = TempDir::new("broken");
        let source = dir.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join(MANIFEST_FILE), "[engine]\nid = \"echo\"\n").unwrap();
        let engines = dir.path().join("engines");

        let error = format!("{:#}", install(&source, &engines, false).unwrap_err());
        assert!(error.contains("engine.toml"), "{error}");
        assert!(!engines.join("echo").exists());
    }

    #[test]
    fn uninstalls_only_from_the_given_directory() {
        let dir = TempDir::new("uninstall");
        let source = engine_source(dir.path());
        let engines = dir.path().join("engines");
        install(&source, &engines, false).unwrap();

        let error = uninstall(
            &dir.path().join("other"),
            "echo",
            std::slice::from_ref(&engines),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("outside your user directory"), "{error}");
        assert!(engines.join("echo/engine.toml").exists());

        uninstall(&engines, "echo", std::slice::from_ref(&engines)).unwrap();
        assert!(!engines.join("echo").exists());

        let error = uninstall(&engines, "echo", std::slice::from_ref(&engines))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no engine `echo` is installed"), "{error}");
    }

    #[test]
    fn uninstall_rejects_ids_that_leave_the_directory() {
        let dir = TempDir::new("escape");
        let engines = dir.path().join("engines");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join(MANIFEST_FILE), MANIFEST).unwrap();

        let error = uninstall(&engines, "../outside", &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("no engine"), "{error}");
        assert!(outside.join(MANIFEST_FILE).exists());
    }
}
