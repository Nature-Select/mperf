//! Cold / hot app-launch timing via `am start -W`.
//!
//! Android's `am start -W` runs synchronously and prints a kernel-
//! measured breakdown — `TotalTime` is the activity-launch latency
//! the user observes, ideal for our purposes. `-S` forces an `am
//! force-stop` of the target package before launch, giving a clean
//! cold start; without it we get the hot-launch / foreground-bring
//! latency.
//!
//! No phase breakdown (PerfDog-iOS-style waterfall) — Android's `am`
//! doesn't expose internal launch phases without a profiler attached.

use crate::adb;
use anyhow::{Context, Result};
use std::time::Duration;

/// `am start -W` output snippet we parse:
/// ```text
/// Starting: Intent { ... }
/// Status: ok
/// LaunchState: COLD
/// Activity: com.example/.MainActivity
/// TotalTime: 1234
/// WaitTime: 1456
/// ```
/// `TotalTime` is "from launch to first frame fully rendered" in
/// kernel ms — matches what users perceive as "app launch time".
#[derive(Debug, Clone, Copy)]
pub struct StartupTiming {
    pub total_ms: u64,
}

/// Cold start: force-stop the package first so `am start -W -S` gets
/// a clean process-creation measurement. `-S` is the documented flag
/// for "stop the target before starting" in `am`'s man page; we keep
/// the explicit `force-stop` in front of it as a belt-and-braces
/// since some OEMs delay `-S`'s implicit stop.
pub async fn measure_cold_start(serial: &str, pkg: &str) -> Result<StartupTiming> {
    if !adb::is_safe_pkg_name(pkg) {
        anyhow::bail!("unsafe package name: {pkg}");
    }
    let component = resolve_launcher_component(serial, pkg).await?;
    // Stop the app explicitly before measurement. Ignore failures
    // (already-stopped → exit non-zero on some toyboxes).
    let _ = adb::shell_raw(serial, &format!("am force-stop {pkg}")).await;
    let cmd = format!(
        "am start -W -S -a android.intent.action.MAIN \
         -c android.intent.category.LAUNCHER -n {component}"
    );
    run_am_start(serial, &cmd).await
}

/// Hot start: relaunch the app expecting the existing process to be
/// reused. If the app is currently foreground, `am start` no-ops
/// (returns "intent delivered to top-most instance" with TotalTime=0
/// — useless), so we detect that case, send a HOME key to background
/// the app, briefly wait for the home-screen transition, then re-fire
/// the launch. The retried measurement reflects actual hot-start
/// latency (UI re-attach + first frame).
pub async fn measure_hot_start(serial: &str, pkg: &str) -> Result<StartupTiming> {
    if !adb::is_safe_pkg_name(pkg) {
        anyhow::bail!("unsafe package name: {pkg}");
    }
    let component = resolve_launcher_component(serial, pkg).await?;
    let cmd = format!(
        "am start -W -a android.intent.action.MAIN \
         -c android.intent.category.LAUNCHER -n {component}"
    );
    let raw = run_am_start_raw(serial, &cmd).await?;
    if is_already_foreground(&raw) {
        tracing::info!(pkg, "hot start: app already foreground, sending HOME and retrying");
        // KEYCODE_HOME = 3. Best-effort — if it fails the second
        // am start probably still no-ops, surfaced as the same error
        // path below.
        let _ = adb::shell_raw(serial, "input keyevent 3").await;
        // 800ms is empirical: Samsung One UI's home transition can
        // take 600-700ms (slower than AOSP's ~250ms), and firing
        // `am start` mid-transition produces a broken `LaunchState:
        // UNKNOWN (0)` reply with no TotalTime line. 800ms is safely
        // past it on the test S9310; Pixel-class hardware just waits
        // a bit longer than strictly necessary.
        tokio::time::sleep(Duration::from_millis(800)).await;
        let raw2 = run_am_start_raw(serial, &cmd).await?;
        return parse_total_time(&raw2)
            .with_context(|| format!("could not find TotalTime after HOME retry: {raw2}"));
    }
    parse_total_time(&raw).with_context(|| format!("could not find TotalTime: {raw}"))
}

