import { useEffect, useMemo, useRef, useState } from 'react'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { SamplePoint } from '@/lib/ipc'
import { attachChartResize } from '@/lib/chartResize'
import { MarkerControls } from '@/lib/markers'
import { pctColor } from '@/lib/format'
import { MarkerOverlay } from './MarkerOverlay'
import { clampTooltip, formatClock } from '@/lib/format'
import styles from './chart-shared.module.scss'

interface Stats {
  min: number
  max: number
  avg: number
  count: number
}

interface Tooltip {
  left: number
  top: number
  t: number
  v: number
  app: number
}

const TOTAL_COLOR = '#3491fa'
const APP_COLOR = '#f53f3f'

/// Static (recorded) CPU chart with both Total and App series, matching
/// PerfDog. App may be empty (older sessions, or sessions where no app
/// was selected); we draw it only when there's data.
export function StaticCpuChart({
  points,
  appPoints,
  markers,
  wallStartMs,
}: {
  points: SamplePoint[]
  appPoints?: SamplePoint[]
  markers?: MarkerControls
  wallStartMs: number
}) {
  const hostRef = useRef<HTMLDivElement>(null)
  const plotRef = useRef<uPlot | null>(null)
  const xsRef = useRef<number[]>([])
  const ysRef = useRef<number[]>([])
  const appYsRef = useRef<number[]>([])
  const [plot, setPlot] = useState<uPlot | null>(null)
  const [tooltip, setTooltip] = useState<Tooltip | null>(null)

  const stats: Stats | null = useMemo(() => {
    if (points.length === 0) return null
    let min = Infinity
    let max = -Infinity
    let sum = 0
    for (const p of points) {
      if (p.value < min) min = p.value
      if (p.value > max) max = p.value
      sum += p.value
    }
    return { min, max, avg: sum / points.length, count: points.length }
  }, [points])

  const appStats: Stats | null = useMemo(() => {
    const arr = appPoints ?? []
    if (arr.length === 0) return null
    let min = Infinity
    let max = -Infinity
    let sum = 0
    for (const p of arr) {
      if (p.value < min) min = p.value
      if (p.value > max) max = p.value
      sum += p.value
    }
    return { min, max, avg: sum / arr.length, count: arr.length }
  }, [appPoints])

  // Convert ts_us (offset from session start) to wall-clock unix seconds.
  // App series is aligned to Total timeline by ts; missing entries become
  // NaN so uPlot draws a gap (spanGaps:false on the App series).
  const { xs, ys, appYs } = useMemo(() => {
    const wallStartSec = wallStartMs / 1000
    const xs = points.map((p) => wallStartSec + p.ts_us / 1_000_000)
    const ys = points.map((p) => p.value)
    const appByTs = new Map<number, number>()
    for (const p of appPoints ?? []) appByTs.set(p.ts_us, p.value)
    const appYs = points.map((p) => appByTs.get(p.ts_us) ?? NaN)
    return { xs, ys, appYs }
  }, [points, appPoints, wallStartMs])

  useEffect(() => {
    if (!hostRef.current) return
    xsRef.current = xs
    ysRef.current = ys
    appYsRef.current = appYs

    const opts: uPlot.Options = {
      width: hostRef.current.clientWidth,
      height: 280,
      scales: {
        x: { time: true },
        y: { range: [0, 100] },
      },
      series: [
        {},
        {
          label: 'Total CPU %',
          stroke: TOTAL_COLOR,
          width: 1.5,
          fill: 'rgba(52,145,250,0.10)',
          points: { show: false },
        },
        {
          label: 'App CPU %',
          stroke: APP_COLOR,
          width: 1.5,
          points: { show: false },
          spanGaps: false,
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
          values: (_u, ticks) => ticks.map((t) => `${t}%`),
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
              app: appYsRef.current[idx],
            })
          },
        ],
      },
    }
    if (plotRef.current) {
      plotRef.current.destroy()
    }
    plotRef.current = new uPlot(opts, [xs, ys, appYs], hostRef.current)
    setPlot(plotRef.current)

    const onResize = () => {
      if (!hostRef.current || !plotRef.current) return
      plotRef.current.setSize({ width: hostRef.current.clientWidth, height: 280 })
    }
    const cleanupResize = attachChartResize(hostRef.current, onResize)
    return () => {
      cleanupResize()
      plotRef.current?.destroy()
      plotRef.current = null
      setPlot(null)
    }
  }, [xs, ys, appYs])

  return (
    <div className={styles.chartCard}>
      <div className={styles.chartHeader}>
        <div className={styles.chartTitle}>CPU</div>
        <div className={styles.chartSub}>Total + App · %</div>
      </div>
      <div className={styles.stats}>
        <StatTile label="app max" value={appStats?.max ?? null} accent />
        <StatTile label="total max" value={stats?.max ?? null} />
        <StatTile label="app avg" value={appStats?.avg ?? null} />
        <StatTile label="total avg" value={stats?.avg ?? null} />
        <StatTile label="samples" value={stats?.count ?? null} unit="" raw />
      </div>
      <div ref={hostRef} className={styles.chartHost}>
        <MarkerOverlay
          plot={plot}
          wallStartSec={wallStartMs / 1000}
          controls={markers}
          />
        {tooltip && (
          <div
            className={styles.tooltip}
            style={{ left: clampTooltip(tooltip.left), top: tooltip.top - 12 }}
          >
            <div className={styles.tooltipTime}>{formatClock(tooltip.t)}</div>
            {Number.isFinite(tooltip.app) && (
              <div className={styles.tooltipRow} style={{ color: pctColor(tooltip.app) }}>
                <span className={styles.tooltipSwatch} style={{ background: APP_COLOR }} />
                App&nbsp;<b>{tooltip.app.toFixed(1)}%</b>
              </div>
            )}
            <div className={styles.tooltipRow} style={{ color: pctColor(tooltip.v) }}>
              <span className={styles.tooltipSwatch} style={{ background: TOTAL_COLOR }} />
              Total&nbsp;<b>{tooltip.v.toFixed(1)}%</b>
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
  unit = '%',
  accent = false,
  raw = false,
  valueColor,
}: {
  label: string
  value: number | null
  unit?: string
  accent?: boolean
  raw?: boolean
  /// Override the default pctColor(value) coloring (e.g. for °C).
  valueColor?: string
}) {
  const color = valueColor ?? (!raw && value != null ? pctColor(value) : undefined)
  return (
    <div className={`${styles.tile} ${accent ? styles.accent : ''}`}>
      <div className={styles.label}>{label}</div>
      <div className={styles.value} style={color ? { color } : undefined}>
        {value == null ? '—' : raw ? value.toFixed(0) : value.toFixed(1)}
        {value != null && unit && <span className={styles.unit}>{unit}</span>}
      </div>
    </div>
  )
}
