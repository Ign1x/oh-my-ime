//! Write input method engines for [oh-my-ime](https://github.com/Ign1x/oh-my-ime).
//!
//! An engine is a separate process. oh-my-ime launches it, feeds it keys of the
//! focused text field and applies what it asks for: committed text, preedit, deletions.
//! The engine's user interface is entirely its own: create a window with any Wayland
//! toolkit and the platform turns it into a popup that follows the text cursor.
//!
//! ```no_run
//! use omime_sdk::{Context, Engine, KeyEvent};
//!
//! struct Shouty;
//!
//! impl Engine for Shouty {
//!     fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool {
//!         if key.released || key.text.is_empty() || key.modifiers != 0 {
//!             return false; // let the application have it
//!         }
//!         ctx.commit(key.text.to_uppercase());
//!         true
//!     }
//! }
//!
//! fn main() -> Result<(), omime_sdk::Error> {
//!     omime_sdk::run(Shouty)
//! }
//! ```
//!
//! [`run`] blocks the calling thread. Engines with a UI toolkit that owns the main
//! thread use [`spawn`] instead and talk to the platform through the returned [`Host`].
//!
//! Rules every engine must follow:
//!
//! - Return actions through the [`Context`] while handling an event; use the [`Host`]
//!   only for things that happen later (for example a mouse click on a candidate).
//! - Windows are hidden until you call [`Host::set_popup_visible`] with `true`. Never
//!   hide or close your toolkit window to "hide" it; the platform maps and unmaps it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use omime_proto::ApiVersion;
use omime_proto::engine::{Action, Dict, WireAction};
use omime_proto::names::{
    DAEMON_BUS_NAME, DAEMON_OBJECT_PATH, ENGINE_OBJECT_PATH, ENV_ENGINE_ADDRESS, ENV_ENGINE_ID,
};
use tracing::warn;
use zbus::OwnedVariant;

pub use omime_proto::engine::{ContextId, ContextInfo, KeyEvent, Preedit, Status, modifiers};
pub use omime_proto::{ContentHints, ContentPurpose, ContentType};

/// Errors that end an engine.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} is not set; engines are started by omimed")]
    MissingEnvironment(&'static str),
    #[error("D-Bus error: {0}")]
    DBus(#[from] zbus::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("the engine thread ended before it registered")]
    ThreadEnded,
}

/// An input method engine.
///
/// All methods run on the SDK's thread, one at a time.
pub trait Engine: Send + 'static {
    /// The engine became responsible for a focused text field.
    fn activate(&mut self, ctx: &mut Context<'_>) {
        let _ = ctx;
    }

    /// The field lost focus or another engine was selected. The platform removes any
    /// preedit from the field; forget the composition.
    fn deactivate(&mut self, ctx: &mut Context<'_>) {
        let _ = ctx;
    }

    /// Properties of the field changed; see [`Context::info`].
    fn update(&mut self, ctx: &mut Context<'_>) {
        let _ = ctx;
    }

    /// A key was pressed or released. Return `true` to consume it.
    fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool;

    /// The application changed the text on its own; drop any composition.
    fn reset(&mut self, ctx: &mut Context<'_>) {
        let _ = ctx;
    }
}

/// A focused text field, handed to [`Engine`] methods. Actions requested here are
/// applied in order, right after the method returns.
#[derive(Debug)]
pub struct Context<'a> {
    id: ContextId,
    info: &'a ContextInfo,
    host: &'a Host,
    actions: Vec<Action>,
}

impl<'a> Context<'a> {
    pub fn id(&self) -> ContextId {
        self.id
    }

    pub fn info(&self) -> &ContextInfo {
        self.info
    }

    pub fn host(&self) -> &Host {
        self.host
    }

    /// Inserts `text`, replacing the preedit.
    pub fn commit(&mut self, text: impl Into<String>) {
        self.actions.push(Action::Commit(text.into()));
    }

