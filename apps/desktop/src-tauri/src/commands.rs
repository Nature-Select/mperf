//! Tauri `#[command]` handlers. Each one is a thin wrapper that delegates
//! to the appropriate crate (`mperf_core`, `mperf_android`,
//! `mperf_ios`, `mperf_storage`) or to `session::start_recording`.

use mperf_core::{Device, MetricKind};
use mperf_schema::{DeviceInfo, Platform};
use mperf_storage::{Marker, SamplePoint, SessionInfo};
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::session::{self, now_ms, AppState};

#[derive(Serialize)]
pub struct AppInfoOut {
    id: String,
    label: String,
}

#[tauri::command]
pub async fn list_devices() -> Result<Vec<Device>, String> {
    let devs = mperf_core::list_devices()
        .await
        .map_err(|e| e.to_string())?;
    let android = devs
        .iter()
        .filter(|d| matches!(d.platform, Platform::Android))
        .count();
    let ios = devs.iter().filter(|d| matches!(d.platform, Platform::Ios)).count();
    tracing::debug!(android, ios, total = devs.len(), "list_devices");
    Ok(devs)
}

#[tauri::command]
pub async fn get_device_info(
    device_id: String,
    platform: Platform,
) -> Result<DeviceInfo, String> {
    tracing::info!(device_id = %device_id, ?platform, "get_device_info");
    match platform {
        Platform::Android => mperf_android::device_info(&device_id)
            .await
            .map_err(|e| e.to_string()),
        Platform::Ios => mperf_ios::device_info(&device_id)
            .await
            .map_err(|e| e.to_string()),
    }
}

#[tauri::command]
pub async fn list_apps(
    device_id: String,
    platform: Platform,
) -> Result<Vec<AppInfoOut>, String> {
    tracing::info!(device_id = %device_id, ?platform, "list_apps");
    match platform {
        Platform::Android => {
            let apps = mperf_android::list_apps(&device_id)
                .await
                .map_err(|e| e.to_string())?;
            Ok(apps
                .into_iter()
                .map(|a| AppInfoOut { id: a.id, label: a.label })
                .collect())
        }
        Platform::Ios => {
            let apps = mperf_ios::list_apps(&device_id)
                .await
                .map_err(|e| e.to_string())?;
            Ok(apps
                .into_iter()
                .map(|a| AppInfoOut { id: a.id, label: a.label })
                .collect())
        }
    }
}

/// Starts a recording.
///
/// `selected_metrics` is the user's metrics-picker selection at the
/// moment Start was clicked — persisted alongside the session so its
/// History view filters charts to what the user was focused on at
/// recording time. Empty/missing → stored as NULL and History falls
/// back to "show every captured metric".
///
/// `sampling_intervals` is the per-chart-card sampling cadence (ms)
/// the user configured. Backend translates each card's interval to its
/// owning sampler — when multiple cards share a sampler (iOS
/// sysmontap, Android CpuSampler emits Total + Core together), the
/// sampler runs at the fastest requested rate. Empty/missing → each
/// sampler uses its hardcoded default.
#[tauri::command]
pub async fn start_session(
    state: State<'_, AppState>,
    app: AppHandle,
    device_id: String,
    platform: Platform,
    device_model: Option<String>,
    target_pkg: String,
    selected_metrics: Option<Vec<String>>,
    sampling_intervals: Option<std::collections::HashMap<String, u64>>,
) -> Result<i64, String> {
    // PerfDog-style: app selection is mandatory. Frontend disables Start
    // until both device + app are picked; this is the backend guard.
    let target_pkg = target_pkg.trim().to_string();
    if target_pkg.is_empty() {
        return Err("a target app must be selected before recording".into());
    }
    let selected_metrics = selected_metrics.filter(|v| !v.is_empty());
    let sampling_intervals = sampling_intervals.filter(|m| !m.is_empty());
    session::start_recording(
        &state,
        &app,
        device_id,
        platform,
        device_model,
        target_pkg,
        selected_metrics,
        sampling_intervals,
    )
    .await
}

#[tauri::command]
pub async fn stop_session(state: State<'_, AppState>) -> Result<(), String> {
    let session = { state.session.lock().await.take() };
    if let Some(s) = session {
        let id = s.db_id;
        tracing::info!(session_id = id, "stop_session");
        s.stop(&state.storage).await;
    } else {
        tracing::debug!("stop_session called but no active session");
    }
    Ok(())
}

/// Drop a marker at "now" of the active recording session. Fails if no
/// session is recording. `ts_us` is the offset from session start so
/// the marker lines up directly with sample timelines.
#[tauri::command]
pub async fn add_marker(
    state: State<'_, AppState>,
    label: Option<String>,
) -> Result<Marker, String> {
    let (db_id, wall_start_ms) = {
        let guard = state.session.lock().await;
        match guard.as_ref() {
            Some(s) => (s.db_id, s.wall_start_ms),
            None => return Err("no active session".into()),
        }
    };
    let now = now_ms();
    let ts_us = (now - wall_start_ms).max(0) * 1_000;
    // Trim + treat empty as None so the UI doesn't have to.
    let label_norm = label
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let id = state
        .storage
        .insert_marker(db_id, ts_us, label_norm.clone(), now)
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!(session_id = db_id, marker_id = id, ts_us, label = ?label_norm, "marker added");
    Ok(Marker {
        id,
        session_id: db_id,
        ts_us,
        label: label_norm,
        created_at_ms: now,
    })
}

