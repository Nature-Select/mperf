//! Cold / hot app-launch timing on iOS.
//!
//! Cold start: coreprofilesessiontap DTX channel — same kdebug stream
//! Xcode Instruments / PerfDog tap. We subscribe to kernel-debug
//! events, anchor t0 via deviceinfo.machTimeInfo (separate transport
//! so kdebug pushes don't compete with the RPC reply), launch the
//! app, then look for the `Initial Frame Rendering END` kdebug event
//! (debug_id = 0x31C00506). This matches PerfDog's top-line "App
//! Launch" number (~200ms on warm hardware).
//!
//! Hot start: RPC-only via processcontrol.launchApp. The kdebug
//! first-frame event doesn't reliably fire on a UIScene re-attach
//! (the path foregrounding takes); the RPC time is the closest single
//! number we have for now.
//!
//! Foreground handling: same SpringBoard pre-step pattern. Without
//! it, DTX-initiated launches on a foreground target can leave the
//! device on a black screen.

use crate::connect;
use crate::core_profile_session_raw::{
    CoreProfileSessionRaw, MachTimeInfo, FIRST_FRAME_END_DEBUG_ID,
};
use crate::launch::launch_app_with_options;
use anyhow::{anyhow, Context, Result};
use idevice::{
    core_device_proxy::CoreDeviceProxy,
    dvt::{
        message::AuxValue, process_control::ProcessControlClient,
        remote_server::RemoteServerClient,
    },
    rsd::RsdHandshake,
    IdeviceService, ReadWrite,
};
use plist::Value;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct StartupTiming {
    pub total_ms: u64,
}

pub async fn measure_cold_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    tracing::info!(bundle_id, "measure_cold_start begin");
    if let Err(e) = launch_app_with_options(udid, "com.apple.springboard", false).await {
        tracing::info!(error = %e, "cold start: SpringBoard pre-launch failed; continuing");
    } else {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    tracing::info!("cold start: SpringBoard prep done, opening coreprofile transport");

    let mut cp = CoreProfileSessionRaw::start(udid)
        .await
        .context("CoreProfileSessionRaw::start")?;
    tracing::info!("cold start: coreprofile streamer ready, opening processcontrol transport");

    let mut remote_pc = build_dtx_remote(udid)
        .await
        .context("DTX session for processcontrol")?;
    tracing::info!("cold start: pc transport open, fetching machTimeInfo");
    let mti = fetch_mach_time_info(&mut remote_pc)
        .await
        .context("machTimeInfo")?;
    tracing::info!(?mti, "cold start: anchored mach time, building ProcessControlClient");

    let mut pc = ProcessControlClient::new(&mut remote_pc)
        .await
        .context("ProcessControlClient::new")?;
    tracing::info!("cold start: dispatching launch_app RPC");

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
    tracing::info!(pid, "cold start: launch_app returned, watching kdebug stream for 0x31C00506");

    // Look for 0x31C00506 with timestamp > mti.mach_absolute_time
    // (to filter out stale events queued before mti was captured).
    // Bound at 15s so a missed first-frame event doesn't hang us.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut events_seen: u64 = 0;
    let total_ms = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = cp.stop().await;
            anyhow::bail!(
                "no Initial Frame Rendering END (0x31C00506) event within 15s after launch \
                 ({events_seen} kdebug events seen); device might be throttled, the app may \
                 not have a UI, or the kdf2 filter wasn't applied"
            );
        }
        let events = tokio::time::timeout(remaining, cp.next_events())
            .await
            .map_err(|_| anyhow!("kdebug stream timed out waiting for first-frame event"))??;
        let batch_n = events.len();
        events_seen += batch_n as u64;
        if events_seen <= 50_000 {
            let mut classes: std::collections::BTreeSet<u8> =
                std::collections::BTreeSet::new();
            // Collect debug_ids of class 0x31 (UI / first-frame) and
            // class 0x2B (app lifecycle) events. iOS 26 may use a
            // different debug_id than py-ios-device's 0x31C00506 for
            // first-frame — we want to find it empirically.
            let mut class31_ids: Vec<String> = Vec::new();
            let mut class2b_ids: Vec<String> = Vec::new();
            let mut class1f_ids: Vec<String> = Vec::new();
            let mut has_target = false;
            for e in &events {
                classes.insert(e.class_code());
                match e.class_code() {
                    0x31 => class31_ids.push(format!("{:#010x}", e.debug_id)),
                    0x2b => class2b_ids.push(format!("{:#010x}", e.debug_id)),
                    0x1f => class1f_ids.push(format!("{:#010x}", e.debug_id)),
                    _ => {}
                }
                if e.debug_id == FIRST_FRAME_END_DEBUG_ID {
                    has_target = true;
                }
            }
            tracing::info!(
                batch_n,
                events_seen,
                ?classes,
                has_target,
                class31_count = class31_ids.len(),
                class2b_count = class2b_ids.len(),
                class1f_count = class1f_ids.len(),
                class31_ids = ?class31_ids,
                class2b_ids = ?class2b_ids,
                class1f_ids = ?class1f_ids,
                "kdebug batch"
            );
        }
        if let Some(ev) = events.iter().find(|e| {
            e.debug_id == FIRST_FRAME_END_DEBUG_ID
                && e.timestamp_mach > mti.mach_absolute_time
        }) {
            let delta = ev.timestamp_mach - mti.mach_absolute_time;
            let total_ns = mti.ticks_delta_to_ns(delta);
            tracing::info!(
                events_seen,
                delta_ticks = delta,
                total_ns,
                "cold start: first-frame event captured"
            );
            break total_ns / 1_000_000;
        }
    };

    let _ = cp.stop().await;
    Ok(StartupTiming { total_ms })
}

