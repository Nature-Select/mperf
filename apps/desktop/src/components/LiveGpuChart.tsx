import { useEffect, useRef, useState } from 'react'
import { listen, UnlistenFn } from '@tauri-apps/api/event'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { EVENT_SAMPLE, Sample } from '@/lib/ipc'
import { attachChartResize } from '@/lib/chartResize'
import { MarkerControls } from '@/lib/markers'
import { MarkerOverlay } from './MarkerOverlay'
import { pctColor } from '@/lib/format'
import { clampTooltip, formatClock } from '@/lib/format'
import styles from './chart-shared.module.scss'

const MAX_POINTS = 300

const DEVICE_COLOR = '#7b3df5'   // purple
const RENDERER_COLOR = '#3491fa' // blue
const TILER_COLOR = '#f7ba1e'    // amber

interface Stats {
  cur: number | null
  max: number | null
}

const EMPTY: Stats = { cur: null, max: null }

interface Tooltip {
  left: number
  top: number
  t: number
  device: number | null
  renderer: number | null
  tiler: number | null
}

/// Independent GPU panel — PerfDog convention. iOS gets three lines
/// (Tiler/Renderer/Device Utilization) from the Instruments graphics
/// channel; Android gets the single `Device` line from KGSL/devfreq
/// (the other two have no Android equivalent). Each line renders only
/// when its sample stream produces data.
export function LiveGpuChart({
  active,
  markers,
}: {
  active: boolean
  markers?: MarkerControls
}) {
  const hostRef = useRef<HTMLDivElement>(null)
  const plotRef = useRef<uPlot | null>(null)
  // Same "anchor on first metric we see, backfill the others" pattern as
  // LiveCpuChart. Device is the most universal so we use it as anchor;
  // renderer/tiler align by approximate timestamp (within 1 second).
  const xsRef = useRef<number[]>([])
  // Use `null` (not NaN) as the gap marker — uPlot's `spanGaps:true`
  // only bridges nulls; NaN slots prevent the line from rendering at all.
  const dYsRef = useRef<(number | null)[]>([])
  const rYsRef = useRef<(number | null)[]>([])
  const tYsRef = useRef<(number | null)[]>([])
  const [plot, setPlot] = useState<uPlot | null>(null)
  const wallStartSecRef = useRef<number>(0)

  const [device, setDevice] = useState<Stats>(EMPTY)
  const [renderer, setRenderer] = useState<Stats>(EMPTY)
  const [tiler, setTiler] = useState<Stats>(EMPTY)
  const [tooltip, setTooltip] = useState<Tooltip | null>(null)

  useEffect(() => {
    if (!hostRef.current) return
    const opts: uPlot.Options = {
      width: hostRef.current.clientWidth,
      height: 240,
      scales: { x: { time: true }, y: { range: [0, 100] } },
      series: [
        {},
        {
          label: 'Device %',
          stroke: DEVICE_COLOR,
          width: 1.5,
          fill: 'rgba(123,61,245,0.10)',
          points: { show: false },
          spanGaps: true,
        },
        {
          label: 'Renderer %',
          stroke: RENDERER_COLOR,
          width: 1.2,
          points: { show: false },
          spanGaps: true,
        },
        {
          label: 'Tiler %',
          stroke: TILER_COLOR,
          width: 1.2,
          points: { show: false },
          spanGaps: true,
        },
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
    plotRef.current = new uPlot(
      opts,
      [xsRef.current, dYsRef.current, rYsRef.current, tYsRef.current],
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
  }, [])

  useEffect(() => {
    if (!active) return

    xsRef.current.length = 0
    dYsRef.current.length = 0
    rYsRef.current.length = 0
    tYsRef.current.length = 0
    wallStartSecRef.current = Date.now() / 1000
    plotRef.current?.setData([
      xsRef.current,
      dYsRef.current,
      rYsRef.current,
      tYsRef.current,
    ] as uPlot.AlignedData)
    setDevice(EMPTY)
    setRenderer(EMPTY)
    setTiler(EMPTY)
    setTooltip(null)

    let unlisten: UnlistenFn | undefined
    let cancelled = false

    listen<Sample>(EVENT_SAMPLE, (e) => {
      const s = e.payload
      const isD = s.kind === 'gpu_device_pct'
      const isR = s.kind === 'gpu_renderer_pct'
      const isT = s.kind === 'gpu_tiler_pct'
      if (!isD && !isR && !isT) return

      const t = wallStartSecRef.current + s.ts_us / 1_000_000
      const lastIdx = xsRef.current.length - 1
      const sameBucket =
        lastIdx >= 0 && Math.abs(xsRef.current[lastIdx] - t) < 1.0

      if (sameBucket) {
        if (isD) dYsRef.current[lastIdx] = s.value
        else if (isR) rYsRef.current[lastIdx] = s.value
        else tYsRef.current[lastIdx] = s.value
      } else {
        xsRef.current.push(t)
        dYsRef.current.push(isD ? s.value : null)
        rYsRef.current.push(isR ? s.value : null)
        tYsRef.current.push(isT ? s.value : null)
        if (xsRef.current.length > MAX_POINTS) {
          xsRef.current.shift()
          dYsRef.current.shift()
          rYsRef.current.shift()
          tYsRef.current.shift()
        }
      }
      plotRef.current?.setData([
        xsRef.current,
        dYsRef.current,
        rYsRef.current,
        tYsRef.current,
      ] as uPlot.AlignedData)
      setDevice(computeStats(dYsRef.current))
      setRenderer(computeStats(rYsRef.current))
      setTiler(computeStats(tYsRef.current))
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
        <div className={styles.chartTitle}>GPU</div>
        {/* Hardware-level on both platforms — Android `/sys/class/kgsl
            /.../gpubusy` or Mali devfreq, iOS DTX `services.graphics.
            opengl`. Neither has per-process GPU breakdowns (would need
            Snapdragon Profiler / Mali Streamline). Subtitle keeps the
            same `series · unit` shape as every other chart; the
            implicit device-scope is industry common knowledge for GPU.
            See CLAUDE.md `Per-app vs device-level metric scope`. */}
        <div className={styles.chartSub}>Device + Renderer + Tiler · %</div>
      </div>
      <div className={styles.stats}>
        <StatTile label="device" value={device.cur} accent />
        <StatTile label="device max" value={device.max} />
        <StatTile label="renderer" value={renderer.cur} />
        <StatTile label="tiler" value={tiler.cur} />
      </div>
      <div ref={hostRef} className={styles.chartHost}>
        <MarkerOverlay
          plot={plot}
          wallStartSec={wallStartSecRef.current}
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

function StatTile({
  label,
  value,
  accent = false,
}: {
  label: string
  value: number | null
  accent?: boolean
}) {
  const color = value != null ? pctColor(value) : undefined
  return (
    <div className={`${styles.tile} ${accent ? styles.accent : ''}`}>
      <div className={styles.label}>{label}</div>
      <div className={styles.value} style={color ? { color } : undefined}>
        {value == null ? '—' : value.toFixed(1)}
        {value != null && <span className={styles.unit}>%</span>}
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
