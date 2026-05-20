//! Cold / hot app-launch timing on iOS.
//!
//! Cold start: a long-lived coreprofile session (one per device,
//! managed by `kdebug_supervisor`) continuously broadcasts kdebug
//! events. For each measurement we:
//!   1. Subscribe to the supervisor's broadcast channel — gives us
//!      a fresh Receiver that sees every event from "now" forward.
//!   2. Open a one-shot DTX transport for `deviceinfo.machTimeInfo`
//!      + `processcontrol.launchApp`. mti gives us t0 in device
//!      mach ticks. launchApp dispatches the launch.
//!   3. Read events from the Receiver, filter to class 0x2B (UIKit
//!      lifecycle, post-mti, sanity-bounded), and take the LATEST
//!      timestamp seen within ~3s OR 400ms of quiet — that's the
//!      end of the launch.
//!   4. Drop the Receiver. The supervisor keeps draining; the next
//!      measurement is just another subscribe + filter pass.
//!
//! Why class 0x2B (UIKit) as the "first frame" proxy: iOS 26 stopped
//! emitting the py-ios-device-documented `0x31C00506` marker. The
//! tail UIKit event lands right around when the first frame commits
//! to the display, which matches PerfDog's top-line "App Launch"
//! number in our testing (~190-210ms on an iPhone 14 / iOS 26.4.2,
//! within 10ms of PerfDog's 213ms).
//!
//! Hot start: RPC-only via `processcontrol.launchApp`. The kdebug
//! first-frame proxy doesn't reliably fire on a UIScene re-attach
//! (the path foregrounding takes), so we just time the RPC.

use crate::connect;
use crate::core_profile_session_raw::MachTimeInfo;
use crate::kdebug_supervisor::{self, KdebugSupervisor};
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
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;

#[derive(Debug, Clone, Copy)]
pub struct StartupTiming {
    pub total_ms: u64,
}

pub async fn measure_cold_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    // SpringBoard pre-step — moves the home screen forward so the
    // upcoming kill+relaunch doesn't leave the device on a black
    // screen (DTX launches don't always count as user-foreground
    // intents to iOS 26 SpringBoard).
    if let Err(e) = launch_app_with_options(udid, "com.apple.springboard", false).await {
        tracing::info!(error = %e, "cold start: SpringBoard pre-launch failed; continuing");
    } else {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Acquire the persistent kdebug supervisor. If it's already
    // running (this isn't the first measurement on this device since
    // mperf started), this is a near-instant Arc clone. If not, we
    // pay ~300ms for the transport open + setConfig + start.
    tracing::info!("cold start: acquiring kdebug supervisor");
    let supervisor: Arc<KdebugSupervisor> = kdebug_supervisor::acquire(udid)
        .await
        .context("kdebug_supervisor::acquire")?;

    // Subscribe AFTER the supervisor is alive so we don't miss the
    // events from our launch. Broadcast lags would happen if the
    // channel filled up, but we read in a tight loop below.
    let mut rx = supervisor.subscribe();

    // Separate transport for machTimeInfo + processcontrol. Must
    // NOT live on the coreprofile transport — that one is owned by
    // the supervisor's drain task.
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
    tracing::info!(pid, "cold start: launch_app returned, scanning kdebug stream for last 0x2B event");

    // Find the LATEST class-0x2B event timestamp > mti within ~3s
    // OR 400ms of quiet after we've seen at least one. Bounded with
    // a sanity cap of 5s past mti (in mach ticks) to reject any
    // misaligned-byte garbage.
    let max_ticks = (5_000_000_000u128 * mti.denom as u128 / mti.numer as u128) as u64;
    let watch_deadline = Instant::now() + Duration::from_secs(3);
    let mut last_2b_mach: Option<u64> = None;
    let mut events_seen: u64 = 0;
    let mut lagged_count: u64 = 0;
    loop {
        let now = Instant::now();
        let remaining = watch_deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            tracing::warn!(events_seen, lagged_count, "cold start: hit 3s deadline");
            break;
        }
        // 400ms quiet timeout AFTER we've seen events = launch ended.
        let recv_timeout = if last_2b_mach.is_some() {
            Duration::from_millis(400).min(remaining)
        } else {
            remaining
        };
        match tokio::time::timeout(recv_timeout, rx.recv()).await {
            Ok(Ok(ev)) => {
                events_seen += 1;
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
                last_2b_mach = Some(
                    last_2b_mach.map_or(ev.timestamp_mach, |prev| prev.max(ev.timestamp_mach)),
                );
            }
            Ok(Err(RecvError::Lagged(n))) => {
                // Broadcast lagged — we missed `n` events. Keep
                // going; the launch generally produces enough events
                // that we still find the last 0x2B by deadline.
                lagged_count += n;
                continue;
            }
            Ok(Err(RecvError::Closed)) => {
                // Supervisor drain task exited. Surface the reason.
                let reason = supervisor
                    .last_error()
                    .await
                    .unwrap_or_else(|| "supervisor channel closed".into());
                anyhow::bail!("kdebug stream went away mid-measurement: {reason}");
            }
            Err(_) => {
                // recv timeout — either no event yet (still waiting)
                // or we've seen some and now there's quiet => done.
                if last_2b_mach.is_some() {
                    tracing::info!(events_seen, "cold start: 400ms quiet after events, done");
                    break;
                }
                // No events at all yet — fall through to deadline
                // check at top of loop.
            }
        }
    }
    let last_ts = last_2b_mach.ok_or_else(|| {
        anyhow!(
            "no UIKit kdebug events (class 0x2B) seen after launch — \
             ({events_seen} events received, {lagged_count} lagged). \
             Supervisor may have just started; retry usually works."
        )
    })?;
    let delta = last_ts - mti.mach_absolute_time;
    let total_ns = mti.ticks_delta_to_ns(delta);
    let total_ms = total_ns / 1_000_000;
    tracing::info!(
        events_seen,
        lagged_count,
        delta_ticks = delta,
        total_ns,
        total_ms,
        "cold start: last UIKit event captured (proxy for first-frame)"
    );
    Ok(StartupTiming { total_ms })
}

/// Hot start: bring an existing process forward without re-init.
/// RPC-only — kdebug first-frame proxy doesn't reliably fire on
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