    /// Shows `preedit` in the field.
    pub fn set_preedit(&mut self, preedit: Preedit) {
        self.actions.push(Action::Preedit(preedit));
    }

    /// Removes the preedit from the field.
    pub fn clear_preedit(&mut self) {
        self.actions.push(Action::Preedit(Preedit::default()));
    }

    /// Deletes `before` bytes before and `after` bytes after the cursor.
    pub fn delete_surrounding(&mut self, before: u32, after: u32) {
        self.actions
            .push(Action::DeleteSurrounding { before, after });
    }

    /// Sends a key (evdev code) to the application.
    pub fn forward_key(&mut self, keycode: u32, pressed: bool) {
        self.actions.push(Action::ForwardKey { keycode, pressed });
    }
}

#[zbus::proxy(
    interface = "io.github.ign1x.OhMyIme.EngineHost1",
    default_service = "io.github.ign1x.OhMyIme",
    default_path = "/io/github/ign1x/OhMyIme"
)]
trait EngineHost {
    fn register(
        &self,
        id: &str,
        api_major: u16,
        api_minor: u16,
        capabilities: &Dict,
    ) -> zbus::Result<Dict>;

    #[zbus(no_reply)]
    fn commit(&self, context: u64, text: &str) -> zbus::Result<()>;

    #[zbus(no_reply)]
    fn set_preedit(
        &self,
        context: u64,
        text: &str,
        cursor_begin: i32,
        cursor_end: i32,
    ) -> zbus::Result<()>;

    #[zbus(no_reply)]
    fn delete_surrounding(&self, context: u64, before: u32, after: u32) -> zbus::Result<()>;

    #[zbus(no_reply)]
    fn forward_key(&self, context: u64, keycode: u32, pressed: bool) -> zbus::Result<()>;

    #[zbus(no_reply)]
    fn set_popup_visible(&self, visible: bool) -> zbus::Result<()>;

    #[zbus(no_reply)]
    fn set_status(&self, status: &Dict) -> zbus::Result<()>;

    #[zbus(no_reply)]
    fn request_switch(&self, engine: &str) -> zbus::Result<()>;
}

/// Handle to the platform for things that happen outside of [`Engine`] callbacks.
///
/// Cheap to clone and usable from any thread. Calls never block; failures are logged.
#[derive(Clone)]
pub struct Host {
    proxy: EngineHostProxy<'static>,
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host").finish_non_exhaustive()
    }
}

