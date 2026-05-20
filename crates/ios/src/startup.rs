//! Cold / hot app-launch timing on iOS via Instruments processcontrol.
//!
//! Two-session approach for cold start: ALL DTX setup happens BEFORE
//! timing starts. We pre-build a sysmontap session and a separate
//! processcontrol session, drain any startup samples, then time the
//! launchApp RPC + the wait for the first sysmontap sample that
//! shows the new PID. Total = RPC duration + time-to-first-activity.
//!
//! Previous attempt did the launch via `launch_app_with_options_timed`
//! which builds its OWN DTX session inside the function (~1-2s setup).
//! That setup time ran AFTER sysmontap started streaming, so sysmontap
//! emitted samples we never read, and they queued up. When the wait
//! loop ran, it consumed those stale samples first before reaching
//! one with the new PID — inflating the reported number by several
//! seconds. By holding both pc and sysmontap on pre-built remotes,
//! the gap between "RPC start" and "wait start" is microseconds and
//! the sysmontap queue is empty when we start reading.
//!
//! Honest semantics:
//! - "First sample with PID" ≈ "device started scheduling work for
//!   the new process" — not literally first frame, but typically
//!   bounded close to it because sysmontap only registers a PID once
//!   the kernel begins accounting for it.
//! - sysmontap interval (300ms) sets a floor on measurement
//!   granularity. The actual first-activity moment may be up to one
//!   interval earlier than what we observe.
//!
//! For TRUE first-frame timing, you'd want DTX trace events
//! (`coreprofilesessiontap` / `activitytracetap`) — idevice 0.1.61
//! doesn't expose a client, and rolling our own is ~500-1000 LOC of
//! brittle binary-protocol work. Deferred.
//!
//! Hot start: keeps RPC-only timing. The sysmontap proxy adds no
//! signal because the target's PID is already in the sample stream
//! before launch (backgrounded process still counts as alive).
//!
//! Foreground handling: SpringBoard pre-step puts the home screen
//! forward before each measurement. Without it, DTX-initiated
//! launches on a foreground target sometimes leave the device on a
//! black screen.

use crate::connect;
use crate::launch::launch_app_with_options;
use crate::sysmontap_raw::{SysmontapConfig, SysmontapRaw};
use anyhow::{anyhow, Context, Result};
use idevice::{
    core_device_proxy::CoreDeviceProxy,
    dvt::{
        device_info::DeviceInfoClient, process_control::ProcessControlClient,
        remote_server::RemoteServerClient,
    },
    rsd::RsdHandshake,
    IdeviceService, ReadWrite,
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct StartupTiming {
    pub total_ms: u64,
}

pub async fn measure_cold_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    if let Err(e) = launch_app_with_options(udid, "com.apple.springboard", false).await {
        tracing::debug!(error = %e, "cold start: SpringBoard pre-launch failed; continuing");
    } else {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // --- Pre-built session A: sysmontap waiter -------------------
    // Built FIRST so the channel is fully streaming by the time we
    // start the launch RPC below. Drains the initial sample so the
    // first one we read in the wait loop is post-launch.
    let mut remote_sysmontap = build_dtx_remote(udid)
        .await
        .context("DTX session for sysmontap")?;
    let (proc_attrs, sys_attrs) = {
        let mut info = DeviceInfoClient::new(&mut remote_sysmontap)
            .await
            .context("DeviceInfoClient::new")?;
        let p = info
            .sysmon_process_attributes()
            .await
            .context("sysmon_process_attributes")?;
        let s = info
            .sysmon_system_attributes()
            .await
            .context("sysmon_system_attributes")?;
        (p, s)
    };
    let pid_idx = proc_attrs
        .iter()
        .position(|a| a == "pid")
        .ok_or_else(|| anyhow!("sysmontap proc_attrs missing 'pid'; got {proc_attrs:?}"))?;
    let mut sysmontap = SysmontapRaw::new(&mut remote_sysmontap)
        .await
        .context("SysmontapRaw::new")?;
    sysmontap
        .set_config(&SysmontapConfig {
            interval_ms: 300,
            process_attributes: proc_attrs,
            system_attributes: sys_attrs,
        })
        .await
        .context("sysmontap set_config")?;
    sysmontap.start().await.context("sysmontap.start")?;
    // Drain one sample (bounded). next_sample arriving inside the
    // wait loop below is guaranteed to be a post-launch one.
    let _ = tokio::time::timeout(Duration::from_millis(1500), sysmontap.next_sample()).await;

    // --- Pre-built session B: process control --------------------
    // Built BEFORE timing so the RPC dispatch isn't behind a ~1-2s
    // DTX channel build. Once `pc` is constructed, calling
    // pc.launch_app is just one DTX message and one DTX reply.
    let mut remote_pc = build_dtx_remote(udid)
        .await
        .context("DTX session for processcontrol")?;
    let mut pc = ProcessControlClient::new(&mut remote_pc)
        .await
        .context("ProcessControlClient::new")?;

    // --- Timed window: RPC + wait for first PID-bearing sample ---
    let t0 = Instant::now();
    let pid = pc
        .launch_app(
            bundle_id.to_string(),
            None,
            None,
            false, // start_suspended
            true,  // kill_existing
        )
        .await
        .with_context(|| format!("processcontrol.launchApp({bundle_id})"))?;
    let deadline = t0 + Duration::from_secs(8);
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
        if sample_has_pid(&sample, pid, pid_idx) {
            break;
        }
    }
    let total = t0.elapsed();

    Ok(StartupTiming {
        total_ms: total.as_millis() as u64,
    })
}

