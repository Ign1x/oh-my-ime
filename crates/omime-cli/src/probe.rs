//! A single registry roundtrip, without binding or taking any input-method objects.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, ensure};
use omime_proto::desktop::Capabilities;
use omime_proto::tr;
use wl_proxy::baseline::Baseline;
use wl_proxy::object::ObjectCoreApi as _;
use wl_proxy::protocols::ObjectInterface;
use wl_proxy::protocols::wayland::wl_callback::{WlCallback, WlCallbackHandler};
use wl_proxy::protocols::wayland::wl_registry::{WlRegistry, WlRegistryHandler};
use wl_proxy::state::State;

const TIMEOUT: Duration = Duration::from_secs(2);
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    NoDisplay,
    Supported,
    Unsupported(Capabilities),
}

impl Verdict {
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Unsupported(capabilities) => capabilities.warning(),
            _ => None,
        }
    }
}

fn verdict<'a>(globals: impl IntoIterator<Item = (&'a str, u32)>) -> Verdict {
    let capabilities = Capabilities::from_globals(globals);
    if capabilities.supported() {
        Verdict::Supported
    } else {
        Verdict::Unsupported(capabilities)
    }
}

pub async fn current() -> anyhow::Result<Verdict> {
    // WAYLAND_SOCKET takes precedence over the display name. A probe cannot use
    // that protocol stream and then hand it to a fresh client with new object IDs.
    ensure!(
        std::env::var_os("WAYLAND_SOCKET").is_none(),
        "{}",
        tr!(
            "cannot safely check the inherited WAYLAND_SOCKET; it overrides WAYLAND_DISPLAY. Run setup from a desktop terminal without an inherited Wayland socket."
        )
    );
    let Some(display) = std::env::var_os("WAYLAND_DISPLAY").filter(|s| !s.is_empty()) else {
        return Ok(Verdict::NoDisplay);
    };
    let display = PathBuf::from(display);
    let path = if display.is_absolute() {
        display
    } else {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .context(tr!(
                "cannot probe the Wayland display: XDG_RUNTIME_DIR is not an absolute path"
            ))?;
        runtime.join(display)
    };
    probe(&path, TIMEOUT).await
}

async fn probe(path: &Path, timeout: Duration) -> anyhow::Result<Verdict> {
    let deadline = Instant::now() + timeout;
    let timed_out = || tr!("the Wayland protocol check timed out; setup cannot safely continue");
    let socket = tokio::time::timeout(timeout, tokio::net::UnixStream::connect(path))
        .await
        .with_context(timed_out)?
        .and_then(tokio::net::UnixStream::into_std)
        .context(tr!("cannot check the Wayland display's input protocols"))?;
    let socket = Rc::new(OwnedFd::from(socket));
    let state = State::builder(Baseline::V6)
        .with_server_fd(&socket)
        .build()
        .map_err(|error| anyhow!("{error}"))
        .context(tr!("cannot check the Wayland display's input protocols"))?;
    let seen = Rc::new(Registry::default());
    let display = state.display();
    let registry = display.new_send_get_registry();
    registry.set_forward_to_client(false);
    registry.set_handler(RegistryEvents(seen.clone()));
    let sync = display.new_send_sync();
    sync.set_forward_to_client(false);
    sync.set_handler(SyncEvents(seen.clone()));
    let result = loop {
        if seen.done.get() {
            break Ok(verdict(
                seen.globals
                    .borrow()
                    .values()
                    .map(|(interface, version)| (interface.name(), *version)),
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Err(anyhow!(timed_out()));
        }
        if let Err(error) = state.dispatch(Some(remaining)) {
            break Err(anyhow!("{error}")
                .context(tr!("cannot check the Wayland display's input protocols")));
        }
    };
    state.destroy();
    result
}

#[derive(Debug, Default)]
struct Registry {
    globals: RefCell<BTreeMap<u32, (ObjectInterface, u32)>>,
    done: Cell<bool>,
}

struct RegistryEvents(Rc<Registry>);

impl WlRegistryHandler for RegistryEvents {
    fn handle_global(
        &mut self,
        _: &Rc<WlRegistry>,
        name: u32,
        interface: ObjectInterface,
        version: u32,
    ) {
        self.0
            .globals
            .borrow_mut()
            .insert(name, (interface, version));
    }

    fn handle_global_remove(&mut self, _: &Rc<WlRegistry>, name: u32) {
        self.0.globals.borrow_mut().remove(&name);
    }
}

struct SyncEvents(Rc<Registry>);

impl WlCallbackHandler for SyncEvents {
    fn handle_done(&mut self, _: &Rc<WlCallback>, _: u32) {
        self.0.done.set(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_uses_the_same_capabilities_as_daemon_startup() {
        use omime_proto::desktop::REQUIRED;
        assert_eq!(verdict([]), Verdict::Unsupported(Capabilities::default()));
        let v1 = [("zwp_input_method_v1", 1), ("zwp_input_panel_v1", 1)];
        assert_eq!(verdict(v1), Verdict::Supported);
        assert!(verdict(v1).warning().is_none());
        assert_eq!(
            verdict([(REQUIRED[0], 1), (REQUIRED[1], 1)]),
            Verdict::Supported
        );
        assert!(
            verdict([(REQUIRED[0], 0), (REQUIRED[1], 1)])
                .warning()
                .is_some()
        );
    }

    #[tokio::test]
    async fn an_unreachable_display_is_an_error() {
        let error = probe(Path::new("/dev/null/wayland-missing"), TIMEOUT)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot check the Wayland display")
        );
    }

    #[tokio::test]
    async fn an_unresponsive_display_times_out_and_closes_the_connection() {
        use tokio::io::AsyncReadExt as _;
        let path = std::env::temp_dir().join(format!("omime-probe-{}.sock", std::process::id()));
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let check = probe(&path, Duration::from_millis(50));
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut requests = Vec::new();
            tokio::time::timeout(Duration::from_secs(1), socket.read_to_end(&mut requests))
                .await
                .unwrap()
                .unwrap();
            assert!(!requests.is_empty());
        };
        let (result, ()) = tokio::join!(check, server);
        std::fs::remove_file(path).unwrap();
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }
}