impl Host {
    fn call<F, Fut>(&self, what: &'static str, f: F)
    where
        F: FnOnce(EngineHostProxy<'static>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = zbus::Result<()>> + Send,
    {
        let proxy = self.proxy.clone();
        self.runtime.spawn(async move {
            if let Err(e) = f(proxy).await {
                warn!("{what} failed: {e}");
            }
        });
    }

    /// Inserts `text` into `context`, replacing its preedit.
    pub fn commit(&self, context: ContextId, text: impl Into<String>) {
        let text = text.into();
        self.call(
            "commit",
            move |p| async move { p.commit(context, &text).await },
        );
    }

    /// Shows `preedit` in `context`.
    pub fn set_preedit(&self, context: ContextId, preedit: Preedit) {
        self.call("set_preedit", move |p| async move {
            p.set_preedit(
                context,
                &preedit.text,
                preedit.cursor_begin,
                preedit.cursor_end,
            )
            .await
        });
    }

    /// Deletes bytes around the cursor of `context`.
    pub fn delete_surrounding(&self, context: ContextId, before: u32, after: u32) {
        self.call("delete_surrounding", move |p| async move {
            p.delete_surrounding(context, before, after).await
        });
    }

    /// Sends a key (evdev code) to the application of `context`.
    pub fn forward_key(&self, context: ContextId, keycode: u32, pressed: bool) {
        self.call("forward_key", move |p| async move {
            p.forward_key(context, keycode, pressed).await
        });
    }

    /// Shows or hides this engine's windows while it is the active engine.
    pub fn set_popup_visible(&self, visible: bool) {
        self.call("set_popup_visible", move |p| async move {
            p.set_popup_visible(visible).await
        });
    }

    /// Publishes a status (label, icon, tooltip) for panels and the CLI.
    pub fn set_status(&self, status: Status) {
        let status = status.to_dict();
        self.call(
            "set_status",
            move |p| async move { p.set_status(&status).await },
        );
    }

    /// Asks the platform to switch to engine `id` (`""` selects direct input).
    pub fn request_switch(&self, id: impl Into<String>) {
        let id = id.into();
        self.call("request_switch", move |p| async move {
            p.request_switch(&id).await
        });
    }
}

struct EngineState {
    engine: Box<dyn Engine>,
    contexts: HashMap<ContextId, ContextInfo>,
}

/// Implements `io.github.ign1x.OhMyIme.Engine1` on top of an [`Engine`].
struct EngineObject {
    state: Mutex<EngineState>,
    host: Host,
    shutdown: Arc<tokio::sync::Notify>,
}

impl EngineObject {
    /// Runs `f` with the engine and a context, and returns the actions it requested.
    fn with_context<R>(
        &self,
        id: ContextId,
        update: impl FnOnce(&mut ContextInfo),
        f: impl FnOnce(&mut dyn Engine, &mut Context<'_>) -> R,
    ) -> (R, Vec<WireAction>) {
        // A panicking engine poisons the mutex; keep serving instead of dying with it.
        let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let EngineState { engine, contexts } = &mut *guard;
        let info = contexts.entry(id).or_default();
        update(info);
        let mut ctx = Context {
            id,
            info,
            host: &self.host,
            actions: Vec::new(),
        };
        let result = f(engine.as_mut(), &mut ctx);
        let actions = ctx.actions.iter().map(Action::to_wire).collect();
        (result, actions)
    }
}

#[zbus::interface(name = "io.github.ign1x.OhMyIme.Engine1")]
impl EngineObject {
    fn activate(&self, context: u64, info: Dict) -> Vec<WireAction> {
        let info = ContextInfo::from_dict(&info);
        self.with_context(context, |i| *i = info, |e, ctx| e.activate(ctx))
            .1
    }

    fn deactivate(&self, context: u64) {
        self.with_context(context, |_| {}, |e, ctx| e.deactivate(ctx));
        let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        guard.contexts.remove(&context);
    }

    fn update_context(&self, context: u64, changes: Dict) -> Vec<WireAction> {
        self.with_context(context, |i| i.apply(&changes), |e, ctx| e.update(ctx))
            .1
    }

    fn process_key(
        &self,
        context: u64,
        key: KeyEvent,
        _extras: HashMap<String, OwnedVariant>,
    ) -> (bool, Vec<WireAction>) {
        self.with_context(context, |_| {}, |e, ctx| e.process_key(ctx, &key))
    }

    fn reset(&self, context: u64) -> Vec<WireAction> {
        self.with_context(context, |_| {}, |e, ctx| e.reset(ctx)).1
    }

    fn shutdown(&self) {
        self.shutdown.notify_one();
    }
}

/// The environment variable that tells a process it was started as an engine.
pub const ENGINE_ADDRESS_ENV: &str = ENV_ENGINE_ADDRESS;

/// The engine protocol version implemented by this SDK.
pub const API_VERSION: ApiVersion = ApiVersion::CURRENT;

async fn serve(
    engine: Box<dyn Engine>,
    ready: Option<std::sync::mpsc::Sender<Host>>,
) -> Result<(), Error> {
    let address = std::env::var(ENV_ENGINE_ADDRESS)
        .map_err(|_| Error::MissingEnvironment(ENV_ENGINE_ADDRESS))?;
    let id = std::env::var(ENV_ENGINE_ID).map_err(|_| Error::MissingEnvironment(ENV_ENGINE_ID))?;
    let builder = zbus::connection::Builder::address(address.as_str());
    serve_on(builder, &id, engine, ready).await
}

/// Serves `engine` on the connection `builder` sets up and registers it as `id`.
async fn serve_on(
    builder: zbus::connection::Builder<'_>,
    id: &str,
    engine: Box<dyn Engine>,
    ready: Option<std::sync::mpsc::Sender<Host>>,
) -> Result<(), Error> {
    let conn = builder.p2p().build().await?;
    let proxy = EngineHostProxy::builder(&conn)
        .destination(DAEMON_BUS_NAME)
        .path(DAEMON_OBJECT_PATH)
        .build()
        .await?;
    let host = Host {
        proxy: proxy.clone(),
        runtime: tokio::runtime::Handle::current(),
    };
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let object = EngineObject {
        state: Mutex::new(EngineState {
            engine,
            contexts: HashMap::new(),
        }),
        host: host.clone(),
        shutdown: shutdown.clone(),
    };
    // Export the engine before registering: the daemon may call it right away.
    conn.object_server().at(ENGINE_OBJECT_PATH, object).await?;
    proxy
        .register(id, API_VERSION.major, API_VERSION.minor, &Dict::new())
        .await?;
    if let Some(ready) = ready {
        // The receiver only goes away if the caller gave up; nothing to do then.
        let _ = ready.send(host);
    }

    tokio::select! {
        () = shutdown.notified() => {}
        () = conn.closed() => {}
    }
    Ok(())
}

fn runtime() -> Result<tokio::runtime::Runtime, Error> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

/// Runs `engine` on the current thread until the platform stops it.
pub fn run(engine: impl Engine) -> Result<(), Error> {
    runtime()?.block_on(serve(Box::new(engine), None))
}

/// Runs `engine` on a background thread and returns once it is registered.
///
/// Use this when a UI toolkit needs the main thread. The [`Host`] lets the UI report
/// events such as clicks on candidates; the join handle yields the engine's result.
pub fn spawn(
    engine: impl Engine,
) -> Result<(Host, std::thread::JoinHandle<Result<(), Error>>), Error> {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("omime-engine".into())
        .spawn(move || runtime()?.block_on(serve(Box::new(engine), Some(tx))))?;
    match rx.recv() {
        Ok(host) => Ok((host, handle)),
        Err(_) => Err(handle
            .join()
            .ok()
            .and_then(Result::err)
            .unwrap_or(Error::ThreadEnded)),
    }
}

/// Helpers for engines.
pub mod util {
    /// Byte length of the last character of `s`, for deleting it from a preedit.
    pub fn last_char_len(s: &str) -> usize {
        s.chars().next_back().map_or(0, char::len_utf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omime_proto::engine::Rect;

    /// A call the engine received.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Seen {
        Activate(ContextInfo),
        Key(KeyEvent),
    }

    /// Records its calls and composes typed text into the preedit.
    struct Recorder(Arc<Mutex<Vec<Seen>>>);

    impl Engine for Recorder {
        fn activate(&mut self, ctx: &mut Context<'_>) {
            self.0
                .lock()
                .unwrap()
                .push(Seen::Activate(ctx.info().clone()));
        }

        fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool {
            self.0.lock().unwrap().push(Seen::Key(key.clone()));
            if key.released || key.text.is_empty() {
                return false;
            }
            ctx.set_preedit(Preedit::with_cursor_at_end(key.text.clone()));
            true
        }
    }

    /// The daemon's side of `EngineHost1`, as far as the tests need it.
    struct FakeDaemon {
        registered: Arc<tokio::sync::Notify>,
    }

    #[zbus::interface(name = "io.github.ign1x.OhMyIme.EngineHost1")]
    impl FakeDaemon {
        fn register(&self, _id: &str, _major: u16, _minor: u16, _capabilities: Dict) -> Dict {
            self.registered.notify_one();
            Dict::new()
        }
    }

    #[zbus::proxy(
        interface = "io.github.ign1x.OhMyIme.Engine1",
        default_service = "io.github.ign1x.OhMyIme",
        default_path = "/io/github/ign1x/OhMyIme/Engine"
    )]
    trait Engine1 {
        fn activate(&self, context: u64, info: &Dict) -> zbus::Result<Vec<WireAction>>;

        fn process_key(
            &self,
            context: u64,
            key: &KeyEvent,
            extras: &Dict,
        ) -> zbus::Result<(bool, Vec<WireAction>)>;
    }

    /// Serves `engine` like `run` does, over a socket pair, and returns the daemon's
    /// proxy for it once it registered.
    async fn connect(engine: impl Engine) -> Engine1Proxy<'static> {
        let (daemon_end, engine_end) = std::os::unix::net::UnixStream::pair().unwrap();
        // Tokio needs non-blocking sockets.
        daemon_end.set_nonblocking(true).unwrap();
        engine_end.set_nonblocking(true).unwrap();
        let engine: Box<dyn Engine> = Box::new(engine);
        let serving = tokio::spawn(async move {
            let builder = zbus::connection::Builder::unix_stream(engine_end);
            serve_on(builder, "test", engine, None).await
        });

        let registered = Arc::new(tokio::sync::Notify::new());
        let daemon = zbus::connection::Builder::unix_stream(daemon_end)
            .server(zbus::Guid::generate())
            .p2p()
            .serve_at(
                DAEMON_OBJECT_PATH,
                FakeDaemon {
                    registered: registered.clone(),
                },
            )
            .build()
            .await
            .unwrap();
        tokio::select! {
            () = registered.notified() => {}
            result = serving => panic!("the engine stopped: {result:?}"),
        }
        Engine1Proxy::new(&daemon).await.unwrap()
    }

    fn run_test(test: impl std::future::Future<Output = ()>) {
        runtime().unwrap().block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(10), test)
                .await
                .expect("the test timed out");
        });
    }

    #[test]
    fn activate_carries_the_whole_context() {
        run_test(async {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let engine = connect(Recorder(seen.clone())).await;
            let info = ContextInfo {
                app_id: Some("kitty".into()),
                content_type: ContentType {
                    purpose: ContentPurpose::Terminal,
                    hints: ContentHints::COMPLETION,
                },
                surrounding: Some(("中文 text".into(), 7, 7)),
                cursor_rect: Some((10, -20, 2, 18) as Rect),
            };
            let actions = engine.activate(7, &info.to_dict()).await.unwrap();
            assert!(actions.is_empty());
            assert_eq!(*seen.lock().unwrap(), [Seen::Activate(info)]);
        });
    }

    #[test]
    fn keys_are_answered_with_their_actions() {
        run_test(async {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let engine = connect(Recorder(seen.clone())).await;
            let press = KeyEvent {
                keysym: 0x61,
                keycode: 30,
                modifiers: 0,
                time: 1,
                text: "a".into(),
                released: false,
                repeat: false,
            };
            let (handled, actions) = engine.process_key(7, &press, &Dict::new()).await.unwrap();
            assert!(handled);
            let actions: Vec<Action> = actions
                .iter()
                .map(|wire| Action::from_wire(wire).unwrap())
                .collect();
            assert_eq!(actions, [Action::Preedit(Preedit::with_cursor_at_end("a"))]);

            let release = KeyEvent {
                text: String::new(),
                released: true,
                ..press.clone()
            };
            let (handled, actions) = engine.process_key(7, &release, &Dict::new()).await.unwrap();
            assert!(!handled && actions.is_empty());
            assert_eq!(
                *seen.lock().unwrap(),
                [Seen::Key(press), Seen::Key(release)]
            );
        });
    }
}
