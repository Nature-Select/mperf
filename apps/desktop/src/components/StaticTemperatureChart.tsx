import { useEffect, useMemo, useRef, useState } from 'react'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { SamplePoint } from '@/lib/ipc'
import { attachChartResize } from '@/lib/chartResize'
import { MarkerControls } from '@/lib/markers'
import { MarkerOverlay } from './MarkerOverlay'
import { clampTooltip, formatClock, tempColor } from '@/lib/format'
import styles from './chart-shared.module.scss'

const CTEMP_COLOR = '#f7ba1e'
const BTEMP_COLOR = '#37c2ff'

interface Tooltip {
  left: number
  top: number
  t: number
  c: number | null
  b: number | null
}

/// Static temperature chart for History view. Mirrors LiveTemperatureChart
/// using recorded `cpu_temp_c` and `battery_temp_c` series.
export function StaticTemperatureChart({
  cpuPoints,
  batteryPoints,
  markers,
  wallStartMs,
}: {
  cpuPoints?: SamplePoint[]
  batteryPoints?: SamplePoint[]
  markers?: MarkerControls
  wallStartMs: number
}) {
  const cpu = cpuPoints ?? []
  const bat = batteryPoints ?? []
  const hostRef = useRef<HTMLDivElement>(null)
  const plotRef = useRef<uPlot | null>(null)
  const xsRef = useRef<number[]>([])
  const cYsRef = useRef<(number | null)[]>([])
  const bYsRef = useRef<(number | null)[]>([])
  const [plot, setPlot] = useState<uPlot | null>(null)
  const [tooltip, setTooltip] = useState<Tooltip | null>(null)

  // Coalesce both series onto a shared timeline by 1-second buckets.
  // ts_us values from different samplers won't coincide exactly because
  // they fire on different intervals, so we round to nearest second and
  // overlay.
  const { xs, cYs, bYs } = useMemo(() => {
    const wallStartSec = wallStartMs / 1000
    const buckets = new Map<number, { c?: number; b?: number }>()
    const bucketOf = (p: SamplePoint) =>
      Math.round(wallStartSec + p.ts_us / 1_000_000)
    for (const p of cpu) {
      const k = bucketOf(p)
      const e = buckets.get(k) ?? {}
      e.c = p.value
      buckets.set(k, e)
    }
    for (const p of bat) {
      const k = bucketOf(p)
      const e = buckets.get(k) ?? {}
      e.b = p.value
      buckets.set(k, e)
    }
    const keys = Array.from(buckets.keys()).sort((a, b) => a - b)
    return {
      xs: keys,
      cYs: keys.map((k) => buckets.get(k)!.c ?? null),
      bYs: keys.map((k) => buckets.get(k)!.b ?? null),
    }
  }, [cpu, bat, wallStartMs])

  const cMax = useMemo(() => {
    let m = -Infinity
    for (const p of cpu) if (p.value > m) m = p.value
    return Number.isFinite(m) ? m : null
  }, [cpu])
  const bMax = useMemo(() => {
    let m = -Infinity
    for (const p of bat) if (p.value > m) m = p.value
    return Number.isFinite(m) ? m : null
  }, [bat])

  useEffect(() => {
    if (!hostRef.current) return
    xsRef.current = xs
    cYsRef.current = cYs
    bYsRef.current = bYs

    const opts: uPlot.Options = {
      width: hostRef.current.clientWidth,
      height: 200,
      scales: { x: { time: true }, y: {} },
      series: [
        {},
        // spanGaps:true so each line is continuous through its own
        // samples even when the OTHER series has NaN at the same x
        // (CTemp and BTemp rarely share a 1-second bucket).
        { label: 'CTemp °C', stroke: CTEMP_COLOR, width: 1.5, points: { show: false }, spanGaps: true },
        { label: 'BTemp °C', stroke: BTEMP_COLOR, width: 1.5, points: { show: false }, spanGaps: true },
      ],
      axes: [
        { stroke: '#888', grid: { stroke: '#eee' }, values: (_u, ticks) => ticks.map(formatClock) },
        { stroke: '#888', grid: { stroke: '#eee' }, values: (_u, ticks) => ticks.map((t) => `${t.toFixed(0)}°`) },
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
              c: cYsRef.current[idx],
              b: bYsRef.current[idx],
            })
          },
        ],
      },
    }
    if (plotRef.current) plotRef.current.destroy()
    plotRef.current = new uPlot(opts, [xs, cYs, bYs] as uPlot.AlignedData, hostRef.current)
    setPlot(plotRef.current)
    const onResize = () => {
      if (!hostRef.current || !plotRef.current) return
      plotRef.current.setSize({ width: hostRef.current.clientWidth, height: 200 })
    }
    const cleanupResize = attachChartResize(hostRef.current, onResize)
    return () => {
      cleanupResize()
      plotRef.current?.destroy()
      plotRef.current = null
      setPlot(null)
    }
  }, [xs, cYs, bYs])

  return (
    <div className={styles.chartCard}>
      <div className={styles.chartHeader}>
        <div className={styles.chartTitle}>Temperature</div>
        <div className={styles.chartSub}>CPU + Battery · °C</div>
      </div>
      <div className={styles.stats}>
        <StatTile label="cpu max" value={cMax} color={tempColor(cMax)} accent />
        <StatTile label="battery max" value={bMax} color={tempColor(bMax)} />
        <StatTile label="cpu samples" value={cpu.length} unit="" raw />
        <StatTile label="battery samples" value={bat.length} unit="" raw />
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
            {tooltip.c != null && (
              <div className={styles.tooltipRow} style={{ color: tempColor(tooltip.c) }}>
                <span className={styles.tooltipSwatch} style={{ background: CTEMP_COLOR }} />
                CPU&nbsp;<b>{tooltip.c.toFixed(1)}°C</b>
              </div>
            )}
            {tooltip.b != null && (
              <div className={styles.tooltipRow} style={{ color: tempColor(tooltip.b) }}>
                <span className={styles.tooltipSwatch} style={{ background: BTEMP_COLOR }} />
                Battery&nbsp;<b>{tooltip.b.toFixed(1)}°C</b>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  )
}

function StatTile({
  label,
  value,
  unit = '°C',
  color,
  accent = false,
  raw = false,
}: {
  label: string
  value: number | null
  unit?: string
  color?: string
  accent?: boolean
  raw?: boolean
}) {
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
