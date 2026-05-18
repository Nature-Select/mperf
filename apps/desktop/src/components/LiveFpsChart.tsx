import { useEffect, useRef, useState } from 'react'
import { listen, UnlistenFn } from '@tauri-apps/api/event'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { EVENT_SAMPLE, Sample } from '@/lib/ipc'
import { attachChartResize } from '@/lib/chartResize'
import { MarkerControls } from '@/lib/markers'
import { fpsColor, jankColor, stutterColor } from '@/lib/format'
import { computeFpsAdvanced, FpsAdvancedStats } from '@/lib/fps-stats'
import { MarkerOverlay } from './MarkerOverlay'
import { clampTooltip, formatClock } from '@/lib/format'
import { FpsAdvancedPanel } from './FpsAdvancedPanel'
import styles from './chart-shared.module.scss'

const MAX_POINTS = 300 // ~5 min at 1Hz

interface Stats {
  cur: number | null
  min: number | null
  max: number | null
  avg: number | null
  smallJank: number
  jank: number
  bigJank: number
  stutter: number | null
  count: number
}

const EMPTY_STATS: Stats = {
  cur: null,
  min: null,
  max: null,
  avg: null,
  smallJank: 0,
  jank: 0,
  bigJank: 0,
  stutter: null,
  count: 0,
}

interface Tooltip {
  left: number
  top: number
  t: number
  v: number
}

export function LiveFpsChart({
  active,
  platform,
  markers,
}: {
  active: boolean
  /// Used only for the subtitle wording. Android FPS comes from
  /// `dumpsys gfxinfo <pkg>` + per-layer SurfaceFlinger and is genuinely
  /// per-app — switching apps mid-recording stops the target's counter
  /// from advancing → FPS drops toward 0. iOS FPS comes from the DTX
  /// CoreAnimation channel and is screen-level — it reports whatever
  /// is on screen regardless of which app the recording is "for".
  platform: 'android' | 'ios'
  markers?: MarkerControls
}) {
  const hostRef = useRef<HTMLDivElement>(null)
  const plotRef = useRef<uPlot | null>(null)
  const xsRef = useRef<number[]>([])
  const ysRef = useRef<number[]>([])
  const wallStartSecRef = useRef<number>(0)
  const [plot, setPlot] = useState<uPlot | null>(null)

  // Accumulated jank totals (since session start). Per-tick increments are
  // surfaced live; the stat panel shows total counts.
  const totalSmallJankRef = useRef<number>(0)
  const totalJankRef = useRef<number>(0)
  const totalBigJankRef = useRef<number>(0)
  // Stutter is sent as a cumulative ratio; we just remember the latest.
  const stutterRef = useRef<number | null>(null)

  const [live, setLive] = useState<Stats>(EMPTY_STATS)
  const [advanced, setAdvanced] = useState<FpsAdvancedStats | null>(null)
  const [tooltip, setTooltip] = useState<Tooltip | null>(null)

  useEffect(() => {
    if (!hostRef.current) return
    const opts: uPlot.Options = {
      width: hostRef.current.clientWidth,
      height: 240,
      scales: {
        x: { time: true },
        y: { range: [0, 130] }, // covers 120Hz displays with headroom
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
    totalSmallJankRef.current = 0
    totalJankRef.current = 0
    totalBigJankRef.current = 0
    stutterRef.current = null
    plotRef.current?.setData([xsRef.current, ysRef.current])
    setLive(EMPTY_STATS)
    setTooltip(null)

    let unlisten: UnlistenFn | undefined
    let cancelled = false

    listen<Sample>(EVENT_SAMPLE, (e) => {
      const s = e.payload
      if (s.kind === 'small_jank_count') {
        totalSmallJankRef.current += Math.round(s.value)
        setLive((p) => ({ ...p, smallJank: totalSmallJankRef.current }))
        return
      }
      if (s.kind === 'jank_count') {
        totalJankRef.current += Math.round(s.value)
        setLive((p) => ({ ...p, jank: totalJankRef.current }))
        return
      }
      if (s.kind === 'big_jank_count') {
        totalBigJankRef.current += Math.round(s.value)
        setLive((p) => ({ ...p, bigJank: totalBigJankRef.current }))
        return
      }
      if (s.kind === 'stutter') {
        stutterRef.current = s.value
        setLive((p) => ({ ...p, stutter: s.value }))
        return
      }
      if (s.kind !== 'fps') return

      const t = wallStartSecRef.current + s.ts_us / 1_000_000
      xsRef.current.push(t)
      ysRef.current.push(s.value)
      if (xsRef.current.length > MAX_POINTS) {
        xsRef.current.shift()
        ysRef.current.shift()
      }
      plotRef.current?.setData([xsRef.current, ysRef.current])
      setLive((p) => ({
        ...p,
        ...computeFpsStats(ysRef.current),
        smallJank: totalSmallJankRef.current,
        jank: totalJankRef.current,
        bigJank: totalBigJankRef.current,
        stutter: stutterRef.current,
      }))
      setAdvanced(computeFpsAdvanced(ysRef.current))
    }).then((fn) => {
      if (cancelled) fn()
      else unlisten = fn
    })

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [active])

  const curColor = live.cur != null ? fpsColor(live.cur) : undefined

  return (
    <div className={styles.chartCard}>
      <div className={styles.chartHeader}>
        <div className={styles.chartTitle}>FPS / Jank</div>
        {/* Subtitle keeps the same `series · unit [· scope]` shape as
            every other chart. iOS FPS gets a `· 屏幕级` tail because
            its scope (CoreAnimation tree, not per-app) reverses the
            Android default and would otherwise mislead the user — for
            every other chart the scope is either obvious from the
            series names or industry common knowledge. */}
        <div className={styles.chartSub}>
          {platform === 'android'
            ? 'Frame rate + Jank · fps'
            : 'Frame rate · fps · 屏幕级'}
        </div>
      </div>
      <div className={styles.stats}>
        <StatTile label="current" value={live.cur} unit="" accent valueColor={curColor} />
        <StatTile label="min" value={live.min} unit="" />
        <StatTile label="max" value={live.max} unit="" />
        <StatTile label="avg" value={live.avg} unit="" />
        <StatTile
          label="jank"
          value={live.jank}
          unit=""
          raw
          valueColor={jankColor(live.jank)}
          hint={`small: ${live.smallJank} · big: ${live.bigJank}`}
        />
        <StatTile
          label="stutter"
          value={live.stutter == null ? null : live.stutter * 100}
          unit="%"
          valueColor={live.stutter == null ? undefined : stutterColor(live.stutter * 100)}
        />
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
  return (
    <div className={`${styles.tile} ${accent ? styles.accent : ''}`}>
      <div className={styles.label}>
        {label}
        {hint && <span style={{ marginLeft: 6, opacity: 0.6 }}>· {hint}</span>}
      </div>
      <div className={styles.value} style={valueColor ? { color: valueColor } : undefined}>
        {value == null ? '—' : raw ? value.toFixed(0) : value.toFixed(1)}
        {value != null && unit && <span className={styles.unit}>{unit}</span>}
      </div>
    </div>
  )
}

function computeFpsStats(ys: number[]) {
  if (ys.length === 0) return { cur: null, min: null, max: null, avg: null, count: 0 }
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
