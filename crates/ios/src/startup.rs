//! Cold / hot app-launch timing on iOS via Instruments processcontrol.
//!
//! Both modes time just the `processcontrol.launchApp` RPC — the
//! ~1-2s of host-side DTX channel setup (CoreDeviceProxy / RSD /
//! dtservicehub / RemoteServerClient / ProcessControlClient) is
//! excluded so the number reflects device-side work, not host
//! transport overhead.
//!
//! Honest limitation: the underlying Apple RPC
//! (`launchSuspendedProcessWithDevicePath:bundleIdentifier:…`) returns
//! once the device has created the process and assigned a PID. It does
//! NOT wait for UIApplicationMain, scene attach, or first frame.
//! Android's `am start -W TotalTime` waits for first-display rendered.
//! Result: iOS numbers will be smaller than Android for the same
//! launch on similar hardware — this is an Apple RPC-semantics
//! limitation, not a measurement bug.
//!
//! A sysmontap-PID-appearance proxy was attempted (see git history)
//! to approximate "first activity ≈ first frame" but the
//! stale-sample-queue inflation made the numbers unreliable in
//! practice. The honest narrow measurement (RPC only) is preferable
//! to a fuzzy wider one. Future work: subscribe to DTX trace events
//! for actual UIKit phase boundaries (~600 LOC, brittle across iOS
//! versions — deferred).
//!
//! Foreground handling: same SpringBoard pre-step pattern. Without
//! it, both cold (kill_existing=true) and hot (kill_existing=false)
//! on a foreground target can leave the device on a black screen
//! because DTX-initiated launches aren't always treated as user-
//! foreground intents by SpringBoard.

use crate::launch::{launch_app_with_options, launch_app_with_options_timed};
use anyhow::Result;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct StartupTiming {
    pub total_ms: u64,
}

/// Cold start: kill any existing process and time the relaunch.
pub async fn measure_cold_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    if let Err(e) = launch_app_with_options(udid, "com.apple.springboard", false).await {
        tracing::debug!(error = %e, "cold start: SpringBoard pre-launch failed; continuing");
    } else {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    measure(udid, bundle_id, true).await
}

/// Hot start: bring an existing process forward without re-init.
pub async fn measure_hot_start(udid: &str, bundle_id: &str) -> Result<StartupTiming> {
    if let Err(e) = launch_app_with_options(udid, "com.apple.springboard", false).await {
        tracing::debug!(error = %e, "hot start: SpringBoard pre-launch failed; continuing");
    } else {
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
