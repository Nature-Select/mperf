//! Cold / hot app-launch timing on iOS via Instruments processcontrol.
//!
//! Cold start: times `processcontrol.launchApp` RPC + waits for the
//! launched PID to appear in a sysmontap sample stream — a first-
//! activity proxy that's closer to Android's "first frame rendered"
//! semantics than the bare RPC return alone.
//!
//! Hot start: times just the launchApp RPC. The sysmontap proxy
//! doesn't help here because the target's PID is already in the
//! sample stream from before the launch (the process never died, we
//! just resumed it from background) — so "first sample with target
//! PID" arrives immediately and tells us nothing about UI re-attach.
//!
//! Foreground handling: same SpringBoard pre-step as the Android
//! HOME-key trick. Without it, both cold (kill_existing=true) and
//! hot (kill_existing=false) on a foreground target can leave the
//! device on a black screen because DTX-initiated launches aren't
//! always treated as user-foreground intents by SpringBoard.
//!
//! Phase breakdown (PerfDog-iOS-style "didFinishLaunching 80.99ms"
//! waterfall) is deferred — that needs a custom DTX trace-events
//! client we haven't implemented.

use crate::connect;
use crate::launch::{launch_app_with_options, launch_app_with_options_timed};
use crate::sysmontap_raw::{SysmontapConfig, SysmontapRaw};
use anyhow::{anyhow, Context, Result};
use idevice::{
    core_device_proxy::CoreDeviceProxy,
    dvt::{device_info::DeviceInfoClient, remote_server::RemoteServerClient},
    rsd::RsdHandshake,
    IdeviceService, ReadWrite,
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct StartupTiming {
    pub total_ms: u64,
}

/// Cold start: kill any existing process and time the relaunch.
///
/// 1. Launch SpringBoard (best-effort, to put the home screen
///    forward so the relaunch is routed as a user-foreground intent
///    by SpringBoard — fixes "black screen after cold start" on
///    foreground targets).
/// 2. Open a sysmontap session and start streaming process samples
///    (untimed setup overhead, ~2s).
/// 3. Launch the target via processcontrol with `kill_existing=true`
///    on a separate DTX session, timing only the inner RPC.
/// 4. Wait for a sysmontap sample whose `processes` Dict contains
///    the new PID — proxy for "app is running and emitting metrics",
///    closer to first-frame than the bare RPC return.
/// 5. Report `rpc_ms + wait_ms` as total launch latency.
pub async fn measure_cold_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    if let Err(e) = launch_app_with_options(udid, "com.apple.springboard", false).await {
        tracing::debug!(error = %e, "cold start: SpringBoard pre-launch failed; continuing");
    } else {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Setup sysmontap session on its own DTX channel (separate from
    // the one launch_app_with_options_timed builds internally — we
    // can't share `remote` because SysmontapRaw and ProcessControlClient
    // each take `&mut RemoteServerClient` and the borrows would
    // conflict). ~1-2s setup, not counted in the measurement.
    let mut remote = build_dtx_remote(udid).await?;

    // proc_attrs is required by sysmontap's set_config even though we
    // don't decode any per-process values — we only key on PID, which
    // is the processes Dict key itself.
    let proc_attrs = {
        let mut info = DeviceInfoClient::new(&mut remote)
            .await
            .context("DeviceInfoClient::new")?;
        info.sysmon_process_attributes()
            .await
            .context("sysmon_process_attributes")?
    };

    let mut sysmontap = SysmontapRaw::new(&mut remote)
        .await
        .context("SysmontapRaw::new")?;
    sysmontap
        .set_config(&SysmontapConfig {
            interval_ms: 300,
            process_attributes: proc_attrs,
            system_attributes: vec![],
        })
        .await
        .context("sysmontap set_config")?;
    sysmontap.start().await.context("sysmontap.start")?;

    // Drain one sample so the next one strictly reflects post-launch
    // state. Bounded — a silently-dropped first sample shouldn't
    // hang the measurement.
    let _ = tokio::time::timeout(Duration::from_millis(1500), sysmontap.next_sample()).await;

    // Launch via a separate DTX session. launch_app_with_options_timed
    // returns only the inner RPC duration — its own setup overhead is
    // excluded from the number we report.
    let (pid, rpc_elapsed) = launch_app_with_options_timed(udid, bundle_id, true).await?;

    // Hand-off: launch_app_with_options_timed timed only the inner
    // RPC, then returns. Microseconds between return and our
    // wait_start, so wait_start ≈ "moment device acknowledged the
    // launch RPC". Total = RPC time + time until sysmontap sees the
    // new PID active = "RPC start → first activity" measurement,
    // excluding both DTX setups.
    let pid_key = pid.to_string();
    let wait_start = Instant::now();
    let deadline = wait_start + Duration::from_secs(8);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!(
                "sysmontap proxy timed out waiting for PID {pid} (8s); device may be \
                 throttled or sysmontap stalled"
            );
        }
        let sample = tokio::time::timeout(remaining, sysmontap.next_sample())
            .await
            .map_err(|_| anyhow!("sysmontap next_sample timed out"))?
            .map_err(|e| anyhow!("sysmontap next_sample error: {e}"))?;
        if let Some(processes) = sample.processes {
            if processes.contains_key(&pid_key) {
                break;
            }
        }
    }
    let wait_elapsed = wait_start.elapsed();

    Ok(StartupTiming {
        total_ms: (rpc_elapsed + wait_elapsed).as_millis() as u64,
    })
}

