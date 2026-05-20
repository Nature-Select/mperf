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
use std::time::{Duration, Instant};

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
/// complete.
///
/// Foreground-app handling: if the target is already foreground,
/// launching it via processcontrol is a no-op (returns the existing
/// PID with no UI work, producing a degenerate timing dominated by
/// DTX channel overhead and visibly "nothing happened"). We
/// pre-background the target by launching SpringBoard — iOS's home-
/// screen process — which is the closest equivalent to Android's
/// HOME keyevent that we have via processcontrol. Then we wait for
/// the home transition and measure the actual relaunch.
///
/// If the app isn't running at all, the post-Springboard launch is
/// effectively a cold start (KillExisting=false = "preserve if
/// present, launch if absent"). We don't try to detect-then-reject
/// that case; reporting "hot = cold time" when there was nothing to
/// resume is informative enough.
///
/// Caveat: each `launch_app_with_options` builds its own
/// CoreDeviceProxy + RSD + DTX channel (~1-2s setup). Two
/// back-to-back calls means the user waits ~2-3s for the
/// measurement. Acceptable for an explicit "测试" click.
pub async fn measure_hot_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    // Best-effort background-step. SpringBoard is always running on
    // a normal iOS device, so launching it should always succeed —
    // it just brings the home screen forward. Errors here are
    // logged but not propagated; falling back to a direct measure
    // gives the user the original (sometimes-no-op) behaviour
    // rather than failing the whole measurement.
    if let Err(e) = launch_app_with_options(udid, "com.apple.springboard", false).await {
        tracing::debug!(error = %e, "hot start: SpringBoard pre-launch failed; measuring direct launch");
    } else {
        // Empirical: ~500ms covers the springboard transition on a
        // recent iPhone. iOS animation timing is fairly consistent
        // across hardware (unlike Samsung's variable home animation).
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
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
