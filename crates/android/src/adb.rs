//! Thin wrapper around the system `adb` binary.
//!
//! Future: replace with a bundled `adb` binary located in `binaries/` and
//! eventually with a direct ADB-server protocol client.

use anyhow::{Context, Result};
use mperf_schema::SamplerError;
use tokio::process::Command;

/// Path to the `adb` executable. Prefers the bundled sidecar (set by
/// the Tauri shell at startup); falls back to whichever `adb` is on
/// PATH so plain `cargo test` still works.
fn adb_binary() -> String {
    std::env::var("MPERF_ADB_PATH").unwrap_or_else(|_| "adb".to_string())
}

/// Build a `Command` for the bundled adb.
///
/// On Windows we attach CREATE_NO_WINDOW so each spawn doesn't flash a
/// console window — during recording the samplers fire several adb
/// calls per second and without this flag the user sees a continuous
/// black-window flicker.
///
/// `kill_on_drop(true)` is mandatory: when a caller wraps the spawn
/// in `tokio::time::timeout(...)` and the timeout fires, the Future
/// is dropped but Tokio does **not** kill the spawned process by
/// default — it just stops `.await`ing it. The orphaned `adb shell`
/// keeps running, talking to the device, and any subsequent shell
/// call queues behind it. On Samsung One UI's adbd this exact
/// pile-up reproduced "Live view spinners forever, PerfDog stuck
/// too" in the field. With kill_on_drop on, the OS SIGKILLs the
/// stale shell as soon as the timeout drops the future.
pub(crate) fn adb_command() -> Command {
    let mut cmd = Command::new(adb_binary());
    #[cfg(target_os = "windows")]
    {
        // 0x0800_0000 = CREATE_NO_WINDOW (windows.h). Hardcoded to avoid
        // pulling in the `windows-sys` crate for a single constant.
        cmd.creation_flags(0x0800_0000);
    }
    cmd.kill_on_drop(true);
    cmd
}

/// Run `adb -s <serial> shell <cmd>` and return stdout as a String.
pub async fn shell(serial: &str, cmd: &str) -> Result<String, SamplerError> {
    let output = adb_command()
        .args(["-s", serial, "shell", cmd])
        .output()
        .await
        .map_err(|e| SamplerError::TransientIo(format!("spawn adb: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // ADB prints "device offline" / "device not found" / "no devices/emulators"
        // when the cable is yanked. Surface that as a typed error.
        let s = stderr.to_ascii_lowercase();
        if s.contains("device") && (s.contains("offline") || s.contains("not found"))
            || s.contains("no devices")
        {
            return Err(SamplerError::DeviceDisconnected(stderr.into_owned()));
        }
        return Err(SamplerError::TransientIo(format!(
            "adb shell '{cmd}' failed: {stderr}"
        )));
    }

    String::from_utf8(output.stdout)
        .with_context(|| "adb output not utf-8")
        .map_err(|e| SamplerError::Fatal(e.into()))
}

/// Like `shell` but returns Ok even for non-typed errors (used for
/// best-effort enrichment queries like `getprop`).
pub async fn shell_raw(serial: &str, cmd: &str) -> Result<String> {
    let output = adb_command()
        .args(["-s", serial, "shell", cmd])
        .output()
        .await
        .context("spawn adb shell")?;
    if !output.status.success() {
        anyhow::bail!(
            "adb shell '{cmd}' failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run `adb -s <serial> shell pm list packages` (third-party + system).
/// Returns the raw stdout text.
pub async fn list_packages(serial: &str) -> Result<String> {
    // `-3` would limit to third-party. We include everything so users can
    // test e.g. the stock browser. The frontend picker filters by typing.
    // -3 limits to third-party apps. System apps (android.*, com.android.*,
    // com.sec.* on Samsung) are usually too many and not what users test.
    let output = adb_command()
        .args(["-s", serial, "shell", "pm list packages -3"])
        .output()
        .await
        .context("failed to spawn adb pm list packages")?;
    if !output.status.success() {
        anyhow::bail!(
            "pm list packages failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Reject package names that aren't plain Android identifiers. Android
/// constrains package names to `[A-Za-z0-9._]+` (`:` for sub-process,
/// `-` rarely), so anything else is shell-injection risk material —
/// refuse rather than escape, since the caller's normal flow never
/// provides such input. Live in `adb` because every command that
/// interpolates a package into a shell string should run this first.
pub(crate) fn is_safe_pkg_name(pkg: &str) -> bool {
    !pkg.is_empty()
        && pkg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == ':' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_pkg_name() {
        assert!(is_safe_pkg_name("com.tencent.eve"));
        assert!(is_safe_pkg_name("com.foo.bar:remote"));
        assert!(!is_safe_pkg_name(""));
        assert!(!is_safe_pkg_name("evil; rm -rf /"));
        assert!(!is_safe_pkg_name("foo$(id)"));
        assert!(!is_safe_pkg_name("foo`id`"));
        assert!(!is_safe_pkg_name("foo|cat"));
    }
}

/// Run `adb devices -l` and return raw stdout.
pub async fn list_raw() -> Result<String> {
    let output = adb_command()
        .args(["devices", "-l"])
        .output()
        .await
        .context("failed to spawn adb; is it on PATH?")?;

    if !output.status.success() {
        anyhow::bail!(
            "adb devices failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