/// Hot start: bring an existing process forward without re-init.
/// RPC-only — kdebug first-frame event doesn't reliably fire on
/// foregrounding.
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

/// Call `deviceinfo.machTimeInfo` and decode the
/// `(mach_absolute_time, numer, denom)` tuple. Uses idevice's
/// RemoteServerClient path because it correctly correlates the reply
/// — we just need to bypass DeviceInfoClient (which doesn't expose
/// this selector publicly) and call the channel directly.
async fn fetch_mach_time_info(
    remote: &mut RemoteServerClient<Box<dyn ReadWrite>>,
) -> Result<MachTimeInfo> {
    let mut ch = remote
        .make_channel("com.apple.instruments.server.services.deviceinfo".to_string())
        .await
        .context("mount deviceinfo channel")?;
    ch.call_method(
        Some(Value::String("machTimeInfo".into())),
        None::<Vec<AuxValue>>,
        true,
    )
    .await
    .context("call machTimeInfo")?;
    let msg = ch.read_message().await.context("read machTimeInfo reply")?;
    let data = msg
        .data
        .ok_or_else(|| anyhow!("machTimeInfo: empty reply"))?;
    let arr = data
        .as_array()
        .ok_or_else(|| anyhow!("machTimeInfo: reply not Array: {data:?}"))?;
    let mach_absolute_time = arr
        .first()
        .and_then(|v| match v {
            Value::Integer(i) => i.as_unsigned(),
            _ => None,
        })
        .ok_or_else(|| anyhow!("machTimeInfo[0] not unsigned: {arr:?}"))?;
    let numer = arr
        .get(1)
        .and_then(|v| match v {
            Value::Integer(i) => i.as_unsigned(),
            _ => None,
        })
        .ok_or_else(|| anyhow!("machTimeInfo[1] not unsigned: {arr:?}"))? as u32;
    let denom = arr
        .get(2)
        .and_then(|v| match v {
            Value::Integer(i) => i.as_unsigned(),
            _ => None,
        })
        .ok_or_else(|| anyhow!("machTimeInfo[2] not unsigned: {arr:?}"))? as u32;
    Ok(MachTimeInfo {
        mach_absolute_time,
        numer,
        denom,
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
