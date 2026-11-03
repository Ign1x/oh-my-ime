//! Desktop notifications (`org.freedesktop.Notifications`) about trouble that would
//! otherwise only be in omimed's log: an engine that stopped, another input method on a
//! seat, a mistake in config.toml. Each trouble is told once, again only after it was
//! resolved in between or after [`REPEAT_AFTER`], and its notification goes away when it
//! is resolved. A click on one opens the settings, where the About page shows the
//! state of things. `[notifications] enabled = false` keeps omimed quiet.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt as _;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info};
use zbus::Variant;

/// How long a trouble that is still there stays untold.
const REPEAT_AFTER: Duration = Duration::from_secs(60 * 60);

/// Something that went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trouble {
    /// An engine stopped and stays stopped until the user switches to it again.
    Engine {
        id: String,
        name: String,
        why: EngineFailure,
    },
    /// Another input method holds the seat, so omimed cannot serve it.
    Seat { name: String },
    /// config.toml has a mistake. `kept`: omimed keeps the configuration it had;
    /// otherwise it uses the defaults.
    Config { error: String, kept: bool },
}

/// Why an engine stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineFailure {
    /// It kept crashing.
    Crashed,
    /// It did not register with omimed in time.
    NoStart,
    /// It sent the compositor a request that the compositor refused.
    Protocol,
    /// It could not be launched; the error, for people.
    Launch(String),
}

/// What the core reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Trouble(Trouble),
    /// The trouble about this is over.
    Resolved(Subject),
}

/// What a trouble is about, to tell it once and to resolve it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    Engine(String),
    Seat(String),
    Config,
}

impl Trouble {
    pub fn subject(&self) -> Subject {
        match self {
            Self::Engine { id, .. } => Subject::Engine(id.clone()),
            Self::Seat { name } => Subject::Seat(name.clone()),
            Self::Config { .. } => Subject::Config,
        }
    }

    /// The summary and body of the notification, in Chinese for Chinese locales.
    fn text(&self, chinese: bool) -> (String, String) {
        match (self, chinese) {
            (Self::Engine { name, why, .. }, false) => {
                let why = match why {
                    EngineFailure::Crashed => "It crashed again and again.".to_owned(),
                    EngineFailure::NoStart => "It did not start in time.".to_owned(),
                    EngineFailure::Protocol => {
                        "It sent the compositor a request that the compositor refused.".to_owned()
                    }
                    EngineFailure::Launch(error) => format!("It could not be started: {error}."),
                };
                (
                    format!("{name} stopped"),
                    format!(
                        "{why} Until you switch to it again, your keys go to applications as you type them."
                    ),
                )
            }
            (Self::Engine { name, why, .. }, true) => {
                let why = match why {
                    EngineFailure::Crashed => "它一再崩溃。".to_owned(),
                    EngineFailure::NoStart => "它没能及时启动。".to_owned(),
                    EngineFailure::Protocol => "它向合成器发送了被拒绝的请求。".to_owned(),
                    EngineFailure::Launch(error) => format!("无法启动它：{error}。"),
                };
                (
                    format!("{name}已停止"),
                    format!("{why}在你再次切换到它之前，按键会原样交给应用。"),
                )
            }
            (Self::Seat { name }, false) => (
                "Another input method is running".to_owned(),
                format!(
                    "It holds the keyboard of seat {name}, so oh-my-ime cannot serve it. \
                     Quit it (Fcitx 5 or IBus, say), or run `omime setup --take-over`; \
                     oh-my-ime takes over once it is gone."
                ),
            ),
            (Self::Seat { name }, true) => (
                "另一个输入法正在运行".to_owned(),
                format!(
                    "它占用了座位 {name} 的键盘，oh-my-ime 无法为其服务。请退出它（例如 Fcitx 5 \
                     或 IBus），或运行 `omime setup --take-over`；它退出后 oh-my-ime 会自动接管。"
                ),
            ),
            (Self::Config { error, kept }, false) => (
                "config.toml has a mistake".to_owned(),
                if *kept {
                    format!(
                        "{}. oh-my-ime keeps the configuration it had.",
                        brief(error)
                    )
                } else {
                    format!(
                        "{}. oh-my-ime uses the defaults until it is fixed.",
                        brief(error)
                    )
                },
            ),
            (Self::Config { error, kept }, true) => (
                "config.toml 有错误".to_owned(),
                if *kept {
                    format!("{}。oh-my-ime 继续使用之前的配置。", brief(error))
                } else {
                    format!("{}。在改正之前，oh-my-ime 使用默认配置。", brief(error))
                },
            ),
        }
    }
}

