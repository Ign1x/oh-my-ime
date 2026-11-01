//! The compositor's own IPC, for what the Wayland protocols do not tell clients: where
//! the focused window is. Applications that reach omimed through the Fcitx5 module
//! protocol report their text cursor relative to their window, and engine windows go to
//! the cursor on the screen. Hyprland and sway answer; elsewhere nothing is known.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tracing::debug;

use crate::placement::Rect;

/// How long the compositor may take to answer; typing must not wait for it.
const TIMEOUT: Duration = Duration::from_millis(300);
/// sway's IPC message type `GET_TREE`.
const SWAY_GET_TREE: u32 = 4;

/// The content area of the focused window, in compositor coordinates, if the compositor
/// says.
pub async fn focused_window() -> Option<Rect> {
    let query = async {
        if let Some(socket) = hyprland_socket() {
            return hyprland(&socket).await;
        }
        if let Some(socket) = std::env::var_os("SWAYSOCK") {
            return sway(Path::new(&socket)).await;
        }
        None
    };
    match tokio::time::timeout(TIMEOUT, query).await {
        Ok(window) => window,
        Err(_) => {
            debug!("the compositor did not say where the focused window is in time");
            None
        }
    }
}

/// Hyprland's request socket: of the instance in `$HYPRLAND_INSTANCE_SIGNATURE`, or else
/// of the one started last (omimed may run from a service that lacks the variable).
fn hyprland_socket() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("hypr");
    let instance = match std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE") {
        Some(instance) => dir.join(instance),
        None => std::fs::read_dir(&dir)
            .ok()?
            .flatten()
            .filter(|entry| entry.path().join(".socket.sock").exists())
            .max_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok())?
            .path(),
    };
    let socket = instance.join(".socket.sock");
    socket.exists().then_some(socket)
}

async fn hyprland(socket: &Path) -> Option<Rect> {
    let mut stream = UnixStream::connect(socket).await.ok()?;
    stream.write_all(b"j/activewindow").await.ok()?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.ok()?;
    hyprland_window(&answer)
}

/// The window in Hyprland's `j/activewindow`: `at` and `size`, in layout coordinates.
fn hyprland_window(answer: &str) -> Option<Rect> {
    let window: Value = serde_json::from_str(answer).ok()?;
    let pair = |key: &str| -> Option<(i32, i32)> {
        let values = window.get(key)?.as_array()?;
        let number = |i: usize| i32::try_from(values.get(i)?.as_i64()?).ok();
        Some((number(0)?, number(1)?))
    };
    let (x, y) = pair("at")?;
    let (width, height) = pair("size")?;
    Some((x, y, width, height))
}

async fn sway(socket: &Path) -> Option<Rect> {
    let mut stream = UnixStream::connect(socket).await.ok()?;
    let mut request = b"i3-ipc".to_vec();
    request.extend(0u32.to_ne_bytes());
    request.extend(SWAY_GET_TREE.to_ne_bytes());
    stream.write_all(&request).await.ok()?;
    let mut header = [0u8; 14];
    stream.read_exact(&mut header).await.ok()?;
    let length = u32::from_ne_bytes(header[6..10].try_into().ok()?);
    let mut payload = vec![0; usize::try_from(length).ok()?];
    stream.read_exact(&mut payload).await.ok()?;
    sway_focused(&serde_json::from_slice(&payload).ok()?)
}

/// The content area of the focused node of a sway tree: its `rect`, moved and sized by
/// its `window_rect` (relative to it) when it has one.
fn sway_focused(node: &Value) -> Option<Rect> {
    if node.get("focused").and_then(Value::as_bool) == Some(true) {
        let rect = |key: &str| -> Option<Rect> {
            let rect = node.get(key)?;
            let number = |name: &str| i32::try_from(rect.get(name)?.as_i64()?).ok();
            Some((
                number("x")?,
                number("y")?,
                number("width")?,
                number("height")?,
            ))
        };
        let (x, y, width, height) = rect("rect")?;
        return Some(match rect("window_rect") {
            Some((dx, dy, w, h)) if w > 0 && h > 0 => (x + dx, y + dy, w, h),
            _ => (x, y, width, height),
        });
    }
    ["nodes", "floating_nodes"]
        .iter()
        .filter_map(|key| node.get(key)?.as_array())
        .flatten()
        .find_map(sway_focused)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyprland_says_where_the_active_window_is() {
        let answer = r#"{"address": "0x55d0", "mapped": true, "at": [1140, 549],
            "size": [280, 380], "class": "wechat", "xwayland": false}"#;
        assert_eq!(hyprland_window(answer), Some((1140, 549, 280, 380)));
        // No window has focus.
        assert_eq!(hyprland_window("{}"), None);
        assert_eq!(hyprland_window("Invalid"), None);
    }

    #[test]
    fn sway_says_where_the_focused_window_is() {
        let tree: Value = serde_json::from_str(
            r#"{"focused": false, "rect": {"x": 0, "y": 0, "width": 1280, "height": 720},
            "nodes": [{"focused": false, "nodes": [
                {"focused": false, "rect": {"x": 0, "y": 0, "width": 640, "height": 720}},
                {"focused": true, "rect": {"x": 640, "y": 0, "width": 640, "height": 720},
                 "window_rect": {"x": 2, "y": 24, "width": 636, "height": 694}}
            ], "floating_nodes": []}]}"#,
        )
        .unwrap();
        assert_eq!(sway_focused(&tree), Some((642, 24, 636, 694)));
        let floating: Value = serde_json::from_str(
            r#"{"nodes": [{"nodes": [], "floating_nodes": [{"focused": true,
            "rect": {"x": 100, "y": 50, "width": 300, "height": 200}}]}]}"#,
        )
        .unwrap();
        assert_eq!(sway_focused(&floating), Some((100, 50, 300, 200)));
        assert_eq!(sway_focused(&serde_json::json!({"nodes": []})), None);
    }
}
