import { useEffect, useMemo, useRef, useState } from 'react'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { SamplePoint } from '@/lib/ipc'
import { attachChartResize } from '@/lib/chartResize'
import { MarkerControls } from '@/lib/markers'
import { fpsColor, jankColor, stutterColor } from '@/lib/format'
import { computeFpsAdvanced } from '@/lib/fps-stats'
import { MarkerOverlay } from './MarkerOverlay'
import { clampTooltip, formatClock } from '@/lib/format'
import { FpsAdvancedPanel } from './FpsAdvancedPanel'
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

export function StaticFpsChart({
  fpsPoints,
  smallJankTotal,
  jankTotal,
  bigJankTotal,
  stutter,
  platform,
  markers,
  wallStartMs,
}: {
  fpsPoints: SamplePoint[]
  smallJankTotal: number
  jankTotal: number
  bigJankTotal: number
  stutter: number | null
  /// Subtitle wording only — Android FPS is per-app (gfxinfo per pkg +
  /// SurfaceFlinger per layer), iOS FPS is screen-level (DTX
  /// CoreAnimation tree). See `LiveFpsChart` for the same prop.
  platform: 'android' | 'ios'
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
    if (fpsPoints.length === 0) return null
    let min = Infinity
    let max = -Infinity
    let sum = 0
    for (const p of fpsPoints) {
      if (p.value < min) min = p.value
      if (p.value > max) max = p.value
      sum += p.value
    }
    return { min, max, avg: sum / fpsPoints.length, count: fpsPoints.length }
  }, [fpsPoints])

  const advanced = useMemo(
    () => computeFpsAdvanced(fpsPoints.map((p) => p.value)),
    [fpsPoints],
  )

  const { xs, ys } = useMemo(() => {
    const wallStartSec = wallStartMs / 1000
    return {
      xs: fpsPoints.map((p) => wallStartSec + p.ts_us / 1_000_000),
      ys: fpsPoints.map((p) => p.value),
    }
  }, [fpsPoints, wallStartMs])

  useEffect(() => {
    if (!hostRef.current) return
    xsRef.current = xs
    ysRef.current = ys

    const opts: uPlot.Options = {
      width: hostRef.current.clientWidth,
      height: 240,
      scales: {
        x: { time: true },
        y: { range: [0, 130] },
      },
      series: [
        {},
        {
          label: 'FPS',
          stroke: '#00b42a',
          width: 1.5,
          fill: 'rgba(0,180,42,0.12)',
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
          values: (_u, ticks) => ticks.map((t) => `${t}`),
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

  return (
    <div className={styles.chartCard}>
      <div className={styles.chartHeader}>
        <div className={styles.chartTitle}>FPS / Jank</div>
        <div className={styles.chartSub}>
          {platform === 'android'
            ? 'Frame rate + Jank · fps'
            : 'Frame rate · fps · 屏幕级'}
        </div>
      </div>
      <div className={styles.stats}>
        <StatTile label="min" value={stats?.min ?? null} unit="" />
        <StatTile label="max" value={stats?.max ?? null} unit="" accent />
        <StatTile label="avg" value={stats?.avg ?? null} unit="" />
        <StatTile
          label="jank"
          value={jankTotal}
          unit=""
          raw
          valueColor={jankColor(jankTotal)}
          hint={`small: ${smallJankTotal} · big: ${bigJankTotal}`}
        />
        <StatTile
          label="stutter"
          value={stutter == null ? null : stutter * 100}
          unit="%"
          valueColor={stutter == null ? undefined : stutterColor(stutter * 100)}
        />
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
            <div className={styles.tooltipRow} style={{ color: fpsColor(tooltip.v) }}>
              FPS&nbsp;<b>{tooltip.v.toFixed(1)}</b>
            </div>
          </div>
        )}
      </div>
      <FpsAdvancedPanel stats={advanced} />
    </div>
  )
}

function StatTile({
  label,
  value,
  unit = '',
  accent = false,
  raw = false,
  valueColor,
  hint,
}: {
  label: string
  value: number | null
  unit?: string
  accent?: boolean
  raw?: boolean
  valueColor?: string
  hint?: string
}) {
  const color = valueColor ?? (!raw && value != null ? fpsColor(value) : undefined)
  return (
    <div className={`${styles.tile} ${accent ? styles.accent : ''}`}>
      <div className={styles.label}>
        {label}
        {hint && <span style={{ marginLeft: 6, opacity: 0.6 }}>· {hint}</span>}
      </div>
      <div className={styles.value} style={color ? { color } : undefined}>
        {value == null ? '—' : raw ? value.toFixed(0) : value.toFixed(1)}
        {value != null && unit && <span className={styles.unit}>{unit}</span>}
      </div>
    </div>
  )
}
