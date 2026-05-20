import { invoke } from '@tauri-apps/api/core'

export type Platform = 'android' | 'ios'
export type Transport = 'usb' | 'wifi'

export interface Device {
  id: string
  platform: Platform
  transport: Transport
  state: string
  model?: string
  /// False when the backend can't actually sample this device — e.g. an
  /// iOS device visible to usbmuxd over Wi-Fi but without the USB
  /// CoreDeviceProxy tunnel that DTX/sysmontap need. UI shows it but
  /// disables Start.
  usable: boolean
}

export interface Sample {
  ts_us: number
  device_ts_us?: number
  kind: MetricKind
  value: number
  labels?: Array<[LabelKey, string]>
}

export type MetricKind =
  | 'cpu_total_pct'
  | 'cpu_app_pct'
  | 'cpu_core_pct'
  | 'cpu_freq_mhz'
  | 'cpu_temp_c'
  | 'mem_system_used_bytes'
  | 'mem_app_pss_bytes'
  | 'fps'
  | 'frame_time_ms'
  | 'small_jank_count'
  | 'jank_count'
  | 'big_jank_count'
  | 'stutter'
  | 'gpu_tiler_pct'
  | 'gpu_renderer_pct'
  | 'gpu_device_pct'
  | 'net_up_bytes'
  | 'net_down_bytes'
  | 'battery_level_pct'
  | 'battery_temp_c'
  | 'battery_voltage_mv'
  | 'battery_current_ma'
  | 'thread_cpu_pct'

export type LabelKey = 'pid' | 'tid' | 'core_idx' | 'iface' | 'power_supply' | 'layer'

export const EVENT_SAMPLE = 'mperf://sample'
export const EVENT_SESSION_ENDED = 'mperf://session-ended'
export const EVENT_LOG_LINE = 'mperf://log-line'
export const EVENT_LOG_STATUS = 'mperf://log-status'

/// Backend-driven attach state for the log terminal. Backend emits one
/// of these whenever it transitions: e.g. "target app not running →
/// pidof returned a PID → attached" or "PID changed → reattaching".
/// `ts_ms` mirrors LogLine.ts_ms so the frontend can fence stale events
/// from a torn-down stream the same way it fences log lines.
export type LogStreamStatus =
  | { state: 'waiting'; ts_ms: number }
  | { state: 'attached'; ts_ms: number; pid: number }

export type LogLevel =
  | 'verbose'
  | 'debug'
  | 'info'
  | 'warn'
  | 'error'
  | 'fatal'
  | 'unknown'

export interface LogLine {
  ts_ms: number
  level: LogLevel
  /// Categorization label — Android logcat tag or iOS os_log subsystem.
  /// Always present (may be empty string when the source didn't set one).
  tag: string
  message: string
  /// Originating process name. iOS = `image_name` (e.g. "Runner",
  /// "kernel"). Android = null (logcat threadtime doesn't expose it).
  process: string | null
  /// Originating PID. Filled on both platforms.
  pid: number | null
  /// iOS os_log finer-grained category nested inside subsystem
  /// (e.g. subsystem=com.apple.WebKit + subcategory=Loading).
  /// Always null on Android.
  subcategory: string | null
}

export function startLogStream(
  deviceId: string,
  platform: Platform,
  targetPkg: string | null,
): Promise<void> {
  return invoke<void>('start_log_stream', { deviceId, platform, targetPkg })
}

export function stopLogStream(): Promise<void> {
  return invoke<void>('stop_log_stream')
}

export function listDevices(): Promise<Device[]> {
  return invoke<Device[]>('list_devices')
}

export function startSession(
  deviceId: string,
  platform: Platform,
  targetPkg: string,
  deviceModel?: string,
  /// Metric ids the user has selected in the picker at the moment Start
  /// was clicked. Persisted with the session; the History view of this
  /// session will use it to gate which chart cards render. `undefined`
  /// (and an empty array) is treated as "show every captured metric"
  /// by the backend — same fallback path older sessions take.
  selectedMetrics?: string[],
): Promise<number> {
  return invoke<number>('start_session', {
    deviceId,
    platform,
    deviceModel,
    targetPkg,
    selectedMetrics,
  })
}

