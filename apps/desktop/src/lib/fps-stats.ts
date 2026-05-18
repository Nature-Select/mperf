/// Compute PerfDog-style FPS statistics over a series of per-second FPS
/// samples. All metrics here are computed in the frontend from history
/// the user has already streamed; nothing extra goes to the Rust side.
///
/// Reference (PerfDog spec):
///   - Avg / Min / Max / Median
///   - Var, Std
///   - 1%Low: bottom 1% of FPS samples → average → derived frame time
///   - MedRange: % of samples within ±20% of median
///   - Drop(/h): per-hour count of adjacent samples where FPS dropped > 8
///   - FPS>=X [%]: % of samples ≥ X (we report 30/45/55 by default)

export interface FpsAdvancedStats {
  count: number
  avg: number
  min: number
  max: number
  median: number
  std: number
  variance: number
  /// Average of the worst 1% of FPS samples.
  oneLow: number
  /// Fraction (0-1) of samples within ±20% of median.
  medRange: number
  /// Hourly-extrapolated count of "FPS dropped > 8 frames" events.
  dropPerHour: number
  ge30: number
  ge45: number
  ge55: number
}

export function computeFpsAdvanced(samples: number[]): FpsAdvancedStats | null {
  const n = samples.length
  if (n === 0) return null

  const sorted = [...samples].sort((a, b) => a - b)
  const sum = sorted.reduce((a, b) => a + b, 0)
  const avg = sum / n
  const median =
    n % 2 === 0 ? (sorted[n / 2 - 1] + sorted[n / 2]) / 2 : sorted[Math.floor(n / 2)]
  const variance = sorted.reduce((acc, v) => acc + (v - avg) ** 2, 0) / n
  const std = Math.sqrt(variance)

  // 1% Low: average of the lowest 1% (at least 1) of FPS samples.
  const lowCount = Math.max(1, Math.floor(n * 0.01))
  const oneLow = sorted.slice(0, lowCount).reduce((a, b) => a + b, 0) / lowCount

  // MedRange: fraction inside [median*0.8, median*1.2].
  const lo = median * 0.8
  const hi = median * 1.2
  const inRange = sorted.filter((v) => v >= lo && v <= hi).length
  const medRange = inRange / n

  // Drop count: adjacent samples where FPS fell > 8. Scaled to per-hour
  // by assuming 1Hz sampling cadence.
  let drops = 0
  for (let i = 1; i < samples.length; i++) {
    if (samples[i - 1] - samples[i] > 8) drops++
  }
  const seconds = Math.max(1, n)
  const dropPerHour = (drops / seconds) * 3600

  return {
    count: n,
    avg,
    min: sorted[0],
    max: sorted[n - 1],
    median,
    std,
    variance,
    oneLow,
    medRange,
    dropPerHour,
    ge30: sorted.filter((v) => v >= 30).length / n,
    ge45: sorted.filter((v) => v >= 45).length / n,
    ge55: sorted.filter((v) => v >= 55).length / n,
  }
}
