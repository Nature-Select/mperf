import { useEffect, useRef, useState } from 'react'
import { listen, UnlistenFn } from '@tauri-apps/api/event'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { EVENT_SAMPLE, Sample } from '@/lib/ipc'
import { attachChartResize } from '@/lib/chartResize'
import { clampTooltip, formatClock, pctColor } from '@/lib/format'
import { MarkerControls } from '@/lib/markers'
import { MarkerOverlay } from './MarkerOverlay'
import { PinTooltip } from './PinTooltip'
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
  app: number
}

interface Pin {
  t: number
  v: number
  app: number
  /// Pixel coords are recomputed from (t,v) on each frame so the pin stays
  /// glued to the data point as the chart scrolls.
  left: number
  top: number
}

const TOTAL_COLOR = '#3491fa'
const APP_COLOR = '#f53f3f'

/// CPU chart matching PerfDog's "two lines on one chart" convention:
/// Total system CPU (blue) and App CPU (red, the headline). When no app
/// is selected, only the Total line is populated.
export function LiveCpuChart({
  active,
  markers,
}: {
  active: boolean
  markers?: MarkerControls
}) {
  const hostRef = useRef<HTMLDivElement>(null)
  const plotRef = useRef<uPlot | null>(null)
  const xsRef = useRef<number[]>([])
  const ysRef = useRef<number[]>([])
  /// App CPU% aligned to xs by index. NaN where the matching cpu_app_pct
  /// hasn't (yet) arrived for this tick, or no app was attributable.
  /// uPlot draws NaN as a gap (spanGaps:false on the App series).
  const appYsRef = useRef<number[]>([])
  const wallStartSecRef = useRef<number>(0)
  /// React state mirror of plotRef so the MarkerOverlay child re-renders
  /// after the uPlot instance is created (refs alone don't trigger
  /// re-render). Trade: one extra render per chart mount.
  const [plot, setPlot] = useState<uPlot | null>(null)
  const pinRef = useRef<Pin | null>(null)

  const [live, setLive] = useState<Stats>(EMPTY_STATS)
  const [liveApp, setLiveApp] = useState<Stats>(EMPTY_STATS)
  const [tooltip, setTooltip] = useState<Tooltip | null>(null)
  const [pin, setPin] = useState<Pin | null>(null)

  // Keep ref in sync with state so chart hooks read the latest value.
  useEffect(() => {
    pinRef.current = pin
  }, [pin])

  useEffect(() => {
    if (!hostRef.current) return
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
    plotRef.current = new uPlot(
      opts,
      [xsRef.current, ysRef.current, appYsRef.current],
      hostRef.current,
    )
    setPlot(plotRef.current)

    const onResize = () => {
      if (!hostRef.current || !plotRef.current) return
      plotRef.current.setSize({ width: hostRef.current.clientWidth, height: 280 })
      syncPinPos()
    }
    const cleanupResize = attachChartResize(hostRef.current, onResize)

    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && pinRef.current) {
        setPin(null)
      }
    }
    window.addEventListener('keydown', onKey)

    return () => {
      cleanupResize()
      window.removeEventListener('keydown', onKey)
      plotRef.current?.destroy()
      plotRef.current = null
      setPlot(null)
    }
  }, [])

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
    const top = u.valToPos(p.v, 'y')
    if (left !== p.left || top !== p.top) {
      setPin({ ...p, left, top })
    }
  }

  useEffect(() => {
    if (!active) return

    xsRef.current.length = 0
    ysRef.current.length = 0
    appYsRef.current.length = 0
    wallStartSecRef.current = Date.now() / 1000
    plotRef.current?.setData([xsRef.current, ysRef.current, appYsRef.current])
    setLive(EMPTY_STATS)
    setLiveApp(EMPTY_STATS)
    setTooltip(null)
    setPin(null)

    let unlisten: UnlistenFn | undefined
    let cancelled = false

    listen<Sample>(EVENT_SAMPLE, (e) => {
      const s = e.payload
      const t = wallStartSecRef.current + s.ts_us / 1_000_000

      if (s.kind === 'cpu_total_pct') {
        // Total is the anchor: each total tick adds an x-point and a
        // NaN slot for App, which app_pct backfills below if it arrives.
        xsRef.current.push(t)
        ysRef.current.push(s.value)
        appYsRef.current.push(NaN)
        if (xsRef.current.length > MAX_POINTS) {
          xsRef.current.shift()
          ysRef.current.shift()
          appYsRef.current.shift()
        }
        plotRef.current?.setData([xsRef.current, ysRef.current, appYsRef.current])
        setLive(computeStats(ysRef.current))
        setLiveApp(computeStats(appYsRef.current.filter((v) => Number.isFinite(v))))
        syncPinPos()
      } else if (s.kind === 'cpu_app_pct') {
        // Same-tick backfill (Total is yielded first by the sampler, so
        // the latest x already exists when App arrives).
        const idx = xsRef.current.length - 1
        if (idx >= 0 && Math.abs(xsRef.current[idx] - t) < 0.5) {
          appYsRef.current[idx] = s.value
          plotRef.current?.setData([xsRef.current, ysRef.current, appYsRef.current])
          setLiveApp(computeStats(appYsRef.current.filter((v) => Number.isFinite(v))))
        }
      }
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
    const v = ysRef.current[idx]
    const app = appYsRef.current[idx]
    setPin({
      t,
      v,
      app,
      left: u.valToPos(t, 'x'),
      top: u.valToPos(v, 'y'),
    })
    setTooltip(null)
  }

  const totalColor = live.cur != null ? pctColor(live.cur) : undefined
  const appColor = liveApp.cur != null ? pctColor(liveApp.cur) : undefined

  return (
    <div className={styles.chartCard}>
      <div className={styles.chartHeader}>
        <div className={styles.chartTitle}>CPU</div>
        <div className={styles.chartSub}>Total + App · %</div>
      </div>
      <div className={styles.stats}>
        <StatTile label="app" value={liveApp.cur} accent valueColor={appColor} />
        <StatTile label="total" value={live.cur} valueColor={totalColor} />
        <StatTile label="app max" value={liveApp.max} />
        <StatTile label="total max" value={live.max} />
        <StatTile label="samples" value={live.count} unit="" raw />
      </div>
      <div ref={hostRef} className={styles.chartHost} onClick={onChartClick}>
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
            rows={[
              { swatch: APP_COLOR, label: 'App', value: pin.app },
              { swatch: TOTAL_COLOR, label: 'Total', value: pin.v },
            ]}
          />
        )}
        {!pin && tooltip && (
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
  valueColor?: string
}) {
  return (
    <div className={`${styles.tile} ${accent ? styles.accent : ''}`}>
      <div className={styles.label}>{label}</div>
      <div className={styles.value} style={valueColor ? { color: valueColor } : undefined}>
        {value == null ? '—' : raw ? value.toFixed(0) : value.toFixed(1)}
        {value != null && unit && <span className={styles.unit}>{unit}</span>}
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

