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
const MAX_SWAY_REPLY: usize = 4 * 1024 * 1024;
const MAX_HYPRLAND_REPLY: u64 = 256 * 1024;

/// The content area of the focused window, in compositor coordinates, if the compositor
/// says.
pub async fn focused_window() -> Option<Rect> {
    let query = async {
        // An explicit Sway socket belongs to this session. Do not let discovery
        // of a different, still-running Hyprland session override it.
        if let Some(socket) = std::env::var_os("SWAYSOCK").filter(|s| !s.is_empty()) {
            return sway(Path::new(&socket)).await;
        }
        if let Some(socket) = hyprland_socket() {
            return hyprland(&socket).await;
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
    stream
        .take(MAX_HYPRLAND_REPLY + 1)
        .read_to_string(&mut answer)
        .await
        .ok()?;
    if answer.len() as u64 > MAX_HYPRLAND_REPLY {
        return None;
    }
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
    if width <= 0 || height <= 0 {
        return None;
    }
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
    let length = sway_payload_length(&header)?;
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).await.ok()?;
    sway_focused(&serde_json::from_slice(&payload).ok()?)
}

fn sway_payload_length(header: &[u8; 14]) -> Option<usize> {
    if &header[..6] != b"i3-ipc"
        || u32::from_ne_bytes(header[10..14].try_into().ok()?) != SWAY_GET_TREE
    {
        return None;
    }
    let length = usize::try_from(u32::from_ne_bytes(header[6..10].try_into().ok()?)).ok()?;
    (length > 0 && length <= MAX_SWAY_REPLY).then_some(length)
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
            Some((dx, dy, w, h)) if w > 0 && h > 0 => {
                (x.checked_add(dx)?, y.checked_add(dy)?, w, h)
            }
            _ if width > 0 && height > 0 => (x, y, width, height),
            _ => return None,
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

    #[test]
    fn sway_reply_is_validated_before_allocating_the_tree() {
        let header = |magic: &[u8; 6], size: u32, kind: u32| {
            let mut bytes = [0; 14];
            bytes[..6].copy_from_slice(magic);
            bytes[6..10].copy_from_slice(&size.to_ne_bytes());
            bytes[10..14].copy_from_slice(&kind.to_ne_bytes());
            bytes
        };
        assert_eq!(
            sway_payload_length(&header(b"i3-ipc", 128, SWAY_GET_TREE)),
            Some(128)
        );
        for invalid in [
            header(b"broken", 128, SWAY_GET_TREE),
            header(b"i3-ipc", 128, 0),
            header(b"i3-ipc", u32::MAX, SWAY_GET_TREE),
            header(b"i3-ipc", 0, SWAY_GET_TREE),
        ] {
            assert_eq!(sway_payload_length(&invalid), None);
        }
    }

    #[test]
    fn invalid_geometry_does_not_overflow_or_place_a_popup() {
        let window = serde_json::json!({
            "focused": true,
            "rect": {"x": i32::MAX, "y": 0, "width": 100, "height": 100},
            "window_rect": {"x": 1, "y": 0, "width": 100, "height": 100},
        });
        assert_eq!(sway_focused(&window), None);
        assert_eq!(hyprland_window(r#"{"at":[0,0],"size":[0,-1]}"#), None);
    }
}
