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
use crate::core_profile_session_raw::{CoreProfileSessionRaw, MachTimeInfo};
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

    // iOS 26 changed the UIKit kdebug codes — py-ios-device's
    // 0x31C00506 (class 0x31, subclass 0xC0, code 1, FUNC_END) never
    // arrives. Real launch events come through as class 0x2B /
    // subclass 0x87 (UIKit) with new code numbers we don't yet have
    // a phase table for.
    //
    // Pragmatic heuristic: collect events for up to 5s OR until the
    // stream goes quiet for 400ms (whichever first), then take the
    // LATEST class-0x2B event whose timestamp is past mti — that's
    // the last UIKit lifecycle event of the launch, which lands
    // roughly at "first frame committed". Bounded by a sanity check
    // (timestamp within ~5s of mti, to ignore misaligned-byte
    // garbage in pre-launch V3-header chunks).
    let watch_deadline = Instant::now() + Duration::from_secs(5);
    let mut events_seen: u64 = 0;
    let mut last_2b_mach: Option<u64> = None;
    // 5s of ticks at our (numer/denom). Anything beyond this is
    // misalignment.
    let max_ticks = (5_000_000_000u128 * mti.denom as u128 / mti.numer as u128) as u64;
    loop {
        let remaining = watch_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let events = match tokio::time::timeout(
            Duration::from_millis(400).min(remaining),
            cp.next_events(),
        )
        .await
        {
            Ok(Ok(events)) => events,
            Ok(Err(e)) => return Err(e),
            // 400ms of silence after seeing some events → stream is
            // quiet, launch is done.
            Err(_) if last_2b_mach.is_some() => break,
            Err(_) => continue, // no events yet, keep waiting
        };
        events_seen += events.len() as u64;
        for ev in &events {
            if ev.class_code() != 0x2b {
                continue;
            }
            if ev.timestamp_mach <= mti.mach_absolute_time {
                continue;
            }
            let delta = ev.timestamp_mach - mti.mach_absolute_time;
            if delta > max_ticks {
                continue;
            }
            last_2b_mach =
                Some(last_2b_mach.map_or(ev.timestamp_mach, |prev| prev.max(ev.timestamp_mach)));
        }
    }
    let last_ts = last_2b_mach.ok_or_else(|| {
        let _ = ();
        anyhow!(
            "no UIKit kdebug events (class 0x2B) seen after launch in 5s ({events_seen} total \
             events received) — kdf2 filter likely wasn't applied; PerfDog parity blocked"
        )
    })?;
    let delta = last_ts - mti.mach_absolute_time;
    let total_ns = mti.ticks_delta_to_ns(delta);
    let total_ms = total_ns / 1_000_000;
    tracing::info!(
        events_seen,
        delta_ticks = delta,
        total_ns,
        total_ms,
        "cold start: last UIKit event captured (proxy for first-frame)"
    );

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
