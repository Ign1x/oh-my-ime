//! Long-session resource regressions. Build the workspace first, then run one test:
//! CARGO_HOME=/tmp/omime-cargo-home cargo test -p omime-e2e --locked \
//!   --test review_growth -- --ignored FILTER --nocapture
//!
//! All fixtures use private headless sessions. Bounds below are proposed policies,
//! not claims that the current implementation documents those limits.

use std::time::{Duration, Instant};

use anyhow::Context as _;
use omime_e2e::harness::{TestEngine, TestEnv, target_dir};

const PORTAL: &str = "org.freedesktop.portal.Fcitx";
const KEY: (u32, u32, u32, bool, u32) = (0x61, 38, 0, false, 1);

fn engine(behavior: &str) -> TestEngine {
    TestEngine {
        id: "review",
        program: target_dir().join("omime-review-growth-engine"),
        args: vec![behavior.into()],
        env: Vec::new(),
        hosted: false,
    }
}

fn run(test: impl Future<Output = anyhow::Result<()>>) -> anyhow::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(120), test)
                .await
                .context("review test exceeded its 120s safety deadline")?
        })
}

async fn bus(env: &TestEnv) -> anyhow::Result<zbus::Connection> {
    let address = format!("unix:path={}", env.dir().join("bus").display());
    Ok(zbus::connection::Builder::address(address.as_str())
        .method_timeout(Duration::from_secs(5))
        .build()
        .await?)
}

async fn context(
    connection: &zbus::Connection,
    program: &str,
    display: &str,
) -> anyhow::Result<zbus::Proxy<'static>> {
    let im = zbus::Proxy::new(
        connection,
        PORTAL,
        "/org/freedesktop/portal/inputmethod",
        "org.fcitx.Fcitx.InputMethod1",
    )
    .await?;
    let (path, _): (zbus::OwnedObjectPath, Vec<u8>) = im
        .call(
            "CreateInputContext",
            &vec![("program", program), ("display", display)],
        )
        .await?;
    Ok(zbus::Proxy::new_owned(
        connection.clone(),
        PORTAL,
        path,
        "org.fcitx.Fcitx.InputContext1",
    )
    .await?)
}

async fn daemon_pid(connection: &zbus::Connection) -> anyhow::Result<u32> {
    let dbus = zbus::Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await?;
    Ok(dbus
        .call("GetConnectionUnixProcessID", &("io.github.ign1x.OhMyIme",))
        .await?)
}

fn rss_kib(pid: u32) -> anyhow::Result<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
    Ok(status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .context("daemon has no VmRSS")?
        .split_whitespace()
        .next()
        .context("empty VmRSS")?
        .parse()?)
}

#[test]
#[ignore = "private headless compositor; build workspace first"]
fn repeated_focus_in_coalesces_compositor_queries() -> anyhow::Result<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let env = TestEnv::start(&[], None)?;
    // The IPC lookup uses the newest Hyprland socket when no instance is selected.
    let instance = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").unwrap_or_else(|_| "review".into());
    let dir = env.dir().join("hypr").join(instance);
    std::fs::create_dir_all(&dir)?;
    let socket = dir.join(".socket.sock");
    run(async {
        let listener = tokio::net::UnixListener::bind(socket)?;
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = accepted.clone();
        let server = tokio::spawn(async move {
            let mut streams = Vec::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                streams.push(stream); // deliberately no reply; omimed has a 300ms timeout
                count.fetch_add(1, Ordering::Relaxed);
            }
        });
        let connection = bus(&env).await?;
        let pid = daemon_pid(&connection).await?;
        let ic = context(&connection, "one-application", "wayland:").await?;
        let baseline_fds = std::fs::read_dir(format!("/proc/{pid}/fd"))?.count();
        let start = Instant::now();
        const N: usize = 256;
        for _ in 1..N {
            ic.call_noreply("FocusIn", &()).await?;
        }
        ic.call::<_, _, ()>("FocusIn", &()).await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let queries = accepted.load(Ordering::Relaxed);
        let fds = std::fs::read_dir(format!("/proc/{pid}/fd"))?.count();
        eprintln!(
            "FocusIn={N}, queries={queries}, elapsed={:?}, daemon_fds={baseline_fds}->{fds}",
            start.elapsed()
        );
        server.abort();
        let _ = server.await;
        assert!(
            queries <= 2,
            "{N} FocusIn calls on ONE already-focused IC created {queries} compositor queries; allow one in flight and at most one coalesced refresh"
        );
        Ok(())
    })
}

struct ResumeOnDrop(rustix::process::Pid);

impl Drop for ResumeOnDrop {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.0, rustix::process::Signal::CONT);
    }
}

#[test]
#[ignore = "private headless compositor; build workspace first"]
fn stalled_session_bus_does_not_retain_every_preedit() -> anyhow::Result<()> {
    let env = TestEnv::start(&[engine("flood-preedit")], Some("review"))?;
    env.configure("[tray]\nenabled = false\n")?;
    env.wait_for_log("engine ready")?;
    run(async {
        let connection = bus(&env).await?;
        let pid = daemon_pid(&connection).await?;
        let ic = context(&connection, "one-application", "x11:").await?;
        ic.call::<_, _, ()>("FocusIn", &()).await?;
        let _: bool = ic.call("ProcessKeyEvent", &KEY).await?;
        let dbus = zbus::Proxy::new(
            &connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await?;
        let bus_pid: u32 = dbus
            .call("GetConnectionUnixProcessID", &("org.freedesktop.DBus",))
            .await?;
        let bus_pid =
            rustix::process::Pid::from_raw(bus_pid.try_into()?).context("invalid bus pid")?;
        let baseline = rss_kib(pid)?;
        // Only the TestEnv's private bus is stopped; RAII resumes it on every exit.
        rustix::process::kill_process(bus_pid, rustix::process::Signal::STOP)?;
        let resume = ResumeOnDrop(bus_pid);
        std::fs::write(env.dir().join("review-flood-start"), b"start")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !env.dir().join("review-flood-done").exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // A fixed implementation may apply backpressure and leave the producer
        // blocked. It need not finish the flood to satisfy the memory budget.
        let completed = env.dir().join("review-flood-done").exists();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let after = rss_kib(pid)?;
        drop(resume);
        eprintln!(
            "preedits=8192, producer_finished={completed}, bytes_per_preedit=2048, daemon_rss_kib={baseline}->{after}"
        );
        assert!(
            after.saturating_sub(baseline) <= 8 * 1024,
            "8192 replacement preedits with a blocked bus retained {} KiB (budget 8192 KiB); ingress/Outgoing need byte budgets and backpressure or coalescing",
            after.saturating_sub(baseline)
        );
        Ok(())
    })
}
