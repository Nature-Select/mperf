import { useEffect, useRef, useState } from 'react'
import { listen, UnlistenFn } from '@tauri-apps/api/event'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { EVENT_SAMPLE, Sample } from '@/lib/ipc'
import { attachChartResize } from '@/lib/chartResize'
import { MarkerControls } from '@/lib/markers'
import { MarkerOverlay } from './MarkerOverlay'
import { formatBytes } from '@/lib/format'
import { clampTooltip, formatClock } from '@/lib/format'
import styles from './chart-shared.module.scss'

const MAX_POINTS = 300 // ~5 min at 1Hz

interface Stats {
  cur: number | null
  min: number | null
  max: number | null
  avg: number | null
  count: number
}

const EMPTY_STATS: Stats = { cur: null, min: null, max: null, avg: null, count: 0 }

interface Tooltip {
  left: number
  top: number
  t: number
  v: number
}

export function LiveMemoryChart({
  active,
  platform,
  markers,
}: {
  active: boolean
  /// iOS hides the system-memory tile + sub-header. Background:
  /// sysmontap exposes a System block but the absolute value disagrees
  /// with Activity Monitor in ways we haven't pinned down, so we don't
  /// surface it. Android still shows both — `/proc/meminfo` is
  /// authoritative there.
  platform: 'android' | 'ios'
  markers?: MarkerControls
}) {
  const hostRef = useRef<HTMLDivElement>(null)
  const plotRef = useRef<uPlot | null>(null)
  const xsRef = useRef<number[]>([])
  /// App PSS in MB (1 MB = 1024 * 1024 bytes, easier on the eye than bytes).
  const ysRef = useRef<number[]>([])
  const wallStartSecRef = useRef<number>(0)
  const [plot, setPlot] = useState<uPlot | null>(null)

  const [live, setLive] = useState<Stats>(EMPTY_STATS)
  const [sysUsedBytes, setSysUsedBytes] = useState<number | null>(null)
  const [tooltip, setTooltip] = useState<Tooltip | null>(null)

  useEffect(() => {
    if (!hostRef.current) return
    const opts: uPlot.Options = {
      width: hostRef.current.clientWidth,
      height: 240,
      scales: {
        x: { time: true },
        // Auto-scale Y; PSS can range from a few MB (small apps) to GB
        // (heavy games).
        y: {},
      },
      series: [
        {},
        {
          label: 'App PSS (MB)',
          stroke: '#7b3df5',
          width: 1.5,
          fill: 'rgba(123,61,245,0.12)',
          points: { show: false },
        },
      ],
      axes: [
        {
          stroke: '#888',
          grid: { stroke: '#eee' },
          values: (_u, ticks) => ticks.map(formatClock),
        },
        {
          stroke: '#888',
          grid: { stroke: '#eee' },
          values: (_u, ticks) => ticks.map((t) => `${Math.round(t)} MB`),
        },
      ],
      legend: { show: false },
      cursor: {
        drag: { x: true, y: false, uni: 10 },
        focus: { prox: 30 },
      },
      hooks: {
        setCursor: [
          (u) => {
            const idx = u.cursor.idx
            if (
              idx == null ||
              idx < 0 ||
              idx >= xsRef.current.length ||
              u.cursor.left == null ||
              u.cursor.left < 0
            ) {
              setTooltip(null)
              return
            }
            setTooltip({
              left: u.cursor.left,
              top: u.cursor.top ?? 0,
              t: xsRef.current[idx],
              v: ysRef.current[idx],
            })
          },
        ],
      },
    }
    plotRef.current = new uPlot(opts, [xsRef.current, ysRef.current], hostRef.current)
    setPlot(plotRef.current)

    const onResize = () => {
      if (!hostRef.current || !plotRef.current) return
      plotRef.current.setSize({ width: hostRef.current.clientWidth, height: 240 })
    }
    const cleanupResize = attachChartResize(hostRef.current, onResize)
    return () => {
      cleanupResize()
      plotRef.current?.destroy()
      plotRef.current = null
      setPlot(null)
    }
  }, [])

  useEffect(() => {
    if (!active) return

    xsRef.current.length = 0
    ysRef.current.length = 0
    wallStartSecRef.current = Date.now() / 1000
    plotRef.current?.setData([xsRef.current, ysRef.current])
    setLive(EMPTY_STATS)
    setSysUsedBytes(null)
    setTooltip(null)

    let unlisten: UnlistenFn | undefined
    let cancelled = false

    listen<Sample>(EVENT_SAMPLE, (e) => {
      const s = e.payload
      if (s.kind === 'mem_system_used_bytes') {
        // Android only — iOS hides the system tile, so skip the state
        // update to avoid the per-tick re-render.
        if (platform === 'android') setSysUsedBytes(s.value)
        return
      }
      if (s.kind !== 'mem_app_pss_bytes') return
      const t = wallStartSecRef.current + s.ts_us / 1_000_000
      const mb = s.value / 1024 / 1024
      xsRef.current.push(t)
      ysRef.current.push(mb)
      if (xsRef.current.length > MAX_POINTS) {
        xsRef.current.shift()
        ysRef.current.shift()
      }
      plotRef.current?.setData([xsRef.current, ysRef.current])
      setLive(computeStats(ysRef.current))
    }).then((fn) => {
      if (cancelled) fn()
      else unlisten = fn
    })

    return () => {
      cancelled = true
      unlisten?.()
    }
    // `platform` is read inside the listener so include it in deps. In
    // practice the device platform never changes mid-session (the
    // Start button is disabled while recording), so this won't churn
    // listeners during a recording.
  }, [active, platform])

  return (
    <div className={styles.chartCard}>
      <div className={styles.chartHeader}>
        <div className={styles.chartTitle}>Memory</div>
        {/* The trend line is App PSS only — overlaying system would
            squash it (system is GB-scale, app is MB-scale). System
            is still shown as a live tile in the stats row on Android,
            but the headline plot is per-app. */}
        <div className={styles.chartSub}>App PSS · MB</div>
      </div>
      <div className={styles.stats}>
        <StatTile label="app" value={live.cur} unit="MB" accent />
        <StatTile label="app max" value={live.max} unit="MB" />
        <StatTile label="app avg" value={live.avg} unit="MB" />
        {platform === 'android' && <SystemMemTile bytes={sysUsedBytes} />}
        <StatTile label="samples" value={live.count} unit="" raw />
      </div>
      <div ref={hostRef} className={styles.chartHost}>
        <MarkerOverlay
          plot={plot}
          wallStartSec={wallStartSecRef.current}
          controls={markers}
          />
        {tooltip && (
          <div
            className={styles.tooltip}
            style={{ left: clampTooltip(tooltip.left), top: tooltip.top - 12 }}
          >
            <div className={styles.tooltipTime}>{formatClock(tooltip.t)}</div>
            <div className={styles.tooltipRow}>
              App PSS&nbsp;<b>{tooltip.v.toFixed(1)} MB</b>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

function StatTile({
  label,
  value,
  unit = '',
  accent = false,
  raw = false,
}: {
  label: string
  value: number | null
  unit?: string
  accent?: boolean
  raw?: boolean
}) {
  return (
    <div className={`${styles.tile} ${accent ? styles.accent : ''}`}>
      <div className={styles.label}>{label}</div>
      <div className={styles.value}>
        {value == null ? '—' : raw ? value.toFixed(0) : value.toFixed(1)}
        {value != null && unit && <span className={styles.unit}>{unit}</span>}
      </div>
    </div>
  )
}

/// Render system memory using `formatBytes` directly — the unit may be
/// MB or GB depending on size, and the displayed precision varies (MB is
/// integer, GB is 1 decimal). Going through `StatTile` would either drop
/// the GB decimal (`raw=true → toFixed(0)`: 1.5 GB → "2 GB") or attach
/// `.0` to every MB value, so we render it inline.
function SystemMemTile({ bytes }: { bytes: number | null }) {
  if (bytes == null) {
    return (
      <div className={styles.tile}>
        <div className={styles.label}>system</div>
        <div className={styles.value}>—</div>
      </div>
    )
  }
  const [num, unit] = formatBytes(bytes).split(' ')
  return (
    <div className={styles.tile}>
      <div className={styles.label}>system</div>
      <div className={styles.value}>
        {num}
        {unit && <span className={styles.unit}>{unit}</span>}
      </div>
    </div>
  )
}

function computeStats(ys: number[]): Stats {
  if (ys.length === 0) return EMPTY_STATS
  let min = Infinity
  let max = -Infinity
  let sum = 0
  for (const v of ys) {
    if (v < min) min = v
    if (v > max) max = v
    sum += v
  }
  return {
    cur: ys[ys.length - 1],
    min,
    max,
    avg: sum / ys.length,
    count: ys.length,
  }
}
