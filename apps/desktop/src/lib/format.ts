/// Color a percentage value (0-100) on a green→amber→red scale.
/// Used in stat tiles to give an at-a-glance feel for CPU/GPU load.
export function pctColor(v: number): string {
  if (v < 30) return '#00b42a' // green
  if (v < 50) return '#7bc616' // lime
  if (v < 70) return '#ff7d00' // amber
  if (v < 85) return '#f53f3f' // red
  return '#cb2634' // dark red
}

/// Color an FPS value (assuming 60Hz target). Higher is better — inverse of pctColor.
export function fpsColor(v: number): string {
  if (v >= 55) return '#00b42a' // green: at target
  if (v >= 45) return '#7bc616' // lime
  if (v >= 30) return '#ff7d00' // amber: noticeable drops
  if (v >= 15) return '#f53f3f' // red: choppy
  return '#cb2634' // dark red: very bad
}

/// Color a jank count — 0 is best.
export function jankColor(v: number): string {
  if (v === 0) return '#00b42a'
  if (v <= 2) return '#ff7d00'
  return '#f53f3f'
}

/// Stutter ratio coloring (0-100, low = better, opposite of FPS).
/// Tighter thresholds than `pctColor`: even a few percent stutter is
/// already perceptible during gameplay, so we don't want CPU's "<30 =
/// green" leniency leaking into a metric where 5% is already bad.
export function stutterColor(v: number): string {
  if (v < 1) return '#00b42a'
  if (v < 3) return '#7bc616'
  if (v < 8) return '#ff7d00'
  if (v < 20) return '#f53f3f'
  return '#cb2634'
}

/// Format a wall-clock unix milliseconds value as `YYYY-MM-DD HH:MM`.
export function formatDateTime(ms: number): string {
  const d = new Date(ms)
  const Y = d.getFullYear()
  const M = String(d.getMonth() + 1).padStart(2, '0')
  const D = String(d.getDate()).padStart(2, '0')
  const h = String(d.getHours()).padStart(2, '0')
  const m = String(d.getMinutes()).padStart(2, '0')
  return `${Y}-${M}-${D} ${h}:${m}`
}

/// Format bytes as a human-readable string (MB / GB).
export function formatBytes(bytes: number): string {
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(0)} MB`
  return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`
}

/// Format a duration in milliseconds as `Xh Ym Zs` (omit empty parts).
export function formatDuration(ms: number | null): string {
  if (ms == null || ms < 0) return '—'
  const totalSec = Math.floor(ms / 1000)
  const h = Math.floor(totalSec / 3600)
  const m = Math.floor((totalSec % 3600) / 60)
  const s = totalSec % 60
  if (h > 0) return `${h}h ${m}m ${s}s`
  if (m > 0) return `${m}m ${s}s`
  return `${s}s`
}

/// Format a wall-clock unix seconds value as `HH:MM:SS` for chart x-axis
/// ticks and tooltip headers. Shared by every Live and Static chart.
export function formatClock(unixSec: number): string {
  const d = new Date(unixSec * 1000)
  const h = String(d.getHours()).padStart(2, '0')
  const m = String(d.getMinutes()).padStart(2, '0')
  const s = String(d.getSeconds()).padStart(2, '0')
  return `${h}:${m}:${s}`
}

/// Clamp a cursor x-coordinate to keep tooltips from running off the left
/// edge of the chart. Returns the inner-left value to pass to CSS `left`.
export function clampTooltip(left: number): number {
  return Math.max(8, left + 12)
}

/// Threshold colors for thermal readings. Mobile SoCs typically throttle
/// somewhere between 65-75°C; ≥75 is "device is in distress".
export function tempColor(c: number | null): string | undefined {
  if (c == null) return undefined
  if (c >= 75) return 'rgb(245, 63, 63)'
  if (c >= 60) return 'rgb(255, 125, 0)'
  if (c >= 40) return 'rgb(255, 195, 0)'
  return 'rgb(0, 180, 42)'
}

/// Palette for multi-series charts (e.g. per-core CPU).
/// Distinct hues, dark enough to read on white background.
export const SERIES_PALETTE = [
  '#3491fa', // blue
  '#00b42a', // green
  '#f53f3f', // red
  '#ff7d00', // orange
  '#7b3df5', // purple
  '#14c9c9', // teal
  '#fb7299', // pink
  '#a9aeb8', // gray
  '#4080ff', // softer blue
  '#23c343', // softer green
  '#f5319d', // magenta
  '#ffb400', // yellow
  '#0fc6c2', // cyan
  '#9fdb1d', // lime-yellow
  '#168cff', // azure
  '#722ed1', // violet
]
