//! List installed Android apps via `pm list packages -3` (third-party).

use crate::adb;
use anyhow::{Context, Result};
use serde::Serialize;
use std::time::Duration;
use tokio::process::Command;

/// One installed app, surfaced to the UI. Shape kept identical to the iOS
/// variant so the frontend has one type to bind against.
#[derive(Debug, Clone, Serialize)]
pub struct AppInfo {
    /// Package name (e.g. "com.example.app").
    pub id: String,
    /// User-visible label. Android requires pulling the APK to read the
    /// manifest for a real label; for Phase 0e we just echo the package
    /// name so the frontend has a single rendering path.
    pub label: String,
}

fn adb_binary() -> String {
    std::env::var("MPERF_ADB_PATH").unwrap_or_else(|_| "adb".to_string())
}

/// Best-effort launch of `pkg` on `serial` via `monkey`. PerfDog-style:
/// click Start → app pops up so the user doesn't have to alt-tab and
/// the samplers find a PID on their first tick instead of emitting
/// zeros until the user manually launches.
///
/// Why monkey, not `am start`: `am start` needs the launcher activity
/// component name (`pkg/.MainActivity`), which varies per app. `monkey
/// -p <pkg> -c android.intent.category.LAUNCHER 1` asks PackageManager
/// to resolve the launcher activity itself, so it works for any app
/// the user could tap from the home screen.
///
/// Errors are swallowed (logged only) — start_recording must succeed
/// even if the device blocks `monkey` (some MDM-locked devices do).
/// The user can still launch the app themselves.
pub async fn launch_app(serial: &str, pkg: &str) -> Result<()> {
    if !adb::is_safe_pkg_name(pkg) {
        anyhow::bail!("unsafe package name");
    }
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(adb_binary())
            .args([
                "-s",
                serial,
                "shell",
                &format!("monkey -p {pkg} -c android.intent.category.LAUNCHER 1"),
            ])
            .output(),
    )
    .await
    .context("monkey launch timed out")?
    .context("spawn adb monkey")?;
    if !out.status.success() {
        anyhow::bail!(
            "monkey exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // monkey returns 0 even when the intent can't be resolved; it
    // writes "** No activities found to run, monkey aborted." to
    // stderr in that case. Surface as error so the caller can log it.
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("No activities found")
        || stderr.contains("monkey aborted")
        || stderr.contains("Error")
    {
        anyhow::bail!("monkey could not launch {pkg}: {}", stderr.trim());
    }
    Ok(())
}

pub async fn list_apps(serial: &str) -> Result<Vec<AppInfo>> {
    let raw = adb::list_packages(serial).await?;
    let mut out: Vec<AppInfo> = raw
        .lines()
        .filter_map(|line| line.trim().strip_prefix("package:").map(String::from))
        .map(|pkg| AppInfo {
            label: pkg.clone(),
            id: pkg,
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}