/// Hot start: bring an existing process forward without re-init.
/// RPC-only timing — sysmontap proxy doesn't help here because the
/// target's PID is in the sample stream from before the launch, so
/// "first sample with PID" arrives immediately and tells us nothing.
pub async fn measure_hot_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    if let Err(e) = launch_app_with_options(udid, "com.apple.springboard", false).await {
        tracing::debug!(error = %e, "hot start: SpringBoard pre-launch failed; continuing");
    } else {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // Build the processcontrol session up-front so the RPC time isn't
    // contaminated with channel setup.
    let mut remote_pc = build_dtx_remote(udid)
        .await
        .context("DTX session for processcontrol")?;
    let mut pc = ProcessControlClient::new(&mut remote_pc)
        .await
        .context("ProcessControlClient::new")?;
    let t0 = Instant::now();
    let _pid = pc
        .launch_app(
            bundle_id.to_string(),
            None,
            None,
            false, // start_suspended
            false, // kill_existing
        )
        .await
        .with_context(|| format!("processcontrol.launchApp({bundle_id})"))?;
    Ok(StartupTiming {
        total_ms: t0.elapsed().as_millis() as u64,
    })
}

/// Robust per-process PID lookup that doesn't rely on the processes
/// Dict's key format (varies by iOS version — sometimes the PID
/// string, sometimes process name). Iterates value arrays, which are
/// positional per `proc_attrs`, and decodes the pid attribute at the
/// resolved index.
fn sample_has_pid(
    sample: &crate::sysmontap_raw::SysmontapSample,
    target_pid: u64,
    pid_idx: usize,
) -> bool {
    let Some(processes) = sample.processes.as_ref() else {
        return false;
    };
    for value in processes.values() {
        let Some(arr) = value.as_array() else { continue };
        let Some(pid_val) = arr.get(pid_idx) else { continue };
        let pid_num = match pid_val {
            plist::Value::Integer(i) => i.as_unsigned(),
            plist::Value::String(s) => s.parse::<u64>().ok(),
            _ => None,
        };
        if pid_num == Some(target_pid) {
            return true;
        }
    }
    false
}

/// Build a fresh DTX RemoteServerClient — same boilerplate as
/// launch.rs but exposed here so measure_cold_start can mount
/// processcontrol + sysmontap on independent pre-built sessions.
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
