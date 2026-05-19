/// Persisted "which metrics is the user collecting" state.
///
/// Backed by localStorage so a reload doesn't reset the picker. Each
/// instance of `useMetricsSelection()` shares the same window-scoped
/// listener, so two open panels (sidebar picker + future history filter,
/// etc.) stay in sync without lifting state to React Context.

import { useCallback, useEffect, useState } from 'react'
import { DEFAULT_SELECTED_IDS, METRICS } from '@/lib/metricsCatalog'

// v2 bump: catalog refactored from per-MetricKind ids ('fps', 'cpu_app'…)
// to per-chart-card ids ('frame', 'cpu_usage'…). Old v1 keys are
// abandoned rather than migrated — none of the old ids exist in the new
// catalog, so a migration would just drop everything anyway.
const STORAGE_KEY = 'mperf.metrics.selected.v2'

function loadFromStorage(): Set<string> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return new Set(DEFAULT_SELECTED_IDS)
    const arr = JSON.parse(raw)
    if (!Array.isArray(arr)) return new Set(DEFAULT_SELECTED_IDS)
    // Drop unknown ids — they're either typos or removed metrics from a
    // future version that downgraded. Implemented gating is enforced at
    // the picker UI level, so we don't filter on `implemented` here.
    const known = new Set(METRICS.map((m) => m.id))
    return new Set(arr.filter((v): v is string => typeof v === 'string' && known.has(v)))
  } catch {
    return new Set(DEFAULT_SELECTED_IDS)
  }
}

function saveToStorage(selected: Set<string>) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(Array.from(selected)))
  } catch {
    // Quota or privacy mode — best-effort, the selection just won't
    // survive a reload. Not worth a user-visible error.
  }
}

const STORAGE_EVENT = 'mperf:metrics-selection-changed'

export function useMetricsSelection() {
  const [selected, setSelected] = useState<Set<string>>(loadFromStorage)

  useEffect(() => {
    // Cross-component sync: when any caller mutates, broadcast a
    // CustomEvent so other live hooks reload from storage. localStorage
    // events alone fire only across windows/tabs, not within the same
    // document — Tauri's single-window app needs the in-doc signal.
    const handler = () => setSelected(loadFromStorage())
    window.addEventListener(STORAGE_EVENT, handler)
    return () => window.removeEventListener(STORAGE_EVENT, handler)
  }, [])

  const commit = useCallback((next: Set<string>) => {
    saveToStorage(next)
    setSelected(next)
    window.dispatchEvent(new CustomEvent(STORAGE_EVENT))
  }, [])

  const toggle = useCallback(
    (id: string) => {
      setSelected((prev) => {
        const next = new Set(prev)
        if (next.has(id)) next.delete(id)
        else next.add(id)
        saveToStorage(next)
        // Defer the dispatch so React's batched setState completes
        // before sibling subscribers re-read storage.
        queueMicrotask(() => window.dispatchEvent(new CustomEvent(STORAGE_EVENT)))
        return next
      })
    },
    [],
  )

  const setMany = useCallback(
    (ids: string[], on: boolean) => {
      setSelected((prev) => {
        const next = new Set(prev)
        for (const id of ids) {
          if (on) next.add(id)
          else next.delete(id)
        }
        saveToStorage(next)
        queueMicrotask(() => window.dispatchEvent(new CustomEvent(STORAGE_EVENT)))
        return next
      })
    },
    [],
  )

  return { selected, toggle, setMany, commit }
}
