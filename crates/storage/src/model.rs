use serde::{Deserialize, Serialize};

/// Information needed at session creation time. Wall-clock start is required
/// so we can later display "this session was recorded at 14:23:45".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub wall_start_ms: i64,
    pub device_id: String,
    pub device_platform: String, // "android" / "ios"
    pub device_model: Option<String>,
    pub app_bundle_id: Option<String>,
    pub meta_json: Option<String>,
    /// Snapshot of the user's metrics-picker selection at recording
    /// start. Persisted alongside the session so its detail view filters
    /// charts to what the user was focused on — independent of whatever
    /// selection the picker holds when the user opens the history later.
    /// `None` = "show every metric this session captured" (legacy
    /// sessions and any future caller that doesn't pass a snapshot).
    pub selected_metrics: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: i64,
    pub wall_start_ms: i64,
    pub wall_end_ms: Option<i64>,
    pub device_id: String,
    pub device_platform: String,
    pub device_model: Option<String>,
    pub app_bundle_id: Option<String>,
    pub selected_metrics: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplePoint {
    pub ts_us: i64,
    pub value: f64,
}

/// User annotation pinned to a moment in a recording. `ts_us` is the
/// offset from session start so it lines up directly with sample
/// timelines (no wall-clock conversion needed at render time).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Marker {
    pub id: i64,
    pub session_id: i64,
    pub ts_us: i64,
    pub label: Option<String>,
    pub created_at_ms: i64,
}
