/// Persisted per-metric sampling-interval state.
///
/// Mirrors `useMetricsSelection`'s shape: a single localStorage entry
/// (`mperf.metrics.frequencies.v1`) keyed by chart-card id → interval
/// in milliseconds. Missing entries fall through to `defaultIntervalMs`
/// declared on the catalog item, so the UI doesn't have to seed
/// defaults at first run.

import { useCallback, useEffect, useState } from 'react'
import { METRICS } from '@/lib/metricsCatalog'

const STORAGE_KEY = 'mperf.metrics.frequencies.v1'
const STORAGE_EVENT = 'mperf:metrics-frequencies-changed'

export type MetricFrequencies = Record<string, number>

function loadFromStorage(): MetricFrequencies {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return {}
    const obj = JSON.parse(raw)
    if (!obj || typeof obj !== 'object') return {}
    const known = new Set(
      METRICS.filter((m) => m.defaultIntervalMs != null).map((m) => m.id),
    )
    // Drop unknown keys (catalog drift) and non-numeric values. Be
    // permissive on range so future catalog can offer wider options.
    const out: MetricFrequencies = {}
    for (const [k, v] of Object.entries(obj)) {
      if (known.has(k) && typeof v === 'number' && v > 0 && v <= 600_000) {
        out[k] = v
      }
    }
    return out
  } catch {
    return {}
  }
}

function saveToStorage(map: MetricFrequencies) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(map))
  } catch {
    // Quota / privacy mode — best-effort.
  }
}

export function useMetricFrequencies() {
  const [map, setMap] = useState<MetricFrequencies>(loadFromStorage)

  useEffect(() => {
    const handler = () => setMap(loadFromStorage())
    window.addEventListener(STORAGE_EVENT, handler)
    return () => window.removeEventListener(STORAGE_EVENT, handler)
  }, [])

  const set = useCallback((id: string, intervalMs: number) => {
    setMap((prev) => {
      const next = { ...prev, [id]: intervalMs }
      saveToStorage(next)
      queueMicrotask(() => window.dispatchEvent(new CustomEvent(STORAGE_EVENT)))
      return next
    })
  }, [])

  /// Resolve the effective interval for a metric id: persisted override
  /// if present, else the catalog default. Returns `undefined` only
  /// for metric ids that have no chart-card backing at all (capture
  /// items, placeholders).
  const resolve = useCallback(
    (id: string): number | undefined => {
      const override = map[id]
      if (override) return override
      return METRICS.find((m) => m.id === id)?.defaultIntervalMs
    },
    [map],
  )

  return { map, set, resolve }
}

/// Snapshot helper for `start_session` — returns the full
/// `{ id: intervalMs }` map covering every chart-card-backed metric,
/// using the persisted override where set and the catalog default
/// elsewhere. The backend uses this map to derive each sampler's
/// poll cadence; ids not present in the map fall back to the
/// sampler's own internal default on the Rust side.
export function snapshotEffectiveFrequencies(map: MetricFrequencies): MetricFrequencies {
  const out: MetricFrequencies = {}
  for (const m of METRICS) {
    if (m.defaultIntervalMs == null) continue
    out[m.id] = map[m.id] ?? m.defaultIntervalMs
  }
  return out
}
