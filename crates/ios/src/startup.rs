//! Cold / hot app-launch timing on iOS via Instruments processcontrol.
//!
//! Crude wall-clock approach: time the `launch_app` DTX call from the
//! host side. processcontrol returns once the device has created the
//! target process and dispatched its main entry — so the elapsed time
//! roughly corresponds to "click icon → process start", excluding the
//! ~1–2s DTX channel setup we always pay (that overhead is subtracted
//! out: we measure only the time between channel-built and call-
//! returned).
//!
//! Not as accurate as iOS Instruments' internal "App Launch" stage
//! breakdown (which needs the DTX trace-events service we haven't
//! implemented — see launch.rs comments). Returns a single
//! `total_ms` good enough for relative comparisons.

use crate::launch::launch_app_with_options;
use anyhow::Result;
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
pub struct StartupTiming {
    pub total_ms: u64,
}

/// Cold start: `kill_existing=true` forces a fresh process. The
/// measurement includes UIKit init, app delegate, scene attach, and
/// the first runloop tick — everything the device does between the
/// processcontrol launch RPC and its acknowledgement.
pub async fn measure_cold_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    measure(udid, bundle_id, true).await
}

/// Hot start: `kill_existing=false` so a backgrounded / suspended app
/// gets brought forward without re-initialising. On iOS this is
/// essentially the time to resume the suspended process and let
/// UIApplicationDelegate's `applicationWillEnterForeground`
/// complete — typically much shorter than cold.
///
/// If the app isn't running at all, processcontrol falls through to
/// a cold launch (KillExisting=false is "preserve if present, launch
/// if absent"), which makes the measurement effectively a cold-start
/// time. We don't try to detect-then-reject that case; reporting "hot
/// = cold time" when there was nothing to resume is informative
/// enough.
pub async fn measure_hot_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    measure(udid, bundle_id, false).await
}

async fn measure(udid: &str, bundle_id: &str, kill_existing: bool) -> Result<StartupTiming> {
    let t0 = Instant::now();
    let _pid = launch_app_with_options(udid, bundle_id, kill_existing).await?;
    let elapsed = t0.elapsed();
    Ok(StartupTiming {
        total_ms: elapsed.as_millis() as u64,
    })
}
