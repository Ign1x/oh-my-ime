//! The per-user runtime directory and the sockets that live in it.

use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use omime_proto::names::RUNTIME_DIR_NAME;

/// `$XDG_RUNTIME_DIR/oh-my-ime`, created with mode `0700`.
pub fn base_dir() -> anyhow::Result<PathBuf> {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let dir = Path::new(&xdg).join(RUNTIME_DIR_NAME);
    create_private_dir(&dir)?;
    Ok(dir)
}

/// The directory of one engine: `$XDG_RUNTIME_DIR/oh-my-ime/<engine>`.
pub fn engine_dir(engine: &str) -> anyhow::Result<PathBuf> {
    validate_engine_id(engine)?;
    let dir = base_dir()?.join(engine);
    create_private_dir(&dir)?;
    Ok(dir)
}

/// The value of `WAYLAND_DISPLAY` for an engine, relative to `$XDG_RUNTIME_DIR`.
pub fn engine_wayland_display(engine: &str) -> String {
    format!("{RUNTIME_DIR_NAME}/{engine}/wayland")
}

/// Engine ids become path components, so they are restricted to a safe alphabet.
pub fn validate_engine_id(id: &str) -> anyhow::Result<()> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if !valid {
        bail!("invalid engine id `{id}`: use 1-64 characters from [A-Za-z0-9._-]");
    }
    Ok(())
}

fn create_private_dir(dir: &Path) -> anyhow::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(dir)
                .with_context(|| format!("cannot inspect {}", dir.display()))?;
            if !metadata.is_dir() {
                bail!("{} exists and is not a directory", dir.display());
            }
            if metadata.permissions().mode() & 0o077 != 0 {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                    .with_context(|| format!("cannot restrict {}", dir.display()))?;
            }
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("cannot create {}", dir.display())),
    }
}

/// Binds a Unix listening socket, replacing a stale socket file left by a crash.
pub fn bind_socket(path: &Path) -> anyhow::Result<tokio::net::UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("cannot remove {}", path.display())),
    }
    tokio::net::UnixListener::bind(path)
        .with_context(|| format!("cannot listen on {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_ids_are_safe_path_components() {
        for ok in ["echo", "sparkle", "ibus.rime", "my_engine-2"] {
            assert!(validate_engine_id(ok).is_ok(), "{ok}");
        }
        for bad in ["", ".", "..", ".hidden", "a/b", "a b", "ä", &"x".repeat(65)] {
            assert!(validate_engine_id(bad).is_err(), "{bad}");
        }
    }
}