/// An error of config.toml on one line, for a notification, which names the file
/// already: without the file's path, and of a TOML error with a snippet of the file only
/// where it is and what is wrong (`TOML parse error at line 2, column 11: invalid type:
/// string "yes", expected a boolean`).
fn brief(error: &str) -> String {
    let error = error.trim();
    let error = match error.split_once(": ") {
        Some((path, rest)) if path.starts_with('/') => rest,
        _ => error,
    };
    // The snippet: lines of the file ("2 | enabled = ...") and markers ("| ^^^").
    let snippet = |line: &&str| {
        line.starts_with('|')
            || line
                .split_once(" |")
                .is_some_and(|(n, _)| n.trim().parse::<u32>().is_ok())
    };
    let mut lines = error.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or_default();
    match lines.rfind(|l| !snippet(l)) {
        Some(last) => format!("{first}: {last}"),
        None => first.to_owned(),
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Variant<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: &str) -> zbus::Result<()>;
}

/// What the notifier's task does.
#[derive(Debug)]
enum Command {
    Tell(Trouble),
    Resolved(Subject),
}

/// Tells the user about trouble. Cheap to call often: it decides what to show.
#[derive(Debug)]
pub struct Notifier {
    commands: mpsc::UnboundedSender<Command>,
    enabled: Cell<bool>,
}

impl Notifier {
    /// Starts telling, on the session bus, which it connects to when there is something
    /// to tell. `display` is the compositor the settings open on.
    pub fn start(enabled: bool, display: Option<String>) -> Self {
        let (commands, pending) = mpsc::unbounded_channel();
        tokio::task::spawn_local(serve(pending, display));
        Self {
            commands,
            enabled: Cell::new(enabled),
        }
    }

    /// Follows `[notifications] enabled`.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.set(enabled);
    }

    /// Tells or resolves what the core reports.
    pub fn report(&self, report: Report) {
        match report {
            Report::Trouble(trouble) => self.tell(trouble),
            Report::Resolved(subject) => self.resolved(subject),
        }
    }

    pub fn tell(&self, trouble: Trouble) {
        if self.enabled.get() {
            let _ = self.commands.send(Command::Tell(trouble));
        }
    }

    /// The trouble about `subject` is over: its notification goes, and it is told again
    /// when it comes back.
    pub fn resolved(&self, subject: Subject) {
        let _ = self.commands.send(Command::Resolved(subject));
    }
}

/// What was told about a subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Told {
    /// The notification's id, 0 while the server has not answered.
    id: u32,
    at: Instant,
    resolved: bool,
}

/// Which troubles to tell, and which notifications to close.
#[derive(Debug, Default)]
struct Ledger {
    told: HashMap<Subject, Told>,
}

impl Ledger {
    /// Whether to tell about `subject` now, and the notification it replaces (0 for a
    /// new one).
    fn tell(&mut self, subject: &Subject, now: Instant) -> Option<u32> {
        match self.told.get(subject) {
            Some(told) if !told.resolved && now.duration_since(told.at) < REPEAT_AFTER => None,
            told => {
                let replaces = told.map_or(0, |t| t.id);
                self.told.insert(
                    subject.clone(),
                    Told {
                        id: replaces,
                        at: now,
                        resolved: false,
                    },
                );
                Some(replaces)
            }
        }
    }

    /// Remembers the id the server gave the notification about `subject`.
    fn shown(&mut self, subject: &Subject, id: u32) {
        if let Some(told) = self.told.get_mut(subject) {
            told.id = id;
        }
    }

    /// The notification to close, now that the trouble about `subject` is over.
    fn resolve(&mut self, subject: &Subject) -> Option<u32> {
        let told = self.told.get_mut(subject)?;
        if told.resolved {
            return None;
        }
        told.resolved = true;
        (told.id != 0).then_some(told.id)
    }

    fn is_ours(&self, id: u32) -> bool {
        id != 0 && self.told.values().any(|t| t.id == id)
    }
}

/// Whether the user reads Chinese, after the locale.
fn chinese() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .is_some_and(|locale| locale.starts_with("zh"))
}

