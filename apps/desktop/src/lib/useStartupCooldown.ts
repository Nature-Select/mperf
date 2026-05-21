import { useCallback, useEffect, useState } from 'react'

/// iOS kperf release latency. The kernel single-owner lock around
/// `coreprofilesessiontap` doesn't release until the previous
/// DTServiceHub child fully exits, which empirically takes ~30s. We
/// pad to 35s so the boundary tick doesn't fire on the wedge edge.
export const COLD_STARTUP_COOLDOWN_MS = 35_000

const storageKey = (deviceId: string) =>
  `mperf.lastColdStartupAt.${deviceId}`

/// Track the cooldown window after a cold-startup measurement. Tied
/// to deviceId because the kperf lock is per-device — moving to a
/// different phone resets the window. Persisted in localStorage so
/// the cooldown survives an app refresh / restart, which matters
/// because kperf-on-device DOES survive a host process restart.
export function useStartupCooldown(deviceId: string | null) {
  const [now, setNow] = useState(() => Date.now())

  const lastAt: number | null = (() => {
    if (!deviceId) return null
    const raw = localStorage.getItem(storageKey(deviceId))
    if (!raw) return null
    const n = Number(raw)
    return Number.isFinite(n) ? n : null
  })()

  const elapsed = lastAt == null ? Infinity : now - lastAt
  const remainingMs = Math.max(0, COLD_STARTUP_COOLDOWN_MS - elapsed)
  const remainingSec = Math.ceil(remainingMs / 1000)
  const inCooldown = remainingMs > 0

  useEffect(() => {
    if (!inCooldown) return
    const id = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(id)
  }, [inCooldown])

  const recordColdMeasurement = useCallback(() => {
    if (!deviceId) return
    localStorage.setItem(storageKey(deviceId), String(Date.now()))
    setNow(Date.now())
  }, [deviceId])

  return { inCooldown, remainingSec, remainingMs, recordColdMeasurement }
}
