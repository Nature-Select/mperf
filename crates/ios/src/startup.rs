//! Cold / hot app-launch timing on iOS via Instruments processcontrol.
//!
//! Times just the `processcontrol.launchApp` RPC, NOT the full
//! `launch_app_with_options` call — the ~1-2s of host-side DTX
//! channel setup (CoreDeviceProxy / RSD / dtservicehub /
//! RemoteServerClient / ProcessControlClient) is excluded so the
//! number is comparable to Android's kernel-measured
//! `am start -W TotalTime`, which also doesn't count host adb-shell
//! overhead.
//!
//! Not as accurate as iOS Instruments' internal "App Launch" stage
//! breakdown (which needs the DTX trace-events service we haven't
//! implemented — see launch.rs comments). The RPC time roughly
//! corresponds to "device received launch request → process created
//! + main entry dispatched"; iOS doesn't publish a "first frame
//! rendered" event we can hook into the way Android's am does.

use crate::launch::{launch_app_with_options, launch_app_with_options_timed};
use anyhow::Result;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct StartupTiming {
    pub total_ms: u64,
}

/// Cold start: `kill_existing=true` forces a fresh process. The
/// measurement includes UIKit init, app delegate, scene attach, and
/// the first runloop tick — everything the device does between the
/// processcontrol launch RPC and its acknowledgement.
///
/// Same SpringBoard pre-step as hot: launching com.apple.springboard
/// puts the home screen forward before we issue the kill+relaunch.
/// Without it, `kill_existing=true` from a foreground target leaves
/// the device on a black screen — DTX-initiated cold launches don't
/// reliably trigger SpringBoard's "foreground the new app" path on
/// iOS 16+, so the new process exists but isn't surfaced. With the
/// pre-step, the relaunch follows the normal "user is on home,
/// tapping an app" path and the UI shows up.
pub async fn measure_cold_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    if let Err(e) = launch_app_with_options(udid, "com.apple.springboard", false).await {
        tracing::debug!(error = %e, "cold start: SpringBoard pre-launch failed; measuring direct kill+launch");
    } else {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
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
    let (_pid, rpc_elapsed) =
        launch_app_with_options_timed(udid, bundle_id, kill_existing).await?;
    Ok(StartupTiming {
        total_ms: rpc_elapsed.as_millis() as u64,
    })
}
