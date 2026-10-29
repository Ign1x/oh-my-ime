//! Standard locations, following the XDG base directory specification.

use std::env;
use std::path::PathBuf;

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

/// Directories searched for engines, highest priority first:
///
/// 1. every entry of `$OMIME_ENGINE_PATH` (colon separated; handy for development),
/// 2. `$XDG_DATA_HOME/oh-my-ime/engines` (user installs),
/// 3. `<dir>/oh-my-ime/engines` for each entry of `$XDG_DATA_DIRS`.
pub fn engine_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(extra) = env::var_os("OMIME_ENGINE_PATH") {
        dirs.extend(env::split_paths(&extra).filter(|p| !p.as_os_str().is_empty()));
    }
    if let Some(data_home) = xdg_dir("XDG_DATA_HOME", ".local/share") {
        dirs.push(data_home.join(PROJECT_DIR).join("engines"));
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