#[tauri::command]
pub async fn list_session_markers(
    state: State<'_, AppState>,
    session_id: i64,
) -> Result<Vec<Marker>, String> {
    state
        .storage
        .list_markers(session_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_marker(state: State<'_, AppState>, marker_id: i64) -> Result<(), String> {
    state
        .storage
        .delete_marker(marker_id)
        .await
        .map_err(|e| e.to_string())
}

/// Adjust an existing marker's timestamp. Used when the user drags it
/// on the chart to fine-tune the position (compensating for human
/// reaction latency between seeing an event and clicking).
#[tauri::command]
pub async fn update_marker(
    state: State<'_, AppState>,
    marker_id: i64,
    ts_us: i64,
) -> Result<(), String> {
    state
        .storage
        .update_marker_ts(marker_id, ts_us.max(0))
        .await
        .map_err(|e| e.to_string())
}

/// Set or clear the marker's label. Pass null/empty to clear.
#[tauri::command]
pub async fn update_marker_label(
    state: State<'_, AppState>,
    marker_id: i64,
    label: Option<String>,
) -> Result<(), String> {
    state
        .storage
        .update_marker_label(marker_id, label)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_sessions(state: State<'_, AppState>) -> Result<Vec<SessionInfo>, String> {
    state.storage.list_sessions().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_session_samples(
    state: State<'_, AppState>,
    session_id: i64,
    kind: MetricKind,
) -> Result<Vec<SamplePoint>, String> {
    state
        .storage
        .load_wide_samples(session_id, kind)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_session_core_samples(
    state: State<'_, AppState>,
    session_id: i64,
) -> Result<Vec<(i64, String, f64)>, String> {
    state
        .storage
        .load_long_samples(session_id, MetricKind::CpuCorePct)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_session(state: State<'_, AppState>, session_id: i64) -> Result<(), String> {
    state
        .storage
        .delete_session(session_id)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct Diagnostics {
    app_version: &'static str,
    /// User-facing data directory (db lives here as `data.db`).
    data_dir: String,
    /// User-facing log directory (rolling daily `mperf.log.YYYY-MM-DD`).
    log_dir: String,
    /// Resolved `adb` binary path — bundled sidecar when present,
    /// otherwise system PATH lookup, otherwise "(not found)".
    adb_path: String,
    /// First line of `adb version`, or "(unavailable)" if the call fails.
    adb_version: String,
    /// idevice crate version (Rust dependency on the iOS side).
    idevice_version: &'static str,
    /// SQLite schema head version applied to the DB.
    schema_version: i32,
    os: &'static str,
}

#[tauri::command]
pub async fn get_diagnostics(app: AppHandle) -> Result<Diagnostics, String> {
    use tauri::Manager;
    let data_dir = app
        .path()
        .app_data_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unavailable)".into());
    let log_dir = app
        .path()
        .app_log_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unavailable)".into());

    // Resolved adb: MPERF_ADB_PATH is set in setup.rs from the bundled
    // sidecar if found, otherwise we fall back to the system `adb`. We
    // resolve the binary *once* so the displayed path matches what we
    // actually exec'd for `adb version` — previously two separate
    // env::var calls with different fallback strings could disagree.
    let resolved_adb = std::env::var("MPERF_ADB_PATH").ok();
    let adb_path = resolved_adb
        .clone()
        .unwrap_or_else(|| "adb (system PATH)".into());
    let adb_version = tokio::process::Command::new(resolved_adb.as_deref().unwrap_or("adb"))
        .arg("version")
        .output()
        .await
        .ok()
        .and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()
                .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
        })
        .unwrap_or_else(|| "(unavailable)".into());

    Ok(Diagnostics {
        app_version: env!("CARGO_PKG_VERSION"),
        data_dir,
        log_dir,
        adb_path,
        adb_version,
        idevice_version: "0.1.61",
        schema_version: mperf_storage::SCHEMA_HEAD,
        os: std::env::consts::OS,
    })
}

/// Reveal a directory in the platform's file manager. macOS Finder /
/// Windows Explorer / Linux xdg-open. Used by the Settings tab's
/// "Open data dir" / "Open log dir" buttons.
#[tauri::command]
pub async fn reveal_path(path: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let cmd = ("open", path.as_str());
    #[cfg(target_os = "windows")]
    let cmd = ("explorer", path.as_str());
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = ("xdg-open", path.as_str());

    tokio::process::Command::new(cmd.0)
        .arg(cmd.1)
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", cmd.0))?;
    Ok(())
}

/// Start streaming device logs. Replaces any in-flight stream.
/// `target_pkg` is optional and only used by Android (PID filter via
/// `pidof <pkg>` resolved once at start); iOS syslog is device-wide
/// and the frontend filters by process name.
#[tauri::command]
pub async fn start_log_stream(
    state: State<'_, AppState>,
    app: AppHandle,
    device_id: String,
    platform: Platform,
    target_pkg: Option<String>,
) -> Result<(), String> {
    crate::log_stream::start_log_stream(&state, &app, device_id, platform, target_pkg).await
}

#[tauri::command]
pub async fn stop_log_stream(state: State<'_, AppState>) -> Result<(), String> {
    crate::log_stream::stop_log_stream(&state).await;
    Ok(())
}
