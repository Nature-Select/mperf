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

const DEVICE_COLOR = '#7b3df5'
const RENDERER_COLOR = '#3491fa'
const TILER_COLOR = '#f7ba1e'

interface Tooltip {
  left: number
  top: number
  t: number
  device: number | null
  renderer: number | null
  tiler: number | null
}

/// History counterpart of LiveGpuChart. Coalesces the three series onto
/// a shared 1-second-bucket timeline.
export function StaticGpuChart({
  devicePoints,
  rendererPoints,
  tilerPoints,
  markers,
  wallStartMs,
}: {
  devicePoints?: SamplePoint[]
  rendererPoints?: SamplePoint[]
  tilerPoints?: SamplePoint[]
  markers?: MarkerControls
  wallStartMs: number
}) {
  const d = devicePoints ?? []
  const r = rendererPoints ?? []
  const t = tilerPoints ?? []
  const hostRef = useRef<HTMLDivElement>(null)
  const plotRef = useRef<uPlot | null>(null)
  const xsRef = useRef<number[]>([])
  const dYsRef = useRef<(number | null)[]>([])
  const rYsRef = useRef<(number | null)[]>([])
  const tYsRef = useRef<(number | null)[]>([])
  const [plot, setPlot] = useState<uPlot | null>(null)
  const [tooltip, setTooltip] = useState<Tooltip | null>(null)

  const { xs, dYs, rYs, tYs } = useMemo(() => {
    const wallStartSec = wallStartMs / 1000
    const buckets = new Map<number, { d?: number; r?: number; t?: number }>()
    const bucketOf = (p: SamplePoint) =>
      Math.round(wallStartSec + p.ts_us / 1_000_000)
    for (const p of d) {
      const k = bucketOf(p)
      const e = buckets.get(k) ?? {}
      e.d = p.value
      buckets.set(k, e)
    }
    for (const p of r) {
      const k = bucketOf(p)
      const e = buckets.get(k) ?? {}
      e.r = p.value
      buckets.set(k, e)
    }
    for (const p of t) {
      const k = bucketOf(p)
      const e = buckets.get(k) ?? {}
      e.t = p.value
      buckets.set(k, e)
    }
    const keys = Array.from(buckets.keys()).sort((a, b) => a - b)
    return {
      xs: keys,
      dYs: keys.map((k) => buckets.get(k)!.d ?? null),
      rYs: keys.map((k) => buckets.get(k)!.r ?? null),
      tYs: keys.map((k) => buckets.get(k)!.t ?? null),
    }
  }, [d, r, t, wallStartMs])

  const dMax = useMemo(() => maxOf(d), [d])
  const rMax = useMemo(() => maxOf(r), [r])
  const tMax = useMemo(() => maxOf(t), [t])

  useEffect(() => {
    if (!hostRef.current) return
    xsRef.current = xs
    dYsRef.current = dYs
    rYsRef.current = rYs
    tYsRef.current = tYs

    const opts: uPlot.Options = {
      width: hostRef.current.clientWidth,
      height: 240,
      scales: { x: { time: true }, y: { range: [0, 100] } },
      series: [
        {},
        { label: 'Device %', stroke: DEVICE_COLOR, width: 1.5, fill: 'rgba(123,61,245,0.10)', points: { show: false }, spanGaps: true },
        { label: 'Renderer %', stroke: RENDERER_COLOR, width: 1.2, points: { show: false }, spanGaps: true },
        { label: 'Tiler %', stroke: TILER_COLOR, width: 1.2, points: { show: false }, spanGaps: true },
      ],
      axes: [
        { stroke: '#888', grid: { stroke: '#eee' }, values: (_u, ticks) => ticks.map(formatClock) },
        { stroke: '#888', grid: { stroke: '#eee' }, values: (_u, ticks) => ticks.map((t) => `${t}%`) },
      ],
      legend: { show: false },
      cursor: { drag: { x: true, y: false, uni: 10 }, focus: { prox: 30 } },
      hooks: {
        setCursor: [
          (u) => {
            const idx = u.cursor.idx
            if (
              idx == null || idx < 0 || idx >= xsRef.current.length ||
              u.cursor.left == null || u.cursor.left < 0
            ) {
              setTooltip(null)
              return
            }
            setTooltip({
              left: u.cursor.left,
              top: u.cursor.top ?? 0,
              t: xsRef.current[idx],
              device: dYsRef.current[idx],
              renderer: rYsRef.current[idx],
              tiler: tYsRef.current[idx],
            })
          },
        ],
      },
    }
    if (plotRef.current) plotRef.current.destroy()
    plotRef.current = new uPlot(opts, [xs, dYs, rYs, tYs] as uPlot.AlignedData, hostRef.current)
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
  }, [xs, dYs, rYs, tYs])

  return (
    <div className={styles.chartCard}>
      <div className={styles.chartHeader}>
        <div className={styles.chartTitle}>GPU</div>
        <div className={styles.chartSub}>Device + Renderer + Tiler · %</div>
      </div>
      <div className={styles.stats}>
        <StatTile label="device max" value={dMax} accent />
        <StatTile label="renderer max" value={rMax} />
        <StatTile label="tiler max" value={tMax} />
        <StatTile label="samples" value={xs.length} unit="" raw />
      </div>
      <div ref={hostRef} className={styles.chartHost}>
        <MarkerOverlay
          plot={plot}
          wallStartSec={wallStartMs / 1000}
          controls={markers}
          />
        {tooltip && (
          <div className={styles.tooltip} style={{ left: clampTooltip(tooltip.left), top: tooltip.top - 12 }}>
            <div className={styles.tooltipTime}>{formatClock(tooltip.t)}</div>
            {tooltip.device != null && (
              <div className={styles.tooltipRow} style={{ color: pctColor(tooltip.device) }}>
                <span className={styles.tooltipSwatch} style={{ background: DEVICE_COLOR }} />
                Device&nbsp;<b>{tooltip.device.toFixed(1)}%</b>
              </div>
            )}
            {tooltip.renderer != null && (
              <div className={styles.tooltipRow} style={{ color: pctColor(tooltip.renderer) }}>
                <span className={styles.tooltipSwatch} style={{ background: RENDERER_COLOR }} />
                Renderer&nbsp;<b>{tooltip.renderer.toFixed(1)}%</b>
              </div>
            )}
            {tooltip.tiler != null && (
              <div className={styles.tooltipRow} style={{ color: pctColor(tooltip.tiler) }}>
                <span className={styles.tooltipSwatch} style={{ background: TILER_COLOR }} />
                Tiler&nbsp;<b>{tooltip.tiler.toFixed(1)}%</b>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  )
}

function maxOf(points: SamplePoint[]): number | null {
  if (points.length === 0) return null
  let m = -Infinity
  for (const p of points) if (p.value > m) m = p.value
  return Number.isFinite(m) ? m : null
}

function StatTile({
  label,
  value,
  unit = '%',
  accent = false,
  raw = false,
}: {
  label: string
  value: number | null
  unit?: string
  accent?: boolean
  raw?: boolean
}) {
  const color = !raw && value != null ? pctColor(value) : undefined
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
