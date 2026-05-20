//! Cold / hot app-launch timing on iOS.
//!
//! Cold start: coreprofilesessiontap DTX channel — same kdebug
//! stream Xcode Instruments / PerfDog tap. We anchor t0 via
//! deviceinfo.machTimeInfo (separate transport so kdebug pushes
//! don't compete with the RPC reply), launch the app via
//! processcontrol, then look for the LAST class 0x2B (UIKit)
//! kdebug event past t0 within ~2s. On iOS 26 the
//! py-ios-device-documented `0x31C00506` first-frame marker
//! doesn't fire; the last UIKit event is the closest proxy and
//! lands inside the user-perceived launch window (matched
//! PerfDog's 213ms in testing).
//!
//! The coreprofilesessiontap session is held in a per-UDID pool
//! (`core_profile_session_raw::SESSIONS`) and reused across
//! measurements — iOS 26 doesn't reliably re-acquire kperf when
//! we tear down and reopen the channel, so we leave it streaming
//! and just demarcate per-measurement windows by timestamp.
//!
//! Hot start: RPC-only via processcontrol.launchApp. The kdebug
//! first-frame event doesn't reliably fire on a UIScene re-attach
//! (the path foregrounding takes); the RPC time is the closest
//! single number we have for now.

use crate::connect;
use crate::core_profile_session_raw::{
    acquire_session, invalidate_session, MachTimeInfo,
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
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct StartupTiming {
    pub total_ms: u64,
}

pub async fn measure_cold_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    // Retry wrapper. iOS 26's kperf release after a previous session
    // is asynchronous — the previous DTServiceHub child can take
    // a few seconds to fully exit, during which a new acquire fails
    // (sometimes with an explicit `_lockKPerf` notice, sometimes
    // silently). We retry up to 3 times with growing backoff. On
    // success, we return the value. On all-failures, we return the
    // last error.
    const MAX_ATTEMPTS: u32 = 3;
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        match measure_cold_start_once(udid, bundle_id).await {
            Ok(t) => {
                if attempt > 1 {
                    tracing::info!(attempt, "measure_cold_start: succeeded on retry");
                }
                return Ok(t);
            }
            Err(e) => {
                if attempt < MAX_ATTEMPTS {
                    let backoff = Duration::from_millis(2000 * attempt as u64);
                    tracing::warn!(
                        attempt,
                        backoff_ms = backoff.as_millis() as u64,
                        error = %e,
                        "measure_cold_start: attempt failed, sleeping then retrying"
                    );
                    last_err = Some(e);
                    tokio::time::sleep(backoff).await;
                } else {
                    last_err = Some(e);
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("measure_cold_start: exhausted retries")))
}

async fn measure_cold_start_once(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    tracing::info!(bundle_id, "measure_cold_start_once begin");
    if let Err(e) = launch_app_with_options(udid, "com.apple.springboard", false).await {
        tracing::info!(error = %e, "cold start: SpringBoard pre-launch failed; continuing");
    } else {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    tracing::info!("cold start: SpringBoard prep done");

    let session = acquire_session(udid).await?;
    let mut cp = session.lock().await;
    tracing::info!("cold start: coreprofile session acquired");

    // Separate transport for machTimeInfo + processcontrol — must
    // not interleave with the kdebug push stream on the coreprofile
    // transport (its reply correlation would eat events otherwise).
    let mut remote_pc = build_dtx_remote(udid)
        .await
        .context("DTX session for processcontrol")?;
    let mti = fetch_mach_time_info(&mut remote_pc)
        .await
        .context("machTimeInfo")?;
    tracing::info!(?mti, "cold start: anchored mach time");

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
    tracing::info!(pid, "cold start: launch_app returned, watching kdebug stream");

    let result = cp
        .capture_post_launch_timestamp(&mti, Duration::from_secs(3))
        .await;

    let last_ts = match result {
        Ok(ts) => ts,
        Err(e) => {
            tracing::warn!(error = %e, "measure_cold_start: capture failed; invalidating session so the next attempt rebuilds the transport");
            drop(cp);
            invalidate_session(udid).await;
            return Err(e);
        }
    };

    let delta = last_ts - mti.mach_absolute_time;
    let total_ns = mti.ticks_delta_to_ns(delta);
    let total_ms = total_ns / 1_000_000;
    tracing::info!(
        delta_ticks = delta,
        total_ns,
        total_ms,
        "cold start: last UIKit event captured (proxy for first-frame)"
    );
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
    let t0 = std::time::Instant::now();
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
/// `(mach_absolute_time, numer, denom)` tuple via idevice's
/// RemoteServerClient (which correctly correlates the reply).
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
    let data = msg.data.ok_or_else(|| anyhow!("machTimeInfo: empty reply"))?;
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
