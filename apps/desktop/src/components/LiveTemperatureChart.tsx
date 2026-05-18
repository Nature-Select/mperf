import { useEffect, useRef, useState } from 'react'
import { listen, UnlistenFn } from '@tauri-apps/api/event'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { EVENT_SAMPLE, Sample } from '@/lib/ipc'
import { attachChartResize } from '@/lib/chartResize'
import { MarkerControls } from '@/lib/markers'
import { MarkerOverlay } from './MarkerOverlay'
import { clampTooltip, formatClock, tempColor } from '@/lib/format'
import styles from './chart-shared.module.scss'

const MAX_POINTS = 300

const CTEMP_COLOR = '#f7ba1e'  // amber — CPU/SoC
const BTEMP_COLOR = '#37c2ff'  // cyan — Battery

interface Stats {
  cur: number | null
  max: number | null
}

const EMPTY_STATS: Stats = { cur: null, max: null }

interface Tooltip {
  left: number
  top: number
  t: number
  c: number | null
  b: number | null
}

/// Independent temperature panel — PerfDog convention. Renders CPU temp
/// (CTemp, Android only when /sys/class/thermal is readable) and battery
/// temp (BTemp, both platforms via dumpsys / lockdown). When only one
/// source has data, the other line is just absent.
export function LiveTemperatureChart({
  active,
  markers,
}: {
  active: boolean
  markers?: MarkerControls
}) {
  const hostRef = useRef<HTMLDivElement>(null)
  const plotRef = useRef<uPlot | null>(null)
  /// Two parallel timelines kept in sync by appending a `null` slot
  /// whenever one source ticks ahead of the other. `null` is uPlot's
  /// canonical gap marker — `spanGaps:true` only bridges null gaps, NOT
  /// NaN (NaN was tried first and silently produced empty charts).
  const xsRef = useRef<number[]>([])
  const cYsRef = useRef<(number | null)[]>([])
  const bYsRef = useRef<(number | null)[]>([])
  const wallStartSecRef = useRef<number>(0)
  const [plot, setPlot] = useState<uPlot | null>(null)

  const [cStats, setCStats] = useState<Stats>(EMPTY_STATS)
  const [bStats, setBStats] = useState<Stats>(EMPTY_STATS)
  const [tooltip, setTooltip] = useState<Tooltip | null>(null)

  useEffect(() => {
    if (!hostRef.current) return
    const opts: uPlot.Options = {
      width: hostRef.current.clientWidth,
      height: 200,
      scales: {
        x: { time: true },
        // Auto-scale Y; mobile temps span ~20-80°C in normal use.
        y: {},
      },
      series: [
        {},
        {
          label: 'CTemp °C',
          stroke: CTEMP_COLOR,
          width: 1.5,
          points: { show: false },
          // spanGaps:true so the line connects across NaN slots that
          // belong to the *other* series (CTemp and BTemp tick on
          // different schedules and rarely share a 1-second bucket;
          // each line should be continuous through its own samples).
          spanGaps: true,
        },
        {
          label: 'BTemp °C',
          stroke: BTEMP_COLOR,
          width: 1.5,
          points: { show: false },
          spanGaps: true,
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
          values: (_u, ticks) => ticks.map((t) => `${t.toFixed(0)}°`),
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
              c: cYsRef.current[idx],
              b: bYsRef.current[idx],
            })
          },
        ],
      },
    }
    plotRef.current = new uPlot(
      opts,
      [xsRef.current, cYsRef.current, bYsRef.current],
      hostRef.current,
    )
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
  }, [])

  useEffect(() => {
    if (!active) return

    xsRef.current.length = 0
    cYsRef.current.length = 0
    bYsRef.current.length = 0
    wallStartSecRef.current = Date.now() / 1000
    plotRef.current?.setData([xsRef.current, cYsRef.current, bYsRef.current])
    setCStats(EMPTY_STATS)
    setBStats(EMPTY_STATS)
    setTooltip(null)

    let unlisten: UnlistenFn | undefined
    let cancelled = false

    /// CTemp / BTemp arrive on independent ~2-3s ticks; coalesce by
    /// rounding ts to the nearest second so two close-in-time samples
    /// share an x-bucket on the chart. Fresh tick → push new x; existing
    /// tick → backfill the matching slot.
    listen<Sample>(EVENT_SAMPLE, (e) => {
      const s = e.payload
      const isCpu = s.kind === 'cpu_temp_c'
      const isBat = s.kind === 'battery_temp_c'
      if (!isCpu && !isBat) return

      const t = wallStartSecRef.current + s.ts_us / 1_000_000
      const lastIdx = xsRef.current.length - 1
      const sameBucket =
        lastIdx >= 0 && Math.abs(xsRef.current[lastIdx] - t) < 1.0

      if (sameBucket) {
        if (isCpu) cYsRef.current[lastIdx] = s.value
        else bYsRef.current[lastIdx] = s.value
      } else {
        xsRef.current.push(t)
        cYsRef.current.push(isCpu ? s.value : null)
        bYsRef.current.push(isBat ? s.value : null)
        if (xsRef.current.length > MAX_POINTS) {
          xsRef.current.shift()
          cYsRef.current.shift()
          bYsRef.current.shift()
        }
      }
      plotRef.current?.setData([
        xsRef.current,
        cYsRef.current,
        bYsRef.current,
      ] as uPlot.AlignedData)
      setCStats(computeStats(cYsRef.current))
      setBStats(computeStats(bYsRef.current))
    }).then((fn) => {
      if (cancelled) fn()
      else unlisten = fn
    })

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [active])

  return (
    <div className={styles.chartCard}>
      <div className={styles.chartHeader}>
        <div className={styles.chartTitle}>Temperature</div>
        <div className={styles.chartSub}>CPU + Battery · °C</div>
      </div>
      <div className={styles.stats}>
        <StatTile
          label="cpu"
          value={cStats.cur}
          color={tempColor(cStats.cur)}
          accent
        />
        <StatTile
          label="cpu max"
          value={cStats.max}
          color={tempColor(cStats.max)}
        />
        <StatTile
          label="battery"
          value={bStats.cur}
          color={tempColor(bStats.cur)}
        />
        <StatTile
          label="battery max"
          value={bStats.max}
          color={tempColor(bStats.max)}
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
            {tooltip.c != null && (
              <div
                className={styles.tooltipRow}
                style={{ color: tempColor(tooltip.c) }}
              >
                <span className={styles.tooltipSwatch} style={{ background: CTEMP_COLOR }} />
                CPU&nbsp;<b>{tooltip.c.toFixed(1)}°C</b>
              </div>
            )}
            {tooltip.b != null && (
              <div
                className={styles.tooltipRow}
                style={{ color: tempColor(tooltip.b) }}
              >
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
  color,
  accent = false,
}: {
  label: string
  value: number | null
  color?: string
  accent?: boolean
}) {
  return (
    <div className={`${styles.tile} ${accent ? styles.accent : ''}`}>
      <div className={styles.label}>{label}</div>
      <div className={styles.value} style={color ? { color } : undefined}>
        {value == null ? '—' : value.toFixed(1)}
        {value != null && <span className={styles.unit}>°C</span>}
      </div>
    </div>
  )
}

function computeStats(ys: (number | null)[]): Stats {
  let cur: number | null = null
  let max = -Infinity
  let any = false
  for (const v of ys) {
    if (v == null || !Number.isFinite(v)) continue
    any = true
    if (v > max) max = v
    cur = v
  }
  return { cur, max: any ? max : null }
}
