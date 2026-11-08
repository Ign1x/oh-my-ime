//! Long-session resource regressions. Build the workspace first, then run one test:
//! CARGO_HOME=/tmp/omime-cargo-home cargo test -p omime-e2e --locked \
//!   --test release_core_bounds -- --ignored FILTER --nocapture
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
fn abandoned_engine_requests_have_a_resource_bound() -> anyhow::Result<()> {
    let env = TestEnv::start(&[engine("hang")], Some("review"))?;
    env.configure("[tray]\nenabled = false\n")?;
    env.wait_for_log("engine ready")?;
    run(async {
        let connection = bus(&env).await?;
        let pid = daemon_pid(&connection).await?;
        let ic = context(&connection, "one-application", "x11:").await?;
        // Reuse ONE protocol IC: context-count caps cannot prevent this leak.
        // A core ContextId is new each time it gains focus.
        for _ in 0..64 {
            ic.call::<_, _, ()>("FocusIn", &()).await?;
            ic.call::<_, _, ()>("FocusOut", &()).await?;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let baseline = rss_kib(pid)?;
        const N: usize = 4096;
        for _ in 0..N {
            ic.call::<_, _, ()>("FocusIn", &()).await?;
            ic.call::<_, _, ()>("FocusOut", &()).await?;
        }
        ic.call::<_, _, ()>("DestroyIC", &()).await?;
        // Longer than activation timeout + recovery. Abandoned calls have lost
        // their Context/InFlight, so the timer never examines them again.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let after = rss_kib(pid)?;
        let log = env.daemon_log();
        let activations = log.matches("review activated ").count();
        let deactivations = log.matches("review deactivated ").count();
        eprintln!(
            "retired={N}, activated={activations}, deactivated={deactivations}, daemon_rss_kib={baseline}->{after}"
        );
        assert!(activations >= N, "fixture did not receive the activations");
        assert!(
            deactivations >= N,
            "fixture did not receive the deactivations"
        );
        assert!(
            after.saturating_sub(baseline) <= 8 * 1024,
            "{N} retired contexts retained {} KiB in omimed (budget 8192 KiB); replies/tasks must be cancelled or expire after deactivation",
            after.saturating_sub(baseline)
        );
        Ok(())
    })
}

#[test]
#[ignore = "private headless compositor; build workspace first"]
fn context_update_flood_does_not_delay_the_next_key_indefinitely() -> anyhow::Result<()> {
    let env = TestEnv::start(&[engine("slow-updates")], Some("review"))?;
    env.wait_for_log("engine ready")?;
    run(async {
        let connection = bus(&env).await?;
        let ic = context(&connection, "one-application", "x11:").await?;
        ic.call::<_, _, ()>("FocusIn", &()).await?;
        let _: bool = ic.call("ProcessKeyEvent", &KEY).await?;
        ic.call::<_, _, ()>("SetSurroundingText", &("ab", 0_u32, 0_u32))
            .await?;
        const N: u32 = 256;
        for i in 0..N {
            // Two bytes of retained text. This is a queue-volume bug, independent
            // of per-message/per-context text limits (known finding F-7).
            let position = (i + 1) % 2;
            ic.call::<_, _, ()>("SetSurroundingTextPosition", &(position, position))
                .await?;
        }
        let start = Instant::now();
        let key = tokio::time::timeout(
            Duration::from_secs(1),
            ic.call::<_, _, bool>("ProcessKeyEvent", &KEY),
        )
        .await;
        eprintln!(
            "queued_updates={N}, key_wait={:?}, answer={key:?}",
            start.elapsed()
        );
        assert!(
            key.is_ok(),
            "a key queued behind {N} obsolete updates did not finish within 1s, although each engine RPC takes only 80ms (<200ms timeout)"
        );
        key??;
        Ok(())
    })
}

#[test]
#[ignore = "private headless compositor; build workspace first"]
fn keys_remain_responsive_while_context_updates_continue() -> anyhow::Result<()> {
    let env = TestEnv::start(&[engine("slow-updates")], Some("review"))?;
    env.wait_for_log("engine ready")?;
    run(async {
        let connection = bus(&env).await?;
        let ic = context(&connection, "continuous-updates", "x11:").await?;
        ic.call::<_, _, ()>("FocusIn", &()).await?;
        let _: bool = ic.call("ProcessKeyEvent", &KEY).await?;
        ic.call::<_, _, ()>("SetSurroundingText", &("ab", 0_u32, 0_u32))
            .await?;
        let sent = std::cell::Cell::new(0_u32);
        let updates = async {
            for i in 0..512_u32 {
                let position = (i + 1) % 2;
                ic.call::<_, _, ()>("SetSurroundingTextPosition", &(position, position))
                    .await?;
                sent.set(i + 1);
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            anyhow::Ok(())
        };
        let keys = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let mut waits = Vec::new();
            for _ in 0..6 {
                let before = sent.get();
                let start = Instant::now();
                let handled = tokio::time::timeout(
                    Duration::from_millis(400),
                    ic.call::<_, _, bool>("ProcessKeyEvent", &KEY),
                )
                .await
                .context("continuous updates held a key past its arrival budget")??;
                waits.push(start.elapsed());
                assert!(!handled, "the fixture passes each accepted key through");
                assert!(
                    sent.get() > before && sent.get() < 512,
                    "updates must continue during each key wait"
                );
            }
            eprintln!("continuous_updates={}, key_waits={waits:?}", sent.get());
            anyhow::Ok(())
        };
        let (updates, keys) = tokio::join!(updates, keys);
        updates?;
        keys?;
        Ok(())
    })
}
