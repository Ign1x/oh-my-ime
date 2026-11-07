//! Desktop notifications (`org.freedesktop.Notifications`) about trouble that would
//! otherwise only be in omimed's log: an engine that stopped, another input method on a
//! seat, a mistake in config.toml, or an unsupported desktop. For an unsupported
//! desktop omimed exits right after telling, so the click action has nothing left
//! to do. Each trouble is told once, again only after it was
//! resolved in between or after [`REPEAT_AFTER`], and its notification goes away when it
//! is resolved. A click on one opens the settings, where the About page shows the
//! state of things. `[notifications] enabled = false` keeps omimed quiet.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt as _;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::{debug, info};
use zbus::Variant;
use zbus::proxy::SignalStream;

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
    /// This compositor lacks the protocols omimed requires.
    UnsupportedDesktop,
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
    UnsupportedDesktop,
}

impl Trouble {
    pub fn subject(&self) -> Subject {
        match self {
            Self::Engine { id, .. } => Subject::Engine(id.clone()),
            Self::Seat { name } => Subject::Seat(name.clone()),
            Self::Config { .. } => Subject::Config,
            Self::UnsupportedDesktop => Subject::UnsupportedDesktop,
        }
    }

    /// The summary and body, translated together with the rest of the platform.
    fn text(&self, tr: &omime_proto::i18n::Translator) -> (String, String) {
        match self {
            Self::UnsupportedDesktop => (
                tr.text("This desktop is not supported").to_owned(),
                tr.text("oh-my-ime cannot provide input on this desktop yet. See the omimed log for details.").to_owned(),
            ),
            Self::Engine { name, why, .. } => {
                let why = match why {
                    EngineFailure::Crashed => tr.text("It crashed again and again.").to_owned(),
                    EngineFailure::NoStart => tr.text("It did not start in time.").to_owned(),
                    EngineFailure::Protocol => tr.text("It sent the compositor a request that the compositor refused.").to_owned(),
                    EngineFailure::Launch(error) => tr.format("It could not be started: {error}.", &[("error", error)]),
                };
                (
                    tr.format("{name} stopped", &[("name", name)]),
                    tr.format("{why} Until you switch to it again, your keys go to applications as you type them.", &[("why", &why)]),
                )
            }
            Self::Seat { name } => (
                tr.text("Another input method is running").to_owned(),
                tr.format("It holds the keyboard of seat {name}, so oh-my-ime cannot serve it. Quit it (Fcitx 5 or IBus, say), or run `omime setup --take-over`; oh-my-ime takes over once it is gone.", &[("name", name)]),
            ),
            Self::Config { error, kept } => (
                tr.text("config.toml has a mistake").to_owned(),
                tr.format(if *kept {
                    omime_proto::i18n::message("{error}. oh-my-ime keeps the configuration it had.")
                } else {
                    omime_proto::i18n::message("{error}. oh-my-ime uses the defaults until it is fixed.")
                }, &[("error", &brief(error))]),
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

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

/// What the notifier's task does.
#[derive(Debug)]
enum Command {
    Tell(Trouble),
    Resolved(Subject),
    Flush(oneshot::Sender<()>),
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

    /// Give the usual rate-limited notification a chance to arrive before exit.
    pub async fn tell_before_exit(&self, trouble: Trouble) {
        if !self.enabled.get() {
            return;
        }
        self.tell(trouble);
        let (done, pending) = oneshot::channel();
        let _ = self.commands.send(Command::Flush(done));
        let _ = tokio::time::timeout(Duration::from_secs(2), pending).await;
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
    /// The shown notification's id, or 0 before the server answers and after it closes.
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
        let id = std::mem::take(&mut told.id);
        (id != 0).then_some(id)
    }

    /// The desktop closed this notification, without necessarily resolving its trouble.
    fn closed(&mut self, id: u32) {
        for told in self.told.values_mut().filter(|t| t.id == id) {
            told.id = 0;
        }
    }

    /// Whether this is one of our notifications that is still shown.
    fn is_ours(&self, id: u32) -> bool {
        id != 0 && self.told.values().any(|t| t.id == id)
    }
}

async fn serve(mut pending: mpsc::UnboundedReceiver<Command>, display: Option<String>) {
    let mut ledger = Ledger::default();
    let mut server: Option<NotificationsProxy<'static>> = None;
    let mut events: Option<SignalStream<'static>> = None;
    loop {
        let event = async {
            match &mut events {
                Some(events) => events.next().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            command = pending.recv() => {
                let Some(command) = command else { return };
                match command {
                    Command::Flush(done) => { let _ = done.send(()); }
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
                                    events = stream;
                                }
                                Err(e) => {
                                    debug!("no notifications: {e}");
                                    continue;
                                }
                            }
                        }
                        let Some(proxy) = &server else { continue };
                        let (summary, body) = trouble.text(omime_proto::i18n::current());
                        let hints = HashMap::from([
                            ("desktop-entry", Variant::from("io.github.ign1x.OhMyIme.Settings")),
                            ("urgency", Variant::U8(1)),
                        ]);
                        let actions = ["default", omime_proto::i18n::text("Open settings")];
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
            Some(signal) = event => {
                if let Some(closed) = NotificationClosed::from_message(signal.clone()) {
                    let Ok(args) = closed.args() else { continue };
                    ledger.closed(args.id);
                } else if let Some(click) = ActionInvoked::from_message(signal) {
                    let Ok(args) = click.args() else { continue };
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
}

/// The notification server on the session bus, and its signals in arrival order.
async fn connect() -> zbus::Result<(NotificationsProxy<'static>, Option<SignalStream<'static>>)> {
    // A full signal queue can block a method reply. Bound Notify and
    // CloseNotification so the loop can resume draining signals afterwards.
    let conn = zbus::connection::Builder::session()
        .method_timeout(Duration::from_secs(2))
        .build()
        .await?;
    let proxy = NotificationsProxy::new(&conn).await?;
    // A click can be followed immediately by NotificationClosed. Separate streams
    // could reorder them, losing a live click or accepting one after closure.
    let events = proxy.inner().receive_all_signals().await.ok();
    Ok((proxy, events))
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
        // Back again: the closed notification is not replaced.
        assert_eq!(ledger.tell(&seat, start + Duration::from_secs(6)), Some(0));
        assert!(!ledger.is_ours(7));
        assert!(!ledger.is_ours(8));
    }

    #[test]
    fn resolved_notification_ids_are_not_actionable() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        assert_eq!(ledger.tell(&Subject::Config, now), Some(0));
        ledger.shown(&Subject::Config, 7);
        assert!(ledger.is_ours(7));

        assert_eq!(ledger.resolve(&Subject::Config), Some(7));
        assert!(!ledger.is_ours(7), "the resolved notification is closed");
        assert!(!ledger.is_ours(0), "zero never names a notification");
        assert_eq!(ledger.resolve(&Subject::Config), None);
    }

    #[test]
    fn a_new_notification_about_a_resolved_subject_is_actionable() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        assert_eq!(ledger.tell(&Subject::Config, now), Some(0));
        ledger.shown(&Subject::Config, 7);
        assert_eq!(ledger.resolve(&Subject::Config), Some(7));

        assert_eq!(ledger.tell(&Subject::Config, now), Some(0));
        assert!(!ledger.is_ours(7));
        ledger.shown(&Subject::Config, 8);
        assert!(ledger.is_ours(8));
        assert!(!ledger.is_ours(7));
        assert_eq!(ledger.resolve(&Subject::Config), Some(8));
        assert!(!ledger.is_ours(8));
    }

    #[test]
    fn desktop_closure_keeps_the_repeat_deadline_and_other_live_ids() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        let seat = Subject::Seat("seat0".to_owned());
        assert_eq!(ledger.tell(&Subject::Config, now), Some(0));
        ledger.shown(&Subject::Config, 7);
        assert_eq!(ledger.tell(&seat, now), Some(0));
        ledger.shown(&seat, 9);
        ledger.closed(0);
        ledger.closed(123);
        assert!(ledger.is_ours(7));
        assert!(ledger.is_ours(9));

        ledger.closed(7);
        assert!(!ledger.is_ours(7));
        assert!(ledger.is_ours(9));
        assert_eq!(ledger.tell(&Subject::Config, now + REPEAT_AFTER / 2), None);
        assert_eq!(ledger.tell(&Subject::Config, now + REPEAT_AFTER), Some(0));
        ledger.shown(&Subject::Config, 8);
        ledger.closed(7); // A delayed old close cannot retire the new notification.
        assert!(ledger.is_ours(8));
        assert!(ledger.is_ours(9));
    }

    #[test]
    fn resolving_a_dismissed_notification_allows_the_subject_to_be_told_again() {
        let mut ledger = Ledger::default();
        let now = Instant::now();
        assert_eq!(ledger.tell(&Subject::Config, now), Some(0));
        ledger.shown(&Subject::Config, 7);
        ledger.closed(7);
        assert_eq!(ledger.resolve(&Subject::Config), None, "already closed");
        assert_eq!(ledger.tell(&Subject::Config, now), Some(0));
        ledger.shown(&Subject::Config, 8);
        assert!(!ledger.is_ours(7));
        assert!(ledger.is_ours(8));
    }

    #[test]
    fn troubles_that_stay_are_told_again_after_a_while() {
        let mut ledger = Ledger::default();
        let engine = Subject::Engine("sparkle".to_owned());
        let start = Instant::now();
        assert_eq!(ledger.tell(&engine, start), Some(0));
        ledger.shown(&engine, 7);
        assert!(ledger.tell(&engine, start + REPEAT_AFTER / 2).is_none());
        assert_eq!(ledger.tell(&engine, start + REPEAT_AFTER), Some(7));
        assert!(ledger.is_ours(7), "a live notification can be replaced");
        // Nothing was told about a subject that never was in trouble.
        assert_eq!(ledger.resolve(&Subject::Config), None);
    }

    #[tokio::test]
    async fn notifications_before_exit_honor_enabled_and_wait_for_delivery() {
        let (commands, mut pending) = mpsc::unbounded_channel();
        let notifier = Notifier {
            commands,
            enabled: Cell::new(false),
        };
        notifier.tell_before_exit(Trouble::UnsupportedDesktop).await;
        assert!(pending.try_recv().is_err());
        notifier.set_enabled(true);
        let send = notifier.tell_before_exit(Trouble::UnsupportedDesktop);
        let deliver = async {
            assert!(matches!(
                pending.recv().await,
                Some(Command::Tell(Trouble::UnsupportedDesktop))
            ));
            let Some(Command::Flush(done)) = pending.recv().await else {
                panic!("missing flush")
            };
            done.send(()).unwrap();
        };
        tokio::join!(send, deliver);
    }

    #[test]
    fn an_unsupported_desktop_is_told_once_in_both_languages() {
        let trouble = Trouble::UnsupportedDesktop;
        let mut ledger = Ledger::default();
        let now = Instant::now();
        assert_eq!(ledger.tell(&trouble.subject(), now), Some(0));
        assert_eq!(ledger.tell(&trouble.subject(), now), None);
        let (summary, body) = trouble.text(&omime_proto::i18n::Translator::for_locale("en"));
        assert_eq!(summary, "This desktop is not supported");
        assert!(body.contains("omimed log"));
        let (summary, body) = trouble.text(&omime_proto::i18n::Translator::for_locale("zh"));
        assert_eq!(summary, "尚不支持此桌面");
        assert!(body.contains("日志"));
    }

    #[test]
    fn notifications_say_what_happened_in_both_languages() {
        let trouble = Trouble::Engine {
            id: "sparkle".to_owned(),
            name: "Sparkle".to_owned(),
            why: EngineFailure::Launch("`omime-sparkle` is not installed".to_owned()),
        };
        let (summary, body) = trouble.text(&omime_proto::i18n::Translator::for_locale("en"));
        assert_eq!(summary, "Sparkle stopped");
        assert!(body.starts_with("It could not be started: `omime-sparkle` is not installed."));
        let (summary, _) = trouble.text(&omime_proto::i18n::Translator::for_locale("zh"));
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
            config
                .text(&omime_proto::i18n::Translator::for_locale("en"))
                .1,
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