/// Hot start: bring an existing process forward without re-init.
///
/// SpringBoard pre-step ensures the target is backgrounded so the
/// launchApp call has something meaningful to do — without it,
/// foreground targets no-op with `LaunchState: UNKNOWN`. We don't
/// use the sysmontap-PID proxy here: the target was already in the
/// sample stream before the launch (background process still counts
/// as "running"), so "first sample with PID" arrives immediately and
/// tells us nothing about UI re-attach time. RPC-only timing is the
/// best honest signal we have for this mode.
pub async fn measure_hot_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    if let Err(e) = launch_app_with_options(udid, "com.apple.springboard", false).await {
        tracing::debug!(error = %e, "hot start: SpringBoard pre-launch failed; measuring direct launch");
    } else {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let (_pid, rpc_elapsed) =
        launch_app_with_options_timed(udid, bundle_id, false).await?;
    Ok(StartupTiming {
        total_ms: rpc_elapsed.as_millis() as u64,
    })
}

/// Build a fresh DTX RemoteServerClient — same boilerplate as
/// launch.rs but exposed here so measure_cold_start can mount a
/// sysmontap channel on its own session.
async fn build_dtx_remote(udid: &str) -> Result<RemoteServerClient<Box<dyn ReadWrite>>> {
    let provider = connect::provider_for(udid).await.context("provider_for")?;
    let proxy = CoreDeviceProxy::connect(&*provider)
        .await
        .context("CoreDeviceProxy::connect")?;
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let adapter = proxy
        .create_software_tunnel()
        .context("create_software_tunnel")?;
    let mut handle = adapter.to_async_handle();
    let rsd_stream = handle
        .connect(rsd_port)
        .await
        .map_err(|e| anyhow!("adapter connect rsd: {e}"))?;
    let handshake = RsdHandshake::new(rsd_stream)
        .await
        .context("RsdHandshake::new")?;

    const DTSERVICEHUB: &str = "com.apple.instruments.dtservicehub";
    let dvt_port = handshake
        .services
        .get(DTSERVICEHUB)
        .ok_or_else(|| anyhow!("RSD service '{DTSERVICEHUB}' not advertised"))?
        .port;
    let dvt_stream = handle
        .connect(dvt_port)
        .await
        .map_err(|e| anyhow!("connect dvt port {dvt_port}: {e}"))?;
    let boxed: Box<dyn ReadWrite> = Box::new(dvt_stream);
    Ok(RemoteServerClient::new(boxed))
}