async fn serve(mut pending: mpsc::UnboundedReceiver<Command>, display: Option<String>) {
    let mut ledger = Ledger::default();
    let mut server: Option<NotificationsProxy<'static>> = None;
    let mut clicks: Option<ActionInvokedStream> = None;
    loop {
        let click = async {
            match &mut clicks {
                Some(clicks) => clicks.next().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            command = pending.recv() => {
                let Some(command) = command else { return };
                match command {
                    Command::Tell(trouble) => {
                        let subject = trouble.subject();
                        let Some(replaces) = ledger.tell(&subject, Instant::now()) else {
                            continue;
                        };
                        // The bus only once there is something to tell.
                        if server.is_none() {
                            match connect().await {
                                Ok((proxy, stream)) => {
                                    server = Some(proxy);
                                    clicks = stream;
                                }
                                Err(e) => {
                                    debug!("no notifications: {e}");
                                    continue;
                                }
                            }
                        }
                        let Some(proxy) = &server else { continue };
                        let (summary, body) = trouble.text(chinese());
                        let hints = HashMap::from([
                            ("desktop-entry", Variant::from("io.github.ign1x.OhMyIme.Settings")),
                            ("urgency", Variant::U8(1)),
                        ]);
                        let actions = ["default", if chinese() { "打开设置" } else { "Open settings" }];
                        match proxy
                            .notify("oh-my-ime", replaces, "input-keyboard", &summary, &body, &actions, hints, -1)
                            .await
                        {
                            Ok(id) => {
                                info!(id, summary, "told the user");
                                ledger.shown(&subject, id);
                            }
                            Err(e) => debug!("cannot show a notification: {e}"),
                        }
                    }
                    Command::Resolved(subject) => {
                        if let (Some(id), Some(proxy)) = (ledger.resolve(&subject), &server) {
                            let _ = proxy.close_notification(id).await;
                        }
                    }
                }
            }
            Some(signal) = click => {
                let Ok(args) = signal.args() else { continue };
                if ledger.is_ours(args.id)
                    && args.action_key == "default"
                    && let Err(e) = crate::programs::open_settings_page(display.as_deref(), "about")
                {
                    debug!("{e}");
                }
            }
        }
    }
}

/// The notification server on the session bus, and its clicks.
async fn connect() -> zbus::Result<(NotificationsProxy<'static>, Option<ActionInvokedStream>)> {
    let conn = zbus::Connection::session().await?;
    let proxy = NotificationsProxy::new(&conn).await?;
    let clicks = proxy.receive_action_invoked().await.ok();
    Ok((proxy, clicks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn troubles_are_told_once_until_resolved() {
        let mut ledger = Ledger::default();
        let seat = Subject::Seat("seat0".to_owned());
        let start = Instant::now();
        assert_eq!(ledger.tell(&seat, start), Some(0));
        ledger.shown(&seat, 7);
        assert_eq!(ledger.tell(&seat, start + Duration::from_secs(5)), None);
        // Another subject is told on its own.
        assert_eq!(ledger.tell(&Subject::Config, start), Some(0));

        assert_eq!(ledger.resolve(&seat), Some(7));
        assert_eq!(ledger.resolve(&seat), None, "closed once");
        // Back again: told again, in place of the old notification.
        assert_eq!(ledger.tell(&seat, start + Duration::from_secs(6)), Some(7));
        assert!(ledger.is_ours(7));
        assert!(!ledger.is_ours(8));
    }

    #[test]
    fn troubles_that_stay_are_told_again_after_a_while() {
        let mut ledger = Ledger::default();
        let engine = Subject::Engine("sparkle".to_owned());
        let start = Instant::now();
        assert!(ledger.tell(&engine, start).is_some());
        assert!(ledger.tell(&engine, start + REPEAT_AFTER / 2).is_none());
        assert!(ledger.tell(&engine, start + REPEAT_AFTER).is_some());
        // Nothing was told about a subject that never was in trouble.
        assert_eq!(ledger.resolve(&Subject::Config), None);
    }

    #[test]
    fn notifications_say_what_happened_in_both_languages() {
        let trouble = Trouble::Engine {
            id: "sparkle".to_owned(),
            name: "Sparkle".to_owned(),
            why: EngineFailure::Launch("`omime-sparkle` is not installed".to_owned()),
        };
        let (summary, body) = trouble.text(false);
        assert_eq!(summary, "Sparkle stopped");
        assert!(body.starts_with("It could not be started: `omime-sparkle` is not installed."));
        let (summary, _) = trouble.text(true);
        assert_eq!(summary, "Sparkle已停止");
        // TOML errors show a snippet of the file, which a notification does without.
        let config = Trouble::Config {
            error: "/home/me/.config/oh-my-ime/config.toml: TOML parse error at line 2, \
                    column 11\n  |\n2 | enabled = \"yes\"\n  |           ^^^^^\n\
                    invalid type: string \"yes\", expected a boolean\n"
                .to_owned(),
            kept: true,
        };
        assert_eq!(
            config.text(false).1,
            "TOML parse error at line 2, column 11: invalid type: string \"yes\", expected \
             a boolean. oh-my-ime keeps the configuration it had."
        );
        assert_eq!(config.subject(), Subject::Config);
        assert_eq!(
            brief("cannot read /home/me/config.toml: Permission denied"),
            "cannot read /home/me/config.toml: Permission denied"
        );
    }
}
