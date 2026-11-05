//! Optional activation in the current session. Frameworks are addressed through
//! their own D-Bus owner, never by killing every process with a matching name.

use omime_proto::tr;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail, ensure};

use crate::DaemonProxy;

const START_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
enum FrameworkKind {
    Fcitx,
    Ibus,
}

#[derive(Debug)]
struct Framework {
    kind: FrameworkKind,
    connection: zbus::Connection,
    owner: String,
}

impl Framework {
    fn name(&self) -> &'static str {
        match self.kind {
            FrameworkKind::Fcitx => "Fcitx5",
            FrameworkKind::Ibus => "IBus",
        }
    }

    async fn stop(&self) -> anyhow::Result<()> {
        let (path, interface) = match self.kind {
            FrameworkKind::Fcitx => ("/controller", "org.fcitx.Fcitx.Controller1"),
            FrameworkKind::Ibus => ("/org/freedesktop/IBus", "org.freedesktop.IBus"),
        };
        let proxy =
            zbus::Proxy::new(&self.connection, self.owner.as_str(), path, interface).await?;
        let exit = async {
            match self.kind {
                FrameworkKind::Fcitx => proxy.call::<_, _, ()>("Exit", &()).await,
                FrameworkKind::Ibus => proxy.call::<_, _, ()>("Exit", &false).await,
            }
        };
        // A daemon may close its bus before replying to Exit. Its disappearance,
        // rather than receipt of the reply, is the completion criterion.
        let reply = tokio::time::timeout(Duration::from_secs(2), exit).await;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match owner(&self.connection, &self.owner).await {
                Ok(None) => return Ok(()),
                Err(_) if matches!(self.kind, FrameworkKind::Ibus) => return Ok(()),
                _ => {}
            }
            ensure!(
                Instant::now() < deadline,
                "{}",
                tr!(
                    "{arg0} did not exit ({reply_1}); stop its session service and retry --start",
                    arg0 = self.name(),
                    reply_1 = format!("{:?}", reply)
                )
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[derive(Debug)]
pub(super) struct Prepared {
    connection: zbus::Connection,
    frameworks: Vec<Framework>,
}

pub(super) async fn prepare(take_over: bool) -> anyhow::Result<Prepared> {
    ensure!(
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|s| !s.is_empty()),
        "{}",
        tr!(
            "--start needs a Wayland session (WAYLAND_DISPLAY is not set); run setup without --start for the next login"
        )
    );
    let connection = zbus::Connection::session()
        .await
        .context(tr!("--start cannot reach the session bus"))?;
    let mut frameworks = Vec::new();
    // A running omimed already owns its compatibility frontends. Its hosted
    // Fcitx5/IBus must not be mistaken for a competing session framework.
    if !crate::daemon_owns_bus_name(&connection).await? {
        if let Some(owner) = owner(&connection, "org.fcitx.Fcitx5").await? {
            frameworks.push(Framework {
                kind: FrameworkKind::Fcitx,
                connection: connection.clone(),
                owner,
            });
        }
        if let Some(ibus) = ibus_connection().await
            && let Ok(Some(owner)) = owner(&ibus, "org.freedesktop.IBus").await
        {
            frameworks.push(Framework {
                kind: FrameworkKind::Ibus,
                connection: ibus,
                owner,
            });
        }
    } else {
        let daemon = DaemonProxy::new(&connection).await?;
        check_display(&daemon.diagnose().await?)?;
    }
    ensure!(
        take_over || frameworks.is_empty(),
        "{}",
        tr!(
            "{arg0} is already running in this session; use --take-over to request its exit before starting omimed",
            arg0 = frameworks
                .iter()
                .map(Framework::name)
                .collect::<Vec<_>>()
                .join(" and ")
        )
    );
    for framework in &frameworks {
        println!(
            "{}",
            tr!(
                "Take over from {arg0} in this session.",
                arg0 = framework.name()
            )
        );
    }
    Ok(Prepared {
        connection,
        frameworks,
    })
}

async fn ibus_connection() -> Option<zbus::Connection> {
    let program = crate::install::find_in_path("ibus")?;
    let mut command = tokio::process::Command::new(program);
    command
        .arg("address")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(2), command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let address = String::from_utf8(output.stdout).ok()?;
    let address = address.trim();
    if !address.starts_with("unix:") {
        return None;
    }
    let builder = zbus::connection::Builder::address(address);
    tokio::time::timeout(Duration::from_secs(2), builder.build())
        .await
        .ok()?
        .ok()
}

async fn owner(connection: &zbus::Connection, name: &str) -> zbus::Result<Option<String>> {
    match connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &name,
        )
        .await
    {
        Ok(reply) => Ok(Some(reply.body().deserialize()?)),
        Err(zbus::Error::MethodError(name, ..))
            if name.as_str() == "org.freedesktop.DBus.Error.NameHasNoOwner" =>
        {
            Ok(None)
        }
        Err(zbus::Error::FDO(error)) if matches!(*error, zbus::fdo::Error::NameHasNoOwner(_)) => {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

pub(super) async fn start(
    prepared: Prepared,
    daemon_path: &Path,
    state_dir: &Path,
    engine: Option<&str>,
) -> anyhow::Result<()> {
    for framework in prepared.frameworks {
        framework.stop().await?;
    }
    let connection = prepared.connection;
    let mut child = None;
    let log_path = state_dir.join("omimed.log");
    if !crate::daemon_owns_bus_name(&connection).await? {
        std::fs::create_dir_all(state_dir)?;
        // Do not follow an unexpected log symlink while preparing a new daemon.
        if let Ok(meta) = std::fs::symlink_metadata(&log_path) {
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "{}",
                tr!(
                    "{arg0} must be a regular log file",
                    arg0 = log_path.display()
                )
            );
        }
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&log_path)?;
        child = Some(
            Command::new(daemon_path)
                .envs(super::ENVIRONMENT)
                .env_remove("IBUS_ADDRESS")
                .env_remove("IBUS_ADDRESS_FILE")
                .stdin(Stdio::null())
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log))
                .process_group(0)
                .spawn()
                .with_context(|| tr!("cannot start {arg0}", arg0 = daemon_path.display()))?,
        );
    }
    let deadline = Instant::now() + START_TIMEOUT;
    while !crate::daemon_owns_bus_name(&connection).await? {
        if let Some(child) = &mut child
            && let Some(status) = child.try_wait()?
        {
            bail!(
                "{}",
                tr!(
                    "omimed exited with {status}; see {arg0}",
                    status = status,
                    arg0 = log_path.display()
                )
            );
        }
        ensure!(
            Instant::now() < deadline,
            "{}",
            tr!(
                "omimed did not start; see {arg0}",
                arg0 = log_path.display()
            )
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let daemon = DaemonProxy::new(&connection).await?;
    check_display(&daemon.diagnose().await?)?;
    daemon.reload().await?;
    if let Some(engine) = engine {
        daemon
            .switch(if engine == "direct" { "" } else { engine })
            .await?;
    }
    loop {
        let diagnosis = daemon.diagnose().await?;
        check_display(&diagnosis)?;
        if let Some(error) = crate::string(&diagnosis, "input-method-error") {
            bail!(
                "{}",
                tr!(
                    "the input method could not be selected: {error}",
                    error = error
                )
            );
        }
        let configured: bool = diagnosis
            .get("input-method-configured")
            .context(tr!(
                "omimed is too old for setup --start; restart it with the installed version"
            ))?
            .try_clone()?
            .try_into()?;
        let seats: Vec<(String, bool)> = diagnosis
            .get("seats")
            .context(tr!("omimed did not report its seats"))?
            .try_clone()?
            .try_into()?;
        let status = daemon.status().await?;
        let selected = crate::string(&status, "engine");
        let method = crate::string(&status, "input-method").or_else(|| selected.clone());
        let requested = match engine {
            Some("direct") => selected.is_none(),
            Some(id) if id.contains('/') => method.as_deref() == Some(id),
            Some(id) => selected.as_deref() == Some(id),
            None => true,
        };
        let engines = daemon.list_engines().await?;
        let ready = selected.as_ref().is_none_or(|id| {
            engines
                .iter()
                .any(|(engine, _, _, state)| engine == id && state == "ready")
        });
        if let Some(id) = &selected
            && engines
                .iter()
                .any(|(engine, _, _, state)| engine == id && state == "failed")
        {
            bail!(
                "{}",
                tr!(
                    "engine {id} failed to start; run omime doctor and inspect {arg0}",
                    id = id,
                    arg0 = log_path.display()
                )
            );
        }
        if !seats.is_empty()
            && seats.iter().all(|(_, ours)| *ours)
            && requested
            && ready
            && configured
        {
            crate::launch_env::apply(&connection).await
                .context(tr!("input is running, but application launch settings could not be updated; retry setup --start or log out and back in"))?;
            println!(
                "{}",
                tr!(
                    "Input is ready: {arg0}.",
                    arg0 = crate::format_status(&status, false)
                )
            );
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "{}",
            tr!(
                "omimed has not acquired its input seat or selected input method; stop any remaining session input method and run omime doctor (log: {arg0})",
                arg0 = log_path.display()
            )
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn check_display(
    diagnosis: &std::collections::HashMap<String, zbus::OwnedVariant>,
) -> anyhow::Result<()> {
    if let (Some(expected), Some(actual)) = (
        std::env::var_os("WAYLAND_DISPLAY"),
        crate::string(diagnosis, "display"),
    ) {
        let display_path = |display: &Path| {
            if display.is_absolute() {
                display.to_path_buf()
            } else {
                std::env::var_os("XDG_RUNTIME_DIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_default()
                    .join(display)
            }
        };
        ensure!(
            display_path(Path::new(&expected)) == display_path(Path::new(&actual)),
            "{}",
            tr!(
                "omimed belongs to another Wayland display ({actual}); use the matching session bus",
                actual = actual
            )
        );
    }
    Ok(())
}
