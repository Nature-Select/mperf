import { useEffect, useMemo, useRef, useState } from 'react'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { SamplePoint } from '@/lib/ipc'
import { attachChartResize } from '@/lib/chartResize'
import { MarkerControls } from '@/lib/markers'
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
}

export function StaticMemoryChart({
  pssPoints,
  markers,
  wallStartMs,
}: {
  pssPoints: SamplePoint[]
  markers?: MarkerControls
  wallStartMs: number
}) {
  const hostRef = useRef<HTMLDivElement>(null)
  const plotRef = useRef<uPlot | null>(null)
  const xsRef = useRef<number[]>([])
  const ysRef = useRef<number[]>([])
  const [plot, setPlot] = useState<uPlot | null>(null)
  const [tooltip, setTooltip] = useState<Tooltip | null>(null)

  const stats: Stats | null = useMemo(() => {
    if (pssPoints.length === 0) return null
    let min = Infinity
    let max = -Infinity
    let sum = 0
    for (const p of pssPoints) {
      const mb = p.value / 1024 / 1024
      if (mb < min) min = mb
      if (mb > max) max = mb
      sum += mb
    }
    return { min, max, avg: sum / pssPoints.length, count: pssPoints.length }
  }, [pssPoints])

  const { xs, ys } = useMemo(() => {
    const wallStartSec = wallStartMs / 1000
    return {
      xs: pssPoints.map((p) => wallStartSec + p.ts_us / 1_000_000),
      ys: pssPoints.map((p) => p.value / 1024 / 1024),
    }
  }, [pssPoints, wallStartMs])

  useEffect(() => {
    if (!hostRef.current) return
    xsRef.current = xs
    ysRef.current = ys
    const opts: uPlot.Options = {
      width: hostRef.current.clientWidth,
      height: 240,
      scales: { x: { time: true }, y: {} },
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
      cursor: { drag: { x: true, y: false, uni: 10 }, focus: { prox: 30 } },
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
    if (plotRef.current) plotRef.current.destroy()
    plotRef.current = new uPlot(opts, [xs, ys], hostRef.current)
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
  }, [xs, ys])

  if (pssPoints.length === 0) {
    return (
      <div className={styles.chartCard}>
        <div className={styles.chartHeader}>
          <div className={styles.chartTitle}>Memory</div>
          <div className={styles.chartSub}>App PSS · MB</div>
        </div>
        <div className={styles.empty}>no memory data recorded</div>
      </div>
    )
  }

  return (
    <div className={styles.chartCard}>
      <div className={styles.chartHeader}>
        <div className={styles.chartTitle}>Memory</div>
        <div className={styles.chartSub}>App PSS · MB</div>
      </div>
      <div className={styles.stats}>
        <StatTile label="min" value={stats?.min ?? null} />
        <StatTile label="max" value={stats?.max ?? null} accent />
        <StatTile label="avg" value={stats?.avg ?? null} />
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
  unit = 'MB',
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