/// Detect the "already at top, no relaunch happened" signal in `am
/// start -W` output. Two markers because OEMs differ: AOSP emits the
/// Warning line, some Samsung builds skip the warning but still show
/// `LaunchState: UNKNOWN (0)` + `TotalTime: 0`.
fn is_already_foreground(raw: &str) -> bool {
    raw.contains("Activity not started, intent has been delivered")
        || raw.contains("LaunchState: UNKNOWN")
}

/// Resolve the package's launcher activity component (returned as the
/// `pkg/.ActivityName` string `am start -n` expects).
///
/// `cmd package resolve-activity --brief` on Android 7+ prints the
/// resolved component on a line by itself, but OEMs (Samsung in
/// particular) prepend a verbose ResolveInfo summary that confuses
/// naive `tail -n1` parsing. We grab the last line that actually
/// looks like a component — has a `/` and starts with the package
/// name. Two-step (resolve + start) instead of shell `$(…)` so the
/// parsing happens in Rust where errors surface clearly.
async fn resolve_launcher_component(serial: &str, pkg: &str) -> Result<String> {
    let raw = adb::shell_raw(serial, &format!("cmd package resolve-activity --brief {pkg}"))
        .await
        .with_context(|| format!("resolve-activity {pkg}"))?;
    let prefix = format!("{pkg}/");
    let component = raw
        .lines()
        .map(|l| l.trim())
        .filter(|l| l.starts_with(&prefix) && l.contains('/'))
        .last()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "could not find launcher component for {pkg} in resolve-activity output: {raw}"
            )
        })?
        .to_string();
    Ok(component)
}

async fn run_am_start(serial: &str, cmd: &str) -> Result<StartupTiming> {
    let raw = run_am_start_raw(serial, cmd).await?;
    parse_total_time(&raw)
        .with_context(|| format!("could not find TotalTime in `am start -W` output: {raw}"))
}

async fn run_am_start_raw(serial: &str, cmd: &str) -> Result<String> {
    adb::shell_raw(serial, cmd)
        .await
        .with_context(|| format!("adb shell '{cmd}'"))
}

fn parse_total_time(raw: &str) -> Result<StartupTiming> {
    // Look for `TotalTime: <n>` — every OEM I've checked emits this
    // line regardless of `Status:` (ok / WARNING). On a launcher-
    // mismatch resolve `Status: error` we still want to fail loudly
    // rather than report a stale timing.
    let mut status_ok = false;
    let mut total: Option<u64> = None;
    for line in raw.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("Status:") {
            if v.trim() == "ok" {
                status_ok = true;
            }
        } else if let Some(v) = line.strip_prefix("TotalTime:") {
            total = v.trim().parse::<u64>().ok();
        }
    }
    if !status_ok {
        anyhow::bail!("am start did not report Status: ok (raw output: {raw})");
    }
    let total_ms = total.ok_or_else(|| anyhow::anyhow!("TotalTime line missing"))?;
    Ok(StartupTiming { total_ms })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_output() {
        let s = "\
Starting: Intent { ... }
Status: ok
LaunchState: COLD
Activity: com.example/.MainActivity
TotalTime: 1234
WaitTime: 1456
";
        let t = parse_total_time(s).unwrap();
        assert_eq!(t.total_ms, 1234);
    }

    #[test]
    fn rejects_status_error() {
        let s = "Status: error\nTotalTime: 999\n";
        assert!(parse_total_time(s).is_err());
    }

    #[test]
    fn rejects_missing_total_time() {
        let s = "Status: ok\nLaunchState: COLD\n";
        assert!(parse_total_time(s).is_err());
    }
}
