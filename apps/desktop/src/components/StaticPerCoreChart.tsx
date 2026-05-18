import { useEffect, useMemo, useRef, useState } from 'react'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { CoreSampleRow } from '@/lib/ipc'
import { attachChartResize } from '@/lib/chartResize'
import { MarkerControls } from '@/lib/markers'
import { pctColor, SERIES_PALETTE } from '@/lib/format'
import { MarkerOverlay } from './MarkerOverlay'
import { clampTooltip, formatClock } from '@/lib/format'
import styles from './chart-shared.module.scss'

interface Tooltip {
  left: number
  top: number
  t: number
  values: number[]
}

/// Static (recorded) per-core CPU chart.
export function StaticPerCoreChart({
  rows,
  markers,
  wallStartMs,
}: {
  rows: CoreSampleRow[]
  markers?: MarkerControls
  wallStartMs: number
}) {
  const hostRef = useRef<HTMLDivElement>(null)
  const plotRef = useRef<uPlot | null>(null)
  const xsRef = useRef<number[]>([])
  const seriesRef = useRef<number[][]>([])
  const coreCountRef = useRef<number>(0)
  const [plot, setPlot] = useState<uPlot | null>(null)
  const [tooltip, setTooltip] = useState<Tooltip | null>(null)

  // Pivot the long-format rows into (xs, per-core ys) aligned arrays.
  const { xs, seriesByCore, coreCount } = useMemo(() => {
    const wallStartSec = wallStartMs / 1000
    // Group by ts_us; within a tick, samples arrive labeled by core idx.
    const byTs = new Map<number, number[]>()
    let maxCore = -1
    for (const [tsUs, idxStr, v] of rows) {
      const idx = parseInt(idxStr, 10)
      if (!Number.isFinite(idx)) continue
      if (idx > maxCore) maxCore = idx
      let arr = byTs.get(tsUs)
      if (!arr) {
        arr = []
        byTs.set(tsUs, arr)
      }
      arr[idx] = v
    }
    const coreCount = maxCore + 1
    const sortedTs = [...byTs.keys()].sort((a, b) => a - b)
    const xs = sortedTs.map((ts) => wallStartSec + ts / 1_000_000)
    const seriesByCore: number[][] = Array.from({ length: coreCount }, (_, i) =>
      sortedTs.map((ts) => {
        const v = byTs.get(ts)![i]
        return v == null ? NaN : v
      }),
    )
    return { xs, seriesByCore, coreCount }
  }, [rows, wallStartMs])

  useEffect(() => {
    if (!hostRef.current) return
    if (coreCount === 0) return
    xsRef.current = xs
    seriesRef.current = seriesByCore
    coreCountRef.current = coreCount

    const series: uPlot.Series[] = [{}]
    for (let i = 0; i < coreCount; i++) {
      series.push({
        label: `cpu${i}`,
        stroke: SERIES_PALETTE[i % SERIES_PALETTE.length],
        width: 1.2,
        points: { show: false },
        spanGaps: true,
      })
    }

    const opts: uPlot.Options = {
      width: hostRef.current.clientWidth,
      height: 240,
      scales: { x: { time: true }, y: { range: [0, 100] } },
      series,
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
            const values: number[] = []
            for (let i = 0; i < coreCountRef.current; i++) {
              values.push(seriesRef.current[i][idx])
            }
            setTooltip({
              left: u.cursor.left,
              top: u.cursor.top ?? 0,
              t: xsRef.current[idx],
              values,
            })
          },
        ],
      },
    }
    if (plotRef.current) plotRef.current.destroy()
    plotRef.current = new uPlot(
      opts,
      [xs, ...seriesByCore] as uPlot.AlignedData,
      hostRef.current,
    )
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
  }, [xs, seriesByCore, coreCount])

  if (coreCount === 0) {
    return (
      <div className={styles.chartCard}>
        <div className={styles.chartHeader}>
          <div className={styles.chartTitle}>per-core CPU</div>
        </div>
        <div className={styles.empty}>no per-core data recorded</div>
      </div>
    )
  }

  return (
    <div className={styles.chartCard}>
      <div className={styles.chartHeader}>
        <div className={styles.chartTitle}>per-core CPU</div>
        <div className={styles.chartSub}>{coreCount} cores · %</div>
      </div>
      <div ref={hostRef} className={styles.chartHost}>
        <MarkerOverlay
          plot={plot}
          wallStartSec={wallStartMs / 1000}
          controls={markers}
          />
        {tooltip && (
          <div
            className={styles.tooltipWide}
            style={{ left: clampTooltip(tooltip.left), top: tooltip.top - 12 }}
          >
            <div className={styles.tooltipTime}>{formatClock(tooltip.t)}</div>
            <div className={styles.tooltipGrid}>
              {tooltip.values.map((v, i) => (
                <div key={i} className={styles.tooltipRow}>
                  <span
                    className={styles.tooltipSwatch}
                    style={{ background: SERIES_PALETTE[i % SERIES_PALETTE.length] }}
                  />
                  cpu{i}&nbsp;
                  <b style={{ color: Number.isFinite(v) ? pctColor(v) : undefined }}>
                    {Number.isFinite(v) ? `${v.toFixed(1)}%` : '—'}
                  </b>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
