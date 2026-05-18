import { useEffect, useRef, useState } from 'react'
import { listen, UnlistenFn } from '@tauri-apps/api/event'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { EVENT_SAMPLE, Sample } from '@/lib/ipc'
import { attachChartResize } from '@/lib/chartResize'
import { MarkerControls } from '@/lib/markers'
import { MarkerOverlay } from './MarkerOverlay'
import { pctColor, SERIES_PALETTE } from '@/lib/format'
import { clampTooltip, formatClock } from '@/lib/format'
import { PinTooltip } from './PinTooltip'
import styles from './chart-shared.module.scss'

const MAX_POINTS = 300

interface Tooltip {
  left: number
  top: number
  t: number
  values: number[]
}

interface Pin {
  t: number
  values: number[]
  left: number
  top: number
}

export function LivePerCoreChart({
  active,
  markers,
}: {
  active: boolean
  markers?: MarkerControls
}) {
  const hostRef = useRef<HTMLDivElement>(null)
  const plotRef = useRef<uPlot | null>(null)
  /// Holds the disposer returned by `attachChartResize`. Stored in a
  /// ref because chart rebuilds (e.g. when CPU core count first
  /// resolves) happen outside the effect that owns the listener.
  const cleanupResizeRef = useRef<(() => void) | null>(null)
  const xsRef = useRef<number[]>([])
  const seriesRef = useRef<number[][]>([])
  const coreCountRef = useRef<number>(0)
  const wallStartSecRef = useRef<number>(0)

  const pendingTsRef = useRef<number | null>(null)
  const pendingVecRef = useRef<number[]>([])

  const pinRef = useRef<Pin | null>(null)
  const [plot, setPlot] = useState<uPlot | null>(null)

  const [coreCount, setCoreCount] = useState(0)
  const [liveCores, setLiveCores] = useState<number[]>([])
  const [tooltip, setTooltip] = useState<Tooltip | null>(null)
  const [pin, setPin] = useState<Pin | null>(null)

  useEffect(() => {
    pinRef.current = pin
  }, [pin])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && pinRef.current) {
        setPin(null)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => {
      window.removeEventListener('keydown', onKey)
      if (cleanupResizeRef.current) {
        cleanupResizeRef.current()
        cleanupResizeRef.current = null
      }
      plotRef.current?.destroy()
      plotRef.current = null
      setPlot(null)
    }
  }, [])

  function ensureChart(n: number) {
    if (plotRef.current || !hostRef.current) return
    const series: uPlot.Series[] = [{}]
    for (let i = 0; i < n; i++) {
      series.push({
        label: `cpu${i}`,
        stroke: SERIES_PALETTE[i % SERIES_PALETTE.length],
        width: 1.2,
        points: { show: false },
        spanGaps: true,
      })
    }
    const data: uPlot.AlignedData = [xsRef.current, ...seriesRef.current] as uPlot.AlignedData
    const opts: uPlot.Options = {
      width: hostRef.current.clientWidth,
      height: 240,
      scales: {
        x: { time: true },
        y: { range: [0, 100] },
      },
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
      cursor: {
        drag: { x: true, y: false, uni: 10 },
        focus: { prox: 30 },
      },
      hooks: {
        setCursor: [
          (u) => {
            if (pinRef.current) {
              setTooltip(null)
              return
            }
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
    plotRef.current = new uPlot(opts, data, hostRef.current)
    setPlot(plotRef.current)

    const onResize = () => {
      if (!hostRef.current || !plotRef.current) return
      plotRef.current.setSize({ width: hostRef.current.clientWidth, height: 240 })
      syncPinPos()
    }
    // Replace any previous attachment — chart rebuilds when core count
    // first resolves go through this path.
    if (cleanupResizeRef.current) cleanupResizeRef.current()
    cleanupResizeRef.current = attachChartResize(hostRef.current, onResize)
  }

  function syncPinPos() {
    const p = pinRef.current
    const u = plotRef.current
    if (!p || !u || xsRef.current.length === 0) return
    const x0 = xsRef.current[0]
    const xN = xsRef.current[xsRef.current.length - 1]
    if (p.t < x0 || p.t > xN) {
      setPin(null)
      return
    }
    const left = u.valToPos(p.t, 'x')
    // Use a representative y (mid of values) for vertical placement;
    // tooltip floats roughly horizontally above all series anyway.
    const validVals = p.values.filter((v) => Number.isFinite(v))
    const yRef =
      validVals.length > 0 ? validVals.reduce((a, b) => a + b, 0) / validVals.length : 50
    const top = u.valToPos(yRef, 'y')
    if (left !== p.left || top !== p.top) {
      setPin({ ...p, left, top })
    }
  }

  function flushPending() {
    const ts = pendingTsRef.current
    if (ts == null) return
    const t = wallStartSecRef.current + ts / 1_000_000
    xsRef.current.push(t)
    const lastValues: number[] = []
    for (let i = 0; i < coreCountRef.current; i++) {
      const v = pendingVecRef.current[i]
      const fv = v == null ? NaN : v
      seriesRef.current[i].push(fv)
      lastValues.push(fv)
    }
    if (xsRef.current.length > MAX_POINTS) {
      xsRef.current.shift()
      for (const s of seriesRef.current) s.shift()
    }
    pendingTsRef.current = null
    pendingVecRef.current = []
    if (plotRef.current) {
      plotRef.current.setData(
        [xsRef.current, ...seriesRef.current] as uPlot.AlignedData,
        true,
      )
    }
    setLiveCores(lastValues)
    syncPinPos()
  }

  useEffect(() => {
    if (!active) return

    xsRef.current = []
    seriesRef.current = []
    coreCountRef.current = 0
    pendingTsRef.current = null
    pendingVecRef.current = []
    wallStartSecRef.current = Date.now() / 1000
    setCoreCount(0)
    setLiveCores([])
    setTooltip(null)
    setPin(null)

    if (plotRef.current) {
      if (cleanupResizeRef.current) {
        cleanupResizeRef.current()
        cleanupResizeRef.current = null
      }
      plotRef.current.destroy()
      plotRef.current = null
      setPlot(null)
    }

    let unlisten: UnlistenFn | undefined
    let cancelled = false

    listen<Sample>(EVENT_SAMPLE, (e) => {
      const s = e.payload
      if (s.kind !== 'cpu_core_pct') return
      const idxStr = s.labels?.find(([k]) => k === 'core_idx')?.[1]
      if (idxStr == null) return
      const idx = parseInt(idxStr, 10)
      if (!Number.isFinite(idx)) return

      while (coreCountRef.current <= idx) {
        seriesRef.current.push([])
        coreCountRef.current += 1
      }

      if (pendingTsRef.current != null && pendingTsRef.current !== s.ts_us) {
        if (!plotRef.current) {
          ensureChart(coreCountRef.current)
          setCoreCount(coreCountRef.current)
        }
        flushPending()
      }
      pendingTsRef.current = s.ts_us
      pendingVecRef.current[idx] = s.value
    }).then((fn) => {
      if (cancelled) fn()
      else unlisten = fn
    })

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [active])

  const onChartClick = () => {
    const u = plotRef.current
    if (!u) return
    if (pinRef.current) {
      setPin(null)
      return
    }
    const idx = u.cursor.idx
    if (idx == null || idx < 0 || idx >= xsRef.current.length) return
    const t = xsRef.current[idx]
    const values: number[] = []
    for (let i = 0; i < coreCountRef.current; i++) {
      values.push(seriesRef.current[i][idx])
    }
    const validVals = values.filter((v) => Number.isFinite(v))
    const yRef =
      validVals.length > 0 ? validVals.reduce((a, b) => a + b, 0) / validVals.length : 50
    setPin({
      t,
      values,
      left: u.valToPos(t, 'x'),
      top: u.valToPos(yRef, 'y'),
    })
    setTooltip(null)
  }

  return (
    <div className={styles.chartCard}>
      <div className={styles.chartHeader}>
        <div className={styles.chartTitle}>per-core CPU</div>
        {coreCount > 0 && <div className={styles.chartSub}>{coreCount} cores · %</div>}
      </div>
      {/* coreRow always renders so the card height is stable when data
          starts flowing in. Empty state shows a single placeholder tile
          spanning the full row, matching the real-tile height exactly. */}
      <div className={styles.coreRow}>
        {coreCount === 0 ? (
          <div
            className={styles.coreTile}
            style={{
              gridColumn: '1 / -1',
              justifyContent: 'center',
              color: 'var(--color-text-3)',
              fontStyle: 'italic',
            }}
          >
            {active ? 'waiting for first tick…' : 'no data — click Start to record'}
          </div>
        ) : (
          Array.from({ length: coreCount }).map((_, i) => {
            const v = liveCores[i]
            const color = SERIES_PALETTE[i % SERIES_PALETTE.length]
            const valueColor = Number.isFinite(v) ? pctColor(v) : undefined
            return (
              <div key={i} className={styles.coreTile}>
                <span className={styles.coreSwatch} style={{ background: color }} />
                <span className={styles.coreLabel}>cpu{i}</span>
                <span
                  className={styles.coreValue}
                  style={valueColor ? { color: valueColor } : undefined}
                >
                  {Number.isFinite(v) ? `${v.toFixed(1)}%` : '—'}
                </span>
              </div>
            )
          })
        )}
      </div>
      {/* Reserve the chart canvas height up-front. uPlot is created
          lazily in ensureChart() once the first sample tells us the core
          count, so without this min-height the card would suddenly grow
          by 240px when data starts arriving. */}
      <div
        ref={hostRef}
        className={styles.chartHost}
        onClick={onChartClick}
        style={{ minHeight: 240 }}
      >
        <MarkerOverlay
          plot={plot}
          wallStartSec={wallStartSecRef.current}
          controls={markers}
          />
        {pin && (
          <PinTooltip
            left={pin.left}
            top={pin.top}
            t={pin.t}
            rows={pin.values.map((v, i) => ({
              swatch: SERIES_PALETTE[i % SERIES_PALETTE.length],
              label: `cpu${i}`,
              value: v,
            }))}
          />
        )}
        {!pin && tooltip && (
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