export interface AppInfo {
  id: string
  label: string
}

export function listApps(deviceId: string, platform: Platform): Promise<AppInfo[]> {
  return invoke<AppInfo[]>('list_apps', { deviceId, platform })
}

export interface DeviceField {
  label: string
  /// `null` is rendered as "unavailable" so the field set stays stable
  /// across devices that don't expose a given kernel/lockdown value.
  value: string | null
}

export interface DeviceInfoFull {
  id: string
  platform: Platform
  /// Ordered list of rows to render top-to-bottom — backend decides
  /// order so PerfDog-style layouts stay platform-specific.
  fields: DeviceField[]
}

export function getDeviceInfo(deviceId: string, platform: Platform): Promise<DeviceInfoFull> {
  return invoke<DeviceInfoFull>('get_device_info', { deviceId, platform })
}

export function stopSession(): Promise<void> {
  return invoke<void>('stop_session')
}

export interface SessionInfo {
  id: number
  wall_start_ms: number
  wall_end_ms: number | null
  device_id: string
  device_platform: string
  device_model: string | null
  app_bundle_id: string | null
  /// Snapshot of the metrics-picker selection at recording start.
  /// `null` on pre-v5 sessions and on any session whose caller didn't
  /// supply a selection — both are rendered as "show every metric this
  /// session has data for" by the History view.
  selected_metrics: string[] | null
}

export interface SamplePoint {
  ts_us: number
  value: number
}

/// Per-core sample triple from samples_long: [ts_us, core_idx_str, value]
export type CoreSampleRow = [number, string, number]

export function listSessions(): Promise<SessionInfo[]> {
  return invoke<SessionInfo[]>('list_sessions')
}

export function getSessionSamples(
  sessionId: number,
  kind: MetricKind,
): Promise<SamplePoint[]> {
  return invoke<SamplePoint[]>('get_session_samples', { sessionId, kind })
}

export function getSessionCoreSamples(sessionId: number): Promise<CoreSampleRow[]> {
  return invoke<CoreSampleRow[]>('get_session_core_samples', { sessionId })
}

export function deleteSession(sessionId: number): Promise<void> {
  return invoke<void>('delete_session', { sessionId })
}

/// User annotation pinned to a session timestamp.
export interface Marker {
  id: number
  session_id: number
  /// Microseconds since session start (matches sample ts_us).
  ts_us: number
  label: string | null
  created_at_ms: number
}

/// Drop a marker on the active recording session. Throws if no session
/// is recording. `label` is optional; trim/empty becomes null.
export function addMarker(label?: string | null): Promise<Marker> {
  return invoke<Marker>('add_marker', { label: label ?? null })
}

export function listSessionMarkers(sessionId: number): Promise<Marker[]> {
  return invoke<Marker[]>('list_session_markers', { sessionId })
}

export function deleteMarker(markerId: number): Promise<void> {
  return invoke<void>('delete_marker', { markerId })
}

/// Move an existing marker's ts_us. Used by the chart drag handle so
/// the user can fine-tune position after dropping a marker.
export function updateMarker(markerId: number, tsUs: number): Promise<void> {
  return invoke<void>('update_marker', { markerId, tsUs })
}

/// Set or clear the marker's label. Empty string / null clears.
export function updateMarkerLabel(
  markerId: number,
  label: string | null,
): Promise<void> {
  return invoke<void>('update_marker_label', { markerId, label })
}

export interface Diagnostics {
  app_version: string
  data_dir: string
  log_dir: string
  adb_path: string
  adb_version: string
  idevice_version: string
  schema_version: number
  os: string
}

export function getDiagnostics(): Promise<Diagnostics> {
  return invoke<Diagnostics>('get_diagnostics')
}

/// Open a folder in the OS file manager. Pass the data_dir / log_dir from
/// getDiagnostics. Errors if the path doesn't exist or the helper binary
/// (open / explorer / xdg-open) isn't available.
export function revealPath(path: string): Promise<void> {
  return invoke<void>('reveal_path', { path })
}
