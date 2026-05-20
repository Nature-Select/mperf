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
    // Stop the app explicitly before measurement. Ignore failures
    // (already-stopped → exit non-zero on some toyboxes).
    let _ = adb::shell_raw(serial, &format!("am force-stop {pkg}")).await;
    let cmd = format!("am start -W -S -n {pkg}/$(cmd package resolve-activity --brief {pkg} 2>/dev/null | tail -n1)");
    run_am_start(serial, &cmd).await
}

/// Hot start: app should be in background (or fully foregrounded —
/// `am start` is idempotent in that case). No `-S`; we want the
/// existing process reused so the measurement reflects UI re-attach,
/// not cold init.
pub async fn measure_hot_start(serial: &str, pkg: &str) -> Result<StartupTiming> {
    if !adb::is_safe_pkg_name(pkg) {
        anyhow::bail!("unsafe package name: {pkg}");
    }
    let cmd = format!("am start -W -n {pkg}/$(cmd package resolve-activity --brief {pkg} 2>/dev/null | tail -n1)");
    run_am_start(serial, &cmd).await
}

async fn run_am_start(serial: &str, cmd: &str) -> Result<StartupTiming> {
    let raw = adb::shell_raw(serial, cmd)
        .await
        .with_context(|| format!("adb shell '{cmd}'"))?;
    parse_total_time(&raw)
        .with_context(|| format!("could not find TotalTime in `am start -W` output: {raw}"))
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
