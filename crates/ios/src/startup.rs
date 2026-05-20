//! Cold / hot app-launch timing on iOS.
//!
//! Cold start: coreprofilesessiontap DTX channel — same kdebug stream
//! Xcode Instruments / PerfDog tap. We subscribe to kernel-debug
//! events, launch the app, then time from the device's mach clock at
//! launch dispatch to the `Initial Frame Rendering END` kdebug event
//! (debug_id = 0x31C00506). This matches PerfDog's top-line "App
//! Launch" number (~200ms on warm hardware).
//!
//! Hot start: still RPC-only via processcontrol.launchApp. The
//! coreprofile approach would also work, but the kdebug "first
//! frame" event doesn't reliably fire on a UIScene re-attach (the
//! path foregrounding takes); the RPC time is the closest single
//! number we have for now.
//!
//! Foreground handling: same SpringBoard pre-step pattern. Without
//! it, DTX-initiated launches on a foreground target can leave the
//! device on a black screen because SpringBoard doesn't treat DTX
//! launches as user-foreground intents.

use crate::connect;
use crate::core_profile_session_raw::{
    CoreProfileSessionRaw, FIRST_FRAME_END_DEBUG_ID,
};
use crate::launch::launch_app_with_options;
use anyhow::{anyhow, Context, Result};
use idevice::{
    core_device_proxy::CoreDeviceProxy,
    dvt::{process_control::ProcessControlClient, remote_server::RemoteServerClient},
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

    // --- coreprofilesessiontap on its own DTX connection ----------
    // SEPARATE transport from processcontrol so the kdebug raw-bytes
    // pushes don't interfere with the NSKeyedArchive-only protocol
    // stream idevice's RemoteServerClient expects.
    let mut cp = CoreProfileSessionRaw::connect(udid)
        .await
        .context("CoreProfileSessionRaw::connect")?;
    cp.set_config().await.context("cp.set_config")?;
    cp.start().await.context("cp.start")?;
    // Drain initial stackshot + any pre-launch system events so the
    // first thing we see in the post-launch loop is OUR app's launch.
    cp.drain_until_quiet(Duration::from_millis(400))
        .await
        .context("drain pre-launch events")?;

    // --- processcontrol on its own DTX connection -----------------
    let mut remote_pc = build_dtx_remote(udid)
        .await
        .context("DTX session for processcontrol")?;
    let mut pc = ProcessControlClient::new(&mut remote_pc)
        .await
        .context("ProcessControlClient::new")?;

    // Anchor mach_t0 right before dispatching the launch RPC. The
    // ~10-20ms machTimeInfo roundtrip is included in the reported
    // number — known small inflation that's tolerable for a spike.
    let mti = cp.fetch_mach_time_info().await.context("machTimeInfo")?;

    let _pid = pc
        .launch_app(
            bundle_id.to_string(),
            None,
            None,
            false, // start_suspended
            true,  // kill_existing
        )
        .await
        .with_context(|| format!("processcontrol.launchApp({bundle_id})"))?;

    // Wait for the first-frame event. Bound at 15s so a missed
    // 0x31C00506 (e.g. app has no UI) doesn't hang us forever.
    let deadline = Instant::now() + Duration::from_secs(15);
    let total_ms = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = cp.stop().await;
            anyhow::bail!(
                "no Initial Frame Rendering END (0x31C00506) event within 15s; device might \
                 be throttled or the app might not have a UI"
            );
        }
        let events = tokio::time::timeout(remaining, cp.next_events())
            .await
            .map_err(|_| anyhow!("kdebug stream timed out waiting for first-frame event"))??;
        if let Some(ev) = events
            .iter()
            .find(|e| e.debug_id == FIRST_FRAME_END_DEBUG_ID)
        {
            let delta = ev.timestamp_mach.saturating_sub(mti.mach_absolute_time);
            let total_ns = mti.ticks_delta_to_ns(delta);
            break total_ns / 1_000_000;
        }
    };

    let _ = cp.stop().await;
    Ok(StartupTiming { total_ms })
}

/// Hot start: bring an existing process forward without re-init.
/// RPC-only — kdebug "first frame" event doesn't reliably fire on a
/// foregrounding (the process is already running, no new dyld init,
/// no fresh UIScene boot).
pub async fn measure_hot_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    if let Err(e) = launch_app_with_options(udid, "com.apple.springboard", false).await {
        tracing::debug!(error = %e, "hot start: SpringBoard pre-launch failed; continuing");
    } else {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
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
